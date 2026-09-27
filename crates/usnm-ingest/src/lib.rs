//! The ingest pipeline (04 §4.4): LoC batch archives → curated Parquet →
//! sealed search indexes + reference snapshots → a published version.
//!
//! Stages are subcommands of the `usnm-ingest` binary, run as Container Apps
//! Jobs (weekly) or ACI Spot groups (backfill). Cosmos DB holds the work
//! queue and document state; Blob holds everything else.

pub mod archive;
pub mod catalog;
pub mod cosmos;
pub mod curated;
pub mod docs;
pub mod release;
pub mod sink;
pub mod source;
pub mod state;
pub mod titles;
pub mod worker;

/// A process-unique id for leases and attempt paths.
pub fn owner_id() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| usnm_store::is_safe_segment(h))
        .unwrap_or_else(|| "local".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{host}-{}-{nanos:08x}", std::process::id())
}
