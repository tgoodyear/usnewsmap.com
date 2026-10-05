//! The `index` stage (04 §4.4, 08 §8.4.1): build a new sealed index and the
//! reference snapshot that goes with it, then publish both by writing
//! `current.json` last.
//!
//! - **Incremental:** batches curated since the published version go into a
//!   new delta index; the version lists base + deltas + the new delta.
//! - **Full** (compaction): every curated batch goes into a new base index.
//!   Also forced when there is no published version or it already has 8
//!   deltas. Replacements (new batch versions) and catalog changes to
//!   published titles take effect only here (04 §4.7).
//!
//! A Quickwit index is merged into a few large splits and closed before it
//! is published (`crate::merges`); the version's manifest records the
//! splits of each of its indexes.
//!
//! A curated batch whose titles aren't all in the catalog yet waits for a
//! later release (it counts as new until it's published): `titles-sync` is
//! paced and stops on LoC's rate limit, and a backlog of titles shouldn't
//! hold up the batches that are ready.
//!
//! The Cosmos `ops/quickwit-writer` lock makes this the only writer. Indexes
//! are never rewritten: a failed run leaves an unpublished index for the
//! janitor, and the retry builds a new one.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::field::Empty;
use tracing::Instrument;
use usnm_core::text::TextStatus;
use usnm_core::time::{date_from_day, day_number, ym_number};
use usnm_store::ObjectStore;

use crate::activity::Reporter;
use crate::build_info;
use crate::catalog::{Catalog, Place, Title};
use crate::curated::{read_part, CuratedRow};
use crate::dedup::{self, Plan};
use crate::merges::IndexLayout;
use crate::ocr_ja;
use crate::progress::{self, Progress};
use crate::sink::IndexSink;
use crate::source::hex;
use crate::state::{
    Curated, IndexRun, LanguageBaselines, LanguageSet, RunBatch, RunStatus, State, Step,
    DUPLICATES_FILE, LANGUAGE_BASELINES_FILE, RUN_BATCHES_FILE, TITLE_PAGES_FILE,
};

pub use crate::state::{MAX_DELTAS, WRITER_LOCK};

pub struct Release {
    pub state: State,
    pub curated: Arc<dyn ObjectStore>,
    pub reference: Arc<dyn ObjectStore>,
    pub owner: String,
    pub full: bool,
    /// Mark the version as synthetic demo data (the fixture batches); the
    /// site then says so and doesn't link to loc.gov.
    pub synthetic: bool,
    /// Names versions (`pages-v{date}-{n}`). `published_at` is stamped when
    /// the version pointer is written, which can be many hours later.
    pub now: DateTime<Utc>,
    /// Why titles-sync stopped before trying every title, if it did. A delta
    /// goes ahead without the batches whose titles are missing; a full base
    /// (asked for, or forced) doesn't, since it would replace the published
    /// one without them.
    pub titles_left: Option<String>,
}

/// The Quickwit binary the release runs as its writer (`USNM_QUICKWIT_BIN`,
/// set by the ingest image), for the build record's version.
fn quickwit_bin() -> Option<std::path::PathBuf> {
    std::env::var_os("USNM_QUICKWIT_BIN").map(std::path::PathBuf::from)
}

/// A full release refused to start because titles-sync left titles unfetched
/// (`Release::titles_left`): nothing was published, and the next execution
/// continues the sync.
#[derive(Debug)]
pub struct TitlesLeft(pub String);

impl std::fmt::Display for TitlesLeft {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TitlesLeft {}

/// The version pointer was written, so the new version is live, but
/// recording the publish in the pipeline state failed afterwards.
#[derive(Debug)]
pub struct PublishedUnrecorded(pub String);

impl std::fmt::Display for PublishedUnrecorded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PublishedUnrecorded {}

/// The batches whose titles are all in `catalog`. The others are logged and
/// left for a later release.
fn catalogued(batches: Vec<RunBatch>, catalog: &Catalog) -> Vec<RunBatch> {
    let is_ready = |b: &RunBatch| b.curated.lccns.iter().all(|l| catalog.title(l).is_some());
    let mut ready = Vec::new();
    let mut waiting = Vec::new();
    for b in batches {
        if is_ready(&b) {
            ready.push(b);
        } else {
            waiting.push(b);
        }
    }
    if !waiting.is_empty() {
        let titles: BTreeSet<&str> = waiting
            .iter()
            .flat_map(|b| &b.curated.lccns)
            .filter(|lccn| catalog.title(lccn).is_none())
            .map(String::as_str)
            .collect();
        tracing::warn!(
            batches = waiting.len(),
            titles = titles.len(),
            first = ?waiting.iter().take(5).map(|b| b.batch.as_str()).collect::<Vec<_>>(),
            "batches wait for titles missing from the catalog (run titles-sync)"
        );
    }
    ready
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub index_version: String,
    pub indexes: Vec<String>,
    pub full: bool,
    pub docs: u64,
    pub pages: u64,
}

/// One engine document for a curated page (05 §5.5).
pub fn page_doc(row: &CuratedRow, title: &Title, place: &Place) -> Value {
    let k = &row.key;
    json!({
        "doc_id": k.doc_id(),
        "day": day_number(k.date),
        "ym": ym_number(k.date),
        "year": k.date.year(),
        "place_id": place.id,
        "place_shard": place.ordinal % 8,
        "lccn": k.lccn,
        "state": title.state,
        "language": title.languages,
        "front_page": k.seq == 1,
        "edition": k.edition,
        "seq": k.seq,
        // Hits order within a day: title, then edition, then page.
        "sort_key": (u64::from(title.ordinal) << 32) | (u64::from(k.edition) << 16) | u64::from(k.seq),
        "date": k.date.to_string(),
        "batch": row.batch,
        "text_cg": row.text.as_deref().map(usnm_core::common_grams::index_text),
        "text": row.text,
    })
}

/// How long the writer lock lasts without renewal, and how often it's renewed.
const LEASE_TTL: Duration = Duration::minutes(30);
const LEASE_RENEW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// A held writer lock, renewed in the background until it is released.
/// If a renewal fails, the release stops at its next checkpoint and never
/// publishes (08 §8.4.1).
pub struct WriterLease {
    lost: Arc<AtomicBool>,
    renewer: tokio::task::JoinHandle<()>,
}

impl WriterLease {
    fn check(&self) -> anyhow::Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            bail!("the writer lock could not be renewed; stopping without publishing");
        }
        Ok(())
    }

    /// Resolves once a renewal has failed. Long waits (the merges, up to
    /// 4 hours by default) race against it, so a writer that may no longer hold the
    /// lock stops within a second instead of at its next checkpoint, long
    /// before the lock expires and another writer could take it.
    async fn lost(&self) {
        while !self.lost.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
}

impl Drop for WriterLease {
    fn drop(&mut self) {
        self.renewer.abort();
    }
}

impl Release {
    /// Take the writer lock and keep renewing it. Callers that run a
    /// Quickwit writer node take it before the node starts and release it
    /// after the node stops, so no two writers ever overlap.
    pub async fn lock(&self) -> anyhow::Result<WriterLease> {
        self.state.lock(WRITER_LOCK, &self.owner, LEASE_TTL).await?;
        let lost = Arc::new(AtomicBool::new(false));
        let (state, owner, flag) = (self.state.clone(), self.owner.clone(), lost.clone());
        let renewer = tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEASE_RENEW).await;
                if let Err(e) = state.lock(WRITER_LOCK, &owner, LEASE_TTL).await {
                    tracing::error!(error = %format!("{e:#}"), "writer lock renewal failed");
                    flag.store(true, Ordering::SeqCst);
                    break;
                }
            }
        });
        Ok(WriterLease { lost, renewer })
    }

    pub async fn unlock(&self, lease: WriterLease) -> anyhow::Result<()> {
        drop(lease);
        self.state.unlock(WRITER_LOCK, &self.owner).await
    }

    /// Build and publish a new version, or `None` if there is nothing new.
    pub async fn run(&self, sink: &mut dyn IndexSink) -> anyhow::Result<Option<Published>> {
        let lease = self.lock().await?;
        let result = self.run_held(sink, &lease).await;
        let unlocked = self.unlock(lease).await;
        let published = result?;
        unlocked?;
        Ok(published)
    }

    /// Confirm this owner still holds the lock (and extend it).
    async fn confirm(&self, lease: &WriterLease) -> anyhow::Result<()> {
        lease.check()?;
        self.state.lock(WRITER_LOCK, &self.owner, LEASE_TTL).await
    }

    /// [`Release::run`] for a caller that already holds the writer lock.
    /// Runs in a `release` span (a request in Application Insights).
    pub async fn run_held(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
    ) -> anyhow::Result<Option<Published>> {
        self.run_reporting(sink, lease, &Reporter::off()).await
    }

    /// [`Release::run_held`], recording its steps (indexing, merging,
    /// publishing) with `report` for the status page.
    pub async fn run_reporting(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
        report: &Reporter,
    ) -> anyhow::Result<Option<Published>> {
        let span = tracing::info_span!(
            "release",
            otel.kind = "consumer",
            version = Empty,
            full = Empty,
            batches = Empty,
            docs = Empty,
            otel.status_code = Empty,
            otel.status_description = Empty,
        );
        let result = self
            .run_in_span(sink, lease, &span, report)
            .instrument(span.clone())
            .await;
        if let Err(e) = &result {
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_description", format!("{e:#}").as_str());
        }
        result
    }

    async fn run_in_span(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
        span: &tracing::Span,
        report: &Reporter,
    ) -> anyhow::Result<Option<Published>> {
        self.confirm(lease).await?;
        let current = Catalog::load(self.reference.as_ref()).await?;
        let (previous, previous_backend, previous_grams) = match self.published_run().await? {
            Some((run, backend, grams)) => (Some(run), Some(backend), grams),
            None => (None, None, None),
        };
        // Every batch's last committed curation, whatever its current status.
        let curated: BTreeMap<String, Curated> = self
            .state
            .batches(&[])
            .await?
            .into_iter()
            .filter_map(|(b, _)| b.curated.map(|c| (b.batch, c)))
            .collect();

        // A delta only makes sense on top of indexes in the same engine.
        let switching = previous_backend
            .as_deref()
            .is_some_and(|b| b != sink.backend());
        if switching {
            tracing::info!(
                to = sink.backend(),
                "search backend changed; building a full base"
            );
        }
        // A delta's pages would have `text_cg` at this version while the
        // indexes it adds to don't, or have another: rebuild them all, so
        // every index in the version has the same pairs (05 §5.5.3).
        let regramming =
            previous.is_some() && previous_grams != Some(usnm_core::common_grams::VERSION);
        if regramming {
            tracing::info!(
                from = ?previous_grams,
                to = usnm_core::common_grams::VERSION,
                "common-word pairs changed; building a full base"
            );
        }
        let full = self.full
            || switching
            || regramming
            || previous
                .as_ref()
                .is_none_or(|p| p.indexes.len() > MAX_DELTAS);
        if let Some(why) = &self.titles_left {
            if full {
                return Err(TitlesLeft(format!(
                    "{why}. A full release now would leave out every batch whose title is \
                     missing, so nothing was released: start the job again"
                ))
                .into());
            }
            tracing::warn!("{why}; releasing with the catalog as it is");
        }
        // An incremental release with no new batches still publishes when our
        // Japanese OCR has changed (04 §4.8): same main indexes, a new
        // Japanese index and snapshot.
        let mut nothing_new = false;
        let (scope, version_batches, catalog, mut indexes) = if full {
            let all: Vec<RunBatch> = curated
                .into_iter()
                .map(|(batch, curated)| RunBatch { batch, curated })
                .collect();
            let all = catalogued(all, &current);
            (all.clone(), all, current, Vec::new())
        } else {
            let prev = previous
                .as_ref()
                .expect("incremental has a previous version");
            let prev_batches = self.run_batches(prev).await?;
            let published: HashMap<&str, &Curated> = prev_batches
                .iter()
                .map(|b| (b.batch.as_str(), &b.curated))
                .collect();
            let mut new = Vec::new();
            for (batch, c) in &curated {
                match published.get(batch.as_str()) {
                    None => new.push(RunBatch {
                        batch: batch.clone(),
                        curated: c.clone(),
                    }),
                    Some(p) if *p != c => {
                        tracing::info!(batch, "re-curated batch waits for the next full release")
                    }
                    Some(_) => {}
                }
            }
            let published_catalog = self.load_snapshot_catalog(&prev.index_version).await?;
            let catalog = Catalog::carried_forward(&published_catalog, &current)?;
            let new = catalogued(new, &catalog);
            nothing_new = new.is_empty();
            let mut all = prev_batches;
            all.extend(new.iter().cloned());
            (new, all, catalog, prev.indexes.clone())
        };
        if scope.is_empty() && !nothing_new {
            tracing::info!("no curated batches; nothing to release");
            return Ok(None);
        }
        // Our own OCR of the Japanese pages (04 §4.8): its own index, rebuilt
        // at every release from the pages of the version's batches.
        let batch_names: BTreeSet<&str> =
            version_batches.iter().map(|b| b.batch.as_str()).collect();
        let overlay = ocr_ja::load(self.curated.as_ref(), &batch_names, &catalog).await?;
        if !overlay.parts.is_empty() {
            tracing::info!(
                parts = overlay.parts.len(),
                pages = overlay.pages.len(),
                skipped = overlay.skipped,
                "Japanese OCR overlay"
            );
        }
        // Decided before the duplicate-page plan, which reads every batch's
        // counts: a run with nothing to release returns without it.
        let overlay_only = nothing_new;
        if overlay_only {
            let prev = previous
                .as_ref()
                .expect("incremental has a previous version");
            if !self.overlay_changed(&prev.index_version, &overlay).await? {
                tracing::info!(
                    "no newly curated batches with catalogued titles, and no new Japanese OCR; \
                     nothing to release"
                );
                return Ok(None);
            }
            // The run records the Japanese index as the index it wrote, so
            // there must be one: OCR that changed without a page to index waits.
            if !overlay.pages.iter().any(ocr_ja::JaPage::indexable) {
                tracing::info!(
                    "the Japanese OCR changed but has no page to index; nothing to release"
                );
                return Ok(None);
            }
            tracing::info!(
                "no newly curated batches; releasing the new Japanese OCR on the same indexes"
            );
        }

        // Pages that ship in more than one batch: which copy the version
        // keeps. The batches outside the scope are in indexes it keeps.
        let new: BTreeSet<&str> = scope.iter().map(|b| b.batch.as_str()).collect();
        let indexed: BTreeSet<&str> = version_batches
            .iter()
            .map(|b| b.batch.as_str())
            .filter(|b| !new.contains(b))
            .collect();
        let previously_hidden = match &previous {
            Some(prev) if !full => self.previously_hidden(&prev.index_version).await?,
            _ => Some(BTreeSet::new()),
        };
        let dedup = dedup::plan(
            self.curated.as_ref(),
            &version_batches,
            &indexed,
            &previously_hidden,
        )
        .await?;
        if dedup.duplicate_pages > 0 {
            tracing::warn!(
                pages = dedup.duplicate_pages,
                title_days = dedup.shared_days,
                hidden = dedup.hidden.len(),
                pairs = ?dedup.pairs.iter().take(10).collect::<Vec<_>>(),
                "pages ship in more than one batch; keeping one copy of each"
            );
        }

        let (version, index_id) = self.next_names(full).await?;
        // An overlay-only release adds no main index: the run's new index is the Japanese one.
        let new_index = if overlay_only {
            ocr_ja::index_id(&version)
        } else {
            indexes.push(index_id.clone());
            index_id.clone()
        };
        let mut run = IndexRun {
            id: version.clone(),
            index_version: version.clone(),
            full,
            indexes: indexes.clone(),
            new_index: new_index.clone(),
            batch_count: Some(version_batches.len() as u64),
            batch_list: None,
            batches: None,
            status: RunStatus::Building,
            docs: 0,
            pages: version_batches.iter().map(|b| b.curated.pages).sum::<u64>()
                - dedup.duplicate_pages,
            duplicate_pages: dedup.duplicate_pages,
            started_at: Utc::now(),
            published_at: None,
            previous_version: previous.as_ref().map(|p| p.index_version.clone()),
            last_error: None,
            failed_at: None,
            build: Some(build_info::summary(
                full,
                !overlay.parts.is_empty(),
                quickwit_bin().as_deref(),
            )),
        };
        if !self.state.create_run(&run).await? {
            bail!("index run `{version}` already exists");
        }
        let (_, mut etag) = self.state.run(&version).await?.context("run vanished")?;
        span.record("version", version.as_str());
        span.record("full", full);
        span.record("batches", scope.len());
        tracing::info!(%version, index = %new_index, full, batches = scope.len(), overlay_only, "building index");

        report.version(&version);
        report.step(Step::Indexing).await;
        let outcome = async {
            let docs = if overlay_only {
                0
            } else {
                self.build_index(
                    lease, sink, &version, &index_id, &scope, &catalog, &dedup, report,
                )
                .await?
            };
            let ja = self
                .build_ja_index(lease, sink, &version, &overlay, &catalog)
                .await?;
            report.step(Step::Publishing).await;
            // What every index of the version is made of, for the log and
            // the manifest: how many splits a cold search opens (05 §5.5.1).
            let mut laid_out = indexes.clone();
            laid_out.extend(ja.iter().map(|(id, _)| id.clone()));
            let layout = sink.layout(&laid_out).await?;
            for l in &layout {
                l.log();
            }
            let (bounds, added) = self
                .write_snapshot(
                    &version,
                    &version_batches,
                    &catalog,
                    &dedup,
                    &layout,
                    &overlay,
                    ja.as_ref(),
                    full,
                )
                .await?;
            Ok::<_, anyhow::Error>((docs, bounds, ja, added))
        }
        .await;
        let (docs, (from, to), ja, added) = match outcome {
            Ok(v) => v,
            Err(e) => {
                run.status = RunStatus::Failed;
                run.failed_at = Some(Utc::now());
                run.last_error = Some(format!("{e:#}").chars().take(2000).collect());
                let _ = self.state.update_run(&run, &etag).await;
                return Err(e);
            }
        };
        run.docs = docs;
        // Pages of our Japanese OCR that curation never had are in the snapshot's counts.
        run.pages += added;
        run.batch_list = Some(format!("{version}/{RUN_BATCHES_FILE}"));
        span.record("docs", docs);
        etag = self.state.update_run(&run, &etag).await?;

        // Publish: the version pointer is the last write (04 §4.7), and only
        // while this release still holds the writer lock.
        self.confirm(lease).await?;
        let published_at = Utc::now();
        let mut pointer = json!({
            "index_version": version,
            "backend": sink.backend(),
            "indexes": indexes,
            "reference": version,
            "bounds": {"from": from.to_string(), "to": to.to_string()},
            "published_at": published_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "previous_version": run.previous_version,
            "synthetic": self.synthetic,
            // Every index in the version has `text_cg` at this version
            // (a release that would mix them builds a full base, above).
            "common_grams": usnm_core::common_grams::VERSION,
        });
        // The Japanese pages' index (#139). An API without Japanese search
        // ignores the field; one with it checks the fold version matches.
        if let Some((id, pages)) = &ja {
            pointer["ja"] = json!({
                "indexes": [id],
                "fold": usnm_core::ja::FOLD_VERSION,
                "pages": pages,
            });
        }
        self.reference
            .put(
                "current.json",
                serde_json::to_vec_pretty(&pointer)?,
                "application/json",
            )
            .await?;
        // The version is live from here on: a failure to record it says so
        // (the next release repairs the record, `published_run`).
        run.status = RunStatus::Published;
        run.published_at = Some(published_at);
        let recorded = async {
            self.state.update_run(&run, &etag).await?;
            self.state.set_current_version(&version).await
        }
        .await;
        if let Err(e) = recorded {
            return Err(PublishedUnrecorded(format!(
                "`{version}` is live, but recording the publish failed: {e:#}"
            ))
            .into());
        }
        tracing::info!(%version, docs, "published");
        Ok(Some(Published {
            index_version: version,
            indexes,
            full,
            docs,
            pages: run.pages,
        }))
    }

    /// The published version's run. `current.json` is the source of truth:
    /// if a previous release crashed after writing it but before recording
    /// the publish in Cosmos, the run and `ops/current` are repaired here.
    async fn published_run(&self) -> anyhow::Result<Option<(IndexRun, String, Option<u32>)>> {
        let Some(bytes) = self.reference.get("current.json").await? else {
            return Ok(None);
        };
        let pointer: Value = serde_json::from_slice(&bytes).context("current.json")?;
        let version = pointer["index_version"]
            .as_str()
            .context("current.json has no index_version")?;
        let (mut run, etag) = self.state.run(version).await?.with_context(|| {
            format!("current.json names `{version}`, which has no index run in the pipeline state")
        })?;
        if run.status != RunStatus::Published {
            tracing::warn!(version, "recording a publish that was interrupted");
            run.status = RunStatus::Published;
            // The pointer's own time, so the run and current.json agree
            // however long after the crash this runs.
            let pointer_at = pointer["published_at"]
                .as_str()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc));
            run.published_at = pointer_at.or(run.published_at).or_else(|| Some(Utc::now()));
            self.state.update_run(&run, &etag).await?;
        }
        if self.state.current_version().await?.as_deref() != Some(version) {
            self.state.set_current_version(version).await?;
        }
        let backend = pointer["backend"].as_str().unwrap_or("memory").to_owned();
        let grams = pointer["common_grams"]
            .as_u64()
            .and_then(|g| u32::try_from(g).ok());
        Ok(Some((run, backend, grams)))
    }

    /// `pages-v{date}-{n}` and its new index, `pages-{base|delta}-{date}-{n}`.
    async fn next_names(&self, full: bool) -> anyhow::Result<(String, String)> {
        let stamp = self.now.format("%Y%m%d");
        for n in 1..1000 {
            let version = format!("pages-v{stamp}-{n}");
            if self.state.run(&version).await?.is_none() {
                let kind = if full { "base" } else { "delta" };
                return Ok((version, format!("pages-{kind}-{stamp}-{n}")));
            }
        }
        bail!("too many index runs today")
    }

    /// The batches a published version was built from. Runs written before
    /// the list moved to the reference snapshot carry it inline; newer runs
    /// point at the snapshot's `batches.json`, which is checked against its
    /// manifest entry like the API checks the files it loads.
    async fn run_batches(&self, run: &IndexRun) -> anyhow::Result<Vec<RunBatch>> {
        if let Some(batches) = &run.batches {
            return Ok(batches.clone());
        }
        let path = run.batch_list.as_deref().with_context(|| {
            format!(
                "index run `{}` has neither a batch list nor inline batches",
                run.index_version
            )
        })?;
        let (dir, name) = path
            .rsplit_once('/')
            .with_context(|| format!("batch list path `{path}` has no directory"))?;
        let manifest_path = format!("{dir}/manifest.json");
        let manifest: Value = serde_json::from_slice(
            &self
                .reference
                .get(&manifest_path)
                .await?
                .with_context(|| format!("`{manifest_path}` is missing"))?,
        )
        .context(manifest_path.clone())?;
        let entry = manifest["files"]
            .as_array()
            .and_then(|files| files.iter().find(|f| f["path"] == name))
            .with_context(|| format!("`{manifest_path}` does not list `{name}`"))?;
        let bytes = self
            .reference
            .get(path)
            .await?
            .with_context(|| format!("`{path}` is missing"))?;
        let digest = hex(&Sha256::digest(&bytes));
        if entry["bytes"].as_u64() != Some(bytes.len() as u64)
            || !entry["sha256"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&digest))
        {
            bail!("`{path}` does not match its manifest entry");
        }
        let batches: Vec<RunBatch> = serde_json::from_slice(&bytes).context(path.to_owned())?;
        if let Some(n) = run.batch_count {
            anyhow::ensure!(
                batches.len() as u64 == n,
                "`{path}` lists {} batches; the run recorded {n}",
                batches.len()
            );
        }
        Ok(batches)
    }

    /// The copies `version` hid (its `duplicates.json`), or `None` when its
    /// snapshot predates the file: its indexes then hold every copy.
    async fn previously_hidden(&self, version: &str) -> anyhow::Result<dedup::PreviouslyHidden> {
        let path = format!("{version}/{DUPLICATES_FILE}");
        let Some(bytes) = self.reference.get(&path).await? else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_slice(&bytes).context(path)?))
    }

    async fn load_snapshot_catalog(&self, version: &str) -> anyhow::Result<Catalog> {
        let get = |name: &'static str| async move {
            let path = format!("{version}/{name}");
            let bytes = self
                .reference
                .get(&path)
                .await?
                .with_context(|| format!("`{path}` is missing"))?;
            Ok::<_, anyhow::Error>((path, bytes))
        };
        let (tp, titles) = get("titles.json").await?;
        let (pp, places) = get("places.json").await?;
        Catalog::new(
            serde_json::from_slice(&titles).context(tp)?,
            serde_json::from_slice(&places).context(pp)?,
        )
    }

    // The lease, sink and reporter are the release's, threaded through.
    #[allow(clippy::too_many_arguments)]
    async fn build_index(
        &self,
        lease: &WriterLease,
        sink: &mut dyn IndexSink,
        version: &str,
        index_id: &str,
        scope: &[RunBatch],
        catalog: &Catalog,
        dedup: &Plan,
        report: &Reporter,
    ) -> anyhow::Result<u64> {
        // Every title must resolve before anything is written.
        let mut missing = BTreeSet::new();
        for b in scope {
            for lccn in &b.curated.lccns {
                if catalog.title(lccn).is_none() {
                    missing.insert(lccn.clone());
                }
            }
        }
        if !missing.is_empty() {
            bail!("titles missing from the catalog: {missing:?}");
        }
        sink.create(index_id).await?;
        // A line every 30 s and after each batch (the saved query
        // `release-progress` and the stall alert read them).
        let expected = scope
            .iter()
            .map(|b| {
                b.curated
                    .ok_pages
                    .saturating_sub(dedup.skipped_docs(&b.batch))
            })
            .sum();
        let progress = Progress::new(sink.stats(), expected);
        progress.snapshot().log(None);
        progress::report(&self.state, version, &progress.snapshot()).await;
        let _ticker = progress.every(progress::INTERVAL);
        // The same counts for the status page, in their own `ops` item: the
        // run item's ETag stays the release's alone.
        let _reporter =
            progress.report_every(progress::INTERVAL, self.state.clone(), version.to_owned());
        let mut docs = 0u64;
        for b in scope {
            for path in &b.curated.parts {
                lease.check()?;
                let bytes = self
                    .curated
                    .get(path)
                    .await?
                    .with_context(|| format!("curated part `{path}` is missing"))?;
                let mut part_docs = Vec::new();
                read_part(bytes.into(), true, |row| {
                    if row.status == TextStatus::Ok && dedup.keeps(&row.key, &b.batch) {
                        let title = catalog.title(&row.key.lccn).context("title")?;
                        let place = catalog.place(&title.place_id).context("place")?;
                        part_docs.push(page_doc(&row, title, place));
                    }
                    Ok(())
                })
                .with_context(|| path.clone())?;
                for d in &part_docs {
                    sink.add(d).await?;
                }
                docs += part_docs.len() as u64;
            }
            progress.snapshot().log(Some(&b.batch));
        }
        // The last commit and the merge wait, given up as soon as the lock is.
        report.step(Step::Merging).await;
        tokio::select! {
            r = sink.finish(docs) => r?,
            () = lease.lost() => lease.check()?,
        }
        progress.snapshot().log(None);
        progress::report(&self.state, version, &progress.snapshot()).await;
        Ok(docs)
    }

    /// Whether the overlay's parts differ from the ones `version` was built
    /// from (its `ocr_ja.json`; none if it has none).
    async fn overlay_changed(
        &self,
        version: &str,
        overlay: &ocr_ja::Overlay,
    ) -> anyhow::Result<bool> {
        let before = match self
            .reference
            .get(&format!("{version}/{}", ocr_ja::OCR_JA_FILE))
            .await?
        {
            Some(bytes) => serde_json::from_slice::<Value>(&bytes)?["parts"].clone(),
            None => json!([]),
        };
        Ok(serde_json::to_value(&overlay.parts)? != before)
    }

    /// Build the Japanese pages' index for `version` from the overlay's pages
    /// with text, or nothing when there are none. Its name and page count.
    async fn build_ja_index(
        &self,
        lease: &WriterLease,
        sink: &mut dyn IndexSink,
        version: &str,
        overlay: &ocr_ja::Overlay,
        catalog: &Catalog,
    ) -> anyhow::Result<Option<(String, u64)>> {
        let mut docs = Vec::new();
        for p in overlay.pages.iter().filter(|p| p.indexable()) {
            let title = catalog.title(&p.key.lccn).context("title")?;
            let place = catalog.place(&title.place_id).context("place")?;
            docs.push(ocr_ja::ja_doc(p, title, place));
        }
        if docs.is_empty() {
            return Ok(None);
        }
        let id = ocr_ja::index_id(version);
        sink.create_with(&id, ocr_ja::JA_TEMPLATE).await?;
        for d in &docs {
            lease.check()?;
            sink.add(d).await?;
        }
        let n = docs.len() as u64;
        tokio::select! {
            r = sink.finish(n) => r?,
            () = lease.lost() => lease.check()?,
        }
        tracing::info!(index = %id, docs = n, "built the Japanese pages' index");
        Ok(Some((id, n)))
    }

    /// Write `{version}/` (titles, places, baselines, pages per title, the
    /// batch list, the hidden duplicate copies, manifest last) and return the
    /// version's date bounds. A page in more than one batch counts once.
    /// The manifest also records `layout`, the splits of each index, when
    /// the engine has splits.
    #[allow(clippy::too_many_arguments)]
    async fn write_snapshot(
        &self,
        version: &str,
        batches: &[RunBatch],
        catalog: &Catalog,
        dedup: &Plan,
        layout: &[IndexLayout],
        overlay: &ocr_ja::Overlay,
        ja: Option<&(String, u64)>,
        full: bool,
    ) -> anyhow::Result<((NaiveDate, NaiveDate), u64)> {
        let mut baselines: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::new();
        // Every page counted once, by its title: the same pages as the
        // baselines, which sum them by place instead.
        let mut title_pages: BTreeMap<String, u64> = BTreeMap::new();
        // The same pages again, by the languages their title lists.
        let mut by_language: BTreeMap<Vec<String>, BTreeMap<String, BTreeMap<u32, u32>>> =
            BTreeMap::new();
        let (mut first, mut last) = (u32::MAX, u32::MIN);
        for b in batches {
            for (lccn, days) in dedup::load_counts(self.curated.as_ref(), b).await? {
                let title = catalog
                    .title(&lccn)
                    .with_context(|| format!("title `{lccn}` is missing from the catalog"))?;
                let series = baselines.entry(title.place_id.clone()).or_default();
                let mut language_series = language_set(title).map(|set| {
                    by_language
                        .entry(set)
                        .or_default()
                        .entry(title.place_id.clone())
                        .or_default()
                });
                let total = title_pages.entry(lccn).or_default();
                for (day, pages) in days {
                    *series.entry(day).or_default() += pages;
                    if let Some(s) = language_series.as_mut() {
                        *s.entry(day).or_default() += pages;
                    }
                    *total += u64::from(pages);
                    first = first.min(day);
                    last = last.max(day);
                }
            }
        }
        if first > last {
            bail!("the version has no pages");
        }
        for (lccn, day, extra) in dedup.excess() {
            let title = catalog.title(lccn).context("title")?;
            let pages = baselines
                .get_mut(&title.place_id)
                .and_then(|s| s.get_mut(&day))
                .context("a duplicated page outside the baselines")?;
            *pages -= extra;
            *title_pages.get_mut(lccn).context("title pages")? -= u64::from(extra);
            if let Some(set) = language_set(title) {
                *by_language
                    .get_mut(&set)
                    .and_then(|places| places.get_mut(&title.place_id))
                    .and_then(|s| s.get_mut(&day))
                    .context("a duplicated page outside the language baselines")? -= extra;
            }
        }
        // Our OCR of pages curation never had (no ocr.txt in LoC's archive):
        // they are in no counts.json, so they join the baselines here. A page
        // missing from one batch's archive can have text in another batch of
        // the version, which counted it already: those are left out.
        let candidates: Vec<&ocr_ja::JaPage> = overlay
            .pages
            .iter()
            .filter(|p| p.missing_from_curation())
            .collect();
        let curated_keys = self.curated_keys(batches, &candidates).await?;
        let mut added = 0u64;
        for p in candidates
            .into_iter()
            .filter(|p| !curated_keys.contains(&p.key.doc_id()))
        {
            let title = catalog.title(&p.key.lccn).context("title")?;
            let day = day_number(p.key.date);
            *baselines
                .entry(title.place_id.clone())
                .or_default()
                .entry(day)
                .or_default() += 1;
            *title_pages.entry(p.key.lccn.clone()).or_default() += 1;
            if let Some(set) = language_set(title) {
                *by_language
                    .entry(set)
                    .or_default()
                    .entry(title.place_id.clone())
                    .or_default()
                    .entry(day)
                    .or_default() += 1;
            }
            first = first.min(day);
            last = last.max(day);
            added += 1;
        }
        // Only titles and places with pages in this version.
        let titles: Vec<&Title> = catalog
            .titles
            .iter()
            .filter(|t| title_pages.contains_key(&t.lccn))
            .collect();
        let place_ids: BTreeSet<&str> = titles.iter().map(|t| t.place_id.as_str()).collect();
        let places: Vec<&Place> = catalog
            .places
            .iter()
            .filter(|p| place_ids.contains(p.id.as_str()))
            .collect();
        let baselines: BTreeMap<String, Vec<(u32, u32)>> = baselines
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();
        let language_baselines = LanguageBaselines {
            sets: by_language
                .into_iter()
                .map(|(languages, places)| LanguageSet {
                    languages,
                    baselines: places
                        .into_iter()
                        .map(|(k, v)| (k, v.into_iter().collect()))
                        .collect(),
                })
                .collect(),
        };

        let mut snapshot_files: Vec<(&str, Vec<u8>)> = vec![
            ("titles.json", serde_json::to_vec(&titles)?),
            ("places.json", serde_json::to_vec(&places)?),
            ("baselines.json", serde_json::to_vec(&baselines)?),
            (TITLE_PAGES_FILE, serde_json::to_vec(&title_pages)?),
            (
                LANGUAGE_BASELINES_FILE,
                serde_json::to_vec(&language_baselines)?,
            ),
            (RUN_BATCHES_FILE, serde_json::to_vec(batches)?),
            (DUPLICATES_FILE, serde_json::to_vec(&dedup.hidden)?),
        ];
        // Which overlay parts the Japanese pages came from (only when there are any,
        // so a version without them has the same files as before).
        if !overlay.parts.is_empty() {
            let record = json!({
                "fold": usnm_core::ja::FOLD_VERSION,
                "index": ja.map(|(id, _)| id),
                "indexed": ja.map_or(0, |(_, n)| *n),
                "pages": overlay.pages.len(),
                "added_to_baselines": added,
                "skipped": overlay.skipped,
                // What the OCR found: Japanese text, near-blank pages, mostly Latin.
                "kinds": ocr_ja::kinds(&overlay.pages),
                "parts": overlay.parts,
            });
            snapshot_files.push((ocr_ja::OCR_JA_FILE, serde_json::to_vec(&record)?));
        }
        let mut files = Vec::new();
        for (name, body) in snapshot_files {
            files.push(json!({
                "path": name,
                "sha256": hex(&Sha256::digest(&body)),
                "bytes": body.len(),
            }));
            self.put_new(&format!("{version}/{name}"), body).await?;
        }
        let mut manifest = json!({
            "index_version": version,
            "files": files,
            "duplicate_pages": dedup.duplicate_pages,
            "built_from": {
                "batches": batches.iter().map(|b| json!({
                    "batch": b.batch, "version": b.curated.version, "counts": b.curated.counts,
                })).collect::<Vec<_>>(),
            },
        });
        if !layout.is_empty() {
            manifest["indexes"] = serde_json::to_value(layout)?;
        }
        if !overlay.parts.is_empty() {
            manifest["built_from"]["ocr_ja"] = serde_json::to_value(&overlay.parts)?;
        }
        // What built it (#161), with the index templates in full.
        manifest["build"] = build_info::record(full, ja.is_some(), quickwit_bin().as_deref());
        self.put_new(
            &format!("{version}/manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )
        .await?;
        Ok(((date_from_day(first), date_from_day(last)), added))
    }

    /// The doc ids of `pages` that some batch of the version has in its
    /// curated parts. Only the parts of batches holding these pages' titles
    /// are read, without their text.
    async fn curated_keys(
        &self,
        batches: &[RunBatch],
        pages: &[&ocr_ja::JaPage],
    ) -> anyhow::Result<BTreeSet<String>> {
        let wanted: BTreeSet<String> = pages.iter().map(|p| p.key.doc_id()).collect();
        let lccns: BTreeSet<&str> = pages.iter().map(|p| p.key.lccn.as_str()).collect();
        let mut found = BTreeSet::new();
        if wanted.is_empty() {
            return Ok(found);
        }
        for b in batches
            .iter()
            .filter(|b| b.curated.lccns.iter().any(|l| lccns.contains(l.as_str())))
        {
            for path in &b.curated.parts {
                let bytes = self
                    .curated
                    .get(path)
                    .await?
                    .with_context(|| format!("curated part `{path}` is missing"))?;
                read_part(bytes.into(), false, |row| {
                    let id = row.key.doc_id();
                    if wanted.contains(&id) {
                        found.insert(id);
                    }
                    Ok(())
                })
                .with_context(|| path.clone())?;
            }
        }
        Ok(found)
    }

    async fn put_new(&self, path: &str, body: Vec<u8>) -> anyhow::Result<()> {
        if !self
            .reference
            .put_new(path, body, "application/json")
            .await?
        {
            bail!("`{path}` already exists; reference snapshots are immutable");
        }
        Ok(())
    }
}

/// The languages a title lists, sorted and without repeats; `None` when it
/// lists none (no language filter matches its pages).
fn language_set(title: &Title) -> Option<Vec<String>> {
    let set: BTreeSet<&str> = title
        .languages
        .iter()
        .map(String::as_str)
        .filter(|l| !l.is_empty())
        .collect();
    (!set.is_empty()).then(|| set.into_iter().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_group_by_their_sorted_language_set() {
        let mut t = Title {
            lccn: "sn1".into(),
            name: "sn1".into(),
            ordinal: 1,
            place_id: "P1".into(),
            state: "IL".into(),
            languages: vec!["ger".into(), "eng".into(), "ger".into(), String::new()],
            extra: BTreeMap::new(),
        };
        assert_eq!(
            language_set(&t),
            Some(vec!["eng".to_owned(), "ger".to_owned()])
        );
        t.languages = vec![];
        assert_eq!(language_set(&t), None);
    }

    #[tokio::test]
    async fn a_lost_lease_ends_a_wait() {
        let lost = Arc::new(AtomicBool::new(false));
        let lease = WriterLease {
            lost: lost.clone(),
            renewer: tokio::spawn(async {}),
        };
        let flag = lost.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
        });
        let r: anyhow::Result<()> = tokio::select! {
            () = std::future::pending::<()>() => Ok(()),
            () = lease.lost() => lease.check(),
        };
        assert!(r.unwrap_err().to_string().contains("could not be renewed"));
    }
}
