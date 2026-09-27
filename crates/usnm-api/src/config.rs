//! Configuration from environment variables (12-factor).

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// In-memory engine over local JSONL indexes (development and tests).
    Memory,
    /// Quickwit at the given base URL (production: the localhost sidecar).
    Quickwit(String),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub backend: BackendKind,
    /// Directory holding `current.json`, reference snapshots and (for the memory backend) `indexes/`.
    pub data_dir: PathBuf,
    pub allowed_origins: Vec<String>,
    pub search_timeout: Duration,
    pub refresh_interval: Duration,
    pub cache_bytes: u64,
    /// Cube cell budget; above it buckets are coarsened (ADR-0003).
    pub max_cells: usize,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
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
        let secs = |k: &str, default: u64| -> Result<Duration, String> {
            var(k)
                .map_or(Ok(default), |v| {
                    v.parse()
                        .map_err(|_| format!("{k} must be a number of seconds"))
                })
                .map(Duration::from_secs)
        };
        Ok(Self {
            bind: var("USNM_BIND").unwrap_or_else(|| "0.0.0.0:8080".to_owned()),
            backend,
            data_dir: PathBuf::from(
                var("USNM_DATA_DIR").unwrap_or_else(|| "fixtures/data".to_owned()),
            ),
            allowed_origins: var("USNM_ALLOWED_ORIGINS")
                .unwrap_or_else(|| "https://usnewsmap.com".to_owned())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            search_timeout: secs("USNM_SEARCH_TIMEOUT_SECS", 10)?,
            refresh_interval: secs("USNM_REFRESH_SECS", 600)?,
            cache_bytes: var("USNM_CACHE_MB")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(256)
                * 1024
                * 1024,
            max_cells: usnm_core::cube::MAX_CELLS,
        })
    }
}
