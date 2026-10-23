//! Finishing a release whose main index passed its exact count but that
//! failed later (`usnm-ingest release-finish`, #283). The case it is for: a
//! full rebuild's merge wait (`USNM_MERGE_TIMEOUT_SECS`, 4 hours by
//! default) runs out with merges still queued. The main index then holds
//! every page, counted, but isn't merged; the run has no Japanese index,
//! snapshot or pointer, and its record says it failed. A release refuses a
//! version name that exists, so it can't be run again on the same index.
//!
//! The finish runs the release once more on that run (`Release::finish`):
//!
//! 1. It refuses unless the evidence says the index passed its count:
//!    the run failed (or stopped while building), the run's own log has the
//!    merge wait's lines for its main index (logged only once the count
//!    matched), the run's last `ops/release-progress` says how many
//!    documents it sent, and the index's published splits hold exactly
//!    that many ([`Resume::load`], `Release::finish`).
//! 2. It plans the release again from the same inputs. The run record the
//!    plan comes to must be the one the run recorded: version and index
//!    names, batches, pages, duplicates, rollback and build record
//!    (features, templates, writer tuning, engine).
//! 3. It replays the main index's build: every batch's documents are made
//!    again exactly as the release made them, and not sent. The writer
//!    merges meanwhile, from when it starts. The replay must come to the
//!    count the run sent and the index holds, decade by decade, and to the
//!    numbers the run logged as it went ([`RunLog`]).
//! 4. It counts, merges and seals the index as the release does
//!    (`IndexSink::finish`), with its own deadline, then runs the rest of
//!    the release as it is: our OCR's files, the Japanese index, the
//!    layout, the snapshot, the pointer and the run record.
//!
//! What the tail needs from the build, which the stopped process held in
//! memory, and where the finish gets it:
//!
//! - **The documents sent** (the count `finish` checks, `IndexRun::docs`):
//!   the replay's count, checked against the run's release progress and the
//!   index.
//! - **The page-language moves** (`page_languages.json`, the page-language
//!   baselines, the Japanese index's languages): recorded from the replayed
//!   documents, as the build records them.
//! - **The pages given our Latin text in place of LoC's** (`ja_latin.json`,
//!   `ocr_ja.json`'s `latin.added`, the credits): from the replay; their
//!   count is checked against the run's "indexed our Japanese OCR's Latin
//!   text" line.
//! - **American Stories' record** (`american_stories.json`, the manifest's
//!   summary): from the replay, which reads each batch's parts again; each
//!   batch's line and the totals are checked against the run's.
//! - **Our OCR's tally and its pages** (`ocr_local.json`,
//!   `ocr_local_pages.bin`, the manifest's summary, the credits): from the
//!   replay, which loads each batch's titles' parts again; each batch's
//!   line is checked against the run's.
//! - **Everything planned before the build** (the overlay, the duplicate
//!   plan, the Latin pages, the listings): planned again, the same way.
//!
//! Only the replay can give the moves and the replaced pages: they come
//! from each document's text. So the finish reads every batch's curated
//! parts, and American Stories' parts, from the lake again, as the run did.
//! It writes nothing there ([`AsOf`] refuses writes).
//!
//! The listings that grow while a release runs (our OCR's parts, its
//! revocations, the overlay's and American Stories' parts) are taken as of
//! the run's start ([`AsOf`]): objects created since are left out. The
//! run's own log lines for them then say whether the finish sees what the
//! run saw.

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{bail, ensure, Context};
use async_trait::async_trait;
use serde_json::{Map, Value};
use usnm_store::{ByteStream, ObjectInfo, ObjectStore, StoreError};

use crate::state::{IndexRun, RunStatus, State};

/// How long a finish waits for the index's merges by default: a day, since
/// it is run when the release's 4 hours weren't enough.
pub const DEFAULT_TIMEOUT_SECS: u64 = 24 * 3600;

/// The release's line once it has its version's names (`version`, `index`).
pub const BUILDING_LINE: &str = "building index";
/// The line a finish logs in its place, so the run's own stays the only one.
pub const FINISHING_LINE: &str = "finishing index";
/// The merge wait's progress line (`index`), logged only after the count.
pub const MERGES_LINE: &str = "release merges";

/// The planning lines a finish checks against the run's: the last of each
/// before its "building index" line.
pub const OVERLAY_LINE: &str = "Japanese OCR overlay";
pub const OCR_LOCAL_LINE: &str = "our OCR in ocr-local";
pub const STORIES_LINE: &str = "American Stories' text";
const PLANNED: [&str; 3] = [OVERLAY_LINE, OCR_LOCAL_LINE, STORIES_LINE];

/// The build lines a finish checks against the run's: the first of each
/// (by batch, for a batch's) after its "building index" line.
pub const BATCH_OCR_LOCAL_LINE: &str = "our OCR for the batch's titles";
pub const BATCH_STORIES_LINE: &str = "American Stories' text for the batch";
pub const STORIES_INDEXED_LINE: &str = "American Stories' text indexed";
pub const LATIN_INDEXED_LINE: &str = "indexed our Japanese OCR's Latin text";
pub const DECADES_SENT_LINE: &str = "sent the pages a decade at a time";
const BUILT: [&str; 5] = [
    BATCH_OCR_LOCAL_LINE,
    BATCH_STORIES_LINE,
    STORIES_INDEXED_LINE,
    LATIN_INDEXED_LINE,
    DECADES_SENT_LINE,
];

/// What the stopped run logged (its JSON log, `release.log` on-prem), for
/// the finish to check its plan and replay against.
#[derive(Debug, Default)]
pub struct RunLog {
    /// Where it was read from, for messages.
    from: String,
    planned: HashMap<String, Map<String, Value>>,
    built: HashMap<(String, Option<String>), Map<String, Value>>,
    /// It has the merge wait's lines for the run's main index.
    merging: bool,
}

impl RunLog {
    /// Read `path`, the log of the run of `version`, whose main index is
    /// `index`.
    pub fn read(path: &Path, version: &str, index: &str) -> anyhow::Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
        Self::parse(
            std::io::BufReader::new(file),
            &path.display().to_string(),
            version,
            index,
        )
    }

    /// [`RunLog::read`] from `reader`, named `from` in messages. Lines that
    /// aren't JSON log lines (`{"fields": {"message": …}}`) are skipped.
    pub fn parse(
        mut reader: impl BufRead,
        from: &str,
        version: &str,
        index: &str,
    ) -> anyhow::Result<Self> {
        let mut out = RunLog {
            from: from.to_owned(),
            ..RunLog::default()
        };
        // The planning lines since the last release started building.
        let mut since: HashMap<String, Map<String, Value>> = HashMap::new();
        let mut found = false;
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let Ok(v) = serde_json::from_slice::<Value>(&buf) else {
                continue;
            };
            let Some(Value::Object(fields)) = v.get("fields") else {
                continue;
            };
            let Some(message) = fields.get("message").and_then(Value::as_str) else {
                continue;
            };
            if message == BUILDING_LINE {
                let ours = fields.get("version").and_then(Value::as_str) == Some(version);
                match (ours, found) {
                    (true, true) => bail!("{from} has two runs of {version}"),
                    (true, false) => {
                        ensure!(
                            fields.get("index").and_then(Value::as_str) == Some(index),
                            "{from}: the run of {version} built {}, not {index}",
                            fields.get("index").unwrap_or(&Value::Null)
                        );
                        found = true;
                        out.planned = std::mem::take(&mut since);
                    }
                    // Another release: its planning isn't this run's.
                    (false, false) => since.clear(),
                    (false, true) => break,
                }
                continue;
            }
            if !found {
                if PLANNED.contains(&message) {
                    since.insert(message.to_owned(), fields.clone());
                }
                continue;
            }
            if message == MERGES_LINE && fields.get("index").and_then(Value::as_str) == Some(index)
            {
                out.merging = true;
            }
            if BUILT.contains(&message) {
                let batch = fields
                    .get("batch")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                out.built
                    .entry((message.to_owned(), batch))
                    .or_insert_with(|| fields.clone());
            }
        }
        ensure!(
            found,
            "{from} has no `{BUILDING_LINE}` line for {version}: is it the run's log?"
        );
        Ok(out)
    }

    /// Whether the run waited for its main index's merges: it only does once
    /// the index holds every document it sent.
    pub fn merging(&self) -> bool {
        self.merging
    }

    /// The documents the run sent a decade at a time, from its "sent the
    /// pages a decade at a time" line (a base laid out by decade only).
    pub fn decades_sent(&self) -> Option<u64> {
        self.built
            .get(&(DECADES_SENT_LINE.to_owned(), None))
            .and_then(|f| f.get("pages"))
            .and_then(Value::as_u64)
    }

    /// Check a planning line the finish logs (or `None`: it logs none)
    /// against the run's.
    pub fn check_planned(&self, message: &str, ours: Option<Value>) -> anyhow::Result<()> {
        self.compare(message, None, self.planned.get(message), ours)
    }

    /// Check a build line the finish logs, of `batch` if it is a batch's,
    /// against the run's.
    pub fn check_built(
        &self,
        message: &str,
        batch: Option<&str>,
        ours: Option<Value>,
    ) -> anyhow::Result<()> {
        let key = (message.to_owned(), batch.map(str::to_owned));
        self.compare(message, batch, self.built.get(&key), ours)
    }

    fn compare(
        &self,
        message: &str,
        batch: Option<&str>,
        logged: Option<&Map<String, Value>>,
        ours: Option<Value>,
    ) -> anyhow::Result<()> {
        let at = match batch {
            Some(b) => format!("`{message}` for {b}"),
            None => format!("`{message}`"),
        };
        let why = "the finish doesn't see what the run saw, so it would publish another \
                   version than the run built; not finishing";
        match (logged, ours) {
            (None, None) => Ok(()),
            (Some(_), None) => bail!(
                "the run logged {at} and the finish has none ({}); {why}",
                self.from
            ),
            (None, Some(_)) => bail!(
                "the finish logs {at} and the run didn't ({}); {why}",
                self.from
            ),
            (Some(logged), Some(ours)) => {
                let ours = ours.as_object().context("a line's fields")?;
                for (k, v) in ours {
                    let had = logged.get(k).unwrap_or(&Value::Null);
                    if had != v {
                        bail!(
                            "{at}: the run logged {k} = {had}, the finish has {v} ({}); {why}",
                            self.from
                        );
                    }
                }
                Ok(())
            }
        }
    }
}

/// The run a finish takes up, and the evidence that its main index holds
/// every document the run sent.
#[derive(Debug)]
pub struct Resume {
    pub run: IndexRun,
    pub etag: String,
    /// The documents the run had sent by its last `ops/release-progress`
    /// (every 30 s while it built and waited for merges), if it recorded
    /// any.
    pub progress_sent: Option<u64>,
    pub log: RunLog,
}

impl Resume {
    /// The run of `version` from `state`, its log from `log`, checked
    /// before anything starts: a run that failed (or stopped while it
    /// built), not published, whose log shows the merge wait for its main
    /// index, and whose last release progress is its own, with every
    /// document sent. The index's own count comes next
    /// (`Release::finish`).
    pub async fn load(
        state: &State,
        reference: &dyn ObjectStore,
        version: &str,
        log: &Path,
    ) -> anyhow::Result<Self> {
        let (run, etag) = state
            .run(version)
            .await?
            .with_context(|| format!("the state has no run named {version}"))?;
        ensure!(
            matches!(run.status, RunStatus::Failed | RunStatus::Building),
            "{version} is {:?}: only a release that stopped is finished",
            run.status
        );
        ensure!(
            !run.new_index.is_empty() && run.batch_count.is_some(),
            "{version} wrote no main index"
        );
        for pointer in ["current.json", crate::onprem::PENDING_POINTER] {
            if let Some(bytes) = reference.get(pointer).await? {
                let v: Value = serde_json::from_slice(&bytes).context(pointer)?;
                ensure!(
                    v["index_version"].as_str() != Some(version),
                    "{pointer} names {version} already: it was published"
                );
            }
        }
        let log = RunLog::read(log, version, &run.new_index)?;
        ensure!(
            log.merging(),
            "{} has no `{MERGES_LINE}` line for {}: the run stopped before its main index passed \
             the exact count, so there is nothing to finish. Release again in a new run",
            log.from,
            run.new_index
        );
        // A release records its progress every 30 s: a run that stopped
        // soon after its last page may not have recorded every one.
        let progress_sent = state
            .release_progress()
            .await?
            .filter(|p| p.index_version == version && p.docs_sent > 0)
            .map(|p| p.docs_sent);
        let resume = Self {
            run,
            etag,
            progress_sent,
            log,
        };
        tracing::info!(
            %version,
            index = %resume.run.new_index,
            status = ?resume.run.status,
            progress_sent = resume.progress_sent,
            decades_sent = resume.log.decades_sent(),
            last_error = resume.run.last_error.as_deref().unwrap_or(""),
            "finishing a release that stopped after its count"
        );
        Ok(resume)
    }

    /// Check the documents `index` holds where searches see them against
    /// what the run recorded it sent: its release progress, which counts
    /// every document once the merge wait has run 30 s, and its "sent the
    /// pages a decade at a time" line (a base laid out by decade). Either
    /// may be missing; one that is there must agree.
    pub fn check_count(&self, index: &str, held: u64) -> anyhow::Result<()> {
        for (what, sent) in [
            ("its last release progress", self.progress_sent),
            ("its log", self.log.decades_sent()),
        ] {
            if let Some(sent) = sent {
                ensure!(
                    held == sent,
                    "`{index}` holds {held} documents in its published splits; the run sent \
                     {sent}, by {what}. Its main index isn't complete: not finishing"
                );
            }
        }
        Ok(())
    }

    /// The run's version and main index, which a release names
    /// `pages-v{date}-{n}` and `pages-{base|delta}-{date}-{n}`.
    pub fn names(&self, full: bool) -> anyhow::Result<(String, String)> {
        let version = self.run.index_version.clone();
        let rest = version
            .strip_prefix("pages-v")
            .with_context(|| format!("`{version}` isn't a release's version name"))?;
        let kind = if full { "base" } else { "delta" };
        let index = format!("pages-{kind}-{rest}");
        ensure!(
            self.run.new_index == index,
            "{version} wrote `{}`, but this finish plans `{index}`",
            self.run.new_index
        );
        Ok((version, index))
    }

    /// Check the run record the finish's plan comes to against the one the
    /// run recorded: the same plan, or nothing is finished.
    pub fn check_run(&self, planned: &IndexRun) -> anyhow::Result<()> {
        let r = &self.run;
        let fields: [(&str, Value, Value); 9] = [
            ("id", r.id.clone().into(), planned.id.clone().into()),
            (
                "index_version",
                r.index_version.clone().into(),
                planned.index_version.clone().into(),
            ),
            ("full", r.full.into(), planned.full.into()),
            (
                "indexes",
                r.indexes.clone().into(),
                planned.indexes.clone().into(),
            ),
            (
                "new_index",
                r.new_index.clone().into(),
                planned.new_index.clone().into(),
            ),
            (
                "batch_count",
                r.batch_count.into(),
                planned.batch_count.into(),
            ),
            ("pages", r.pages.into(), planned.pages.into()),
            (
                "duplicate_pages",
                r.duplicate_pages.into(),
                planned.duplicate_pages.into(),
            ),
            (
                "previous_version",
                r.previous_version.clone().into(),
                planned.previous_version.clone().into(),
            ),
        ];
        for (name, had, now) in fields {
            ensure!(
                had == now,
                "the run recorded {name} = {had}; planned again, it is {now}. Finish with the \
                 release's own settings and inputs"
            );
        }
        let (had, now) = (
            r.build.clone().unwrap_or(Value::Null),
            planned.build.clone().unwrap_or(Value::Null),
        );
        ensure!(
            had == now,
            "the run's build record differs from the finish's (the same flags, writer settings \
             and Quickwit are needed): recorded {had}, now {now}"
        );
        Ok(())
    }
}

/// A store as of a time: listings leave out the objects created after
/// `before`, by the store's clock, and nothing can be written. A finish
/// reads the curated store through it, so the listings that grew since the
/// run started (our OCR's parts and revocations, the overlay's and
/// American Stories' parts, all create-only) are as the run listed them.
/// An object whose store doesn't say when it was created is kept.
#[derive(Debug)]
pub struct AsOf {
    inner: Arc<dyn ObjectStore>,
    before: SystemTime,
}

impl AsOf {
    pub fn new(inner: Arc<dyn ObjectStore>, before: SystemTime) -> Self {
        Self { inner, before }
    }
}

fn read_only(path: &str) -> StoreError {
    StoreError::Io(format!(
        "`{path}`: a finish reads the curated store as of the run's start and writes nothing"
    ))
}

#[async_trait]
impl ObjectStore for AsOf {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(path).await
    }

    async fn put_new(&self, path: &str, _: Vec<u8>, _: &str) -> Result<bool, StoreError> {
        Err(read_only(path))
    }

    async fn put(&self, path: &str, _: Vec<u8>, _: &str) -> Result<(), StoreError> {
        Err(read_only(path))
    }

    async fn exists(&self, path: &str) -> Result<bool, StoreError> {
        self.inner.exists(path).await
    }

    async fn modified(&self, path: &str) -> Result<Option<SystemTime>, StoreError> {
        self.inner.modified(path).await
    }

    async fn get_stream(&self, path: &str) -> Result<Option<ByteStream>, StoreError> {
        self.inner.get_stream(path).await
    }

    async fn put_stream(
        &self,
        path: &str,
        _: &str,
        _: Option<&str>,
        _: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        _: tokio::sync::oneshot::Receiver<bool>,
    ) -> Result<u64, StoreError> {
        Err(read_only(path))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .list_info(prefix)
            .await?
            .into_iter()
            .map(|o| o.path)
            .collect())
    }

    async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let mut listed = self.inner.list_info(prefix).await?;
        let before = listed.len();
        listed.retain(|o| o.created.is_none_or(|t| t <= self.before));
        if listed.len() < before {
            tracing::info!(
                prefix,
                left_out = before - listed.len(),
                "objects created after the run started, left out"
            );
        }
        Ok(listed)
    }

    async fn get_if_match(
        &self,
        path: &str,
        etag: &str,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get_if_match(path, etag, max_bytes).await
    }

    async fn delete_if_match(&self, path: &str, _: &str) -> Result<bool, StoreError> {
        Err(read_only(path))
    }
}

/// Documents by decade, as a replay counts them and a partitioned index's
/// splits hold them.
pub type DecadeDocs = BTreeMap<u16, u64>;

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &str, fields: Value) -> String {
        let mut f = fields;
        f["message"] = message.into();
        serde_json::json!({"timestamp": "t", "level": "INFO", "fields": f, "target": "x"})
            .to_string()
    }

    fn log(lines: &[String]) -> String {
        lines.join("\n") + "\n"
    }

    #[test]
    fn the_runs_lines_are_read_around_its_building_line() {
        let text = log(&[
            // An earlier release's planning and build.
            line(OCR_LOCAL_LINE, serde_json::json!({"parts": 1})),
            line(
                BUILDING_LINE,
                serde_json::json!({"version": "v0", "index": "i0"}),
            ),
            "not json".into(),
            // This run's.
            line(OCR_LOCAL_LINE, serde_json::json!({"parts": 2, "titles": 1})),
            line(OCR_LOCAL_LINE, serde_json::json!({"parts": 3, "titles": 1})),
            line(
                BUILDING_LINE,
                serde_json::json!({"version": "v1", "index": "i1"}),
            ),
            line(
                BATCH_STORIES_LINE,
                serde_json::json!({"batch": "b1", "pages": 5}),
            ),
            line(MERGES_LINE, serde_json::json!({"index": "i0"})),
            // A finish's own lines, appended: the run's come first.
            line(FINISHING_LINE, serde_json::json!({"version": "v1"})),
            line(
                BATCH_STORIES_LINE,
                serde_json::json!({"batch": "b1", "pages": 6}),
            ),
        ]);
        let l = RunLog::parse(text.as_bytes(), "release.log", "v1", "i1").unwrap();
        assert!(!l.merging(), "another index's merge wait");
        l.check_planned(
            OCR_LOCAL_LINE,
            Some(serde_json::json!({"parts": 3, "titles": 1})),
        )
        .unwrap();
        let err = l
            .check_planned(OCR_LOCAL_LINE, Some(serde_json::json!({"parts": 2})))
            .unwrap_err()
            .to_string();
        assert!(err.contains("parts = 3, the finish has 2"), "{err}");
        // A line neither has, and one only one has.
        l.check_planned(OVERLAY_LINE, None).unwrap();
        assert!(l
            .check_planned(STORIES_LINE, Some(serde_json::json!({})))
            .is_err());
        assert!(l.check_planned(OCR_LOCAL_LINE, None).is_err());
        l.check_built(
            BATCH_STORIES_LINE,
            Some("b1"),
            Some(serde_json::json!({"pages": 5})),
        )
        .unwrap();
        assert!(l.check_built(BATCH_STORIES_LINE, Some("b2"), None).is_ok());
        // A field the run's line lacks is null there.
        let err = l
            .check_built(
                BATCH_STORIES_LINE,
                Some("b1"),
                Some(serde_json::json!({"pages": 5, "empty": 0})),
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty = null"), "{err}");

        let merged = log(&[
            line(
                BUILDING_LINE,
                serde_json::json!({"version": "v1", "index": "i1"}),
            ),
            line(
                MERGES_LINE,
                serde_json::json!({"index": "i1", "step": "settle"}),
            ),
        ]);
        assert!(RunLog::parse(merged.as_bytes(), "l", "v1", "i1")
            .unwrap()
            .merging());
        let err = RunLog::parse(merged.as_bytes(), "l", "v2", "i2")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no `building index` line for v2"), "{err}");
        let err = RunLog::parse(merged.as_bytes(), "l", "v1", "i9")
            .unwrap_err()
            .to_string();
        assert!(err.contains("built \"i1\", not i9"), "{err}");
    }

    #[tokio::test]
    async fn a_store_as_of_a_time_lists_only_what_was_there_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        store.put("p/a", b"a".to_vec(), "x").await.unwrap();
        let cutoff = SystemTime::now() + std::time::Duration::from_secs(1);
        let as_of = AsOf::new(store.clone(), cutoff);
        assert_eq!(as_of.list("p").await.unwrap(), ["p/a"]);
        store.put("p/b", b"b".to_vec(), "x").await.unwrap();
        let past = AsOf::new(store.clone(), SystemTime::UNIX_EPOCH);
        assert!(past.list("p").await.unwrap().is_empty());
        // Reads go through; writes don't.
        assert_eq!(past.get("p/b").await.unwrap().unwrap(), b"b");
        assert!(past.put_new("p/c", vec![], "x").await.is_err());
        assert!(past.put("p/a", vec![], "x").await.is_err());
        assert!(!dir.path().join("p/c").exists());
    }
}
