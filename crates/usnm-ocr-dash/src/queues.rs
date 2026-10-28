//! Certificate-authenticated pgmq metrics, without reading or taking messages.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use usnm_ocr_contract::status::QueueStats;
use usnm_store::pgmq::{Location, PgmqQueue};
use usnm_store::queue::{Queue, BULK, DEADLETTER, PRIORITY};

use crate::view::{AzureView, QUEUES_FRESH};

pub const SOURCE: &str = "PostgreSQL / pgmq (dash_reader)";
pub const NOTE: &str = "Certificate-authenticated, read-only pgmq metrics. Counts include leased \
messages. No messages are taken, changed or peeked; dead-letter message bodies are not read.";
pub(crate) const POLL_TIMEOUT: Duration = Duration::from_secs(15);

#[async_trait::async_trait]
pub(crate) trait MetricsSource: Send + Sync {
    async fn count(&self, name: &str) -> anyhow::Result<u64>;
    async fn discard(&self);
}

#[async_trait::async_trait]
impl MetricsSource for PgmqQueue {
    async fn count(&self, name: &str) -> anyhow::Result<u64> {
        self.approximate_count(name).await
    }
    async fn discard(&self) {
        self.discard_connection().await;
    }
}

#[derive(Default)]
pub struct Queues {
    pub(crate) source: Option<Arc<dyn MetricsSource>>,
    last: Option<QueueStats>,
    error: Option<String>,
}

impl Queues {
    pub fn new(location: Option<&str>) -> anyhow::Result<Self> {
        let Some(location) = location else {
            return Ok(Self::default());
        };
        let loc = Location::parse(location)?;
        ensure!(
            loc.user == "dash_reader",
            "dashboard queues require dash_reader"
        );
        Ok(Self {
            source: Some(Arc::new(
                PgmqQueue::new(location)?.with_statement_timeout(Duration::from_secs(5))?,
            )),
            ..Self::default()
        })
    }

    pub(crate) fn apply(
        &mut self,
        result: anyhow::Result<BTreeMap<String, u64>>,
        now: DateTime<Utc>,
    ) {
        match result {
            Ok(counts) => {
                self.last = Some(QueueStats {
                    checked_at: now,
                    counts,
                    dead_letters: Vec::new(),
                    error: None,
                });
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }

    pub(crate) fn apply_to(&self, view: &mut AzureView, now: DateTime<Utc>) {
        if self.source.is_none() {
            return;
        }
        view.queues = self.last.clone();
        if let Some(q) = &mut view.queues {
            q.error.clone_from(&self.error);
        }
        view.queues_from = Some(SOURCE.to_owned());
        view.queues_stale = self.error.is_some()
            || !self.last.as_ref().is_some_and(|q| {
                (0..=QUEUES_FRESH.num_seconds()).contains(&(now - q.checked_at).num_seconds())
            });
        view.queues_error.clone_from(&self.error);
        view.queues_metrics_only = true;
        view.queues_note = NOTE;
    }
}

pub(crate) async fn read_counts(
    source: &dyn MetricsSource,
    budget: Duration,
) -> anyhow::Result<BTreeMap<String, u64>> {
    let result = tokio::time::timeout(budget, async {
        let mut counts = BTreeMap::new();
        for name in [PRIORITY, BULK, DEADLETTER] {
            counts.insert(
                name.to_owned(),
                source
                    .count(name)
                    .await
                    .with_context(|| format!("{name} metrics"))?,
            );
        }
        Ok(counts)
    })
    .await
    .context("pgmq metrics poll timed out")
    .and_then(|result| result);
    if result.is_err() {
        source.discard().await;
    }
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug)]
    pub(crate) struct Metrics {
        pub(crate) fail: bool,
    }

    #[async_trait::async_trait]
    impl MetricsSource for Metrics {
        async fn count(&self, name: &str) -> anyhow::Result<u64> {
            if self.fail && name == BULK {
                anyhow::bail!("database unavailable");
            }
            Ok(match name {
                PRIORITY => 7,
                BULK => 0,
                DEADLETTER => 3,
                _ => anyhow::bail!("unexpected queue"),
            })
        }
        async fn discard(&self) {}
    }

    struct Blocked {
        discarded: AtomicBool,
    }

    #[async_trait::async_trait]
    impl MetricsSource for Blocked {
        async fn count(&self, _: &str) -> anyhow::Result<u64> {
            std::future::pending().await
        }
        async fn discard(&self) {
            self.discarded.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn timed_out_poll_discards_its_transport_before_returning() {
        let source = Blocked {
            discarded: AtomicBool::new(false),
        };
        assert!(read_counts(&source, Duration::from_millis(5))
            .await
            .is_err());
        assert!(source.discarded.load(Ordering::SeqCst));
    }

    fn view() -> AzureView {
        AzureView {
            queues: None,
            queues_from: None,
            queues_stale: false,
            queues_error: None,
            queues_metrics_only: false,
            queues_note: crate::view::QUEUES_NOTE,
            imports: "",
            workbook_url: None,
        }
    }

    #[tokio::test]
    async fn failed_partial_poll_keeps_the_last_complete_observation_but_not_its_health() {
        let source = Arc::new(Metrics { fail: false });
        let mut reader = Queues {
            source: Some(source.clone()),
            ..Queues::default()
        };
        let at = Utc::now();
        reader.apply(read_counts(source.as_ref(), POLL_TIMEOUT).await, at);
        let later = at + chrono::Duration::seconds(1);
        reader.apply(
            read_counts(&Metrics { fail: true }, POLL_TIMEOUT).await,
            later,
        );
        let mut v = view();
        reader.apply_to(&mut v, later);
        let q = v.queues.unwrap();
        assert_eq!(
            q.counts,
            [
                (PRIORITY.into(), 7),
                (BULK.into(), 0),
                (DEADLETTER.into(), 3)
            ]
            .into()
        );
        assert_eq!(q.checked_at, at);
        assert!(q.error.unwrap().contains("database unavailable"));
        assert!(v.queues_stale);
        assert!(v.queues_error.unwrap().contains("ocr-bulk metrics"));
    }

    #[tokio::test]
    async fn unavailable_before_first_success_is_not_zero_and_old_success_is_stale() {
        let mut reader = Queues {
            source: Some(Arc::new(Metrics { fail: false })),
            ..Queues::default()
        };
        let at = Utc::now();
        reader.apply(read_counts(&Metrics { fail: true }, POLL_TIMEOUT).await, at);
        let mut v = view();
        reader.apply_to(&mut v, at);
        assert!(v.queues.is_none());
        assert!(v.queues_error.is_some());
        assert!(v.queues_stale);
        reader.apply(
            read_counts(&Metrics { fail: false }, POLL_TIMEOUT).await,
            at,
        );
        reader.apply_to(&mut v, at + QUEUES_FRESH + chrono::Duration::seconds(1));
        assert!(v.queues.is_some());
        assert!(v.queues_error.is_none());
        assert!(v.queues_stale);
    }

    #[test]
    fn mutating_roles_are_rejected_before_credentials_are_opened() {
        let url = "postgres://localhost/usnm_queue?user=boinc_claim&sslcert=unused&sslkey=unused&sslrootcert=unused";
        assert!(Queues::new(Some(url))
            .err()
            .unwrap()
            .to_string()
            .contains("dash_reader"));
    }
}
