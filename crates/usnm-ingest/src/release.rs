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
use usnm_core::text::TextStatus;
use usnm_core::time::{date_from_day, day_number, ym_number};
use usnm_store::ObjectStore;

use crate::catalog::{Catalog, Place, Title};
use crate::curated::{read_part, CuratedRow};
use crate::sink::IndexSink;
use crate::source::hex;
use crate::state::{Curated, IndexRun, RunBatch, RunStatus, State};
use crate::worker::Counts;

pub const WRITER_LOCK: &str = "quickwit-writer";

/// Deltas a version may carry before the next release compacts (08 §8.4.1).
pub const MAX_DELTAS: usize = 8;

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
    pub async fn run_held(
        &self,
        sink: &mut dyn IndexSink,
        lease: &WriterLease,
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
            let published: HashMap<&str, &Curated> = prev
                .batches
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
            let mut all = prev.batches.clone();
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
            batches: version_batches.clone(),
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
        tracing::info!(%version, index = %index_id, full, batches = scope.len(), "building index");

        let outcome = async {
            let docs = self
                .build_index(lease, sink, &index_id, &scope, &catalog)
                .await?;
            let bounds = self
                .write_snapshot(&version, &version_batches, &catalog)
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
        }
        sink.finish(docs).await?;
        Ok(docs)
    }

    /// Write `{version}/` (titles, places, baselines, manifest last) and
    /// return the version's date bounds.
    async fn write_snapshot(
        &self,
        version: &str,
        batches: &[RunBatch],
        catalog: &Catalog,
    ) -> anyhow::Result<(NaiveDate, NaiveDate)> {
        let mut baselines: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::new();
        let mut lccns = BTreeSet::new();
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
                for (day, pages) in days {
                    *series.entry(day).or_default() += pages;
                    first = first.min(day);
                    last = last.max(day);
                }
                lccns.insert(lccn);
            }
        }
        if first > last {
            bail!("the version has no pages");
        }
        // Only titles and places with pages in this version.
        let titles: Vec<&Title> = catalog
            .titles
            .iter()
            .filter(|t| lccns.contains(&t.lccn))
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
        ] {
            files.push(json!({
                "path": name,
                "sha256": hex(&Sha256::digest(&body)),
                "bytes": body.len(),
            }));
            self.put_new(&format!("{version}/{name}"), body).await?;
        }
        let manifest = json!({
            "index_version": version,
            "files": files,
            "built_from": {
                "batches": batches.iter().map(|b| json!({
                    "batch": b.batch, "version": b.curated.version, "counts": b.curated.counts,
                })).collect::<Vec<_>>(),
            },
        });
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
