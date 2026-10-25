//! The writer lock (08 §8.4.1): taken before anything changes the indexes
//! Quickwit holds, and renewed in the background until it is released.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::bail;
use chrono::Duration;

use crate::state::{State, WRITER_LOCK};

/// How long the writer lock lasts without renewal, and how often it's renewed.
pub(super) const LEASE_TTL: Duration = Duration::minutes(30);
const LEASE_RENEW: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// A held writer lock, renewed in the background until it is released.
/// If a renewal fails, the release stops at its next checkpoint and never
/// publishes (08 §8.4.1).
pub struct WriterLease {
    lost: Arc<AtomicBool>,
    renewer: tokio::task::JoinHandle<()>,
}

impl WriterLease {
    /// Fails once a renewal has failed: the holder may no longer hold the
    /// lock, so it must stop before its next write.
    pub fn check(&self) -> anyhow::Result<()> {
        if self.lost.load(Ordering::SeqCst) {
            bail!("the writer lock could not be renewed; stopping without publishing");
        }
        Ok(())
    }

    /// Resolves once a renewal has failed. Long waits (the merges, up to
    /// 4 hours by default) race against it, so a writer that may no longer hold the
    /// lock stops within a second instead of at its next checkpoint, long
    /// before the lock expires and another writer could take it.
    /// Set once a renewal failed: for work beside the release, such as the
    /// background decade sender, to stop on as `check` does.
    pub fn lost_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.lost)
    }

    pub(crate) async fn lost(&self) {
        while !self.lost.load(Ordering::SeqCst) {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    /// A lease with no lock behind it, lost once `lost` is set.
    #[cfg(test)]
    pub(crate) fn for_test(lost: Arc<AtomicBool>) -> Self {
        Self {
            lost,
            renewer: tokio::spawn(async {}),
        }
    }
}

impl Drop for WriterLease {
    fn drop(&mut self) {
        self.renewer.abort();
    }
}

/// Take the writer lock as `owner` and keep renewing it until
/// [`unlock_writer`]. Whatever changes the indexes Quickwit holds takes it
/// first: a release, and `prune` (`crate::prune`), so neither runs beside
/// the other.
pub async fn lock_writer(state: &State, owner: &str) -> anyhow::Result<WriterLease> {
    state.lock(WRITER_LOCK, owner, LEASE_TTL).await?;
    let lost = Arc::new(AtomicBool::new(false));
    let (state, owner, flag) = (state.clone(), owner.to_owned(), lost.clone());
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

/// Stop renewing `lease` and release the writer lock `owner` holds.
pub async fn unlock_writer(state: &State, owner: &str, lease: WriterLease) -> anyhow::Result<()> {
    drop(lease);
    state.unlock(WRITER_LOCK, owner).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_lost_lease_ends_a_wait() {
        let lost = Arc::new(AtomicBool::new(false));
        let lease = WriterLease::for_test(lost.clone());
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
