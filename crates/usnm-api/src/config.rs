//! Configuration from environment variables (12-factor).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// In-memory engine over local JSONL indexes (development and tests).
    Memory,
    /// Quickwit at the given base URL (production: the localhost sidecar).
    Quickwit(String),
}

/// Per-client token bucket (06 §6.2, 08 §8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub per_minute: NonZeroU32,
    pub burst: NonZeroU32,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub backend: BackendKind,
    /// Local directory with the memory backend's `indexes/`; also the default
    /// reference location.
    pub data_dir: PathBuf,
    /// Where `current.json` and reference snapshots live: a Blob container URL
    /// (read with the managed identity) or a local directory.
    pub reference_url: String,
    /// Persistent response cache (`cache/` container), if any.
    pub response_cache_url: Option<String>,
    /// Responses that took at least this long to compute are persisted.
    pub persist_after: Duration,
    pub allowed_origins: Vec<String>,
    /// How long a visitor's request waits for a search before the API
    /// answers `202 Accepted` and the search carries on without it.
    pub search_timeout: Duration,
    /// The limit on one search computation (each backend call and the whole
    /// response), whether or not anyone still waits for it. Past it the
    /// search is cancelled and fails with a timeout, which is not cached.
    pub compute_cap: Duration,
    /// Search computations (`/v1/aggregate`, `/v1/hits`) allowed to run at
    /// once, including those still running after their visitor got a `202`.
    /// A new one waits for a slot up to the visitor's wait, then gets a `503`.
    pub compute_concurrency: usize,
    /// A computation nobody has waited on for this long is cancelled (the
    /// visitor changed the search or left). Visitors ask again within
    /// `Retry-After` (2 s) of each `202`.
    pub abandon_after: Duration,
    /// Warm-up (`crate::prewarm`): limit on each query. Well above
    /// `search_timeout`, because a cold search can take longer than a visitor
    /// is allowed to wait.
    pub prewarm_query_timeout: Duration,
    /// Warm-up: limit on the whole run; queries left when it runs out are skipped.
    pub prewarm_budget: Duration,
    /// Warm-up: after the examples, this many of the most frequent searches
    /// in the search log (0 turns it off).
    pub prewarm_top_searches: usize,
    /// Warm-up: how many of the search log's most recent days are counted.
    pub prewarm_log_days: u64,
    /// Warm-up: the first pause before retrying a query the backend failed
    /// (it doubles, up to 10 s). The searcher can still be starting when a
    /// replica warms up.
    pub prewarm_retry_first: Duration,
    /// After a start, `/readyz` reports ready once the warm-up finishes or
    /// this much time passes, whichever is first.
    pub ready_cap: Duration,
    pub refresh_interval: Duration,
    pub cache_bytes: u64,
    /// Cube cell budget; above it buckets are coarsened (ADR-0003).
    pub max_cells: usize,
    /// `None` disables rate limiting.
    pub rate_limit: Option<RateLimit>,
    /// Proxies in front of the API that append to `X-Forwarded-For`
    /// (1 = Container Apps ingress; 2 = a proxy such as Front Door + ingress).
    /// 0 ignores the header and uses the peer address.
    pub trusted_proxy_hops: usize,
    /// Requests allowed to query the search backend at once.
    pub backend_concurrency: usize,
    /// The built web app to serve alongside the API (`web/dist`); none
    /// serves the API alone.
    pub site_dir: Option<PathBuf>,
    /// The site's canonical hostname: requests for `www.` + this host are
    /// redirected to `https://` + this host.
    pub site_host: String,
    /// Cosmos DB endpoint of the pipeline state, read-only for `/v1/status`
    /// (the same variable the ingest jobs use).
    pub cosmos_endpoint: Option<String>,
    /// The ingest CLI's local state file, for `/v1/status` in development.
    pub state_file: Option<PathBuf>,
    /// How often `/v1/status` is recomputed at most.
    pub status_refresh: Duration,
    /// Where the anonymous search log goes (`searches/` container), if
    /// anywhere (06 §6.8).
    pub search_log_url: Option<String>,
    /// How often the search log appends its batch.
    pub search_log_flush: Duration,
    /// End-to-end tests only, and only with the memory backend: an aggregate
    /// search whose query contains this term waits this long before it runs,
    /// to stand in for a cold search on the real corpus.
    pub fixture_slow: Option<(String, Duration)>,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from any key lookup (tests pass `|_| None` for the defaults).
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let num = |k: &str, default: u64| -> Result<u64, String> {
            var(k).map_or(Ok(default), |v| {
                v.parse().map_err(|_| format!("{k} must be a whole number"))
            })
        };
        let backend = match var("USNM_BACKEND").as_deref().unwrap_or("memory") {
            "memory" => BackendKind::Memory,
            "quickwit" => BackendKind::Quickwit(
                var("USNM_QUICKWIT_URL").unwrap_or_else(|| "http://127.0.0.1:7280".to_owned()),
            ),
            other => {
                return Err(format!(
                    "USNM_BACKEND must be memory or quickwit, not `{other}`"
                ))
            }
        };
        let data_dir = var("USNM_DATA_DIR").unwrap_or_else(|| "fixtures/data".to_owned());
        let per_minute = num("USNM_RATE_PER_MIN", 120)?;
        let rate_limit = match NonZeroU32::new(u32::try_from(per_minute).unwrap_or(u32::MAX)) {
            None => None,
            Some(per_minute) => Some(RateLimit {
                per_minute,
                burst: NonZeroU32::new(
                    u32::try_from(num("USNM_RATE_BURST", 40)?).unwrap_or(u32::MAX),
                )
                .ok_or("USNM_RATE_BURST must be at least 1")?,
            }),
        };
        let fixture_slow = match var("USNM_FIXTURE_SLOW_TERM") {
            None => None,
            Some(_) if backend != BackendKind::Memory => {
                return Err("USNM_FIXTURE_SLOW_TERM works with the memory backend only".into())
            }
            Some(term) => Some((
                term.to_lowercase(),
                Duration::from_millis(num("USNM_FIXTURE_SLOW_MS", 5000)?),
            )),
        };
        Ok(Self {
            bind: var("USNM_BIND").unwrap_or_else(|| "0.0.0.0:8080".to_owned()),
            backend,
            reference_url: var("USNM_REFERENCE_URL").unwrap_or_else(|| data_dir.clone()),
            data_dir: PathBuf::from(data_dir),
            response_cache_url: var("USNM_RESPONSE_CACHE_URL"),
            persist_after: Duration::from_millis(num("USNM_PERSIST_AFTER_MS", 500)?),
            allowed_origins: var("USNM_ALLOWED_ORIGINS")
                .unwrap_or_else(|| "https://usnewsmap.com".to_owned())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            search_timeout: Duration::from_secs(num("USNM_SEARCH_TIMEOUT_SECS", 10)?),
            compute_cap: Duration::from_secs(num("USNM_COMPUTE_CAP_SECS", 120)?.max(1)),
            compute_concurrency: usize::try_from(num("USNM_COMPUTE_CONCURRENCY", 4)?.max(1))
                .map_err(|e| e.to_string())?,
            abandon_after: Duration::from_secs(num("USNM_ABANDON_AFTER_SECS", 15)?.max(1)),
            prewarm_query_timeout: Duration::from_secs(num("USNM_PREWARM_QUERY_SECS", 60)?),
            prewarm_budget: Duration::from_secs(num("USNM_PREWARM_BUDGET_SECS", 300)?),
            prewarm_top_searches: usize::try_from(num("USNM_PREWARM_TOP_SEARCHES", 20)?)
                .map_err(|e| e.to_string())?,
            prewarm_log_days: num("USNM_PREWARM_LOG_DAYS", 28)?,
            prewarm_retry_first: Duration::from_secs(1),
            ready_cap: Duration::from_secs(num("USNM_READY_CAP_SECS", 120)?),
            refresh_interval: Duration::from_secs(num("USNM_REFRESH_SECS", 600)?.max(1)),
            cache_bytes: num("USNM_CACHE_MB", 256)? * 1024 * 1024,
            max_cells: usnm_core::cube::MAX_CELLS,
            rate_limit,
            trusted_proxy_hops: usize::try_from(num("USNM_TRUSTED_PROXY_HOPS", 1)?)
                .map_err(|e| e.to_string())?,
            backend_concurrency: usize::try_from(num("USNM_BACKEND_CONCURRENCY", 8)?.max(1))
                .map_err(|e| e.to_string())?,
            site_dir: var("USNM_SITE_DIR").map(PathBuf::from),
            site_host: var("USNM_SITE_HOST")
                .unwrap_or_else(|| "usnewsmap.com".to_owned())
                .to_ascii_lowercase(),
            cosmos_endpoint: var("USNM_COSMOS_ENDPOINT"),
            state_file: var("USNM_STATE_FILE").map(PathBuf::from),
            status_refresh: Duration::from_secs(num("USNM_STATUS_REFRESH_SECS", 60)?.max(1)),
            search_log_url: var("USNM_SEARCH_LOG_URL"),
            search_log_flush: Duration::from_secs(num("USNM_SEARCH_LOG_FLUSH_SECS", 300)?.max(1)),
            fixture_slow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = Config::from_lookup(|_| None).unwrap();
        assert_eq!(c.reference_url, "fixtures/data");
        assert_eq!(c.rate_limit.unwrap().per_minute.get(), 120);
        assert_eq!(c.persist_after, Duration::from_millis(500));
        assert_eq!(c.prewarm_query_timeout, Duration::from_secs(60));
        assert_eq!(c.prewarm_budget, Duration::from_secs(300));
        assert_eq!(c.ready_cap, Duration::from_secs(120));
        assert_eq!(c.site_host, "usnewsmap.com");
        assert!(c.search_log_url.is_none());
        assert_eq!(c.search_log_flush, Duration::from_secs(300));
        assert_eq!(c.compute_cap, Duration::from_secs(120));
        assert_eq!(c.compute_concurrency, 4);
        assert_eq!(c.abandon_after, Duration::from_secs(15));
        assert_eq!(c.prewarm_top_searches, 20);
        assert_eq!(c.prewarm_log_days, 28);
        assert!(c.fixture_slow.is_none());
        let c = Config::from_lookup(|k| match k {
            "USNM_RATE_PER_MIN" => Some("0".into()),
            "USNM_REFERENCE_URL" => Some("https://a.blob.core.windows.net/reference".into()),
            _ => None,
        })
        .unwrap();
        assert!(c.rate_limit.is_none());
        assert_eq!(c.status_refresh, Duration::from_secs(60));
        assert!(c.cosmos_endpoint.is_none());
        assert!(c.reference_url.starts_with("https://"));
        assert!(Config::from_lookup(|k| (k == "USNM_RATE_BURST").then(|| "0".into())).is_err());
        assert!(Config::from_lookup(|k| (k == "USNM_CACHE_MB").then(|| "x".into())).is_err());
        let slow = Config::from_lookup(|k| (k == "USNM_FIXTURE_SLOW_TERM").then(|| "Slow".into()))
            .unwrap();
        assert_eq!(
            slow.fixture_slow,
            Some(("slow".to_owned(), Duration::from_secs(5)))
        );
        // Never with the real search engine.
        assert!(Config::from_lookup(|k| match k {
            "USNM_FIXTURE_SLOW_TERM" => Some("slow".into()),
            "USNM_BACKEND" => Some("quickwit".into()),
            _ => None,
        })
        .is_err());
    }
}
