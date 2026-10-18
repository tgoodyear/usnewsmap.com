//! Reserve BOINC pages using the same lock and native requested-work ledger
//! as enqueue/seed. [`reserve`] writes the ledger rows of an explicitly
//! allocated cohort and sends no Azure queue message. [`adopt`] is for pages
//! that came off the work queues (`usnm-boinc claim`): their ledger rows
//! exist already, so it only checks them and writes nothing.
use std::collections::{HashMap, HashSet};

use super::{ledger, sync::Lease, LOCK};
use usnm_state::state::{RunBatch, State};
use usnm_store::ObjectStore;

/// Immutable published metadata shared by language routing and reservation.
/// Read before staging scans, then check its version again under the lease.
pub struct Snapshot {
    version: String,
    batches: Vec<RunBatch>,
    pub titles: HashMap<String, Vec<String>>,
}
impl Snapshot {
    /// The curation version of `batch` in the published index, if it is there.
    pub fn batch_version(&self, batch: &str) -> Option<u16> {
        self.batches
            .iter()
            .find(|b| b.batch == batch)
            .map(|b| b.curated.version)
    }

    /// Whether the title lists Japanese, which has its own OCR pipeline.
    pub fn is_japanese(&self, lccn: &str) -> bool {
        self.titles
            .get(lccn)
            .is_some_and(|l| l.iter().any(|l| l == "jpn"))
    }

    pub async fn load(state: &State, reference: &dyn ObjectStore) -> anyhow::Result<Self> {
        let version = state
            .current_version()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no published index version"))?;
        anyhow::ensure!(
            usnm_store::is_safe_segment(&version),
            "invalid published version id"
        );
        let bytes = reference
            .get(&format!("{version}/batches.json"))
            .await?
            .ok_or_else(|| anyhow::anyhow!("published batch list missing"))?;
        let batches = serde_json::from_slice(&bytes)?;
        #[derive(serde::Deserialize)]
        struct Title {
            lccn: String,
            #[serde(default)]
            languages: Vec<String>,
        }
        let bytes = reference
            .get(&format!("{version}/titles.json"))
            .await?
            .ok_or_else(|| anyhow::anyhow!("published title list missing"))?;
        let published: Vec<Title> = serde_json::from_slice(&bytes)?;
        let mut titles = HashMap::new();
        for title in published {
            anyhow::ensure!(
                titles.insert(title.lccn, title.languages).is_none(),
                "duplicate published title"
            );
        }
        Ok(Self {
            version,
            batches,
            titles,
        })
    }
}

/// Validate every row before writing a single create-only cohort part. Exact
/// prior reservations are replays, including when publication of that part
/// succeeded but its response (or the local journal update) was lost.
pub async fn reserve(
    state: &State,
    curated: &dyn ObjectStore,
    snapshot: &Snapshot,
    rows: &[ledger::Queued],
    run: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty() && rows.len() <= 100,
        "reserve 1 to 100 pages"
    );
    let owner = crate::owner_id();
    let lease = Lease::take(state, &owner).await?;
    let result = async {
        anyhow::ensure!(
            state.current_version().await?.as_deref() == Some(&snapshot.version),
            "published snapshot changed during export; repeat export"
        );
        let mut seen = HashSet::new();
        for row in rows {
            anyhow::ensure!(seen.insert(&row.doc_id), "duplicate cohort page");
            anyhow::ensure!(
                snapshot
                    .batches
                    .iter()
                    .any(|b| { b.batch == row.batch && b.curated.version == row.batch_version }),
                "selected batch version is not published"
            );
            let key = usnm_core::ids::PageKey::from_doc_id(&row.doc_id)?;
            let langs = snapshot
                .titles
                .get(&key.lccn)
                .ok_or_else(|| anyhow::anyhow!("selected title is not published"))?;
            anyhow::ensure!(
                !langs.iter().any(|l| l == "jpn"),
                "Japanese titles use the existing Japanese OCR pipeline"
            );
        }
        let ledger = ledger::load(curated).await?;
        let mut fresh = Vec::new();
        for row in rows {
            if let Some(old) = ledger.queued.get(&row.doc_id) {
                anyhow::ensure!(
                    old == row,
                    "page has prior OCR work; allocate a fresh pilot cohort"
                );
            } else {
                fresh.push(row.clone());
            }
        }
        if let Some(first) = fresh.first() {
            lease.check()?;
            ledger::write_queued(curated, &fresh, first.queued_at, run).await?;
        }
        Ok(())
    }
    .await;
    drop(lease);
    let unlock = state.unlock(LOCK, &owner).await;
    result?;
    unlock
}

/// Check, under the same lock, that every page's ledger row is still the
/// outstanding one a queue message carried, and write nothing: the row was
/// written when the message was queued (`ocr-enqueue`, the seeder or the
/// requests step), and the page's result will be imported against it. A row
/// that has since been answered, replaced or never existed is refused.
pub async fn adopt(
    state: &State,
    curated: &dyn ObjectStore,
    snapshot: &Snapshot,
    rows: &[ledger::Queued],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !rows.is_empty() && rows.len() <= 100,
        "adopt 1 to 100 pages"
    );
    let owner = crate::owner_id();
    let lease = Lease::take(state, &owner).await?;
    let result = async {
        anyhow::ensure!(
            state.current_version().await?.as_deref() == Some(&snapshot.version),
            "published snapshot changed during export; repeat export"
        );
        let ledger = ledger::load(curated).await?;
        let mut seen = HashSet::new();
        for row in rows {
            anyhow::ensure!(seen.insert(&row.doc_id), "duplicate cohort page");
            anyhow::ensure!(
                snapshot.batch_version(&row.batch) == Some(row.batch_version),
                "selected batch version is not published"
            );
            let key = usnm_core::ids::PageKey::from_doc_id(&row.doc_id)?;
            anyhow::ensure!(
                snapshot.titles.contains_key(&key.lccn),
                "selected title is not published"
            );
            anyhow::ensure!(
                !snapshot.is_japanese(&key.lccn),
                "Japanese titles use the existing Japanese OCR pipeline"
            );
            anyhow::ensure!(
                ledger.outstanding(&row.doc_id) == Some(row),
                "the queued page is no longer outstanding in the ledger"
            );
        }
        lease.check()?;
        Ok(())
    }
    .await;
    drop(lease);
    let unlock = state.unlock(LOCK, &owner).await;
    result?;
    unlock
}

/// The `ocr-sync` lock held across a read-only pass. Every producer writes a
/// page's ledger row before it sends the page's message and holds this lock
/// while it does, so a ledger read under it has the row of every message
/// that is already on a queue, and no row changes until it is released.
pub struct Held {
    lease: Lease,
    owner: String,
}

impl Held {
    pub async fn take(state: &State) -> anyhow::Result<Self> {
        let owner = crate::owner_id();
        let lease = Lease::take(state, &owner).await?;
        Ok(Self { lease, owner })
    }

    /// Fails if the lock could not be renewed.
    pub fn check(&self) -> anyhow::Result<()> {
        self.lease.check()
    }

    pub async fn release(self, state: &State) -> anyhow::Result<()> {
        let Self { lease, owner } = self;
        drop(lease);
        state.unlock(LOCK, &owner).await
    }
}
