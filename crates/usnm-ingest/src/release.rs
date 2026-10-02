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

use crate::catalog::{Catalog, Place, Title};
use crate::curated::{read_part, CuratedRow};
use crate::merges::IndexLayout;
use crate::progress::{self, Progress};
use crate::sink::IndexSink;
use crate::source::hex;
use crate::state::{
    Curated, IndexRun, RunBatch, RunStatus, State, RUN_BATCHES_FILE, TITLE_PAGES_FILE,
};
use crate::worker::Counts;

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
    /// Names versions and stamps `published_at`.
    pub now: DateTime<Utc>,
}

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
    /// 90 minutes) race against it, so a writer that may no longer hold the
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
            .run_in_span(sink, lease, &span)
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
    ) -> anyhow::Result<Option<Published>> {
        self.confirm(lease).await?;
        let current = Catalog::load(self.reference.as_ref()).await?;
        let (previous, previous_backend) = match self.published_run().await? {
            Some((run, backend)) => (Some(run), Some(backend)),
            None => (None, None),
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
        let full = self.full
            || switching
            || previous
                .as_ref()
                .is_none_or(|p| p.indexes.len() > MAX_DELTAS);
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
            if new.is_empty() {
                tracing::info!("no newly curated batches; nothing to release");
                return Ok(None);
            }
            let published_catalog = self.load_snapshot_catalog(&prev.index_version).await?;
            let catalog = Catalog::carried_forward(&published_catalog, &current)?;
            let new = catalogued(new, &catalog);
            if new.is_empty() {
                tracing::info!(
                    "no newly curated batches with catalogued titles; nothing to release"
                );
                return Ok(None);
            }
            let mut all = prev_batches;
            all.extend(new.iter().cloned());
            (new, all, catalog, prev.indexes.clone())
        };
        if scope.is_empty() {
            tracing::info!("no curated batches; nothing to release");
            return Ok(None);
        }

        let (version, index_id) = self.next_names(full).await?;
        indexes.push(index_id.clone());
        let mut run = IndexRun {
            id: version.clone(),
            index_version: version.clone(),
            full,
            indexes: indexes.clone(),
            new_index: index_id.clone(),
            batch_count: Some(version_batches.len() as u64),
            batch_list: None,
            batches: None,
            status: RunStatus::Building,
            docs: 0,
            pages: version_batches.iter().map(|b| b.curated.pages).sum(),
            started_at: Utc::now(),
            published_at: None,
            previous_version: previous.as_ref().map(|p| p.index_version.clone()),
            last_error: None,
        };
        if !self.state.create_run(&run).await? {
            bail!("index run `{version}` already exists");
        }
        let (_, mut etag) = self.state.run(&version).await?.context("run vanished")?;
        span.record("version", version.as_str());
        span.record("full", full);
        span.record("batches", scope.len());
        tracing::info!(%version, index = %index_id, full, batches = scope.len(), "building index");

        let outcome = async {
            let docs = self
                .build_index(lease, sink, &version, &index_id, &scope, &catalog)
                .await?;
            // What every index of the version is made of, for the log and
            // the manifest: how many splits a cold search opens (05 §5.5.1).
            let layout = sink.layout(&indexes).await?;
            for l in &layout {
                l.log();
            }
            let bounds = self
                .write_snapshot(&version, &version_batches, &catalog, &layout)
                .await?;
            Ok::<_, anyhow::Error>((docs, bounds))
        }
        .await;
        let (docs, (from, to)) = match outcome {
            Ok(v) => v,
            Err(e) => {
                run.status = RunStatus::Failed;
                run.last_error = Some(format!("{e:#}").chars().take(2000).collect());
                let _ = self.state.update_run(&run, &etag).await;
                return Err(e);
            }
        };
        run.docs = docs;
        run.batch_list = Some(format!("{version}/{RUN_BATCHES_FILE}"));
        span.record("docs", docs);
        etag = self.state.update_run(&run, &etag).await?;

        // Publish: the version pointer is the last write (04 §4.7), and only
        // while this release still holds the writer lock.
        self.confirm(lease).await?;
        let pointer = json!({
            "index_version": version,
            "backend": sink.backend(),
            "indexes": indexes,
            "reference": version,
            "bounds": {"from": from.to_string(), "to": to.to_string()},
            "published_at": self.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "previous_version": run.previous_version,
            "synthetic": self.synthetic,
        });
        self.reference
            .put(
                "current.json",
                serde_json::to_vec_pretty(&pointer)?,
                "application/json",
            )
            .await?;
        run.status = RunStatus::Published;
        run.published_at = Some(Utc::now());
        self.state.update_run(&run, &etag).await?;
        self.state.set_current_version(&version).await?;
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
    async fn published_run(&self) -> anyhow::Result<Option<(IndexRun, String)>> {
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
            run.published_at.get_or_insert_with(Utc::now);
            self.state.update_run(&run, &etag).await?;
        }
        if self.state.current_version().await?.as_deref() != Some(version) {
            self.state.set_current_version(version).await?;
        }
        let backend = pointer["backend"].as_str().unwrap_or("memory").to_owned();
        Ok(Some((run, backend)))
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

    async fn build_index(
        &self,
        lease: &WriterLease,
        sink: &mut dyn IndexSink,
        version: &str,
        index_id: &str,
        scope: &[RunBatch],
        catalog: &Catalog,
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
        let expected = scope.iter().map(|b| b.curated.ok_pages).sum();
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
                    if row.status == TextStatus::Ok {
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
        tokio::select! {
            r = sink.finish(docs) => r?,
            () = lease.lost() => lease.check()?,
        }
        progress.snapshot().log(None);
        progress::report(&self.state, version, &progress.snapshot()).await;
        Ok(docs)
    }

    /// Write `{version}/` (titles, places, baselines, pages per title, the
    /// batch list, manifest last) and return the version's date bounds.
    /// The manifest also records `layout`, the splits of each index, when
    /// the engine has splits.
    async fn write_snapshot(
        &self,
        version: &str,
        batches: &[RunBatch],
        catalog: &Catalog,
        layout: &[IndexLayout],
    ) -> anyhow::Result<(NaiveDate, NaiveDate)> {
        let mut baselines: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::new();
        // Every page counted once, by its title: the same pages as the
        // baselines, which sum them by place instead.
        let mut title_pages: BTreeMap<String, u64> = BTreeMap::new();
        let (mut first, mut last) = (u32::MAX, u32::MIN);
        for b in batches {
            let path = &b.curated.counts;
            let bytes = self
                .curated
                .get(path)
                .await?
                .with_context(|| format!("`{path}` is missing"))?;
            let counts: Counts = serde_json::from_slice(&bytes).context(path.clone())?;
            for (lccn, days) in counts {
                let title = catalog
                    .title(&lccn)
                    .with_context(|| format!("title `{lccn}` is missing from the catalog"))?;
                let series = baselines.entry(title.place_id.clone()).or_default();
                let total = title_pages.entry(lccn).or_default();
                for (day, pages) in days {
                    *series.entry(day).or_default() += pages;
                    *total += u64::from(pages);
                    first = first.min(day);
                    last = last.max(day);
                }
            }
        }
        if first > last {
            bail!("the version has no pages");
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

        let mut files = Vec::new();
        for (name, body) in [
            ("titles.json", serde_json::to_vec(&titles)?),
            ("places.json", serde_json::to_vec(&places)?),
            ("baselines.json", serde_json::to_vec(&baselines)?),
            (TITLE_PAGES_FILE, serde_json::to_vec(&title_pages)?),
            (RUN_BATCHES_FILE, serde_json::to_vec(batches)?),
        ] {
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
            "built_from": {
                "batches": batches.iter().map(|b| json!({
                    "batch": b.batch, "version": b.curated.version, "counts": b.curated.counts,
                })).collect::<Vec<_>>(),
            },
        });
        if !layout.is_empty() {
            manifest["indexes"] = serde_json::to_value(layout)?;
        }
        self.put_new(
            &format!("{version}/manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )
        .await?;
        Ok((date_from_day(first), date_from_day(last)))
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

#[cfg(test)]
mod tests {
    use super::*;

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
