//! The Quickwit searcher sidecar's cache metrics, reported as the API's own
//! (#125, 08 §8.1.2, 08 §8.4).
//!
//! The sidecar listens on localhost only, and its storage is reachable only
//! inside the network, so its Prometheus endpoint can't be read from
//! outside. Every `USNM_SEARCHER_METRICS_SECS` (60 s by default; 0 turns it
//! off) the API reads it and reports, for the split footer, fast field,
//! partial request and predicate caches (`cache`: `split_footer`,
//! `fast_field`, `partial_request`, `predicate`):
//!
//! - `api.searcher_cache_bytes` and `api.searcher_cache_items`: what the
//!   cache holds now (gauges). Recorded when they change, and at least every
//!   [`HEARTBEAT`] so a quiet replica still reports them.
//! - `api.searcher_cache_hits`, `api.searcher_cache_misses` and
//!   `api.searcher_cache_evictions`: what happened since the last scrape
//!   (counters), recorded only when not zero. The first scrape reports
//!   everything since the searcher started, which in a fresh replica is the
//!   warm-up. A restarted searcher starts every cache from 0, so if any
//!   counter of any cache went down (or a cache disappeared), every cache's
//!   new values are what happened since.
//!
//! Whether the split footer cache holds every footer is read from two
//! things together: the release's footer total (`footer_bytes` on its
//! `index layout` lines) against `split_footer_cache_capacity`, and whether
//! footer misses and evictions keep coming after the warm-up. An eviction on
//! its own proves nothing: Quickwit 0.9.1 also counts replacing an entry as
//! one, and after a publish the cache drops the old version's footers. The
//! first footer eviction a searcher run shows is logged once, without a
//! cause.
//!
//! A scrape that fails (the sidecar starting, restarting or gone) records
//! nothing. The first failure in a row is logged once, and the recovery once.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use opentelemetry::metrics::{Counter, Gauge, Meter};
use opentelemetry::KeyValue;
use usnm_search::cache_metrics::{Cache, CacheReport};

/// The longest a gauge goes unrecorded while the scrapes succeed.
pub const HEARTBEAT: Duration = Duration::from_secs(15 * 60);

/// The instruments the scrapes are recorded with.
pub struct Instruments {
    bytes: Gauge<u64>,
    items: Gauge<u64>,
    hits: Counter<u64>,
    misses: Counter<u64>,
    evictions: Counter<u64>,
}

impl Instruments {
    pub fn new(meter: &Meter) -> Self {
        Self {
            bytes: meter
                .u64_gauge("api.searcher_cache_bytes")
                .with_unit("By")
                .with_description("Bytes in a searcher cache, by cache")
                .build(),
            items: meter
                .u64_gauge("api.searcher_cache_items")
                .with_description("Items in a searcher cache (split footers: splits), by cache")
                .build(),
            hits: meter
                .u64_counter("api.searcher_cache_hits")
                .with_description("Searcher cache hits, by cache")
                .build(),
            misses: meter
                .u64_counter("api.searcher_cache_misses")
                .with_description("Searcher cache misses, by cache")
                .build(),
            evictions: meter
                .u64_counter("api.searcher_cache_evictions")
                .with_description("Items evicted from a searcher cache, by cache")
                .build(),
        }
    }
}

/// What the scrapes have seen so far; [`Scraper::observe`] takes each one.
pub struct Scraper {
    instruments: Instruments,
    /// The last successful scrape, for the counters' increments.
    last: Option<CacheReport>,
    /// Per cache: the gauges' last recorded values, and when.
    recorded: BTreeMap<Cache, ((u64, u64), Instant)>,
    /// Scrapes failed in a row.
    failures: u32,
    /// Whether this searcher run's first footer eviction has been logged.
    footer_evictions_logged: bool,
}

impl Scraper {
    pub fn new(instruments: Instruments) -> Self {
        Self {
            instruments,
            last: None,
            recorded: BTreeMap::new(),
            failures: 0,
            footer_evictions_logged: false,
        }
    }

    /// Scrapes failed in a row (0 after a success).
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Record one scrape taken at `now`.
    pub fn observe(&mut self, scraped: Result<CacheReport, String>, now: Instant) {
        let report = match scraped {
            Ok(r) => r,
            Err(error) => {
                self.failures += 1;
                if self.failures == 1 {
                    tracing::warn!(%error, "searcher cache metrics unavailable; retrying quietly");
                }
                return;
            }
        };
        if self.failures > 0 {
            tracing::info!(
                failed_scrapes = self.failures,
                "searcher cache metrics available again"
            );
            self.failures = 0;
        }
        // Counters that went down: the searcher restarted, and all of its
        // caches count from 0 again.
        if self.last.as_ref().is_some_and(|l| restarted(l, &report)) {
            tracing::info!("the searcher restarted; its cache counters start again from 0");
            self.last = None;
            self.footer_evictions_logged = false;
        }
        for (&cache, now_stats) in &report {
            let attrs = [KeyValue::new("cache", cache.label())];
            let gauges = (now_stats.bytes, now_stats.items);
            let due = self
                .recorded
                .get(&cache)
                .is_none_or(|&(last, at)| last != gauges || now.duration_since(at) >= HEARTBEAT);
            if due {
                self.instruments.bytes.record(gauges.0, &attrs);
                self.instruments.items.record(gauges.1, &attrs);
                self.recorded.insert(cache, (gauges, now));
            }
            let before = self
                .last
                .as_ref()
                .and_then(|l| l.get(&cache))
                .copied()
                .unwrap_or_default();
            let (hits, misses, evictions) = (
                now_stats.hits.saturating_sub(before.hits),
                now_stats.misses.saturating_sub(before.misses),
                now_stats.evictions.saturating_sub(before.evictions),
            );
            for (counter, n) in [
                (&self.instruments.hits, hits),
                (&self.instruments.misses, misses),
                (&self.instruments.evictions, evictions),
            ] {
                if n > 0 {
                    counter.add(n, &attrs);
                }
            }
        }
        if let Some(footers) = report.get(&Cache::SplitFooter) {
            if footers.evictions > 0 && !self.footer_evictions_logged {
                self.footer_evictions_logged = true;
                tracing::info!(
                    bytes = footers.bytes,
                    items = footers.items,
                    evictions = footers.evictions,
                    "split footer cache evictions seen"
                );
            }
        }
        self.last = Some(report);
    }
}

/// Whether the searcher restarted between two scrapes: a counter of any
/// cache went down, or a cache it had reported is gone.
fn restarted(before: &CacheReport, now: &CacheReport) -> bool {
    before.iter().any(|(cache, b)| match now.get(cache) {
        None => true,
        Some(n) => n.hits < b.hits || n.misses < b.misses || n.evictions < b.evictions,
    })
}

/// Scrape with `fetch` every `interval` (the first after one interval, once
/// the sidecar has had time to start), for as long as the process runs.
pub fn spawn<F, Fut>(interval: Duration, meter: &Meter, mut fetch: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<CacheReport, String>> + Send,
{
    let mut scraper = Scraper::new(Instruments::new(meter));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            let scraped = fetch().await;
            scraper.observe(scraped, Instant::now());
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_search::cache_metrics::CacheStats;

    fn stats(hits: u64, misses: u64, evictions: u64) -> CacheStats {
        CacheStats {
            bytes: 0,
            items: 0,
            hits,
            misses,
            evictions,
        }
    }

    fn report(entries: &[(Cache, CacheStats)]) -> CacheReport {
        entries.iter().copied().collect()
    }

    #[test]
    fn a_restart_is_seen_in_any_cache() {
        let before = report(&[
            (Cache::SplitFooter, stats(5, 3, 0)),
            (Cache::FastField, stats(9, 9, 1)),
        ]);
        assert!(!restarted(
            &before,
            &report(&[
                (Cache::SplitFooter, stats(5, 4, 0)),
                (Cache::FastField, stats(9, 9, 1)),
                (Cache::Predicate, stats(1, 1, 0)),
            ])
        ));
        // Only the fast field cache went down.
        assert!(restarted(
            &before,
            &report(&[
                (Cache::SplitFooter, stats(6, 4, 0)),
                (Cache::FastField, stats(1, 2, 0)),
            ])
        ));
        // A cache it had reported is gone.
        assert!(restarted(
            &before,
            &report(&[(Cache::SplitFooter, stats(6, 4, 0))])
        ));
    }
}
