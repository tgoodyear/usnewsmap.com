//! `usnm-ocr-worker ghn-fetch`: GHN's page images for the issues `ghn-catalog` selected.
//!
//! Each issue's manifest comes first, then its pages' full-size JPEGs, all through the
//! fetcher (#1413) under the `ghn` policy alone, so every request is paced in GHN's budget,
//! within `robots.txt` and the policy's allowed paths, and a push-back ends the run with the
//! budget slowed for every process. GHN lists no size or checksum for its images, so a page's
//! size is capped and its SHA-256 is computed as it arrives. The image goes to
//! `<out>/pages/<lccn>/<year>/<doc_id>.jpg`, then a sidecar beside it (`<doc_id>.json`) records
//! its `ghn:` page id, where it came from (the image and manifest URLs, and the manifest's
//! SHA-256), its size and SHA-256, and its rights basis. A page is done once its sidecar is
//! there, so a rerun goes on where the last stopped. Nothing is enqueued for OCR.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use async_trait::async_trait;
use serde::Serialize;
use tokio::sync::watch;

use super::catalog::ISSUES_HEADER;
use super::iiif::{self, Hosts, IssueRef, Page};
use super::{sha256_hex, write_atomic};
use crate::fetcher::run::RunOptions;
use crate::fetcher::{Attempt, FileJobs, HeldBack, Job, JobStore, Policies, Status};

/// The most a manifest may be: GHN's are about 2.5 KB a page.
pub const MAX_MANIFEST: u64 = 8 << 20;
/// The most a page image may be: GHN's full-size JPEGs are 5-10 MB.
pub const MAX_IMAGE: u64 = 64 << 20;

#[derive(Debug, Clone)]
pub struct Options {
    /// `ghn-catalog`'s `issues.tsv`.
    pub issues: PathBuf,
    pub out: PathBuf,
    /// GHN's budget, shared by every process that asks GHN anything.
    pub pacer: PathBuf,
    pub policies: Policies,
    pub hosts: Hosts,
    /// Stop starting files after this many hours.
    pub max_hours: f64,
    pub min_free_bytes: u64,
    /// At most this many issues started this run.
    pub limit: Option<usize>,
    /// Take issues whose rights statement wasn't checked (a sample, before the rights filter).
    pub unchecked_rights: bool,
}

/// One row of `issues.tsv`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub issue: IssueRef,
    /// DLG's statement, or `unchecked`.
    pub rights: String,
}

/// The rows of `issues.tsv`, each checked: a GHN manifest URL in its canonical form, dated
/// up to 1930, not a title taken elsewhere, and with a NoC-US statement unless `unchecked`.
pub fn read_issues(text: &str, hosts: &Hosts, unchecked: bool) -> anyhow::Result<Vec<Row>> {
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() || (i == 0 && line == ISSUES_HEADER) {
            continue;
        }
        let n = i + 1;
        let [lccn, date, edition, url, rights] = line.split('\t').collect::<Vec<_>>()[..] else {
            bail!("issues line {n}: not {ISSUES_HEADER}");
        };
        let issue = hosts
            .issue_of(url)
            .with_context(|| format!("issues line {n}: not a GHN manifest: {url}"))?;
        ensure!(
            issue.lccn == lccn
                && issue.date.to_string() == date
                && issue.edition.to_string() == edition,
            "issues line {n}: the manifest URL names another issue"
        );
        ensure!(
            issue.date <= super::latest(),
            "issues line {n}: {date} is after {}",
            super::latest()
        );
        if let Some((_, why)) = super::ELSEWHERE.iter().find(|(l, _)| *l == lccn) {
            bail!("issues line {n}: {lccn} isn't taken here: {why}");
        }
        if !super::is_noc_us(rights) {
            ensure!(
                rights == "unchecked",
                "issues line {n}: {rights} isn't a public-domain claim"
            );
            ensure!(
                unchecked,
                "issues line {n}: its rights statement wasn't checked (--unchecked-rights takes such issues)"
            );
        }
        rows.push(Row {
            issue,
            rights: rights.to_owned(),
        });
    }
    Ok(rows)
}

/// A page's sidecar: its identity and provenance.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Sidecar {
    pub page_id: String,
    pub source: String,
    pub lccn: String,
    pub date: String,
    pub edition: u16,
    pub seq: u16,
    pub image_url: String,
    pub image_service: String,
    pub manifest_url: String,
    pub manifest_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u64>,
    pub bytes: u64,
    pub sha256: String,
    pub fetched_at: String,
    /// Why it may be taken: published up to 1930, and DLG's statement or `unchecked`.
    pub published_through: String,
    pub rights: String,
}

/// Where a job's file goes and what it is.
#[derive(Debug, Clone)]
enum Kind {
    Manifest(Row),
    Page {
        page: Page,
        row: Row,
        manifest_url: String,
        manifest_sha256: String,
    },
}

#[derive(Debug, Default)]
struct Queue {
    issues: VecDeque<Row>,
    jobs: VecDeque<(Job, Kind)>,
    claimed: HashMap<String, Kind>,
    started: usize,
    limit: Option<usize>,
    limit_hit: bool,
    /// A job was put back: the run is ending, so nothing more is handed out.
    over: bool,
    problems: usize,
}

/// GHN's jobs, planned an issue at a time from what's on disk. Status and verification
/// failures go to a [`FileJobs`].
struct GhnJobs {
    out: PathBuf,
    hosts: Hosts,
    files: FileJobs,
    q: Mutex<Queue>,
}

fn manifest_path(out: &Path, issue: &IssueRef) -> PathBuf {
    out.join("manifests")
        .join(&issue.lccn)
        .join(format!("{}.json", issue.stem()))
}

fn page_dir(out: &Path, page: &Page) -> PathBuf {
    out.join("pages")
        .join(&page.key.lccn)
        .join(page.key.date.format("%Y").to_string())
}

/// The page's image and sidecar.
pub fn page_paths(out: &Path, page: &Page) -> (PathBuf, PathBuf) {
    let dir = page_dir(out, page);
    let doc = page.key.doc_id();
    (
        dir.join(format!("{doc}.jpg")),
        dir.join(format!("{doc}.json")),
    )
}

/// Whether the file at `path` is a whole JPEG: it ends with its end-of-image marker (a decoder
/// may pad a cut-off scan rather than fail) and it decodes.
fn whole_jpeg(path: &Path) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let end = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    if !bytes[..end].ends_with(&[0xFF, 0xD9]) {
        return Err("no end-of-image marker".to_owned());
    }
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .map(drop)
        .map_err(|e| e.to_string())
}

impl GhnJobs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.q.lock().expect("ghn jobs lock")
    }

    fn problem(&self, name: &str, message: &str) {
        tracing::warn!(file = %name, error = %message, "ghn");
        self.lock().problems += 1;
        let at = chrono::Utc::now().format("%FT%TZ");
        let line = format!("{at}\t{name}\t{}\n", super::field(message));
        let path = self.out.join("ghn-problems.tsv");
        let r = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
        if let Err(e) = r {
            tracing::warn!(path = %path.display(), error = %e, "can't record a problem");
        }
    }

    /// The issue's pages and the manifest's SHA-256, if it's on disk; an error if it's there
    /// but isn't this issue's manifest.
    fn manifest(&self, row: &Row) -> anyhow::Result<Option<(Vec<Page>, String)>> {
        let path = manifest_path(&self.out, &row.issue);
        let body = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).context(path.display().to_string()),
        };
        let v = serde_json::from_slice(&body).context("the manifest isn't JSON")?;
        let pages = iiif::issue(&v, &row.issue, &self.hosts)?;
        Ok(Some((pages, sha256_hex(&body))))
    }

    /// What's still to do for `row`: its manifest, or the pages it lists without a sidecar.
    fn plan(&self, row: &Row) -> anyhow::Result<Vec<(Job, Kind)>> {
        let path = manifest_path(&self.out, &row.issue);
        let manifest_url = row.issue.url(&self.hosts);
        let pages = self.manifest(row).unwrap_or_else(|e| {
            tracing::warn!(issue = %row.issue.stem(), error = %format!("{e:#}"), "asking for the manifest again");
            None
        });
        let Some((pages, sha)) = pages else {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let job = Job {
                name: format!("manifest_{}", row.issue.stem()),
                size: MAX_MANIFEST,
                sha256: None,
                dest: path,
                sources: vec![manifest_url],
            };
            return Ok(vec![(job, Kind::Manifest(row.clone()))]);
        };
        let mut jobs = Vec::new();
        for page in pages {
            let (image, sidecar) = page_paths(&self.out, &page);
            if sidecar.exists() && image.exists() {
                continue;
            }
            // A sidecar describes the bytes it was written for: none may outlive them, or a
            // replacement interrupted before its own sidecar would look done.
            match std::fs::remove_file(&sidecar) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context(sidecar.display().to_string()),
            }
            std::fs::create_dir_all(page_dir(&self.out, &page))?;
            let job = Job {
                name: page.key.doc_id(),
                size: MAX_IMAGE,
                sha256: None,
                dest: image,
                sources: vec![page.image.clone()],
            };
            let kind = Kind::Page {
                page,
                row: row.clone(),
                manifest_url: manifest_url.clone(),
                manifest_sha256: sha.clone(),
            };
            jobs.push((job, kind));
        }
        Ok(jobs)
    }

    async fn page_done(
        &self,
        job: &Job,
        how: &Attempt,
        page: &Page,
        row: &Row,
        manifest: (&str, &str),
    ) -> anyhow::Result<()> {
        let Attempt::Fetched { size, sha256, .. } = how else {
            return Ok(());
        };
        let dest = job.dest.clone();
        let whole = tokio::task::spawn_blocking(move || whole_jpeg(&dest))
            .await
            .context("checking a page image")?;
        if let Err(why) = whole {
            let _ = std::fs::remove_file(&job.dest);
            self.problem(&job.name, &format!("not a whole JPEG ({why}); removed"));
            return Ok(());
        }
        let (manifest_url, manifest_sha256) = manifest;
        let sidecar = Sidecar {
            page_id: page.id.to_string(),
            source: page.id.source().to_string(),
            lccn: page.key.lccn.clone(),
            date: page.key.date.to_string(),
            edition: page.key.edition,
            seq: page.key.seq,
            image_url: page.image.clone(),
            image_service: page.service.clone(),
            manifest_url: manifest_url.to_owned(),
            manifest_sha256: manifest_sha256.to_owned(),
            width: page.width,
            height: page.height,
            bytes: *size,
            sha256: sha256.clone(),
            fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            published_through: super::latest().to_string(),
            rights: row.rights.clone(),
        };
        let (_, path) = page_paths(&self.out, page);
        write_atomic(&path, &serde_json::to_vec_pretty(&sidecar)?)?;
        Ok(())
    }

    /// Whether every issue in the list is done, as far as this run saw.
    fn complete(&self) -> bool {
        let q = self.lock();
        q.issues.is_empty()
            && q.jobs.is_empty()
            && q.claimed.is_empty()
            && !q.over
            && !q.limit_hit
            && q.problems == 0
    }
}

#[async_trait]
impl JobStore for GhnJobs {
    async fn claim(&self) -> anyhow::Result<Option<Job>> {
        loop {
            let row = {
                let mut q = self.lock();
                if q.over {
                    return Ok(None);
                }
                if let Some((job, kind)) = q.jobs.pop_front() {
                    q.claimed.insert(job.name.clone(), kind);
                    return Ok(Some(job));
                }
                if q.limit.is_some_and(|l| q.started >= l) {
                    q.limit_hit = !q.issues.is_empty();
                    return Ok(None);
                }
                match q.issues.pop_front() {
                    Some(row) => row,
                    None => return Ok(None),
                }
            };
            let jobs = self.plan(&row)?;
            let mut q = self.lock();
            if !jobs.is_empty() {
                q.started += 1;
                q.jobs.extend(jobs);
            }
        }
    }

    async fn release(&self, job: &Job) -> anyhow::Result<()> {
        let mut q = self.lock();
        q.claimed.remove(&job.name);
        q.over = true;
        Ok(())
    }

    async fn finished(&self, job: &Job, how: &Attempt) -> anyhow::Result<()> {
        let kind = self.lock().claimed.remove(&job.name);
        if let Attempt::Failed { message } = how {
            self.problem(&job.name, message);
            return Ok(());
        }
        match kind {
            Some(Kind::Manifest(row)) => {
                if let Err(e) = self.manifest(&row) {
                    let _ = std::fs::remove_file(&job.dest);
                    self.problem(&job.name, &format!("{e:#}; removed"));
                    return Ok(());
                }
                // Its pages go next, before another issue starts.
                let jobs = self.plan(&row)?;
                let mut q = self.lock();
                for j in jobs.into_iter().rev() {
                    q.jobs.push_front(j);
                }
            }
            Some(Kind::Page {
                page,
                row,
                manifest_url,
                manifest_sha256,
            }) => {
                self.page_done(job, how, &page, &row, (&manifest_url, &manifest_sha256))
                    .await?
            }
            None => {}
        }
        Ok(())
    }

    async fn held_back(&self, rows: &[HeldBack]) -> anyhow::Result<()> {
        self.files.held_back(rows).await
    }

    async fn checksum_failures(&self) -> anyhow::Result<u64> {
        self.files.checksum_failures().await
    }

    async fn checksum_failed(&self, job: &Job, detail: &str) -> anyhow::Result<u64> {
        self.files.checksum_failed(job, detail).await
    }

    async fn status(&self, status: &Status) -> anyhow::Result<()> {
        self.files.status(status).await
    }
}

/// `ghn-status.json` counts files (manifests and pages); its state is `done` once every issue
/// listed is.
pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Status> {
    let text =
        std::fs::read_to_string(&opts.issues).with_context(|| opts.issues.display().to_string())?;
    let rows = read_issues(&text, &opts.hosts, opts.unchecked_rights)?;
    std::fs::create_dir_all(&opts.out)?;
    // One GHN fetch at a time on this output.
    let _lock = crate::bulk::hold_lock(&opts.out.join(".ghn.lock"))?;
    let part_dir = opts.out.join(".part");
    std::fs::create_dir_all(&part_dir)?;
    let (engine, _run) = super::engine(&opts.policies, &opts.pacer, "ghn-fetch")?;
    let engine = Arc::new(engine);
    // After a push-back GHN's budget is slowed for a day; a run waits for it rather than add to it.
    engine.check_not_slowed(&opts.hosts.newspapers()).await?;
    tracing::info!(issues = rows.len(), "GHN fetch starting");
    let store = Arc::new(GhnJobs {
        out: opts.out.clone(),
        hosts: opts.hosts.clone(),
        files: FileJobs::new(Vec::new(), opts.out.join("ghn-status.json"), None),
        q: Mutex::new(Queue {
            issues: rows.into(),
            limit: opts.limit,
            ..Queue::default()
        }),
    });
    let result = crate::fetcher::run::run(
        engine,
        Arc::clone(&store) as Arc<dyn JobStore>,
        RunOptions {
            part_dir,
            out: opts.out.clone(),
            workers: 1,
            run_for: Duration::from_secs_f64(opts.max_hours.max(0.0) * 3600.0),
            min_free_bytes: opts.min_free_bytes,
            limit: None,
            max_checksum_failures: None,
        },
        Status::default(),
        stop,
    )
    .await;
    let mut s = result?;
    // The fetcher can't know how many files an issue holds before its manifest comes.
    if matches!(s.state.as_str(), "done" | "incomplete") {
        s.state = if store.complete() {
            "done"
        } else if store.lock().limit_hit {
            "limit"
        } else {
            "incomplete"
        }
        .to_owned();
        store.status(&s).await?;
    }
    Ok(s)
}
