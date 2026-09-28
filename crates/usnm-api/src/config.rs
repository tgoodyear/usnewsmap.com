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
    pub search_timeout: Duration,
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
            refresh_interval: Duration::from_secs(num("USNM_REFRESH_SECS", 600)?.max(1)),
            cache_bytes: num("USNM_CACHE_MB", 256)? * 1024 * 1024,
            max_cells: usnm_core::cube::MAX_CELLS,
            rate_limit,
            trusted_proxy_hops: usize::try_from(num("USNM_TRUSTED_PROXY_HOPS", 1)?)
                .map_err(|e| e.to_string())?,
            backend_concurrency: usize::try_from(num("USNM_BACKEND_CONCURRENCY", 8)?.max(1))
                .map_err(|e| e.to_string())?,
            site_dir: var("USNM_SITE_DIR").map(PathBuf::from),
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
        let c = Config::from_lookup(|k| match k {
            "USNM_RATE_PER_MIN" => Some("0".into()),
            "USNM_REFERENCE_URL" => Some("https://a.blob.core.windows.net/reference".into()),
            _ => None,
        })
        .unwrap();
        assert!(c.rate_limit.is_none());
        assert!(c.reference_url.starts_with("https://"));
        assert!(Config::from_lookup(|k| (k == "USNM_RATE_BURST").then(|| "0".into())).is_err());
        assert!(Config::from_lookup(|k| (k == "USNM_CACHE_MB").then(|| "x".into())).is_err());
    }
}
