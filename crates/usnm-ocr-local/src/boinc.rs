//! Reserve BOINC pages using the same lock and native requested-work ledger
//! as enqueue/seed. [`reserve`] writes the ledger rows of an explicitly
//! allocated cohort and sends no Azure queue message. [`adopt`] is for pages
//! that came off the work queues (`usnm-boinc claim`): their ledger rows
//! exist already, so it only checks them and writes nothing.
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use super::{ledger, sync::Lease, LOCK};
use usnm_state::state::{LockHeld, RunBatch, State};
use usnm_store::ObjectStore;

/// How long claim and export wait for another holder of `ocr-sync` before
/// failing. The hourly `ocr-sync` pass holds it for under a minute (#1133).
const HELD_WAIT_MAX: Duration = Duration::from_secs(120);
/// How often they try again while they wait.
const HELD_WAIT_POLL: Duration = Duration::from_secs(10);

/// Take the `ocr-sync` lease, waiting up to [`HELD_WAIT_MAX`] while another
/// owner holds it. Other errors fail at once.
async fn take_waiting(state: &State, owner: &str) -> anyhow::Result<Lease> {
    take_waiting_for(state, owner, HELD_WAIT_MAX, HELD_WAIT_POLL).await
}

async fn take_waiting_for(
    state: &State,
    owner: &str,
    max: Duration,
    poll: Duration,
) -> anyhow::Result<Lease> {
    let start = tokio::time::Instant::now();
    loop {
        let err = match Lease::take(state, owner).await {
            Ok(lease) => {
                let waited = start.elapsed();
                if !waited.is_zero() {
                    tracing::info!(
                        waited_s = waited.as_secs(),
                        "took the ocr-sync lock after waiting"
                    );
                }
                return Ok(lease);
            }
            Err(e) => e,
        };
        let Some(held) = err.downcast_ref::<LockHeld>() else {
            return Err(err);
        };
        let waited = start.elapsed();
        if waited >= max {
            tracing::warn!(
                holder = %held.holder,
                until = %held.until,
                waited_s = waited.as_secs(),
                "gave up waiting for the ocr-sync lock"
            );
            let waited_s = waited.as_secs();
            return Err(err.context(format!("waited {waited_s} s for the ocr-sync lock")));
        }
        // The holder's lease ends at `until` unless it renews; it usually
        // releases well before. Try again at the poll interval, at `until`
        // if that is sooner, and never past the cap.
        let to_end = (held.until - chrono::Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO);
        let nap = poll
            .min(to_end)
            .min(max - waited)
            .max(Duration::from_millis(50));
        tracing::info!(
            holder = %held.holder,
            until = %held.until,
            waited_s = waited.as_secs(),
            retry_in_ms = nap.as_millis() as u64,
            "the ocr-sync lock is held; waiting"
        );
        tokio::time::sleep(nap).await;
    }
}

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
    let owner = usnm_state::owner_id();
    let lease = take_waiting(state, &owner).await?;
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

pub fn next_attempt(
    previous: &ledger::Queued,
    at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<ledger::Queued> {
    let at = chrono::DateTime::from_timestamp_micros(at.timestamp_micros())
        .ok_or_else(|| anyhow::anyhow!("retry time is not representable in the native ledger"))?;
    let mut next = previous.clone();
    next.attempt = previous
        .attempt
        .checked_add(1)
        .filter(|n| *n <= i32::MAX as u32)
        .ok_or_else(|| anyhow::anyhow!("native attempt overflow"))?;
    anyhow::ensure!(
        at > previous.queued_at,
        "retry must follow previous queued row"
    );
    next.queued_at = at;
    next.retry_message_at = Some(at);
    if next.plan == "hit-results" && next.request_day.is_none() {
        next.request_day = Some(previous.queued_at.date_naive());
    }
    Ok(next)
}

/// Reserve exactly the next attempt after a permanent terminal outcome.
pub async fn retry(
    state: &State,
    curated: &dyn ObjectStore,
    snapshot: &Snapshot,
    previous: &ledger::Queued,
    next: &ledger::Queued,
    run: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        next_attempt(previous, next.queued_at)? == *next,
        "retry changes more than attempt, time and original request-day attribution"
    );
    let owner = usnm_state::owner_id();
    let lease = take_waiting(state, &owner).await?;
    let result = async {
        anyhow::ensure!(
            state.current_version().await?.as_deref() == Some(&snapshot.version),
            "published snapshot changed during retry"
        );
        anyhow::ensure!(
            snapshot.batch_version(&next.batch) == Some(next.batch_version),
            "retry batch version is not published"
        );
        let key = usnm_core::ids::PageKey::from_doc_id(&next.doc_id)?;
        anyhow::ensure!(
            snapshot.titles.contains_key(&key.lccn) && !snapshot.is_japanese(&key.lccn),
            "retry title is unavailable or uses the Japanese OCR pipeline"
        );
        let ledger = ledger::load(curated).await?;
        if ledger.queued.get(&next.doc_id) == Some(next) {
            anyhow::ensure!(
                ledger.outstanding(&next.doc_id) == Some(next),
                "retry reservation already settled; reconcile"
            );
            lease.check()?;
            return Ok(());
        }
        anyhow::ensure!(
            ledger.queued.get(&next.doc_id) == Some(previous),
            "previous attempt is no longer the latest ledger row"
        );
        let outcome = ledger
            .outcomes
            .get(&next.doc_id)
            .ok_or_else(|| anyhow::anyhow!("previous attempt has no settled outcome"))?;
        anyhow::ensure!(
            outcome.outcome == ledger::DEAD_LETTERED
                && outcome.cause.as_deref() == Some("permanent")
                && outcome.at >= previous.queued_at
                && outcome.at < next.queued_at,
            "only a settled permanent failure may be retried"
        );
        lease.check()?;
        ledger::write_queued(curated, std::slice::from_ref(next), next.queued_at, run).await?;
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
    let owner = usnm_state::owner_id();
    let lease = take_waiting(state, &owner).await?;
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
    /// Waits up to two minutes while another owner holds the lock.
    pub async fn take(state: &State) -> anyhow::Result<Self> {
        let owner = usnm_state::owner_id();
        let lease = take_waiting(state, &owner).await?;
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use usnm_state::docs::MemoryDocs;

    fn state() -> State {
        State::new(Arc::new(MemoryDocs::default()))
    }

    #[tokio::test]
    async fn waits_for_the_holder_to_release() {
        let s = state();
        s.lock(LOCK, "ocr-sync-run", chrono::Duration::minutes(15))
            .await
            .unwrap();
        let releaser = {
            let s = s.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                s.unlock(LOCK, "ocr-sync-run").await.unwrap();
            })
        };
        let started = std::time::Instant::now();
        let lease = take_waiting_for(
            &s,
            "export",
            Duration::from_secs(5),
            Duration::from_millis(50),
        )
        .await
        .expect("taken once the holder releases");
        assert!(started.elapsed() >= Duration::from_millis(300));
        lease.check().unwrap();
        releaser.await.unwrap();
        drop(lease);
        s.unlock(LOCK, "export").await.unwrap();
    }

    #[tokio::test]
    async fn gives_up_at_the_cap_with_the_holder_named() {
        let s = state();
        s.lock(LOCK, "ocr-sync-run", chrono::Duration::minutes(15))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let err = take_waiting_for(
            &s,
            "claim",
            Duration::from_millis(200),
            Duration::from_millis(50),
        )
        .await
        .err()
        .expect("still held at the cap");
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(200), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
        let held = err.downcast_ref::<LockHeld>().expect("the held error");
        assert_eq!(held.holder, "ocr-sync-run");
        assert!(format!("{err:#}").contains("is held by `ocr-sync-run`"));
        assert!(err.to_string().starts_with("waited "));
    }

    #[tokio::test]
    async fn an_expired_lease_is_taken_at_once() {
        let s = state();
        s.lock(LOCK, "gone", chrono::Duration::seconds(-1))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let lease = take_waiting(&s, "export").await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(lease);
    }

    #[tokio::test]
    async fn its_own_owner_takes_it_at_once() {
        let s = state();
        s.lock(LOCK, "export", chrono::Duration::minutes(15))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        drop(take_waiting(&s, "export").await.unwrap());
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
