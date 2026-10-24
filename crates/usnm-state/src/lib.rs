//! Pipeline state (05 §5.9.1): the document store behind the ingest
//! pipeline's work queue, locks and index runs, and the typed items in it.
//!
//! The ingest pipeline writes this state. The API reads it, read-only, for
//! the public status page, which is why it lives apart from the pipeline's
//! heavier dependencies.

pub mod cosmos;
pub mod docs;
pub mod ocr_local;
pub mod state;
pub mod summary;

/// The Cosmos DB database that holds every container.
pub const DATABASE: &str = "usnm";

/// A process-unique id for leases and attempt paths: the lock owners of
/// [`state::State`].
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
