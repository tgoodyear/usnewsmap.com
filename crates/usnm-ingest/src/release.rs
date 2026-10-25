//! The `index` stage (04 §4.4, 08 §8.4.1): build a new sealed index and the
//! reference snapshot that goes with it, then publish both by writing
//! `current.json` last.
//!
//! - **Incremental:** batches curated since the published version go into a
//!   new delta index; the version lists base + deltas + the new delta.
//! - **Full** (compaction): every curated batch goes into a new base index.
//!   Also forced when there is no published version or it already has 8
//!   deltas. Replacements (new batch versions) and catalog changes to
//!   published titles, a move to another place included, take effect only
//!   here (04 §4.7); a published place's corrected name or point rides the
//!   next delta (04 §4.6).
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

mod document;
mod lease;
mod parts;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::field::Empty;
use tracing::Instrument;
use usnm_core::text::{Analyzer, TextStatus};
use usnm_core::text_layout::TextLayout;
use usnm_core::time::{date_from_day, day_number};
use usnm_store::ObjectStore;

use crate::activity::Reporter;
use crate::american_stories;
use crate::build_info;
use crate::catalog::{Catalog, Place, Title};
use crate::congress;
use crate::curated::read_part;
use crate::decade_order::DecadeOrder;
use crate::dedup::{self, Plan};
use crate::finish::{self, Resume};
use crate::merges::IndexLayout;
use crate::ocr_ja;
use crate::ocr_local;
use crate::page_language::{self as page_lang, Moves};
use crate::progress::{self, Progress};
use crate::sink::{Decades, IndexSink, MainLayout, SinkStats};
use crate::source::hex;
use crate::state::{
    Curated, IndexRun, JaLatin, JaLatinPage, LanguageBaselines, LanguageSet, RunBatch, RunStatus,
    State, Step, TitleDates, DUPLICATES_FILE, JA_LATIN_FILE, LANGUAGE_BASELINES_FILE,
    RUN_BATCHES_FILE, TITLE_DATES_FILE, TITLE_PAGES_FILE,
};

pub use crate::state::{MAX_DELTAS, WRITER_LOCK};

pub use document::{main_doc, page_doc, DocFormat, SideTexts};
pub use lease::{lock_writer, unlock_writer, WriterLease};

use lease::LEASE_TTL;
use parts::{
    build_part_blocking, prefetch_batch, PartContext, PartDocs, Prefetch, Prefetched, RestParts,
};

pub struct Release {
    pub state: State,
    pub curated: Arc<dyn ObjectStore>,
    pub reference: Arc<dyn ObjectStore>,
    pub owner: String,
    pub full: bool,
    /// A storage migration builds every index anew and publishes a pending
    /// pointer, leaving the serving pointer and ops/current untouched.
    pub stage_index_storage: bool,
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
    /// Index American Stories' text with LoC's (#218, 04 §4.9): `text_as` on
    /// the pages it covers, and the pages only it has text for. The version
    /// then says so in `current.json` (`american_stories`), and a published
    /// version built without it is rebuilt in full first. Off: nothing is
    /// read, and the documents and `current.json` are as before.
    pub american_stories: bool,
    /// Give the pages LoC ships without text (`loc_text = missing`) a
    /// main-index document with the Latin-script text our Japanese OCR read
    /// on them (#203, 04 §4.8), so English searches reach their English ads
    /// and sections. The pages are in the baselines already (#139). The
    /// release's new main index (a delta or a base) takes the pages no main
    /// index of the version holds yet; a delta keeps the earlier ones. Off:
    /// no new ones are written, and a full release drops them.
    pub ja_latin: bool,
    /// Lay a full base out by decade (05 §5.5.5, #123): every split holds
    /// one decade's pages, sent a decade at a time (`crate::decade_order`),
    /// and `current.json` says so (`decades`), so date-limited searches skip
    /// the other decades' splits. Only a full base takes it: a delta follows
    /// the published version (its pages get the `decade` field and its
    /// splits the decade tags when that version has them), whatever this
    /// says. Off: the documents, the index config and `current.json` are as
    /// before.
    pub partition_decade: bool,
    /// Search a full base's texts in one field (05 §5.5.7, #283): `text_all`
    /// and `text_all_cg` hold LoC's and American Stories' words with a gap
    /// between them, `text` and `text_as` are stored only, and
    /// `current.json` says so (`text_layout`). The layout is the latest one
    /// field ([`TextLayout::LATEST`]): since #925, on a page our own OCR
    /// replaced LoC's text on (`ocr_local`), LoC's text is a third text of
    /// `text_all`, after another gap, and stored in `text_loc`. Only a full
    /// base takes it: a delta follows the published version, whatever this
    /// says. Off: the documents, the index config and `current.json` are as
    /// before.
    pub single_text_field: bool,
    /// Give a full base's pages their own languages, found from their
    /// words (05 §5.5.8, #289): `page_language` and `page_language_set` on
    /// every main-index document, the page-language baselines and the
    /// pages that move to them in the snapshot, and `current.json` says so
    /// (`page_language`). Only a full base takes it: a delta follows the
    /// published version, whatever this says. Off: the documents, the index
    /// config, the snapshot and `current.json` are as before.
    pub page_language: bool,
    /// The split target of a full base's main index in place of the
    /// template's 30,000 pages (#283): a merge stops taking splits once it
    /// holds this many, so a larger one leaves fewer, larger splits for a
    /// cold search to open. The manifest's `build.features` records it
    /// (`split_docs`). Only a full base takes it: a delta's few splits
    /// keep the template's. `None`: the index config is as before.
    pub split_docs: Option<u64>,
    /// Weigh our own OCR from `ocr-local` against LoC's text (12 §12.5 Step
    /// 9): on the pages of the batches this release indexes, ours replaces
    /// LoC's where rule v1 prefers it (`crate::ocr_local::rule`), and the
    /// version records it in `ocr_local.json` and `ocr_local_pages.bin`.
    /// Pages of published batches take it at the next full rebuild. Off:
    /// nothing is read and the documents are as before; a delta still
    /// carries the published version's `ocr_local_pages.bin` forward.
    pub ocr_local: bool,
    /// Search the Congress pages the fetch jobs curated under `congress/`
    /// (#229, 04 §4.10): the version gets a Congress index set
    /// (`congress-…`), `congress.json`, `congress/baselines.json` and a
    /// `congress` block in `current.json`. The set is rebuilt when the
    /// curated units changed since the published version (or its analyzer
    /// did), else kept; a release with no new batches still publishes for a
    /// changed set. Off: nothing is read, and the version has no Congress set;
    /// a published set is removed then (or when no curated pages with text
    /// are left), by a release of its own if there are no new batches.
    pub congress: bool,
}

/// What a release does with the Congress set (`Release::congress`).
enum CongressPlan {
    /// No set: the setting is off, or there are no Congress pages with text.
    Off,
    /// Build a new index from these units.
    Build(congress::release::Listing),
    /// Keep the published version's index, block and files.
    Keep {
        index: String,
        block: Value,
        files: Vec<(&'static str, Vec<u8>)>,
    },
}

/// The Congress set a version ends up with.
struct CongressOut {
    index: String,
    /// This run wrote the index (not kept from the published version).
    built: bool,
    block: Value,
    files: Vec<(&'static str, Vec<u8>)>,
}

/// What the release reads of `ocr-local` (`Release::ocr_local`).
struct LocalRun {
    listing: ocr_local::replace::Listing,
    revoked: Vec<ocr_local::replace::Revocation>,
    /// Pages the Japanese overlay has, or whose document is its Latin text:
    /// that overlay wins.
    ja: HashSet<String>,
    /// Pages of published batches ours would replace (stored scores).
    estimate: Option<u64>,
}

/// What a version records of our OCR from `ocr-local`.
#[derive(Debug, Default)]
struct LocalFiles {
    /// `ocr_local_pages.bin`, when the version holds such pages.
    pages: Option<Vec<u8>>,
    /// `ocr_local.json`, with the setting.
    record: Option<Value>,
    /// The manifest's `built_from.ocr_local`: the parts summary.
    summary: Option<Value>,
}

/// The pages this release's main index gave our text: (doc_id, engine,
/// the batch of the copy that holds it).
type LocalReplaced = Vec<(String, usnm_state::ocr_local::Engine, String)>;

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

/// The pages of our OCR the snapshot accounts for (#139, #203).
struct Latin<'a> {
    /// Of the overlay's pages that curation never had, and of the pages the
    /// version's main indexes hold with our Latin text, the ones some batch
    /// of the version has in its curated parts.
    curated_keys: &'a BTreeMap<String, CuratedCopies>,
    /// Pages this release's main index added with our Latin text.
    added: u64,
    /// Pages an earlier index holds with our Latin text, now hidden.
    hidden: u64,
    /// Every page the version's main indexes hold with our Latin text.
    version: &'a JaLatin,
}

/// A page's copies in the curated parts of the version's batches.
#[derive(Debug, Default)]
struct CuratedCopies {
    /// Some copy has LoC's text (`ok`).
    with_text: bool,
    /// The batches with a copy without it (`empty`, `short`).
    without_text: BTreeSet<String>,
}

/// What the published `current.json` says about the version's indexes.
struct PublishedPointer {
    backend: String,
    /// `common_grams`: the pairs every index of the version has.
    grams: Option<u32>,
    /// `american_stories`: every index of the version has American Stories' text.
    american_stories: Option<u32>,
    /// `decades`: every index of the version has the `decade` field (05 §5.5.5).
    decades: Option<u32>,
    /// `text_layout`: the text fields of every main index (05 §5.5.7).
    text_layout: Option<u32>,
    /// `page_language`: every main index of the version has the page
    /// languages, and its snapshot their baselines (05 §5.5.8).
    page_language: Option<u32>,
    /// `congress`: the Congress index set (#229), as published.
    congress: Option<Value>,
}

/// The live pointer, `current.json`: the version it names and the pointer
/// itself, or `None` when there is none.
pub async fn live_pointer(reference: &dyn ObjectStore) -> anyhow::Result<Option<(String, Value)>> {
    let Some(bytes) = reference.get("current.json").await? else {
        return Ok(None);
    };
    let pointer: Value = serde_json::from_slice(&bytes).context("current.json")?;
    let version = pointer["index_version"]
        .as_str()
        .context("current.json has no index_version")?
        .to_owned();
    Ok(Some((version, pointer)))
}

/// What [`record_current_version`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentVersion {
    /// The version `ops/current` names now.
    pub version: String,
    /// The one it named before: the same when nothing changed.
    pub previous: Option<String>,
}

impl CurrentVersion {
    pub fn changed(&self) -> bool {
        self.previous.as_deref() != Some(self.version.as_str())
    }
}

/// Make the pipeline state's `ops/current` name `live`, the version
/// `current.json` names, if it names another. `run` must be that version's
/// run, recorded as published: `ops/current` follows the live pointer and
/// is never ahead of it. A release does this before it builds, after a
/// publish that wasn't recorded or a staged one that was promoted
/// (`published_run`), and `import-staged-run --current` does it after a
/// promotion. `/v1/status` and the BOINC export read `ops/current`.
pub async fn record_current_version(
    state: &State,
    live: &str,
    run: &IndexRun,
) -> anyhow::Result<CurrentVersion> {
    if run.index_version != live {
        bail!(
            "current.json names `{live}`, not `{}`: promote it first; `ops/current` never names \
             a version ahead of the live pointer",
            run.index_version
        );
    }
    if run.status != RunStatus::Published {
        bail!(
            "`{live}`'s run is {:?}, not published: the next release records an interrupted \
             publish",
            run.status
        );
    }
    let previous = state.current_version().await?;
    if previous.as_deref() != Some(live) {
        state.set_current_version(live).await?;
    }
    Ok(CurrentVersion {
        version: live.to_owned(),
        previous,
    })
}

impl Release {
    /// Take the writer lock and keep renewing it. Callers that run a
    /// Quickwit writer node take it before the node starts and release it
    /// after the node stops, so no two writers ever overlap.
    pub async fn lock(&self) -> anyhow::Result<WriterLease> {
        lock_writer(&self.state, &self.owner).await
    }

    pub async fn unlock(&self, lease: WriterLease) -> anyhow::Result<()> {
        unlock_writer(&self.state, &self.owner, lease).await
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
        self.run_spanned(sink, lease, report, None).await
    }

    /// Finish the release `resume` took up: one whose main index passed its
    /// exact count before it stopped (`crate::finish`). This release's
    /// settings must be that run's. `sink` runs a writer on the run's
    /// Quickwit data; the caller holds the writer lock, as for
    /// [`Release::run_held`]. The version it publishes is the one the run
    /// would have.
    pub async fn finish(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
        report: &Reporter,
        resume: &Resume,
    ) -> anyhow::Result<Option<Published>> {
        self.run_spanned(sink, lease, report, Some(resume)).await
    }

    async fn run_spanned(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
        report: &Reporter,
        resume: Option<&Resume>,
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
            .run_in_span(sink, lease, &span, report, resume)
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
        resume: Option<&Resume>,
    ) -> anyhow::Result<Option<Published>> {
        self.confirm(lease).await?;
        // A finish refuses before it plans: the run's index must hold every
        // document the run sent, and the indexes the tail builds mustn't
        // exist yet (`crate::finish`).
        if let Some(r) = resume {
            let index = &r.run.new_index;
            let held = sink.published_docs(index).await?;
            r.check_count(index, held)?;
            tracing::info!(index = %index, docs = held, "the run's main index holds the documents it recorded sending");
            for id in [
                ocr_ja::index_id(&r.run.index_version),
                congress::index_id(&r.run.index_version),
            ] {
                if sink.has_index(&id).await? {
                    bail!(
                        "`{id}` exists already: the run stopped after it started building it. \
                         Delete that index and finish again"
                    );
                }
            }
        }
        let current = Catalog::load(self.reference.as_ref()).await?;
        let (previous, pointer) = match self.published_run().await? {
            Some((run, pointer)) => (Some(run), Some(pointer)),
            None => (None, None),
        };
        // Keep the lake version as rollback without inheriting its indexes.
        let previous_version = if self.stage_index_storage {
            match self.reference.get("current.json").await? {
                Some(bytes) => {
                    let pointer: Value = serde_json::from_slice(&bytes).context("current.json")?;
                    Some(
                        pointer["index_version"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .context("current.json has no index_version")?
                            .to_owned(),
                    )
                }
                None => None,
            }
        } else {
            previous.as_ref().map(|p| p.index_version.clone())
        };
        let previous_backend = pointer.as_ref().map(|p| p.backend.as_str());
        let previous_grams = pointer.as_ref().and_then(|p| p.grams);
        let previous_as = pointer.as_ref().and_then(|p| p.american_stories);
        let previous_decades = pointer.as_ref().and_then(|p| p.decades);
        let previous_layout = pointer.as_ref().map(|p| p.text_layout);
        let previous_page_language = pointer.as_ref().and_then(|p| p.page_language);
        // Every batch's last committed curation, whatever its current status.
        let curated: BTreeMap<String, Curated> = self
            .state
            .batches(&[])
            .await?
            .into_iter()
            .filter_map(|(b, _)| b.curated.map(|c| (b.batch, c)))
            .collect();

        // A delta only makes sense on top of indexes in the same engine.
        let switching = previous_backend.is_some_and(|b| b != sink.backend());
        if switching {
            tracing::info!(
                to = sink.backend(),
                "search backend changed; building a full base"
            );
        }
        // Every index of a version has its pairs, and folds its words, with
        // one analyzer (05 §5.5.3, #168): a delta is built with the
        // published version's, and only a full base moves to the latest.
        // A version without pairs, or with ones this code doesn't know,
        // can't take a delta: rebuild every index.
        let published_analyzer = previous_grams.and_then(usnm_core::common_grams::analyzer);
        let regramming = previous.is_some() && published_analyzer.is_none();
        if regramming {
            tracing::info!(
                from = ?previous_grams,
                to = usnm_core::common_grams::VERSION,
                "the published version has no common-word pairs this release can add to; \
                 building a full base"
            );
        }
        // The same for American Stories' text (05 §5.5.4): a version says it
        // has the text only when every index in it does, so the first
        // release with it, or with another version of it, rebuilds them all.
        // Without the setting the text is neither written nor searched,
        // whatever the indexes hold.
        let adding_stories = self.american_stories
            && previous.is_some()
            && previous_as != Some(usnm_core::american_stories::VERSION);
        if adding_stories {
            tracing::info!(
                from = ?previous_as,
                to = usnm_core::american_stories::VERSION,
                "American Stories' text is new to the published version; building a full base"
            );
        }
        // The text fields (05 §5.5.7): a delta is laid out as the published
        // version is, so a layout this release doesn't know can't take one.
        let published_layout = previous_layout.map(TextLayout::from_version);
        let relayouting = matches!(published_layout, Some(None));
        if relayouting {
            tracing::info!(
                from = ?previous_layout.flatten(),
                "the published version's text fields are laid out in a way this release \
                 doesn't know; building a full base"
            );
        }
        // With one field for both texts, American Stories' text can't be
        // left out of the indexes a delta keeps, as it is from a field of
        // its own: a release without it rebuilds them all.
        let dropping_stories = !self.american_stories
            && previous_as.is_some()
            && published_layout
                .flatten()
                .is_some_and(TextLayout::searches_one_field);
        if dropping_stories {
            tracing::info!(
                "the published version searches American Stories' text in one field with \
                 LoC's, and this release leaves it out; building a full base"
            );
        }
        let full = self.full
            || switching
            || regramming
            || adding_stories
            || relayouting
            || dropping_stories
            || previous
                .as_ref()
                .is_none_or(|p| p.indexes.len() > MAX_DELTAS);
        let decades = self.decade_layout(full, previous_decades);
        let text_layout = self.text_layout(full, published_layout.flatten());
        let page_language = self.page_language_on(full, previous_page_language);
        let split_docs = self.split_docs(full);
        let analyzer = match published_analyzer {
            Some(a) if !full => a,
            _ => Analyzer::LATEST,
        };
        if analyzer != Analyzer::LATEST {
            tracing::info!(
                common_grams = usnm_core::common_grams::version(analyzer),
                latest = usnm_core::common_grams::VERSION,
                "a delta folds its words as the published version's indexes do; a full \
                 rebuild (USNM_INGEST_FULL) moves the version to the latest analyzer"
            );
        }
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
        // Japanese index and snapshot. With `--ja-latin` and Latin text of
        // ours that no main index holds yet, a delta of just those pages.
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
            // Published titles the catalog now puts in another place (a
            // merge, an alias, 04 §4.6) stay where they were published: a
            // full release on the E4 profile moves them, since one on the
            // Consumption profile runs out of memory (#172).
            let moved = Catalog::regrouped(&published_catalog, &current);
            if !moved.is_empty() {
                let n = moved.len();
                tracing::warn!(
                    titles = n,
                    first = ?moved.iter().take(10).collect::<Vec<_>>(),
                    "{n} published titles' place changes wait for the next full release \
                     (USNM_INGEST_FULL, run on the E4 profile: docs/operations/rebuild-and-index-storage.md, Full rebuild)"
                );
            }
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
        // The Congress pages (#229): listed once, from their counts.
        let congress_plan = self
            .congress_plan(previous.as_ref(), pointer.as_ref(), analyzer, switching)
            .await?;
        let congress_builds = matches!(congress_plan, CongressPlan::Build(_));
        // The published version has a Congress set this one won't (the
        // setting turned off, or no curated pages with text left): a change
        // to publish even without new batches, or `current.json` would keep
        // naming the set.
        let congress_removed = matches!(congress_plan, CongressPlan::Off)
            && pointer.as_ref().is_some_and(|p| p.congress.is_some());
        let congress_changes = congress_builds || congress_removed;
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
        if let Some(r) = resume {
            r.log.check_planned(
                finish::OVERLAY_LINE,
                (!overlay.parts.is_empty()).then(|| {
                    json!({
                        "parts": overlay.parts.len(),
                        "pages": overlay.pages.len(),
                        "skipped": overlay.skipped,
                    })
                }),
            )?;
        }
        // Of the pages of our OCR that curation never had, and of the pages
        // a main index of the version holds with our OCR's Latin text
        // (#203), the ones some batch of the version has in its curated
        // parts: those are counted, and indexed, from LoC's copy.
        let latin_before = match &previous {
            Some(prev) if !full => self.previous_latin(&prev.index_version).await?,
            _ => JaLatin::default(),
        };
        // With no newly curated batches, our Latin text of pages LoC ships
        // without text that no main index of the version holds yet still
        // gets a main index of its own (a delta of just those pages), so it
        // doesn't wait for LoC's next batch (#203). Pages with none of it
        // can skip reading the curated parts below, and so can a run whose
        // published version already weighed every page of this overlay by
        // this rule: those its main indexes don't hold were left out for a
        // copy of LoC's, and with no new batch they still are.
        let latin_pending = nothing_new
            && self.ja_latin
            && overlay.pages.iter().any(|p| {
                p.missing_from_curation()
                    && !latin_before.pages.contains_key(&p.key.doc_id())
                    && p.latin().is_some()
            })
            && !self.latin_settled(previous.as_ref(), &overlay).await?;
        // Decided before the duplicate-page plan, which reads every batch's
        // counts: a run with nothing to release returns without it.
        if nothing_new
            && !latin_pending
            && !congress_changes
            && !self.overlay_releasable(previous.as_ref(), &overlay).await?
        {
            return Ok(None);
        }
        if nothing_new && congress_builds {
            tracing::info!(
                "no newly curated batches; releasing the changed Congress pages on the same indexes"
            );
        }
        if nothing_new && congress_removed {
            tracing::info!(
                "no newly curated batches; releasing the same indexes without the published \
                 Congress set"
            );
        }
        let curated_keys = {
            let mut wanted: BTreeSet<String> = overlay
                .pages
                .iter()
                .filter(|p| p.missing_from_curation())
                .map(|p| p.key.doc_id())
                .collect();
            wanted.extend(latin_before.pages.keys().cloned());
            self.curated_keys(&version_batches, &wanted).await?
        };
        // The Latin text this release's main index adds: the pages no main
        // index of the version holds yet. A copy with LoC's text supersedes
        // ours; one without doesn't, and gets no document (`latin_owned`),
        // unless an index the version keeps may hold it as a document
        // already: with American Stories' text, a copy without LoC's is one
        // when that text covers it.
        let scope_names: BTreeSet<&str> = scope.iter().map(|b| b.batch.as_str()).collect();
        let latin_new: Vec<(&ocr_ja::JaPage, String)> = if self.ja_latin {
            overlay
                .pages
                .iter()
                .filter(|p| {
                    let id = p.key.doc_id();
                    let copies = curated_keys.get(&id);
                    p.missing_from_curation()
                        && !copies.is_some_and(|c| c.with_text)
                        && !(self.american_stories
                            && copies.is_some_and(|c| {
                                c.without_text
                                    .iter()
                                    .any(|b| !scope_names.contains(b.as_str()))
                            }))
                        && !latin_before.pages.contains_key(&id)
                })
                .filter_map(|p| p.latin().map(|t| (p, t)))
                .collect()
        } else {
            Vec::new()
        };
        // An incremental release with no new batches and no such pages
        // publishes only a new Japanese index and snapshot, on the same
        // main indexes.
        let overlay_only = nothing_new && latin_new.is_empty();
        // Every page was weighed and none added (each left out for a copy
        // of LoC's): an overlay-only release, once, so its `ocr_ja.json`
        // records `latin.rule` and later runs skip the curated parts. The
        // run records the Japanese index as the index it wrote, so there
        // must be one.
        if overlay_only && latin_pending && !congress_changes {
            if !overlay.pages.iter().any(ocr_ja::JaPage::indexable) {
                tracing::info!(
                    "no newly curated batches and no Latin text of ours to add, and the \
                     Japanese OCR has no page to index; nothing to release"
                );
                return Ok(None);
            }
            tracing::info!(
                "no newly curated batches and no Latin text of ours to add; releasing the \
                 Japanese OCR on the same indexes, which records that every page was weighed"
            );
        }
        if nothing_new && !overlay_only {
            tracing::info!(
                pages = latin_new.len(),
                "no newly curated batches; releasing our OCR's Latin text of pages LoC ships \
                 without text in a delta of its own. Our text in place of LoC's waits for a \
                 release that indexes the page's batch"
            );
        }
        // Pages curation had that our OCR read Latin text on: a batch this
        // release indexes takes ours in place of LoC's text where ours reads
        // more words (#203). A release with no new batches indexes none.
        let latin_curated: HashMap<String, (&ocr_ja::JaPage, String)> =
            if self.ja_latin && !nothing_new {
                overlay
                    .pages
                    .iter()
                    .filter(|p| !p.missing_from_curation())
                    .filter_map(|p| p.latin().map(|t| (p.key.doc_id(), (p, t))))
                    .collect()
            } else {
                HashMap::new()
            };

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
        let mut dedup = dedup::plan(
            self.curated.as_ref(),
            &version_batches,
            &indexed,
            &previously_hidden,
            self.american_stories,
        )
        .await?;
        // A page an earlier index holds with our Latin text (a delta can't
        // take it out) that a copy with LoC's text now wins over: hidden, so
        // the page counts once, from LoC's copy. That is a page curation
        // never had that some batch of the version now has with text, or a
        // copy given our text in place of LoC's that another batch's copy
        // with text wins over. A copy without LoC's text doesn't take over:
        // ours stays searchable, and the release writes no document for the
        // other (`latin_kept`), so the page is still one document.
        let (latin_hidden, latin_kept): (Vec<_>, Vec<_>) =
            latin_before.pages.iter().partition(|(id, p)| {
                if p.loc_text == "missing" {
                    curated_keys.get(*id).is_some_and(|c| c.with_text)
                } else {
                    usnm_core::ids::PageKey::from_doc_id(id).is_ok_and(|key| {
                        !dedup.keeps(&key, &p.batch) && dedup.kept_has_text(&key) == Some(true)
                    })
                }
            });
        let latin_hidden: Vec<dedup::Hidden> = latin_hidden
            .into_iter()
            .map(|(id, p)| dedup::Hidden {
                doc_id: id.clone(),
                batch: p.batch.clone(),
            })
            .collect();
        // Our copies that stay searchable, by page, with their batch.
        let latin_kept: HashMap<String, String> = latin_kept
            .into_iter()
            .map(|(id, p)| (id.clone(), p.batch.clone()))
            .collect();
        // The pages whose document is ours, in an index the version keeps or
        // in this release's: a copy of one without LoC's text gets none.
        let latin_owned: HashMap<String, String> = latin_kept
            .iter()
            .map(|(id, b)| (id.clone(), b.clone()))
            .chain(
                latin_new
                    .iter()
                    .map(|(p, _)| (p.key.doc_id(), p.batch.clone())),
            )
            .collect();
        // With American Stories' text on, the plan hides a losing copy
        // without LoC's text too: not ours, which stays the page's document.
        dedup
            .hidden
            .retain(|h| latin_kept.get(&h.doc_id) != Some(&h.batch));
        if !latin_hidden.is_empty() {
            tracing::warn!(
                pages = latin_hidden.len(),
                "pages indexed with our OCR's Latin text now have LoC's; hiding our copies"
            );
            dedup.hidden.extend(latin_hidden.iter().cloned());
            dedup.hidden.sort();
            dedup.hidden.dedup();
        }
        // What the version's main indexes hold with our Latin text: the
        // pages kept from the indexes it keeps, this release's pages that
        // curation never had, and (after the build) the pages it gave our
        // text in place of LoC's.
        let mut latin_after = latin_before;
        for (p, _) in &latin_new {
            latin_after.pages.insert(
                p.key.doc_id(),
                JaLatinPage {
                    batch: p.batch.clone(),
                    loc_text: p.loc_text.clone(),
                    ocr_source: p.ocr_source.clone(),
                    ocr_engine: p.ocr_engine.clone(),
                },
            );
        }
        if dedup.duplicate_pages > 0 {
            tracing::warn!(
                pages = dedup.duplicate_pages,
                title_days = dedup.shared_days,
                hidden = dedup.hidden.len(),
                pairs = ?dedup.pairs.iter().take(10).collect::<Vec<_>>(),
                "pages ship in more than one batch; keeping one copy of each"
            );
        }

        // The pages the version's main indexes hold in a set of page
        // languages other than their title's (05 §5.5.8): a delta keeps the
        // published version's, but for the copies it now hides, and adds its
        // own documents' below; a full base starts afresh.
        let mut moves = if page_language {
            let mut m = match &previous {
                Some(prev) if !full => self.previous_moves(&prev.index_version).await?,
                _ => Moves::new(),
            };
            m.drop_hidden(&dedup.hidden);
            Some(m)
        } else {
            None
        };

        // Our own OCR (12 §12.5 Step 9): listed once, loaded batch by batch
        // as the build reaches each.
        let local = if self.ocr_local && !overlay_only {
            let listing = ocr_local::replace::Listing::list(self.curated.as_ref()).await?;
            let revoked = ocr_local::replace::revocations(self.curated.as_ref()).await?;
            // Pages of published batches only take ours at a full rebuild:
            // how many would, by the scores `ocr-sync` stored.
            let estimate = if indexed.is_empty() || listing.parts() == 0 {
                None
            } else {
                let n = listing
                    .estimate(
                        self.curated.as_ref(),
                        &indexed,
                        |lccn| {
                            catalog
                                .title(lccn)
                                .map(|t| t.languages.clone())
                                .unwrap_or_default()
                        },
                        &revoked,
                    )
                    .await?;
                tracing::info!(
                    pages = n,
                    "pages of ocr-local that would replace LoC's text in published batches, by \
                     the scores ocr-sync stored: a full rebuild (USNM_INGEST_FULL) applies them"
                );
                Some(n)
            };
            tracing::info!(
                parts = listing.parts(),
                titles = listing.titles(),
                revocations = revoked.len(),
                "our OCR in ocr-local"
            );
            let ja: HashSet<String> = overlay
                .pages
                .iter()
                .map(|p| p.key.doc_id())
                .chain(latin_owned.keys().cloned())
                .collect();
            Some(LocalRun {
                listing,
                revoked,
                ja,
                estimate,
            })
        } else {
            None
        };
        if let Some(r) = resume {
            r.log.check_planned(
                finish::OCR_LOCAL_LINE,
                local.as_ref().map(|l| {
                    json!({
                        "parts": l.listing.parts(),
                        "titles": l.listing.titles(),
                        "revocations": l.revoked.len(),
                    })
                }),
            )?;
        }

        // American Stories' text, listed once: the parts of the years the
        // writer has finished. With the setting on and nothing written yet,
        // nothing is built: a version without the text mustn't say it has it.
        let stories = if self.american_stories && !overlay_only {
            american_stories::check_written(self.curated.as_ref()).await?;
            let source = american_stories::Source::list(self.curated.as_ref()).await?;
            tracing::info!(
                years = source.years.len(),
                first = source.years.first(),
                last = source.years.last(),
                parts = source.parts(),
                unfinished_parts = source.unfinished_parts,
                "American Stories' text"
            );
            // Shared with the task that loads the next batch's text ahead.
            Some(Arc::new(source))
        } else {
            None
        };

        if let Some(r) = resume {
            r.log.check_planned(
                finish::STORIES_LINE,
                stories.as_ref().map(|s| {
                    json!({
                        "years": s.years.len(),
                        "first": s.years.first(),
                        "last": s.years.last(),
                        "parts": s.parts(),
                        "unfinished_parts": s.unfinished_parts,
                    })
                }),
            )?;
            if overlay_only {
                bail!(
                    "{} built no main index: there is nothing to finish",
                    r.run.index_version
                );
            }
            // The catalog the finish plans with must be the one the index's
            // documents were written from.
            match sink.title_fields(&r.run.new_index).await? {
                Some(held) => finish::check_titles(&catalog, &r.run.new_index, &held)?,
                None => tracing::warn!(
                    "this sink can't say what the index's documents carry from the catalog; not \
                     checked"
                ),
            }
        }
        // A finish takes the run's names; `next_names` would skip them.
        let (version, index_id) = match resume {
            Some(r) => r.names(full)?,
            None => self.next_names(full).await?,
        };
        // An overlay-only release adds no main index: the run's new index is
        // the Japanese one, or with no Japanese pages to index, the Congress
        // one. A release that only removes the Congress set writes none, and
        // its run names none (empty) rather than an index that doesn't exist.
        let new_index = if overlay_only {
            if overlay.pages.iter().any(ocr_ja::JaPage::indexable) {
                ocr_ja::index_id(&version)
            } else if congress_builds {
                congress::index_id(&version)
            } else if congress_removed {
                String::new()
            } else {
                ocr_ja::index_id(&version)
            }
        } else {
            indexes.push(index_id.clone());
            index_id.clone()
        };
        // What this run builds, for the version's build record (#161): the
        // same predicate as `build_ja_index` for the Japanese index.
        let mut built = build_info::Built {
            full,
            main_index: !overlay_only,
            ja_index: overlay.pages.iter().any(|p| p.indexable()),
            engine: sink.engine().await,
            writer: crate::sink::WriterTuning::from_env()?,
            american_stories: self.american_stories,
            ja_latin: self.ja_latin,
            ocr_local: local.is_some(),
            decades,
            text_layout,
            page_language,
            split_docs,
            analyzer,
            congress: !matches!(congress_plan, CongressPlan::Off),
            congress_index: congress_builds,
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
            previous_version,
            last_error: None,
            failed_at: None,
            build: Some(build_info::summary(&built)),
        };
        let mut etag = match resume {
            None => {
                if !self.state.create_run(&run).await? {
                    bail!("index run `{version}` already exists");
                }
                self.state.run(&version).await?.context("run vanished")?.1
            }
            // The run as the plan comes to it, building again from when it
            // started; checked against its record first.
            Some(r) => {
                r.check_run(&run)?;
                run.started_at = r.run.started_at;
                self.state.update_run(&run, &r.etag).await?
            }
        };
        span.record("version", version.as_str());
        span.record("full", full);
        span.record("batches", scope.len());
        // A finish's line is its own: the run's "building index" is what
        // its log is read from (`finish::RunLog`).
        if resume.is_some() {
            tracing::info!(%version, index = %new_index, full, batches = scope.len(), "finishing index");
        } else {
            tracing::info!(%version, index = %new_index, full, batches = scope.len(), overlay_only, decades = ?decades, text_layout = text_layout.version(), page_language, split_docs, "building index");
        }

        report.version(&version);
        report.step(Step::Indexing).await;
        let outcome = async {
            let (docs, stories_record, replaced, (local_tally, local_replaced)) = if overlay_only {
                (0, None, Vec::new(), Default::default())
            } else {
                self.build_index(
                    lease,
                    sink,
                    &version,
                    &index_id,
                    &scope,
                    &catalog,
                    &dedup,
                    stories.as_ref(),
                    &latin_new,
                    &latin_curated,
                    &latin_owned,
                    local.as_ref(),
                    MainLayout {
                        decades,
                        text: text_layout,
                        page_language,
                        split_docs,
                    },
                    analyzer,
                    moves.as_mut(),
                    report,
                    resume,
                )
                .await?
            };
            let added_latin = (latin_new.len() + replaced.len()) as u64;
            latin_after.pages.extend(replaced);
            let stories_record = stories.as_deref().zip(stories_record.as_ref());
            // The pages the version holds with our OCR's text.
            let local_files = self
                .ocr_local_files(
                    previous.as_ref().filter(|_| !full),
                    &version_batches,
                    &dedup.hidden,
                    &local_replaced,
                    local.as_ref().map(|l| (l, &local_tally)),
                )
                .await?;
            let ja = self
                .build_ja_index(
                    lease,
                    sink,
                    &version,
                    &overlay,
                    &catalog,
                    analyzer,
                    moves.as_ref(),
                )
                .await?;
            // What it built in the end: the Japanese index can come out empty.
            built.ja_index = ja.is_some();
            let congress_out = self
                .congress_set(lease, sink, &version, &congress_plan, analyzer)
                .await?;
            report.step(Step::Publishing).await;
            // What every index of the version is made of, for the log and
            // the manifest: how many splits a cold search opens (05 §5.5.1).
            // A kept Congress index is listed too, so prune keeps it for
            // this version.
            let mut laid_out = indexes.clone();
            laid_out.extend(ja.iter().map(|(id, _)| id.clone()));
            laid_out.extend(congress_out.iter().map(|c| c.index.clone()));
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
                    &Latin {
                        curated_keys: &curated_keys,
                        added: added_latin,
                        hidden: latin_hidden.len() as u64,
                        version: &latin_after,
                    },
                    stories_record,
                    &local_files,
                    moves.as_ref(),
                    congress_out.as_ref(),
                    &build_info::record(&built),
                    analyzer,
                    resume.is_some(),
                )
                .await?;
            Ok::<_, anyhow::Error>((docs, bounds, ja, added, congress_out))
        }
        .await;
        let (docs, (from, to), ja, added, congress_out) = match outcome {
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
        // What it built in the end, as the manifest has it.
        run.build = Some(build_info::summary(&built));
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
            // Every index in the version has `text_cg` at this version, the
            // one of its analyzer (a delta keeps the published version's,
            // above), which the API parses queries with (#168).
            "common_grams": usnm_core::common_grams::version(analyzer),
        });
        // Every index in the version has American Stories' text: this one was
        // built with it, and so was the published version it adds to (or the
        // release built a full base, above). An overlay-only release keeps
        // the published main indexes, which have it for the same reason.
        if self.american_stories {
            pointer["american_stories"] = json!(usnm_core::american_stories::VERSION);
        }
        // Every main index of the version has the `decade` field at this
        // version: a partitioned base this run built, or one the published
        // version has, with deltas tagged like this one (05 §5.5.5).
        if decades.on() {
            pointer["decades"] = json!(usnm_core::decade::VERSION);
        }
        // Every main index of the version has one searched field for both
        // texts: a base this run built with it, or one the published version
        // has, with deltas written like it (05 §5.5.7). Absent: a field per
        // text, as before the setting.
        if text_layout != TextLayout::Separate {
            pointer["text_layout"] = json!(text_layout.version());
        }
        // Every main index of the version has the page languages at this
        // version: a base this run built with them, or one the published
        // version has, with deltas written like it (05 §5.5.8). Absent: the
        // API filters and counts by title language, as before the setting.
        if page_language {
            pointer["page_language"] = json!(usnm_core::page_language::VERSION);
        }
        // The Japanese pages' index (#139), folded with the same analyzer
        // as the main indexes. An API without Japanese search ignores the
        // field; one with it folds Japanese queries as `fold` says.
        if let Some((id, pages)) = &ja {
            pointer["ja"] = json!({
                "indexes": [id],
                "fold": usnm_core::ja::fold_version(analyzer),
                "pages": pages,
            });
        }
        // The Congress index set (#229), searched with `source=congress`
        // only, with the same analyzer as the main indexes.
        if let Some(c) = &congress_out {
            pointer["congress"] = c.block.clone();
        }
        self.reference
            .put(
                if self.stage_index_storage {
                    "index-storage-pending.json"
                } else {
                    "current.json"
                },
                serde_json::to_vec_pretty(&pointer)?,
                "application/json",
            )
            .await?;
        // The pointer is committed (pending for a migration). A recording failure
        // is repaired by the next ordinary release after promotion.
        run.status = RunStatus::Published;
        run.published_at = Some(published_at);
        let recorded = async {
            self.state.update_run(&run, &etag).await?;
            if !self.stage_index_storage {
                self.state.set_current_version(&version).await?;
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(e) = recorded {
            return Err(PublishedUnrecorded(format!(
                "`{version}` pointer is committed (staged: {}), but recording failed: {e:#}",
                self.stage_index_storage
            ))
            .into());
        }
        tracing::info!(%version, docs, staged = self.stage_index_storage, "published");
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
    async fn published_run(&self) -> anyhow::Result<Option<(IndexRun, PublishedPointer)>> {
        // Index URIs are relative to Quickwit's configured Azure account.
        // Inheriting even an unchanged Congress/JA index would point at blobs
        // that exist only in the lake. No previous version forces all sets
        // to be rebuilt, not just the main base.
        if self.stage_index_storage {
            return Ok(None);
        }
        let Some((version, pointer)) = live_pointer(self.reference.as_ref()).await? else {
            return Ok(None);
        };
        let version = version.as_str();
        // A version built on-prem has its run in its snapshot (onprem.rs).
        let (mut run, etag) = crate::onprem::import_staged_run(
            &self.state,
            self.reference.as_ref(),
            version,
        )
        .await?
        .with_context(|| {
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
        let current = record_current_version(&self.state, version, &run).await?;
        if current.changed() {
            tracing::warn!(
                version,
                previous = current.previous.as_deref().unwrap_or(""),
                "ops/current repaired from current.json"
            );
        }
        let feature = |key: &str| pointer[key].as_u64().and_then(|v| u32::try_from(v).ok());
        let published = PublishedPointer {
            backend: pointer["backend"].as_str().unwrap_or("memory").to_owned(),
            grams: feature("common_grams"),
            american_stories: feature("american_stories"),
            decades: feature("decades"),
            text_layout: feature("text_layout"),
            page_language: feature("page_language"),
            congress: pointer.get("congress").filter(|c| c.is_object()).cloned(),
        };
        Ok(Some((run, published)))
    }

    /// How this release's main index lays its pages out by decade (05
    /// §5.5.5): a full base as `partition_decade` says; a delta (or an
    /// overlay-only release, which keeps the main indexes) as the published
    /// version has it, so every main index of the new version has the
    /// `decade` field or none does. A delta on a version laid out by decade
    /// tags its splits instead of partitioning them: it is small, a few
    /// splits at most, and partitions would cut those into one per decade,
    /// which every full-range search (the site's default) would open.
    fn decade_layout(&self, full: bool, published: Option<u32>) -> Decades {
        if full {
            return if self.partition_decade {
                Decades::Partitioned
            } else {
                Decades::Off
            };
        }
        match published {
            Some(v) if v == usnm_core::decade::VERSION => Decades::Tagged,
            Some(v) => {
                tracing::warn!(
                    published = v,
                    api = usnm_core::decade::VERSION,
                    "the published version is laid out by another version of the decades; this \
                     delta has no decade field, so searches skip no splits until the next full release"
                );
                Decades::Off
            }
            None => {
                if self.partition_decade {
                    tracing::info!(
                        "the published version isn't laid out by decade: --partition-decade \
                         takes effect at the next full release"
                    );
                }
                Decades::Off
            }
        }
    }

    /// The text fields of this release's main index (05 §5.5.7): a full
    /// base's as `single_text_field` says; a delta's (or an overlay-only
    /// release's, which keeps the main indexes) as the published version
    /// has them, so every main index of a version has the same fields.
    /// `published` is `None` for a version this code doesn't know, which
    /// takes a full base (`relayouting`).
    fn text_layout(&self, full: bool, published: Option<TextLayout>) -> TextLayout {
        if full {
            return if self.single_text_field {
                TextLayout::LATEST
            } else {
                TextLayout::Separate
            };
        }
        let layout = published.unwrap_or_default();
        if self.single_text_field && layout != TextLayout::LATEST {
            tracing::info!(
                from = layout.version(),
                to = TextLayout::LATEST.version(),
                "the published version's text fields are laid out differently from what \
                 --single-text-field builds: it takes effect at the next full release"
            );
        }
        layout
    }

    /// Whether this release's main index has the page languages (05
    /// §5.5.8): a full base's as `page_language` says; a delta's (or an
    /// overlay-only release's, which keeps the main indexes) as the
    /// published version has them, at this code's version, so every main
    /// index of a version has the fields or none does. A published version
    /// with another version of them gets a delta without: its
    /// `current.json` loses the key, and searches filter by title language
    /// until the next full rebuild.
    fn page_language_on(&self, full: bool, published: Option<u32>) -> bool {
        if full {
            return self.page_language;
        }
        match published {
            Some(v) if v == usnm_core::page_language::VERSION => true,
            Some(v) => {
                tracing::warn!(
                    published = v,
                    api = usnm_core::page_language::VERSION,
                    "the published version has another version of the page languages; this \
                     delta has none, so searches filter by title language until the next full release"
                );
                false
            }
            None => {
                if self.page_language {
                    tracing::info!(
                        "the published version has no page languages: --page-language takes \
                         effect at the next full release"
                    );
                }
                false
            }
        }
    }

    /// The pages `version`'s main indexes hold in a set of page languages
    /// other than their title's (its `page_languages.json`), which a
    /// version with page languages must have.
    async fn previous_moves(&self, version: &str) -> anyhow::Result<Moves> {
        let path = format!("{version}/{}", page_lang::PAGE_LANGUAGES_FILE);
        let bytes = self.reference.get(&path).await?.with_context(|| {
            format!("`{path}` is missing, though the published version has page languages")
        })?;
        let moves: Moves = serde_json::from_slice(&bytes).context(path.clone())?;
        anyhow::ensure!(
            moves.version == usnm_core::page_language::VERSION,
            "`{path}` is of page-language version {}, not {}",
            moves.version,
            usnm_core::page_language::VERSION
        );
        Ok(moves)
    }

    /// The split target of this release's main index (#283): a full
    /// base's as `split_docs` says; a delta's the template's, since a
    /// week's pages make a few splits either way.
    fn split_docs(&self, full: bool) -> Option<u64> {
        if full {
            return self.split_docs;
        }
        if let Some(docs) = self.split_docs {
            tracing::info!(
                split_docs = docs,
                "a delta keeps the template's split target: --split-docs takes effect at the \
                 next full release"
            );
        }
        None
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
        stories: Option<&Arc<american_stories::Source>>,
        latin: &[(&ocr_ja::JaPage, String)],
        latin_curated: &HashMap<String, (&ocr_ja::JaPage, String)>,
        latin_owned: &HashMap<String, String>,
        local: Option<&LocalRun>,
        layout: MainLayout,
        analyzer: Analyzer,
        mut moves: Option<&mut Moves>,
        report: &Reporter,
        resume: Option<&Resume>,
    ) -> anyhow::Result<(
        u64,
        Option<american_stories::Record>,
        Vec<(String, JaLatinPage)>,
        (ocr_local::replace::Tally, LocalReplaced),
    )> {
        // A finish replays the build: every document is made as the release
        // makes it, and none is sent; the stopped release's index takes
        // them in its place (`crate::finish`).
        let replay = resume.is_some();
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
        if replay {
            sink.attach(index_id, layout).await?;
        } else {
            sink.create(index_id, layout).await?;
        }
        let format = DocFormat {
            decade: layout.decades.on(),
            analyzer,
            text: layout.text,
        };
        // A partitioned base's pages go out a decade at a time, through
        // files on the writer's scratch disk (`crate::decade_order`).
        let mut order = match layout.decades {
            Decades::Partitioned if !replay => {
                let work = sink
                    .stats()
                    .work_dir
                    .clone()
                    .unwrap_or_else(std::env::temp_dir);
                Some(DecadeOrder::new(
                    &work,
                    layout.split_target(),
                    sink.line_sender().await?,
                    Some(lease.lost_flag()),
                )?)
            }
            _ => None,
        };
        // A line every 30 s and after each batch (the saved query
        // `release-progress` and the stall alert read them). Pages with
        // only American Stories' text join the expected count as they're
        // found: only their batch's parts say which pages LoC has no text for.
        let expected = scope
            .iter()
            .map(|b| {
                b.curated
                    .ok_pages
                    .saturating_sub(dedup.skipped_docs(&b.batch))
            })
            .sum();
        // A replay counts what it makes on counters of its own, and leaves
        // the stopped run's last progress (what it sent) as it is until the
        // index is sealed.
        let replayed =
            replay.then(|| Arc::new(SinkStats::replaying(sink.stats().work_dir.clone())));
        let progress = Progress::new(replayed.clone().unwrap_or_else(|| sink.stats()), expected);
        progress.snapshot().log(None);
        if !replay {
            progress::report(&self.state, version, &progress.snapshot()).await;
        }
        let _ticker = progress.every(progress::INTERVAL);
        // The same counts for the status page, in their own `ops` item: the
        // run item's ETag stays the release's alone.
        let _reporter = (!replay).then(|| {
            progress.report_every(progress::INTERVAL, self.state.clone(), version.to_owned())
        });
        // The replay's documents by decade, for the index's own count.
        let mut by_decade = finish::DecadeDocs::new();
        let mut docs = 0u64;
        let mut record = stories.map(|_| american_stories::Record::default());
        // The pages given our Latin text in place of LoC's (#203).
        let mut replaced: Vec<(String, JaLatinPage)> = Vec::new();
        // Our OCR from `ocr-local` weighed and given in place of LoC's (Step 9).
        let mut local_tally = ocr_local::replace::Tally::default();
        let mut local_replaced: LocalReplaced = Vec::new();
        // The next batch's American Stories text and curated parts download
        // while this one is built (they're the release's network reads); one
        // batch ahead, so two batches' downloads are held at most.
        let fetch_ahead = |b: &RunBatch| {
            Prefetch(tokio::spawn(prefetch_batch(
                Arc::clone(&self.curated),
                stories.cloned(),
                b.clone(),
            )))
        };
        let mut ahead = scope.first().map(fetch_ahead);
        for (i, b) in scope.iter().enumerate() {
            let current = ahead.take().context("no prefetch for the batch")?;
            ahead = scope.get(i + 1).map(fetch_ahead);
            let t_fetch = std::time::Instant::now();
            lease.check()?;
            let Prefetched {
                stories: loaded_stories,
                parts,
            } = current.join().await?;
            let mut fetch_secs = t_fetch.elapsed().as_secs_f64();
            // This batch's titles' readings of ours, dropped after the batch.
            let mine = match local {
                Some(l)
                    if l.listing
                        .has_any(b.curated.lccns.iter().map(String::as_str)) =>
                {
                    lease.check()?;
                    let loaded = l
                        .listing
                        .load(
                            self.curated.as_ref(),
                            b.curated.lccns.iter().map(String::as_str),
                            &l.revoked,
                        )
                        .await?;
                    tracing::info!(
                        batch = %b.batch,
                        pages = loaded.pages.len(),
                        revoked = loaded.revoked.len(),
                        parts = loaded.parts.len(),
                        "our OCR for the batch's titles"
                    );
                    if let Some(r) = resume {
                        r.log.check_built(
                            finish::BATCH_OCR_LOCAL_LINE,
                            Some(&b.batch),
                            Some(json!({
                                "pages": loaded.pages.len(),
                                "revoked": loaded.revoked.len(),
                                "parts": loaded.parts.len(),
                            })),
                        )?;
                    }
                    local_tally.add_parts(&loaded.parts);
                    loaded
                }
                _ => {
                    if let Some(r) = resume {
                        r.log
                            .check_built(finish::BATCH_OCR_LOCAL_LINE, Some(&b.batch), None)?;
                    }
                    ocr_local::replace::Loaded::default()
                }
            };
            // This batch's American Stories texts, dropped after the batch.
            let texts = match loaded_stories {
                Some(loaded) => {
                    tracing::info!(
                        batch = %b.batch,
                        pages = loaded.texts.len(),
                        text_mb = loaded.text_bytes / (1024 * 1024),
                        parts = loaded.parts.len(),
                        empty = loaded.empty,
                        "American Stories' text for the batch"
                    );
                    if let Some(r) = resume {
                        r.log.check_built(
                            finish::BATCH_STORIES_LINE,
                            Some(&b.batch),
                            Some(json!({
                                "pages": loaded.texts.len(),
                                "text_mb": loaded.text_bytes / (1024 * 1024),
                                "parts": loaded.parts.len(),
                                "empty": loaded.empty,
                            })),
                        )?;
                    }
                    if let Some(r) = record.as_mut() {
                        r.add(&loaded);
                    }
                    loaded.texts
                }
                None => HashMap::new(),
            };
            // The batch's parts, a few downloading ahead, each turned into
            // documents on several threads and sent before the next is
            // built: the documents and everything recorded about them come
            // out in part order, as when one thread read them, and only one
            // part's documents are held at a time.
            let ctx = PartContext {
                batch: b,
                dedup,
                texts: &texts,
                latin_curated,
                latin_owned,
                catalog,
                mine: &mine,
                local,
                format,
                page_language: moves.is_some(),
            };
            let (mut build_secs, mut lang_secs, mut add_secs) = (0f64, 0f64, 0f64);
            let wait_before = order.as_ref().map_or(0.0, |o| o.queue_wait_secs());
            // The parts past the prefetch's limit, downloading a few at once
            // from the start of this batch, in order (`RestParts`).
            let deferred: Vec<String> = parts
                .iter()
                .filter(|(_, b)| b.is_none())
                .map(|(p, _)| p.clone())
                .collect();
            let mut rest = (!deferred.is_empty())
                .then(|| RestParts::start(Arc::clone(&self.curated), deferred));
            for (path, bytes) in parts {
                lease.check()?;
                let bytes = match bytes {
                    Some(bytes) => bytes,
                    None => {
                        let t = std::time::Instant::now();
                        let (got, bytes) = rest
                            .as_mut()
                            .context("no download for the batch's remaining parts")?
                            .next()
                            .await?;
                        if got != path {
                            bail!("curated part `{got}` came in place of `{path}`");
                        }
                        fetch_secs += t.elapsed().as_secs_f64();
                        // The lock may have gone while the part downloaded.
                        lease.check()?;
                        bytes
                    }
                };
                let t_build = std::time::Instant::now();
                let part = build_part_blocking(&ctx, &path, &bytes)?;
                drop(bytes);
                build_secs += t_build.elapsed().as_secs_f64();
                let PartDocs {
                    docs: part_docs,
                    lines: part_lines,
                    replaced: part_replaced,
                    tally,
                    local_replaced: part_local,
                    with_as,
                    only_as,
                    without_locs,
                } = part;
                replaced.extend(part_replaced);
                local_tally.merge(tally);
                local_replaced.extend(part_local);
                // The pages that move to another set than their title's
                // (05 §5.5.8); their own languages were found as they were built.
                let t_lang = std::time::Instant::now();
                if let Some(m) = moves.as_deref_mut() {
                    for d in &part_docs {
                        m.record(d)?;
                    }
                }
                lang_secs += t_lang.elapsed().as_secs_f64();
                // Pages LoC has no usable text for are documents with another's.
                progress.add_expected(without_locs);
                if let Some(r) = record.as_mut() {
                    r.docs += with_as;
                    r.only += only_as;
                }
                let t_add = std::time::Instant::now();
                if let Some(stats) = &replayed {
                    let bytes: usize = part_lines.iter().map(|l| l.len() + 1).sum();
                    stats.replayed(part_docs.len() as u64, bytes as u64);
                    count_decades(&mut by_decade, &part_docs);
                } else {
                    for (d, line) in part_docs.iter().zip(part_lines) {
                        match order.as_mut() {
                            Some(o) => o.add_line(sink, d, line).await?,
                            None => sink.add_line(&line).await?,
                        }
                    }
                }
                add_secs += t_add.elapsed().as_secs_f64();
                docs += part_docs.len() as u64;
            }
            // Where the batch's time went, to find what bounds a release.
            tracing::info!(
                batch = %b.batch,
                fetch_secs = (fetch_secs * 10.0).round() / 10.0,
                build_secs = (build_secs * 10.0).round() / 10.0,
                lang_secs = (lang_secs * 10.0).round() / 10.0,
                add_secs = (add_secs * 10.0).round() / 10.0,
                queue_wait_secs = ((order.as_ref().map_or(0.0, |o| o.queue_wait_secs()) - wait_before) * 10.0).round() / 10.0,
                "batch timing"
            );
            progress.snapshot().log(Some(&b.batch));
        }
        if let Some(r) = &record {
            tracing::info!(
                docs_with_text = r.docs,
                only_american_stories = r.only,
                "American Stories' text indexed"
            );
        }
        // The pages LoC ships without text that our OCR read Latin text on
        // (#203): after the batches, each with its own batch's name.
        if !latin.is_empty() {
            progress.add_expected(latin.len() as u64);
            for (p, text) in latin {
                lease.check()?;
                let title = catalog.title(&p.key.lccn).context("title")?;
                let place = catalog.place(&title.place_id).context("place")?;
                // With the version's decade field, and a decade at a time
                // in a partitioned base, like LoC's pages.
                let mut d = ocr_ja::latin_doc(p, text, title, place, format);
                if let Some(m) = moves.as_deref_mut() {
                    page_lang::annotate_one(&mut d);
                    m.record(&d)?;
                }
                if let Some(stats) = &replayed {
                    stats.replayed(1, 0);
                    count_decades(&mut by_decade, std::slice::from_ref(&d));
                    continue;
                }
                match order.as_mut() {
                    Some(o) => o.add(sink, &d).await?,
                    None => sink.add(&d).await?,
                }
            }
            docs += latin.len() as u64;
        }
        if !latin.is_empty() || !replaced.is_empty() {
            tracing::info!(
                missing = latin.len(),
                in_place_of_locs = replaced.len(),
                "indexed our Japanese OCR's Latin text"
            );
        }
        if let Some(o) = order.as_mut() {
            lease.check()?;
            let s = drain(o, sink, lease).await?;
            tracing::info!(
                runs = s.runs,
                pages = s.docs,
                peak_held_pages = s.peak_docs,
                peak_disk_mb = s.peak_bytes / (1024 * 1024),
                "sent the pages a decade at a time"
            );
        }
        drop(order);
        // The replay has made what the run sent: the same numbers as the
        // run's lines, the same count, and decade by decade what the index
        // holds.
        if let Some(r) = resume {
            r.log.check_built(
                finish::STORIES_INDEXED_LINE,
                None,
                record
                    .as_ref()
                    .map(|r| json!({"docs_with_text": r.docs, "only_american_stories": r.only})),
            )?;
            r.log.check_built(
                finish::LATIN_INDEXED_LINE,
                None,
                (!latin.is_empty() || !replaced.is_empty())
                    .then(|| json!({"missing": latin.len(), "in_place_of_locs": replaced.len()})),
            )?;
            r.log.check_built(
                finish::DECADES_SENT_LINE,
                None,
                (layout.decades == Decades::Partitioned).then(|| json!({"pages": docs})),
            )?;
            let held = sink.published_docs(index_id).await?;
            if docs != held {
                bail!("the replay made {docs} documents; `{index_id}` holds {held}: not finishing");
            }
            r.check_count(index_id, docs)?;
            tracing::info!(
                index = index_id,
                docs,
                decades = ?by_decade,
                "the replay made the documents the run sent"
            );
        }
        // The last commit and the merge wait, given up as soon as the lock is.
        report.step(Step::Merging).await;
        tokio::select! {
            r = sink.finish(docs) => r?,
            () = lease.lost() => lease.check()?,
        }
        // Decade by decade too, once the index is sealed and its split list
        // stays as it is.
        if replay && format.decade {
            if let Some(held) = sink.decade_docs(index_id).await? {
                if held != by_decade {
                    bail!(
                        "`{index_id}` holds {held:?} documents by decade; the replay made \
                         {by_decade:?}: not finishing"
                    );
                }
            }
        }
        if local.is_some() {
            tracing::info!(
                considered = local_tally.considered,
                replaced = local_tally.replaced,
                kept = ?local_tally.kept,
                "weighed our OCR against LoC's text"
            );
        }
        if let Some(m) = &moves {
            tracing::info!(
                pages = m.pages(),
                "pages whose own languages differ from their title's, in the version"
            );
        }
        progress.snapshot().log(None);
        progress::report(&self.state, version, &progress.snapshot()).await;
        Ok((docs, record, replaced, (local_tally, local_replaced)))
    }

    /// What this release does with the Congress set (#229): nothing without
    /// the setting or Congress pages with text; keep the published set when
    /// it was built by this rule from the same units with this analyzer on
    /// this engine; otherwise build a new one.
    async fn congress_plan(
        &self,
        previous: Option<&IndexRun>,
        pointer: Option<&PublishedPointer>,
        analyzer: Analyzer,
        switching: bool,
    ) -> anyhow::Result<CongressPlan> {
        let published = pointer.and_then(|p| p.congress.as_ref());
        if !self.congress {
            if published.is_some() {
                tracing::warn!(
                    "the published version has a Congress index set and --congress is off: \
                     this version leaves it out"
                );
            }
            return Ok(CongressPlan::Off);
        }
        let listing = congress::release::Listing::load(self.curated.as_ref()).await?;
        tracing::info!(
            units = listing.units.len(),
            indexable = listing.indexable(),
            left_out = ?listing.left_out,
            "Congress pages"
        );
        if listing.indexable() == 0 {
            if published.is_some() {
                tracing::warn!(
                    "no curated Congress pages with text, and the published version has a \
                     Congress index set: this version leaves it out"
                );
            }
            tracing::info!("no curated Congress pages with text; the version has no Congress set");
            return Ok(CongressPlan::Off);
        }
        let grams = usnm_core::common_grams::version(analyzer);
        // An index of another engine (a backend switch) is never kept.
        if let (Some(prev), Some(block), false) = (previous, published, switching) {
            let path = format!("{}/{}", prev.index_version, congress::release::FILE);
            if let Some(record) = self.reference.get(&path).await? {
                let parsed: Value = serde_json::from_slice(&record).context(path.clone())?;
                if congress::release::unchanged(&parsed, &listing, grams)?
                    && block["common_grams"] == json!(grams)
                {
                    let bpath = format!(
                        "{}/{}",
                        prev.index_version,
                        congress::release::BASELINES_FILE
                    );
                    let baselines = self
                        .reference
                        .get(&bpath)
                        .await?
                        .with_context(|| format!("`{bpath}` is missing"))?;
                    let index = parsed["index"]
                        .as_str()
                        .with_context(|| format!("`{path}` names no index"))?
                        .to_owned();
                    tracing::info!(%index, "the Congress pages are unchanged; keeping the published set");
                    return Ok(CongressPlan::Keep {
                        index,
                        block: block.clone(),
                        files: vec![
                            (congress::release::FILE, record),
                            (congress::release::BASELINES_FILE, baselines),
                        ],
                    });
                }
            }
        }
        Ok(CongressPlan::Build(listing))
    }

    /// The version's Congress set: kept, or built from scratch into
    /// `congress-{stamp}-{n}` from each unit's parts (checked against its
    /// counts), one document per page with text.
    async fn congress_set(
        &self,
        lease: &WriterLease,
        sink: &mut dyn IndexSink,
        version: &str,
        plan: &CongressPlan,
        analyzer: Analyzer,
    ) -> anyhow::Result<Option<CongressOut>> {
        let listing = match plan {
            CongressPlan::Off => return Ok(None),
            CongressPlan::Keep {
                index,
                block,
                files,
            } => {
                return Ok(Some(CongressOut {
                    index: index.clone(),
                    built: false,
                    block: block.clone(),
                    files: files.clone(),
                }))
            }
            CongressPlan::Build(listing) => listing,
        };
        let id = congress::index_id(version);
        sink.create_with(&id, congress::TEMPLATE).await?;
        let mut n = 0u64;
        for (unit, counts) in &listing.units {
            for part in &counts.parts {
                lease.check()?;
                let rows = congress::curated::read_checked(self.curated.as_ref(), part)
                    .await
                    .with_context(|| format!("{}/{}", unit.series, unit.unit))?;
                for r in rows.iter().filter(|r| r.indexable()) {
                    sink.add(&congress::congress_doc(&r.to_page(), analyzer))
                        .await?;
                    n += 1;
                }
            }
        }
        anyhow::ensure!(
            n == listing.indexable(),
            "the Congress parts hold {n} pages with text; their counts say {}",
            listing.indexable()
        );
        tokio::select! {
            r = sink.finish(n) => r?,
            () = lease.lost() => lease.check()?,
        }
        tracing::info!(index = %id, docs = n, units = listing.units.len(), "built the Congress index");
        let grams = usnm_core::common_grams::version(analyzer);
        let baselines = listing.baselines();
        let block = json!({
            "indexes": [id],
            "common_grams": grams,
            "series": congress::release::series_block(&baselines),
        });
        let record = congress::release::record(listing, &id, n, grams);
        Ok(Some(CongressOut {
            index: id,
            built: true,
            block,
            files: vec![
                (congress::release::FILE, serde_json::to_vec(&record)?),
                (
                    congress::release::BASELINES_FILE,
                    serde_json::to_vec(&baselines)?,
                ),
            ],
        }))
    }

    /// Whether a release with no newly curated batches and no Latin text
    /// to add publishes the overlay on the published main indexes (04
    /// §4.8): when our Japanese OCR has changed and has a page to index.
    /// Logs why not.
    async fn overlay_releasable(
        &self,
        previous: Option<&IndexRun>,
        overlay: &ocr_ja::Overlay,
    ) -> anyhow::Result<bool> {
        let prev = previous.expect("incremental has a previous version");
        if !self.overlay_changed(&prev.index_version, overlay).await? {
            tracing::info!(
                "no newly curated batches with catalogued titles, and no new Japanese OCR; \
                 nothing to release"
            );
            return Ok(false);
        }
        // The run records the Japanese index as the index it wrote, so
        // there must be one: OCR that changed without a page to index waits.
        if !overlay.pages.iter().any(ocr_ja::JaPage::indexable) {
            tracing::info!("the Japanese OCR changed but has no page to index; nothing to release");
            return Ok(false);
        }
        tracing::info!(
            "no newly curated batches; releasing the new Japanese OCR on the same indexes"
        );
        Ok(true)
    }

    /// Whether the published version weighed every page of `overlay` for
    /// our Latin text: it was built from the same overlay parts with
    /// `--ja-latin` at this rule (its `ocr_ja.json` has `latin.rule`). A
    /// release with the setting weighs them all; one before #203's
    /// follow-up, overlay-only, weighed none and recorded no rule.
    async fn latin_settled(
        &self,
        previous: Option<&IndexRun>,
        overlay: &ocr_ja::Overlay,
    ) -> anyhow::Result<bool> {
        let prev = previous.expect("incremental has a previous version");
        let path = format!("{}/{}", prev.index_version, ocr_ja::OCR_JA_FILE);
        let Some(bytes) = self.reference.get(&path).await? else {
            return Ok(false);
        };
        let record: Value = serde_json::from_slice(&bytes).context(path)?;
        Ok(record["latin"]["rule"] == json!(ocr_ja::LATIN_VERSION)
            && record["parts"] == serde_json::to_value(&overlay.parts)?)
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
    // The lease, sink and moves are the release's, threaded through.
    #[allow(clippy::too_many_arguments)]
    async fn build_ja_index(
        &self,
        lease: &WriterLease,
        sink: &mut dyn IndexSink,
        version: &str,
        overlay: &ocr_ja::Overlay,
        catalog: &Catalog,
        analyzer: Analyzer,
        moves: Option<&Moves>,
    ) -> anyhow::Result<Option<(String, u64)>> {
        // On a version with page languages (05 §5.5.8), each page carries
        // the languages the page-language baselines count it in, so that a
        // Japanese search filters and counts as its baseline does.
        let by_page = moves.map(Moves::sets_by_page);
        let mut docs = Vec::new();
        for p in overlay.pages.iter().filter(|p| p.indexable()) {
            let title = catalog.title(&p.key.lccn).context("title")?;
            let place = catalog.place(&title.place_id).context("place")?;
            let mut doc = ocr_ja::ja_doc(p, title, place, analyzer);
            if let Some(by_page) = &by_page {
                Moves::annotate_as_counted(&mut doc, title, by_page);
            }
            docs.push(doc);
        }
        if docs.is_empty() {
            return Ok(None);
        }
        let id = ocr_ja::index_id(version);
        let template = crate::sink::page_language_fields(moves.is_some(), ocr_ja::JA_TEMPLATE)?;
        sink.create_with(&id, &template).await?;
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

    /// Write `{version}/` (titles, places, baselines, pages per title and
    /// their first and last dates, the batch list, the hidden duplicate
    /// copies, manifest last) and return the version's date bounds. A page in more than one batch counts once.
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
        latin: &Latin<'_>,
        stories: Option<(&american_stories::Source, &american_stories::Record)>,
        local: &LocalFiles,
        moves: Option<&Moves>,
        congress: Option<&CongressOut>,
        build: &Value,
        analyzer: Analyzer,
        finishing: bool,
    ) -> anyhow::Result<((NaiveDate, NaiveDate), u64)> {
        let mut baselines: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::new();
        // Every page counted once, by its title: the same pages as the
        // baselines, which sum them by place instead.
        let mut title_pages: BTreeMap<String, u64> = BTreeMap::new();
        // And each title's first and last day with a page.
        let mut title_days = TitleDays::default();
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
                let total = title_pages.entry(lccn.clone()).or_default();
                for (day, pages) in days {
                    if pages > 0 {
                        title_days.add(&lccn, day);
                    }
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
        // the version, which counted it already: those are left out. A page
        // with a main-index document of our Latin text (#203) is one of these,
        // counted here and nowhere else.
        let mut added = 0u64;
        for p in overlay.pages.iter().filter(|p| {
            p.missing_from_curation() && !latin.curated_keys.contains_key(&p.key.doc_id())
        }) {
            let title = catalog.title(&p.key.lccn).context("title")?;
            let day = day_number(p.key.date);
            *baselines
                .entry(title.place_id.clone())
                .or_default()
                .entry(day)
                .or_default() += 1;
            *title_pages.entry(p.key.lccn.clone()).or_default() += 1;
            title_days.add(&p.key.lccn, day);
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
        // The same pages by their own languages (05 §5.5.8): the title
        // sets with the moves applied, so a page still counts once.
        let page_language_baselines = moves
            .map(|m| m.apply(&by_language, catalog).map(language_baselines))
            .transpose()?;
        let language_baselines = language_baselines(by_language);

        let mut snapshot_files: Vec<(&str, Vec<u8>)> = vec![
            ("titles.json", serde_json::to_vec(&titles)?),
            ("places.json", serde_json::to_vec(&places)?),
            ("baselines.json", serde_json::to_vec(&baselines)?),
            (TITLE_PAGES_FILE, serde_json::to_vec(&title_pages)?),
            (TITLE_DATES_FILE, serde_json::to_vec(&title_days.dates())?),
            (
                LANGUAGE_BASELINES_FILE,
                serde_json::to_vec(&language_baselines)?,
            ),
            (RUN_BATCHES_FILE, serde_json::to_vec(batches)?),
            (DUPLICATES_FILE, serde_json::to_vec(&dedup.hidden)?),
        ];
        // Only for a version with page languages, so one without has the
        // same files as before.
        if let (Some(m), Some(b)) = (moves, &page_language_baselines) {
            snapshot_files.push((
                page_lang::PAGE_LANGUAGE_BASELINES_FILE,
                serde_json::to_vec(b)?,
            ));
            snapshot_files.push((page_lang::PAGE_LANGUAGES_FILE, serde_json::to_vec(m)?));
        }
        // Which overlay parts the Japanese pages came from (only when there are any,
        // so a version without them has the same files as before).
        if !overlay.parts.is_empty() {
            let mut record = json!({
                "fold": usnm_core::ja::fold_version(analyzer),
                "index": ja.map(|(id, _)| id),
                "indexed": ja.map_or(0, |(_, n)| *n),
                "pages": overlay.pages.len(),
                "added_to_baselines": added,
                // Pages with a main-index document of our Latin text (#203):
                // added by this release, in the version, and hidden.
                "latin": {
                    "added": latin.added,
                    "pages": latin.version.pages.len(),
                    "hidden": latin.hidden,
                },
                "skipped": overlay.skipped,
                // What the OCR found: Japanese text, near-blank pages, mostly Latin.
                "kinds": ocr_ja::kinds(&overlay.pages),
                "parts": overlay.parts,
            });
            // With the setting, every page of the overlay was weighed for
            // our Latin text by this rule (`latin_settled`).
            if self.ja_latin {
                record["latin"]["rule"] = json!(ocr_ja::LATIN_VERSION);
            }
            snapshot_files.push((ocr_ja::OCR_JA_FILE, serde_json::to_vec(&record)?));
        }
        // The pages the main indexes hold with our Latin text, for the API to
        // mark their hits as our OCR (only when there are any).
        if !latin.version.pages.is_empty() {
            snapshot_files.push((JA_LATIN_FILE, serde_json::to_vec(latin.version)?));
        }
        // Which American Stories parts the new main index was built from
        // (only when it was built with them). The list can be long, so the
        // manifest, which the API reads, has only its summary.
        if let Some((source, record)) = stories {
            snapshot_files.push((
                american_stories::FILE,
                serde_json::to_vec(&record.file(source))?,
            ));
        }
        // Our OCR from `ocr-local` (Step 9): the pages the main indexes hold
        // with it, for the API, and with the setting, how this run weighed it.
        if let Some(body) = &local.pages {
            snapshot_files.push((usnm_state::ocr_local::OCR_LOCAL_PAGES_FILE, body.clone()));
        }
        if let Some(record) = &local.record {
            snapshot_files.push((
                usnm_state::ocr_local::OCR_LOCAL_FILE,
                serde_json::to_vec(record)?,
            ));
        }
        // Freeze the credits used by this version, and refuse publication if
        // any serving engine key lacks complete attribution.
        let local_pages = local
            .pages
            .as_ref()
            .map(|bytes| {
                usnm_state::ocr_local::OcrLocalPages::decode(bytes, batches.len())
                    .map_err(|e| anyhow::anyhow!(e))
            })
            .transpose()?;
        // Japanese search text and main-index Latin text may share a page.
        // Count that page once per engine, while retaining both engines if
        // the two indexes use different readings of the same page.
        let mut japanese = BTreeMap::<(&str, &str), BTreeSet<String>>::new();
        for p in overlay.pages.iter().filter(|p| p.indexable()) {
            japanese
                .entry((&p.ocr_source, &p.ocr_engine))
                .or_default()
                .insert(p.key.doc_id());
        }
        let hidden: BTreeSet<_> = dedup
            .hidden
            .iter()
            .map(|h| (h.doc_id.as_str(), h.batch.as_str()))
            .collect();
        for (doc_id, p) in &latin.version.pages {
            if hidden.contains(&(doc_id.as_str(), p.batch.as_str())) {
                continue;
            }
            japanese
                .entry((&p.ocr_source, &p.ocr_engine))
                .or_default()
                .insert(doc_id.clone());
        }
        let keys = japanese.keys().map(|(_, engine)| *engine).chain(
            local_pages
                .iter()
                .flat_map(|p| p.engines.iter().map(|e| e.engine.as_str())),
        );
        let mut credits = usnm_ocr_route::ENGINES.credits(keys)?;
        let pages = japanese.values().flatten().collect::<BTreeSet<_>>().len() as u64;
        credits.japanese = usnm_ocr_route::credits::JapaneseUsage {
            pages,
            by_engine: japanese
                .into_iter()
                .map(
                    |((source, engine), pages)| usnm_ocr_route::credits::EngineUsage {
                        source: source.to_owned(),
                        engine: engine.to_owned(),
                        pages: pages.len() as u64,
                    },
                )
                .collect(),
        };
        if !credits.engines.is_empty() {
            snapshot_files.push((usnm_ocr_route::credits::FILE, serde_json::to_vec(&credits)?));
        }
        // The Congress set's units and baselines (#229), when it has one.
        if let Some(c) = congress {
            snapshot_files.extend(c.files.iter().map(|(n, b)| (*n, b.clone())));
        }
        let mut files = Vec::new();
        for (name, body) in snapshot_files {
            files.push(json!({
                "path": name,
                "sha256": hex(&Sha256::digest(&body)),
                "bytes": body.len(),
            }));
            self.put_new(&format!("{version}/{name}"), body, finishing)
                .await?;
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
        if let Some((source, record)) = stories {
            manifest["built_from"]["american_stories"] = record.summary(source);
        }
        if let Some(summary) = &local.summary {
            manifest["built_from"]["ocr_local"] = summary.clone();
        }
        if let Some(m) = moves {
            manifest["page_languages"] = m.summary();
        }
        if let Some(c) = congress {
            manifest["built_from"]["congress"] = json!({
                "index": c.index,
                "built": c.built,
                "series": c.block["series"],
            });
        }
        // What built it (#161), with the index templates in full.
        manifest["build"] = build.clone();
        self.put_new(
            &format!("{version}/manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
            finishing,
        )
        .await?;
        Ok(((date_from_day(first), date_from_day(last)), added))
    }

    /// The doc ids in `wanted` that some batch of the version has in its
    /// curated parts, with which of those copies have LoC's text. Only the
    /// parts of batches holding these pages' titles are read, without
    /// their text.
    async fn curated_keys(
        &self,
        batches: &[RunBatch],
        wanted: &BTreeSet<String>,
    ) -> anyhow::Result<BTreeMap<String, CuratedCopies>> {
        // A doc id starts with its title's LCCN (`sn83025517_1945-01-01_ed-1_seq-4`).
        let lccns: BTreeSet<&str> = wanted
            .iter()
            .filter_map(|id| id.split_once('_').map(|(lccn, _)| lccn))
            .collect();
        let mut found = BTreeMap::new();
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
                        let c: &mut CuratedCopies = found.entry(id).or_default();
                        if row.status == TextStatus::Ok {
                            c.with_text = true;
                        } else {
                            c.without_text.insert(b.batch.clone());
                        }
                    }
                    Ok(())
                })
                .with_context(|| path.clone())?;
            }
        }
        Ok(found)
    }

    /// The version's [`LocalFiles`]: the published version's pages carried
    /// forward on a delta (`previous`), less the copies the version hides,
    /// plus `replaced`; and with the setting, the record of this run.
    async fn ocr_local_files(
        &self,
        previous: Option<&IndexRun>,
        batches: &[RunBatch],
        hidden: &[dedup::Hidden],
        replaced: &LocalReplaced,
        run: Option<(&LocalRun, &ocr_local::replace::Tally)>,
    ) -> anyhow::Result<LocalFiles> {
        use usnm_state::ocr_local::{Builder, OcrLocalPages, OCR_LOCAL_PAGES_FILE};
        let index: HashMap<&str, u16> = batches
            .iter()
            .enumerate()
            .map(|(i, b)| Ok((b.batch.as_str(), u16::try_from(i)?)))
            .collect::<Result<_, std::num::TryFromIntError>>()
            .context("more than 65,536 batches")?;
        // A delta's batch list starts with its published version's, in the
        // same order, so the published indexes still hold.
        let mut pages = match previous {
            Some(prev) => {
                let path = format!("{}/{OCR_LOCAL_PAGES_FILE}", prev.index_version);
                match self.reference.get(&path).await? {
                    Some(bytes) => {
                        let before = OcrLocalPages::decode(&bytes, batches.len())
                            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
                        Builder::from_pages(&before, |e| Some(e.batch))
                            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?
                    }
                    None => Builder::default(),
                }
            }
            None => Builder::default(),
        };
        let carried = pages.len();
        // A copy the version hides isn't what its hits show.
        let mut dropped = 0u64;
        for h in hidden {
            if pages.batch_of(&h.doc_id).is_some()
                && pages.batch_of(&h.doc_id) == index.get(h.batch.as_str()).copied()
            {
                pages.remove(&h.doc_id);
                dropped += 1;
            }
        }
        for (doc_id, engine, batch) in replaced {
            let i = *index
                .get(batch.as_str())
                .with_context(|| format!("`{batch}` isn't in the version"))?;
            pages
                .add(doc_id, engine.clone(), i)
                .map_err(|e| anyhow::anyhow!(e))?;
        }
        if dropped > 0 {
            tracing::warn!(
                pages = dropped,
                "pages indexed with our OCR's text now have another copy; dropping them"
            );
        }
        let pages = pages.finish();
        let mut files = LocalFiles {
            pages: (!pages.is_empty())
                .then(|| pages.encode())
                .transpose()
                .map_err(|e| anyhow::anyhow!(e))?,
            ..LocalFiles::default()
        };
        if let Some((run, tally)) = run {
            let summary = tally.parts_summary();
            let mut record = ocr_local::rule::record();
            record["parts"] = summary.clone();
            record["pages"] = serde_json::to_value(tally)?;
            record["version"] = json!({
                "pages": pages.len(),
                "carried": carried,
                "dropped": dropped,
                "added": replaced.len(),
            });
            record["revocations"] = run
                .revoked
                .iter()
                .map(|r| json!({"path": r.path, "reason": r.reason, "reference": r.reference}))
                .collect();
            // Of published batches, by stored scores (`Listing::estimate`).
            record["estimate_published"] = json!(run.estimate);
            files.record = Some(record);
            files.summary = Some(summary);
        }
        Ok(files)
    }

    /// The pages `version`'s main indexes hold with our Latin text (its
    /// `ja_latin.json`; none if it has none).
    async fn previous_latin(&self, version: &str) -> anyhow::Result<JaLatin> {
        let path = format!("{version}/{JA_LATIN_FILE}");
        match self.reference.get(&path).await? {
            Some(bytes) => serde_json::from_slice(&bytes).context(path),
            None => Ok(JaLatin::default()),
        }
    }

    /// Write a snapshot file, create-only. A finish (`finishing`) takes one
    /// a stopped run wrote already when it has the same bytes.
    async fn put_new(&self, path: &str, body: Vec<u8>, finishing: bool) -> anyhow::Result<()> {
        // Every snapshot file is JSON but `ocr_local_pages.bin`.
        let content_type = if path.ends_with(".bin") {
            "application/octet-stream"
        } else {
            "application/json"
        };
        let same = finishing.then(|| body.clone());
        if !self.reference.put_new(path, body, content_type).await? {
            if same.is_some() && self.reference.get(path).await? == same {
                tracing::info!(
                    path,
                    "the stopped run wrote this snapshot file already, the same"
                );
                return Ok(());
            }
            bail!("`{path}` already exists; reference snapshots are immutable");
        }
        Ok(())
    }
}

/// Count `docs` by their `decade` field (05 §5.5.5), those that have one.
fn count_decades(by: &mut finish::DecadeDocs, docs: &[Value]) {
    for d in docs {
        if let Some(decade) = d["decade"].as_u64().and_then(|d| u16::try_from(d).ok()) {
            *by.entry(decade).or_default() += 1;
        }
    }
}

/// Send the pages `order` still holds, given up as soon as the writer lock
/// is: up to 15 decades of 30,000 pages can take longer than the lock lasts
/// without renewal.
async fn drain(
    order: &mut DecadeOrder,
    sink: &mut dyn IndexSink,
    lease: &WriterLease,
) -> anyhow::Result<crate::decade_order::OrderStats> {
    tokio::select! {
        r = order.finish(sink) => r,
        () = lease.lost() => {
            lease.check()?;
            bail!("the writer lock was lost while sending the held pages")
        }
    }
}

/// Each title's first and last day with a published page, as day numbers
/// (`usnm_core::time::day_number`).
#[derive(Debug, Default)]
struct TitleDays(BTreeMap<String, (u32, u32)>);

impl TitleDays {
    fn add(&mut self, lccn: &str, day: u32) {
        match self.0.get_mut(lccn) {
            Some((first, last)) => {
                *first = (*first).min(day);
                *last = (*last).max(day);
            }
            None => {
                self.0.insert(lccn.to_owned(), (day, day));
            }
        }
    }

    /// `{lccn: {first, last}}`, as `title_dates.json` has it.
    fn dates(&self) -> BTreeMap<&str, TitleDates> {
        self.0
            .iter()
            .map(|(lccn, &(first, last))| {
                (
                    lccn.as_str(),
                    TitleDates {
                        first: date_from_day(first),
                        last: date_from_day(last),
                    },
                )
            })
            .collect()
    }
}

/// The languages a title lists, sorted and without repeats; `None` when it
/// lists none (no language filter matches its pages).
fn language_set(title: &Title) -> Option<Vec<String>> {
    page_lang::title_set(title)
}

/// `{languages: {place: {day: pages}}}` as the snapshot's language baselines.
fn language_baselines(by_language: page_lang::BySet) -> LanguageBaselines {
    LanguageBaselines {
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
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

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

    #[test]
    fn title_days_keep_each_titles_first_and_last_page() {
        let day = |y, m, d| day_number(NaiveDate::from_ymd_opt(y, m, d).unwrap());
        let mut days = TitleDays::default();
        days.add("sn2", day(1890, 5, 1));
        days.add("sn1", day(1900, 1, 2));
        days.add("sn1", day(1883, 6, 30));
        days.add("sn1", day(1922, 12, 31));
        days.add("sn1", day(1901, 1, 1));
        assert_eq!(
            serde_json::to_value(days.dates()).unwrap(),
            json!({
                "sn1": {"first": "1883-06-30", "last": "1922-12-31"},
                "sn2": {"first": "1890-05-01", "last": "1890-05-01"},
            })
        );
    }

    /// A sink that never answers, like a writer that stopped taking pages.
    struct Stuck;

    #[async_trait::async_trait]
    impl IndexSink for Stuck {
        async fn create(&mut self, _: &str, _: crate::sink::MainLayout) -> anyhow::Result<()> {
            Ok(())
        }
        async fn add(&mut self, _: &Value) -> anyhow::Result<()> {
            std::future::pending().await
        }
        async fn finish(&mut self, _: u64) -> anyhow::Result<()> {
            Ok(())
        }
        fn backend(&self) -> &'static str {
            "stuck"
        }
    }

    #[tokio::test]
    async fn a_lost_lease_stops_sending_the_held_pages() {
        let dir = tempfile::tempdir().unwrap();
        let mut order = DecadeOrder::new(dir.path(), 1000, None, None).unwrap();
        let mut sink = Stuck;
        order
            .add(&mut sink, &json!({"doc_id": "a", "decade": 1890}))
            .await
            .unwrap();
        let lost = Arc::new(AtomicBool::new(false));
        let lease = WriterLease::for_test(lost.clone());
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            lost.store(true, Ordering::SeqCst);
        });
        let err = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            drain(&mut order, &mut sink, &lease),
        )
        .await
        .expect("the lost lease ends the drain")
        .unwrap_err()
        .to_string();
        assert!(err.contains("could not be renewed"), "{err}");
    }
}
