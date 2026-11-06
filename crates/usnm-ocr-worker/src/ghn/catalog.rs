//! `usnm-ocr-worker ghn-catalog`: which GHN issues to take, as a dry run.
//!
//! GHN's titles come from its newspapers list and each title's issues from its collection,
//! both IIIF documents kept in a cache directory (`newspapers.json`, `lccn/<lccn>.json`). With
//! [`Fetch`], the missing ones are asked for through the `ghn` policy, one at a time; without
//! it, nothing is asked of anyone. A title is selected when we have none of its pages (no
//! batch in our published `batches.json` lists its LCCN), it isn't taken some other way
//! ([`super::ELSEWHERE`]) and it has issues up to [`Options::through`]. With a rights list,
//! only issues whose DLG statement is NoC-US are kept.
//!
//! It writes `titles.tsv` (every title and the decision), `issues.tsv` (the selected issues,
//! `ghn-fetch`'s input) and `summary.json`, and enqueues nothing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use chrono::NaiveDate;
use serde::Serialize;
use tokio::sync::watch;
use tokio::time::Instant;

use super::iiif::{self, Hosts, IssueRef, TitleIssues};
use super::{field, sha256_hex, write_atomic};
use crate::fetcher::{Engine, Fail, Job, Policies, Reservation};

/// The largest IIIF document read: a title's collection is about 170 bytes an issue.
const MAX_DOC: u64 = 32 << 20;

pub const ISSUES_HEADER: &str = "lccn\tdate\tedition\tmanifest_url\trights";

#[derive(Debug, Clone)]
pub struct Options {
    /// `newspapers.json` and `lccn/<lccn>.json`.
    pub cache: PathBuf,
    /// Our published `batches.json`: each batch's `curated.lccns` are titles we have.
    pub ours: PathBuf,
    pub out: PathBuf,
    /// The last publication date taken; never after [`super::latest`].
    pub through: NaiveDate,
    /// DLG's rights statement per issue: TSV of `lccn`, `date` and the statement's URI.
    pub rights: Option<PathBuf>,
    pub hosts: Hosts,
    pub fetch: Option<Fetch>,
}

/// Asking GHN for missing metadata.
#[derive(Debug, Clone)]
pub struct Fetch {
    /// GHN's budget, shared with `ghn-fetch`.
    pub pacer: PathBuf,
    pub policies: Policies,
    /// At most this many title collections this run.
    pub max_titles: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decision {
    Selected,
    /// We have pages of it: a page-level duplicate check comes first.
    Ours,
    /// Taken some other way.
    Elsewhere,
    NoneInRange,
    NoMetadata,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::Selected => "selected",
            Decision::Ours => "ours",
            Decision::Elsewhere => "elsewhere",
            Decision::NoneInRange => "none-in-range",
            Decision::NoMetadata => "no-metadata",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    /// Always true: nothing is fetched beyond metadata, and nothing enqueued.
    pub dry_run: bool,
    pub enqueued: u64,
    pub through: NaiveDate,
    pub titles: BTreeMap<Decision, usize>,
    pub issues_selected: usize,
    /// Kept out by a rights statement other than NoC-US, or none.
    pub issues_without_noc_us: usize,
    /// False: the selected issues' rights statements weren't checked.
    pub rights_checked: bool,
    pub collections_fetched: usize,
    pub newspapers_sha256: String,
    pub ours_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights_sha256: Option<String>,
    pub generated_at: String,
}

/// Our titles: every `curated.lccns` in `batches.json`, a list or `{"batches": [...]}`.
pub fn our_lccns(v: &serde_json::Value) -> anyhow::Result<HashSet<String>> {
    let batches = v
        .as_array()
        .or_else(|| v.get("batches")?.as_array())
        .context("no batches")?;
    let mut out = HashSet::new();
    for b in batches {
        let lccns = b
            .pointer("/curated/lccns")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten();
        for l in lccns {
            out.insert(l.as_str().context("an LCCN that isn't text")?.to_owned());
        }
    }
    ensure!(!out.is_empty(), "no LCCNs in our batches");
    Ok(out)
}

/// DLG's statements: `(lccn, date)` to URI. A header line starting `lccn` is skipped.
pub fn rights_list(text: &str) -> anyhow::Result<HashMap<(String, NaiveDate), String>> {
    let mut out = HashMap::new();
    for (i, line) in text.lines().enumerate() {
        if line.is_empty() || (i == 0 && line.starts_with("lccn\t")) {
            continue;
        }
        let [lccn, date, uri] = line.split('\t').collect::<Vec<_>>()[..] else {
            bail!("rights line {}: not lccn, date and statement", i + 1);
        };
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .with_context(|| format!("rights line {}: {date}", i + 1))?;
        let uri = uri.trim();
        if let Some(was) = out.insert((lccn.to_owned(), date), uri.to_owned()) {
            ensure!(
                was == uri,
                "rights line {}: {lccn} {date} is already listed as {was}",
                i + 1
            );
        }
    }
    Ok(out)
}

/// One title's row in `titles.tsv`.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleRow {
    pub lccn: String,
    pub label: String,
    pub decision: Decision,
    pub issues_total: usize,
    pub in_range: Vec<IssueRef>,
    pub collection_sha256: String,
}

/// The decision for one title, from its collection if we have it.
pub fn decide(
    title: &iiif::Title,
    collection: Option<(&TitleIssues, &str)>,
    ours: &HashSet<String>,
    through: NaiveDate,
) -> TitleRow {
    let mut row = TitleRow {
        lccn: title.lccn.clone(),
        label: title.label.clone(),
        decision: Decision::NoMetadata,
        issues_total: 0,
        in_range: Vec::new(),
        collection_sha256: String::new(),
    };
    if let Some((c, sha)) = collection {
        row.issues_total = c.issues.len();
        row.in_range = c
            .issues
            .iter()
            .filter(|i| i.date <= through)
            .cloned()
            .collect();
        row.collection_sha256 = sha.to_owned();
    }
    row.decision = if super::ELSEWHERE.iter().any(|(l, _)| *l == title.lccn) {
        Decision::Elsewhere
    } else if ours.contains(&title.lccn) {
        Decision::Ours
    } else if collection.is_none() {
        Decision::NoMetadata
    } else if row.in_range.is_empty() {
        Decision::NoneInRange
    } else {
        Decision::Selected
    };
    row
}

fn cached(cache: &Path, lccn: &str) -> PathBuf {
    cache.join("lccn").join(format!("{lccn}.json"))
}

/// `url` through the engine as an unlisted file, so the policy's byte cap and retries apply,
/// and kept at `path` only once `parse` accepts it.
async fn fetch_doc(
    engine: &Engine,
    url: &str,
    path: &Path,
    parse: impl Fn(&serde_json::Value) -> anyhow::Result<()>,
    stop: &watch::Receiver<bool>,
) -> Result<(), Fail> {
    let local = |e: std::io::Error| Fail::Transient(format!("{}: {e}", path.display()));
    let dir = path.parent().expect("a cache file has a directory");
    std::fs::create_dir_all(dir).map_err(local)?;
    let name = format!(
        "{}.new",
        path.file_name().expect("a file name").to_string_lossy()
    );
    let job = Job {
        name: name.clone(),
        size: MAX_DOC,
        sha256: None,
        dest: dir.join(&name),
        sources: vec![url.to_owned()],
    };
    let far = Instant::now() + Duration::from_secs(24 * 3600);
    let total = AtomicU64::new(0);
    let room = Reservation::new(&total);
    let got = engine
        .fetch(
            &job,
            url,
            &dir.join(format!("{name}.part")),
            stop,
            far,
            &room,
        )
        .await;
    drop(room);
    got?;
    let checked = std::fs::read(&job.dest)
        .map_err(anyhow::Error::from)
        .and_then(|body| Ok(serde_json::from_slice(&body)?))
        .and_then(|v| parse(&v));
    match checked {
        Ok(()) => std::fs::rename(&job.dest, path).map_err(local),
        Err(e) => {
            let _ = std::fs::remove_file(&job.dest);
            Err(Fail::Refused(format!("{url}: {e:#}")))
        }
    }
}

/// A failure that ends the catalogue's asking.
fn fatal(e: Fail) -> anyhow::Error {
    match e {
        Fail::Blocked(m) => anyhow::anyhow!(
            "GHN pushed back or kept failing ({m}); nothing more is asked this run, and a \
             push-back puts its pacer at the slow rate"
        ),
        Fail::Slowed(e) => e,
        Fail::Stopped | Fail::Closed | Fail::RangeRefused => anyhow::anyhow!("stopped"),
        Fail::Transient(m) | Fail::Refused(m) | Fail::Checksum(m) => anyhow::anyhow!(m),
    }
}

fn read_json(path: &Path) -> anyhow::Result<(serde_json::Value, String)> {
    let body = std::fs::read(path).with_context(|| path.display().to_string())?;
    let v = serde_json::from_slice(&body).with_context(|| path.display().to_string())?;
    Ok((v, sha256_hex(&body)))
}

pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Summary> {
    ensure!(
        opts.through <= super::latest(),
        "--through {} is after {}, the last public-domain date the maintainer has set",
        opts.through,
        super::latest()
    );
    std::fs::create_dir_all(opts.cache.join("lccn"))?;
    std::fs::create_dir_all(&opts.out)?;
    // One catalogue asking GHN at a time on this cache: the `.new` and `.part` names are fixed.
    let _lock = match &opts.fetch {
        Some(_) => Some(crate::bulk::hold_lock(
            &opts.cache.join(".ghn-catalog.lock"),
        )?),
        None => None,
    };
    let hosts = &opts.hosts;
    let (engine, _run) = match &opts.fetch {
        Some(f) => {
            let (e, run) = super::engine(&f.policies, &f.pacer, "ghn-catalog")?;
            e.check_not_slowed(&hosts.newspapers()).await?;
            (Some(e), Some(run))
        }
        None => (None, None),
    };
    let list_path = opts.cache.join("newspapers.json");
    if !list_path.exists() {
        let Some(engine) = &engine else {
            bail!(
                "{} is missing; --fetch-metadata asks GHN for it",
                list_path.display()
            );
        };
        let check = |v: &serde_json::Value| iiif::newspapers(v, hosts).map(|_| ());
        fetch_doc(engine, &hosts.newspapers(), &list_path, check, &stop)
            .await
            .map_err(fatal)?;
    }
    let (v, newspapers_sha256) = read_json(&list_path)?;
    let titles = iiif::newspapers(&v, hosts)?;
    let (v, ours_sha256) = read_json(&opts.ours)?;
    let ours = our_lccns(&v)?;
    let rights = match &opts.rights {
        Some(p) => {
            let body = std::fs::read(p).with_context(|| p.display().to_string())?;
            let text = String::from_utf8(body).context("the rights list isn't UTF-8")?;
            Some((rights_list(&text)?, sha256_hex(text.as_bytes())))
        }
        None => None,
    };

    let mut fetched = 0;
    if let (Some(engine), Some(f)) = (&engine, &opts.fetch) {
        let wanted = titles.iter().filter(|t| {
            !ours.contains(&t.lccn)
                && !super::ELSEWHERE.iter().any(|(l, _)| *l == t.lccn)
                && !cached(&opts.cache, &t.lccn).exists()
        });
        for t in wanted.take(f.max_titles.unwrap_or(usize::MAX)) {
            if *stop.borrow() {
                break;
            }
            let check = |v: &serde_json::Value| iiif::title(v, &t.lccn, hosts).map(|_| ());
            let path = cached(&opts.cache, &t.lccn);
            match fetch_doc(engine, &hosts.title(&t.lccn), &path, check, &stop).await {
                Ok(()) => fetched += 1,
                // One title's trouble leaves it without metadata; the next run asks again.
                Err(Fail::Transient(m) | Fail::Refused(m)) => {
                    tracing::warn!(lccn = %t.lccn, error = %m, "no collection for this title");
                }
                Err(e) => return Err(fatal(e)),
            }
        }
    }

    let mut rows = Vec::with_capacity(titles.len());
    for t in &titles {
        let path = cached(&opts.cache, &t.lccn);
        let collection = if path.exists() {
            let (v, sha) = read_json(&path)?;
            Some((iiif::title(&v, &t.lccn, hosts)?, sha))
        } else {
            None
        };
        let c = collection.as_ref().map(|(c, s)| (c, s.as_str()));
        rows.push(decide(t, c, &ours, opts.through));
    }

    let mut titles_tsv = String::from(
        "lccn\tlabel\tdecision\tissues_in_range\tfirst\tlast\tissues_total\tcollection_sha256\n",
    );
    let mut issues_tsv = format!("{ISSUES_HEADER}\n");
    let mut counts = BTreeMap::new();
    let (mut selected, mut without) = (0, 0);
    for r in &rows {
        *counts.entry(r.decision).or_insert(0) += 1;
        let day = |i: Option<&IssueRef>| i.map(|i| i.date.to_string()).unwrap_or_default();
        titles_tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.lccn,
            field(&r.label),
            r.decision.as_str(),
            r.in_range.len(),
            day(r.in_range.first()),
            day(r.in_range.last()),
            r.issues_total,
            r.collection_sha256
        ));
        if r.decision != Decision::Selected {
            continue;
        }
        for i in &r.in_range {
            let statement = match &rights {
                Some((list, _)) => match list.get(&(i.lccn.clone(), i.date)) {
                    Some(s) if super::is_noc_us(s) => s.as_str(),
                    _ => {
                        without += 1;
                        continue;
                    }
                },
                None => "unchecked",
            };
            selected += 1;
            issues_tsv.push_str(&format!(
                "{}\t{}\t{}\t{}\t{statement}\n",
                i.lccn,
                i.date,
                i.edition,
                i.url(hosts)
            ));
        }
    }
    let summary = Summary {
        dry_run: true,
        enqueued: 0,
        through: opts.through,
        titles: counts,
        issues_selected: selected,
        issues_without_noc_us: without,
        rights_checked: rights.is_some(),
        collections_fetched: fetched,
        newspapers_sha256,
        ours_sha256,
        rights_sha256: rights.map(|(_, s)| s),
        generated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    write_atomic(&opts.out.join("titles.tsv"), titles_tsv.as_bytes())?;
    write_atomic(&opts.out.join("issues.tsv"), issues_tsv.as_bytes())?;
    write_atomic(
        &opts.out.join("summary.json"),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(summary)
}
