//! `usnm-ocr-worker loc-text` (#1419): LoC's own text of pages a bulk OCR archive lacks, from
//! loc.gov's item API.
//!
//! LoC lists the Wiregrass (`sn90052158`) in `gu_allmanbrothers_ver01`, but the batch's bulk
//! archive has no file of it, so curation has none of its pages. loc.gov serves each page's
//! text through the `fulltext_service` its item JSON names. This fetches that text a page at a
//! time and keeps it with where it came from.
//!
//! `loc-text enqueue` plans the pages from the batch's bag manifest (`manifest-md5.txt`, one
//! JP2 a page): an issue's JP2s are its pages, seq 1 on. A title-day the batch's curated
//! `counts.json` has any page for is left out, so a page curation holds isn't fetched again.
//! The pages go into `<out>/queue.json`, merged with what's there, so enqueueing again adds
//! nothing twice. `--dry-run` prints the plan and writes nothing. Enqueueing asks LoC nothing,
//! unless `--fetch-manifest` is given and the manifest isn't on disk: then it makes one
//! request for it, the way `run` makes its requests. A dry run never does.
//!
//! `loc-text run` works through the queue an issue at a time. It makes one request for the
//! issue's item JSON (kept as `items/<issue>.json`), then one a page for the
//! `fulltext_service` URL the item JSON gives the page. The answer is kept as it came
//! (`responses/<doc_id>.json`), then the page's record is written (`pages/<doc_id>.json`).
//! The record goes last, so a page with a record is done, and a rerun skips it and doesn't ask
//! again for an item JSON it kept. Every request goes through [`crate::fetcher::Engine`] under
//! LoC's host policy, inside the host's global LoC budget, at least the policy's gap apart
//! unless `--gap-secs` says otherwise, and only to hosts LoC's policy covers. A push-back drops
//! the budget to the slow rate for every process and ends the run. A run doesn't start while
//! the budget is slowed, and stops if it is slowed while a request waits.
//!
//! A record names its sources: the item JSON's URL and the SHA-256 of that response, the text
//! URL and the SHA-256 of that response, and `ocr_source = "loc-api-fulltext"`. It names no
//! archive and carries no archive checksum, because the text didn't come from one.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::time::Instant;
use usnm_core::ids::PageKey;
use usnm_core::time::day_number;

use crate::batch::{self, BagIndex};
use crate::bulk::hold_lock;
use crate::fetcher::{plain, Engine, EngineOptions, Fail, FileBudgets, Policies, LOC};
use crate::loc::{issue_id, item_url, page_texts, read_limited, MAX_BAG_BYTES, MAX_ITEM_BYTES};

/// What the records give as the text's source.
pub const OCR_SOURCE: &str = "loc-api-fulltext";
/// loc.gov's API.
pub const API_ROOT: &str = "https://www.loc.gov";
/// Chronicling America's batch directories, which redirect to the storage service.
pub const BATCHES_ROOT: &str = "https://chroniclingamerica.loc.gov/data/batches/";
/// The shortest gap a `LocAccess` gap override takes: www.loc.gov's `Crawl-Delay`.
pub const MIN_GAP: Duration = Duration::from_secs(5);

const QUEUE: &str = "queue.json";
const STATUS: &str = "status.json";
const LOCK: &str = ".loc-text.lock";
/// Names this command in a slow-down's recorded reason.
const WHO: &str = "LoC text";
/// The largest text service answer read (a dense page's text is under 100 KB).
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// The longest run, in hours: LoC may stop programs that run past 24.
const MAX_HOURS: f64 = 23.0;
/// Transient failures in a row that end a run.
const MAX_TRANSIENT_IN_A_ROW: usize = 5;

/// How requests to LoC are made.
#[derive(Debug, Clone)]
pub struct LocAccess {
    /// The host's global LoC budget.
    pub global_pacer: PathBuf,
    pub policies: Policies,
    /// This process's gap between requests, in place of the LoC policy's: at least `MIN_GAP`.
    pub gap: Option<Duration>,
}

/// The pages to fetch: `<out>/queue.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    /// `<batch>_ver<NN>`.
    pub batch: String,
    /// doc_ids.
    pub pages: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub struct EnqueueOptions {
    pub out: PathBuf,
    /// `<batch>_ver<NN>`.
    pub batch: String,
    /// Only this title's pages; every title's without it.
    pub lccn: Option<String>,
    /// The batch's `manifest-md5.txt`.
    pub bag_manifest: PathBuf,
    /// The batch's curated `counts.json`: pages per title and day.
    pub counts: PathBuf,
    /// `bulk-ocr`'s copy of LoC's datasets table, for the pages LoC lists in the batch.
    pub datasets: Option<PathBuf>,
    pub dry_run: bool,
    /// Fetch a missing bag manifest from under this root (`.../data/batches/`). Never on a
    /// dry run.
    pub fetch_manifest: Option<(String, LocAccess)>,
}

/// What `enqueue` found, printed as JSON.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Plan {
    pub batch: String,
    pub lccn: Option<String>,
    pub dry_run: bool,
    /// The bag manifest's SHA-256, as read.
    pub bag_manifest_sha256: String,
    /// The batch's pages in LoC's datasets table.
    pub listed_pages: Option<u64>,
    /// Pages the batch's counts.json has, every title.
    pub held_pages: u64,
    /// Listed less held: the pages the batch lacks, by LoC's count.
    pub missing_pages: Option<u64>,
    /// The bag's issues and pages (JP2s), every title.
    pub bag_issues: usize,
    pub bag_pages: usize,
    /// Issue directories of the title whose name isn't `<YYYYMMDD><EE>`, left out.
    pub unreadable_issues: Vec<String>,
    /// The bag's issues and pages of the title (of every title without `--lccn`).
    pub title_issues: usize,
    pub title_pages: usize,
    /// Of those, pages on title-days counts.json has pages for, left out.
    pub held_title_pages: usize,
    /// Title-days left out where counts.json and the bag disagree on the pages:
    /// `<lccn> <date>: held <n>, bag <m>`.
    pub mismatched_days: Vec<String>,
    /// Pages to fetch, and their first and last dates.
    pub planned: usize,
    pub first_date: Option<NaiveDate>,
    pub last_date: Option<NaiveDate>,
    /// Of the planned pages, those already queued, and those already fetched.
    pub already_queued: usize,
    pub already_fetched: usize,
    /// Added to the queue (on a dry run, what would be).
    pub added: usize,
    /// The queue's pages afterwards (on a dry run, what it would hold).
    pub queued: usize,
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub out: PathBuf,
    /// loc.gov's API (`https://www.loc.gov`).
    pub api_root: String,
    pub loc: LocAccess,
    /// Stop starting pages after this many hours (at most 23).
    pub max_hours: f64,
    /// At most this many pages this run.
    pub limit: Option<usize>,
}

/// A run's outcome: `<out>/status.json`, and printed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunStatus {
    /// `done`, `incomplete`, `stopped`, `blocked` or `slowed`.
    pub state: String,
    pub queued: usize,
    /// Pages with a record, from this run and earlier ones.
    pub done: usize,
    /// Pages fetched this run.
    pub fetched: usize,
    /// Item JSONs asked for this run.
    pub items_fetched: usize,
    /// Pages that failed this run, `<doc_id>: <why>`. A later run tries them again.
    pub failed: Vec<String>,
    pub error: Option<String>,
}

/// A page's text from loc.gov and where it came from: `<out>/pages/<doc_id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageText {
    pub doc_id: String,
    pub page_key: String,
    pub lccn: String,
    pub date: NaiveDate,
    pub edition: u16,
    pub seq: u16,
    /// The batch LoC lists the page in. The text didn't come from its archive.
    pub batch: String,
    pub batch_version: u16,
    /// [`OCR_SOURCE`].
    pub ocr_source: String,
    /// The text as the service gave it.
    pub text: String,
    pub text_chars: usize,
    pub text_sha256: String,
    /// The service's key for the page: the path of LoC's ALTO.
    pub alto_path: String,
    /// The issue's item JSON, and the SHA-256 of that response (`items/<issue>.json`).
    pub item_url: String,
    pub item_sha256: String,
    /// The text service URL, and the SHA-256 and size of its response
    /// (`responses/<doc_id>.json`).
    pub text_url: String,
    pub response_sha256: String,
    pub response_bytes: usize,
    pub fetched_at: String,
}

fn sha256_hex(b: &[u8]) -> String {
    usnm_core::digest::hex(&Sha256::digest(b))
}

/// `<batch>_ver<NN>` → (`<batch>`, NN).
fn split_batch(dir: &str) -> anyhow::Result<(String, u16)> {
    dir.rsplit_once("_ver")
        .and_then(|(name, v)| {
            let v: u16 = v.parse().ok()?;
            (batch::batch_dir(name, v).as_deref() == Some(dir)).then(|| (name.to_owned(), v))
        })
        .with_context(|| format!("not a batch directory name (`<batch>_ver<NN>`): {dir}"))
}

fn record_path(out: &Path, doc_id: &str) -> PathBuf {
    out.join("pages").join(format!("{doc_id}.json"))
}

/// The doc_ids of the pages with a record.
async fn recorded(out: &Path) -> anyhow::Result<HashSet<String>> {
    let dir = out.join("pages");
    let mut found = HashSet::new();
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(e) => return Err(e).with_context(|| dir.display().to_string()),
    };
    while let Some(e) = entries
        .next_entry()
        .await
        .with_context(|| dir.display().to_string())?
    {
        if let Some(doc_id) = e.file_name().to_str().and_then(|n| n.strip_suffix(".json")) {
            found.insert(doc_id.to_owned());
        }
    }
    Ok(found)
}

/// Writes `body` to a temporary file of this writer's own beside `path`, then renames it over
/// `path`, so a reader sees all of one write or none of it.
async fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = match tokio::fs::write(&tmp, body).await {
        Ok(()) => tokio::fs::rename(&tmp, path).await,
        Err(e) => Err(e),
    };
    if written.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    written
}

async fn read_queue(path: &Path) -> anyhow::Result<Option<Queue>> {
    match tokio::fs::read(path).await {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).with_context(|| path.display().to_string())?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// The fetcher, under LoC's policy and the host's budget, and the policies it holds.
fn engine(loc: &LocAccess, out: &Path) -> anyhow::Result<(Engine, Policies)> {
    let mut policies = loc.policies.clone();
    let p = policies
        .named_mut(LOC)
        .context("the policies have none for LoC")?;
    if let Some(gap) = loc.gap {
        p.min_gap_secs = gap.max(MIN_GAP).as_secs();
    }
    let budgets = FileBudgets::new(None).with_file(
        LOC,
        loc.global_pacer.clone(),
        Some(out.join(".last-request")),
    );
    let engine = Engine::new(
        policies.clone(),
        Box::new(budgets),
        EngineOptions {
            who: WHO.into(),
            rate_file: None,
        },
    )?;
    Ok((engine, policies))
}

/// Refuses a URL LoC's policy doesn't cover, before it's asked: one another policy covers
/// would be outside LoC's budget.
fn on_loc(policies: &Policies, url: &str) -> Result<(), String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    match policies.for_url(&u)? {
        p if p.name == LOC => Ok(()),
        p => Err(format!("{}: {}'s host, not LoC's", plain(&u), p.label)),
    }
}

/// GET `url` through the fetcher, at most `max` bytes.
async fn get(
    engine: &Engine,
    url: &str,
    max: usize,
    stop: &watch::Receiver<bool>,
    deadline: Instant,
) -> Result<bytes::Bytes, Fail> {
    let resp = engine.open(url, None, stop, deadline).await?;
    read_limited(resp, max)
        .await
        .map_err(|e| Fail::Transient(format!("{url}: {e}")))
}

/// The page's ALTO path and text from a text service answer:
/// `{"<ALTO path>": {"full_text": "…", "height": …, "width": …}}`, one entry.
pub fn parse_text(body: &[u8]) -> Result<(String, String), String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("the answer isn't JSON: {e}"))?;
    let page = v.as_object().ok_or("the answer isn't a JSON object")?;
    if page.len() != 1 {
        return Err(format!("the answer has {} entries, not one", page.len()));
    }
    let (path, page) = page.iter().next().expect("one entry");
    let text = page["full_text"]
        .as_str()
        .ok_or("the answer has no full_text")?;
    Ok((path.clone(), text.to_owned()))
}

/// Fetches the bag manifest into `path`: one request.
async fn fetch_manifest(
    root: &str,
    loc: &LocAccess,
    batch: &str,
    path: &Path,
    out: &Path,
    stop: &watch::Receiver<bool>,
) -> anyhow::Result<Vec<u8>> {
    let url = format!("{}/{batch}/manifest-md5.txt", root.trim_end_matches('/'));
    let (engine, policies) = engine(loc, out)?;
    on_loc(&policies, &url).map_err(anyhow::Error::msg)?;
    engine.check_not_slowed(&url).await?;
    let far = Instant::now() + Duration::from_secs(3600);
    let body = get(&engine, &url, MAX_BAG_BYTES, stop, far)
        .await
        .map_err(|f| match f {
            Fail::Slowed(e) => e,
            f => anyhow::anyhow!("the bag manifest: {f:?}"),
        })?;
    let text = std::str::from_utf8(&body).context("the bag manifest isn't UTF-8")?;
    if BagIndex::parse(text).jp2s == 0 {
        bail!("{url}: lists no JP2s");
    }
    write_atomic(path, &body)
        .await
        .with_context(|| path.display().to_string())?;
    Ok(body.to_vec())
}

/// Plans the batch's pages that curation lacks, and queues them unless it's a dry run.
pub async fn enqueue(opts: EnqueueOptions, stop: watch::Receiver<bool>) -> anyhow::Result<Plan> {
    split_batch(&opts.batch)?;
    if let Some(l) = &opts.lccn {
        PageKey::new(l, NaiveDate::MIN, 1, 1).with_context(|| format!("--lccn {l}"))?;
    }
    let _lock = if opts.dry_run {
        None
    } else {
        tokio::fs::create_dir_all(&opts.out).await?;
        Some(hold_lock(&opts.out.join(LOCK))?)
    };
    let manifest = match tokio::fs::read(&opts.bag_manifest).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match &opts.fetch_manifest {
            _ if opts.dry_run => bail!(
                "{} is missing, and a dry run asks LoC nothing (`--fetch-manifest` fetches it)",
                opts.bag_manifest.display()
            ),
            Some((root, loc)) => {
                fetch_manifest(root, loc, &opts.batch, &opts.bag_manifest, &opts.out, &stop).await?
            }
            None => bail!(
                "{} is missing (`--fetch-manifest` fetches it)",
                opts.bag_manifest.display()
            ),
        },
        Err(e) => return Err(e).with_context(|| opts.bag_manifest.display().to_string()),
    };
    let text = std::str::from_utf8(&manifest).context("the bag manifest isn't UTF-8")?;
    let bag = BagIndex::parse(text);
    if bag.jp2s == 0 {
        bail!("{}: lists no JP2s", opts.bag_manifest.display());
    }
    let counts: BTreeMap<String, BTreeMap<u32, u32>> = serde_json::from_slice(
        &tokio::fs::read(&opts.counts)
            .await
            .with_context(|| opts.counts.display().to_string())?,
    )
    .with_context(|| opts.counts.display().to_string())?;
    let mut plan = Plan {
        batch: opts.batch.clone(),
        lccn: opts.lccn.clone(),
        dry_run: opts.dry_run,
        bag_manifest_sha256: sha256_hex(&manifest),
        held_pages: counts
            .values()
            .flat_map(|d| d.values())
            .map(|&n| u64::from(n))
            .sum(),
        bag_pages: bag.jp2s,
        ..Plan::default()
    };
    if let Some(path) = &opts.datasets {
        let v: serde_json::Value = serde_json::from_slice(
            &tokio::fs::read(path)
                .await
                .with_context(|| path.display().to_string())?,
        )
        .with_context(|| path.display().to_string())?;
        let name = format!("{}.tar.bz2", opts.batch);
        let row = crate::bulk::parse_list(&v)?
            .into_iter()
            .find(|a| a.archive_name == name)
            .with_context(|| format!("{}: no row for {name}", path.display()))?;
        plan.listed_pages = row.page_count;
        plan.missing_pages = row.page_count.map(|n| n.saturating_sub(plan.held_pages));
    }
    // The title's pages in the bag, by title-day.
    let mut days: BTreeMap<(String, NaiveDate), Vec<PageKey>> = BTreeMap::new();
    for (lccn, dir, files) in bag.iter() {
        plan.bag_issues += 1;
        if opts.lccn.as_deref().is_some_and(|l| l != lccn) {
            continue;
        }
        let Some((date, edition)) = batch::parse_issue_dir(dir) else {
            plan.unreadable_issues.push(format!("{lccn}/{dir}"));
            continue;
        };
        plan.title_issues += 1;
        let day = days.entry((lccn.to_owned(), date)).or_default();
        for seq in 1..=files.jp2.len() {
            let seq = u16::try_from(seq).context("an issue of over 65,535 pages")?;
            day.push(PageKey::new(lccn, date, edition, seq)?);
        }
    }
    plan.unreadable_issues.sort();
    if let Some(l) = &opts.lccn {
        if plan.title_issues == 0 {
            bail!("the bag manifest lists no issues of {l}");
        }
    }
    let mut planned: Vec<PageKey> = Vec::new();
    for ((lccn, date), pages) in days {
        plan.title_pages += pages.len();
        let held = counts
            .get(&lccn)
            .and_then(|d| d.get(&day_number(date)))
            .copied()
            .unwrap_or(0);
        if held > 0 {
            plan.held_title_pages += pages.len();
            if held as usize != pages.len() {
                plan.mismatched_days
                    .push(format!("{lccn} {date}: held {held}, bag {}", pages.len()));
            }
            continue;
        }
        planned.extend(pages);
    }
    plan.planned = planned.len();
    plan.first_date = planned.iter().map(|k| k.date).min();
    plan.last_date = planned.iter().map(|k| k.date).max();
    let queue_path = opts.out.join(QUEUE);
    let existing = read_queue(&queue_path).await?;
    let fresh = existing.is_none();
    let mut queue = existing.unwrap_or_else(|| Queue {
        batch: opts.batch.clone(),
        pages: BTreeSet::new(),
    });
    if queue.batch != opts.batch {
        bail!(
            "{} queues {}, not {}: one batch to an output directory",
            queue_path.display(),
            queue.batch,
            opts.batch
        );
    }
    let recorded = recorded(&opts.out).await?;
    for k in &planned {
        let doc_id = k.doc_id();
        if recorded.contains(&doc_id) {
            plan.already_fetched += 1;
        }
        if queue.pages.insert(doc_id) {
            plan.added += 1;
        } else {
            plan.already_queued += 1;
        }
    }
    plan.queued = queue.pages.len();
    // A first enqueue writes the queue even when it's empty, so a run reports done.
    if !opts.dry_run && (fresh || plan.added > 0) {
        write_atomic(&queue_path, &serde_json::to_vec(&queue)?)
            .await
            .with_context(|| queue_path.display().to_string())?;
    }
    tracing::info!(
        batch = %plan.batch,
        planned = plan.planned,
        added = plan.added,
        dry_run = plan.dry_run,
        "LoC text pages planned"
    );
    Ok(plan)
}

/// What a failed request means for the run.
enum Step {
    /// This page (or issue) failed; go on.
    Failed(String),
    /// Failed, and may not next time; go on, unless it keeps happening.
    Transient(String),
    /// Stop the run: its state, and the error.
    End(&'static str, Option<anyhow::Error>),
}

fn step(f: Fail) -> Step {
    match f {
        Fail::Blocked(m) => Step::End(
            "blocked",
            Some(anyhow::anyhow!(
                "LoC pushed back ({m}); the global pacer is at the slow rate; stopping"
            )),
        ),
        Fail::Slowed(e) => Step::End("slowed", Some(e)),
        Fail::Stopped | Fail::Closed => Step::End("stopped", None),
        Fail::Transient(m) => Step::Transient(m),
        Fail::Refused(m) | Fail::Checksum(m) => Step::Failed(m),
        Fail::RangeRefused => Step::Failed("a range refused".into()),
    }
}

/// An issue's item JSON, kept or fetched (and then kept), and its SHA-256.
async fn item_json(
    engine: &Engine,
    url: &str,
    path: &Path,
    status: &mut RunStatus,
    stop: &watch::Receiver<bool>,
    deadline: Instant,
) -> Result<(serde_json::Value, String), Step> {
    if let Ok(b) = tokio::fs::read(path).await {
        if let Ok(v) = serde_json::from_slice(&b) {
            return Ok((v, sha256_hex(&b)));
        }
        tracing::warn!(path = %path.display(), "a kept item JSON doesn't parse; asking again");
    }
    status.items_fetched += 1;
    let body = get(engine, url, MAX_ITEM_BYTES, stop, deadline)
        .await
        .map_err(step)?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| Step::Transient(format!("{url}: the item JSON doesn't parse: {e}")))?;
    if v["resources"][0]["files"]
        .as_array()
        .is_none_or(|f| f.is_empty())
    {
        return Err(Step::Failed(format!("{url}: the item JSON lists no pages")));
    }
    write_atomic(path, &body).await.map_err(|e| {
        Step::End(
            "incomplete",
            Some(anyhow::anyhow!("{}: {e}", path.display())),
        )
    })?;
    Ok((v, sha256_hex(&body)))
}

/// One page's text, kept with its record.
#[allow(clippy::too_many_arguments)]
async fn page_text(
    engine: &Engine,
    policies: &Policies,
    out: &Path,
    (batch, batch_version): (&str, u16),
    key: &PageKey,
    (item_url, item_sha256, texts): (&str, &str, &BTreeMap<u16, String>),
    stop: &watch::Receiver<bool>,
    deadline: Instant,
) -> Result<(), Step> {
    let doc_id = key.doc_id();
    let url = texts.get(&key.seq).ok_or_else(|| {
        Step::Failed(format!(
            "{item_url}: the item JSON gives no text for seq {}",
            key.seq
        ))
    })?;
    on_loc(policies, url).map_err(Step::Failed)?;
    let body = get(engine, url, MAX_TEXT_BYTES, stop, deadline)
        .await
        .map_err(step)?;
    let response_sha256 = sha256_hex(&body);
    let (alto_path, text) = parse_text(&body).map_err(|e| Step::Failed(format!("{url}: {e}")))?;
    let io = |p: &Path, e: std::io::Error| {
        Step::End("incomplete", Some(anyhow::anyhow!("{}: {e}", p.display())))
    };
    let raw = out.join("responses").join(format!("{doc_id}.json"));
    write_atomic(&raw, &body).await.map_err(|e| io(&raw, e))?;
    let record = PageText {
        page_key: key.to_string(),
        lccn: key.lccn.clone(),
        date: key.date,
        edition: key.edition,
        seq: key.seq,
        batch: batch.to_owned(),
        batch_version,
        ocr_source: OCR_SOURCE.into(),
        text_chars: text.chars().count(),
        text_sha256: sha256_hex(text.as_bytes()),
        text,
        alto_path,
        item_url: item_url.to_owned(),
        item_sha256: item_sha256.to_owned(),
        text_url: url.clone(),
        response_sha256,
        response_bytes: body.len(),
        fetched_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        doc_id,
    };
    let path = record_path(out, &record.doc_id);
    let json = serde_json::to_vec(&record).expect("a record serializes");
    write_atomic(&path, &json).await.map_err(|e| io(&path, e))
}

async fn write_status(out: &Path, s: &RunStatus) -> anyhow::Result<()> {
    write_atomic(&out.join(STATUS), &serde_json::to_vec(s)?)
        .await
        .context("the status file")
}

/// Fetches the queued pages that have no record yet.
pub async fn run(opts: RunOptions, stop: watch::Receiver<bool>) -> anyhow::Result<RunStatus> {
    if !opts.max_hours.is_finite() {
        bail!("max hours: {} isn't a number of hours", opts.max_hours);
    }
    let hours = opts.max_hours.clamp(0.0, MAX_HOURS);
    tokio::fs::create_dir_all(&opts.out).await?;
    let _lock = hold_lock(&opts.out.join(LOCK))?;
    let queue_path = opts.out.join(QUEUE);
    let queue = read_queue(&queue_path).await?.with_context(|| {
        format!(
            "{} is missing: `loc-text enqueue` first",
            queue_path.display()
        )
    })?;
    let (batch_name, batch_version) = split_batch(&queue.batch)?;
    let mut keys = queue
        .pages
        .iter()
        .map(|d| PageKey::from_doc_id(d).with_context(|| format!("{}: {d}", queue_path.display())))
        .collect::<anyhow::Result<Vec<_>>>()?;
    keys.sort();
    let mut s = RunStatus {
        state: "running".into(),
        queued: keys.len(),
        ..RunStatus::default()
    };
    let recorded = recorded(&opts.out).await?;
    keys.retain(|k| {
        let done = recorded.contains(&k.doc_id());
        s.done += usize::from(done);
        !done
    });
    let (engine, policies) = engine(&opts.loc, &opts.out)?;
    on_loc(&policies, &opts.api_root).map_err(anyhow::Error::msg)?;
    if !keys.is_empty() {
        // After a push-back the budget is at the slow rate for every process for a day; this
        // waits for it to lift (or an operator's reset) rather than add to it.
        if let Err(e) = engine.check_not_slowed(&opts.api_root).await {
            s.state = "slowed".into();
            s.error = Some(format!("{e:#}"));
            write_status(&opts.out, &s).await?;
            return Err(e);
        }
    }
    tracing::info!(queued = s.queued, done = s.done, "LoC text run starting");
    let deadline = Instant::now() + Duration::from_secs_f64(hours * 3600.0);
    let mut tried = 0usize;
    let mut transient_in_a_row = 0usize;
    let mut end: Option<(&'static str, Option<anyhow::Error>)> = None;
    let mut rest = keys.as_slice();
    'issues: while let Some(first) = rest.first() {
        let n = rest
            .iter()
            .take_while(|k| issue_id(k) == issue_id(first))
            .count();
        let (pages, next) = rest.split_at(n);
        rest = next;
        if opts.limit.is_some_and(|l| tried >= l) {
            break;
        }
        if *stop.borrow() || Instant::now() >= deadline {
            end = Some(("stopped", None));
            break;
        }
        let url = item_url(&opts.api_root, first);
        let path = opts
            .out
            .join("items")
            .join(format!("{}.json", issue_id(first).replace('/', "_")));
        let (item, item_sha256) =
            match item_json(&engine, &url, &path, &mut s, &stop, deadline).await {
                Ok(v) => v,
                Err(Step::Failed(m)) => {
                    transient_in_a_row = 0;
                    tried += pages.len();
                    fail(&mut s, pages, &m);
                    continue;
                }
                Err(Step::Transient(m)) => {
                    tried += pages.len();
                    fail(&mut s, pages, &m);
                    transient_in_a_row += 1;
                    if transient_in_a_row >= MAX_TRANSIENT_IN_A_ROW {
                        end = Some(("incomplete", None));
                        break;
                    }
                    continue;
                }
                Err(Step::End(state, e)) => {
                    end = Some((state, e));
                    break;
                }
            };
        let texts = page_texts(&item);
        for k in pages {
            if opts.limit.is_some_and(|l| tried >= l) {
                break 'issues;
            }
            if *stop.borrow() || Instant::now() >= deadline {
                end = Some(("stopped", None));
                break 'issues;
            }
            tried += 1;
            match page_text(
                &engine,
                &policies,
                &opts.out,
                (&batch_name, batch_version),
                k,
                (&url, &item_sha256, &texts),
                &stop,
                deadline,
            )
            .await
            {
                Ok(()) => {
                    transient_in_a_row = 0;
                    s.fetched += 1;
                    s.done += 1;
                }
                Err(Step::Failed(m)) => {
                    transient_in_a_row = 0;
                    fail(&mut s, std::slice::from_ref(k), &m);
                }
                Err(Step::Transient(m)) => {
                    fail(&mut s, std::slice::from_ref(k), &m);
                    transient_in_a_row += 1;
                    if transient_in_a_row >= MAX_TRANSIENT_IN_A_ROW {
                        end = Some(("incomplete", None));
                        break 'issues;
                    }
                }
                Err(Step::End(state, e)) => {
                    end = Some((state, e));
                    break 'issues;
                }
            }
        }
    }
    let (state, error) = match end {
        Some((state, e)) => (state, e),
        None if s.done == s.queued => ("done", None),
        None => ("incomplete", None),
    };
    s.state = state.into();
    if state == "incomplete" && error.is_none() && transient_in_a_row >= MAX_TRANSIENT_IN_A_ROW {
        s.error = Some(format!(
            "{transient_in_a_row} transient failures in a row; stopping"
        ));
    }
    if let Some(e) = &error {
        s.error = Some(format!("{e:#}"));
    }
    tracing::info!(
        state = %s.state,
        fetched = s.fetched,
        failed = s.failed.len(),
        done = s.done,
        queued = s.queued,
        "LoC text run ended"
    );
    write_status(&opts.out, &s).await?;
    match error {
        Some(e) => Err(e),
        None => Ok(s),
    }
}

fn fail(s: &mut RunStatus, pages: &[PageKey], why: &str) {
    for k in pages {
        s.failed.push(format!("{}: {why}", k.doc_id()));
    }
}

#[cfg(test)]
mod tests;
