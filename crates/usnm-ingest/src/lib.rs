//! The ingest pipeline (04 §4.4): LoC batch archives → curated Parquet →
//! sealed search indexes + reference snapshots → a published version.
//!
//! Stages are subcommands of the `usnm-ingest` binary, run as Container Apps
//! Jobs (weekly) or ACI Spot groups (backfill). Cosmos DB holds the work
//! queue and document state; Blob holds everything else.

pub mod activity;
pub mod american_stories;
pub mod archive;
pub mod audit;
pub mod build_info;
pub mod catalog;
pub mod cluster;
pub mod congress_damage;
pub mod decade_order;
pub mod dedup;
pub mod finish;
pub mod heartbeat;
pub mod kafka;
pub mod local_holder;
pub mod merges;
pub mod ocr_ja;
pub mod onprem;
pub mod page_language;
pub mod pictures;
pub mod places;
pub mod progress;
pub mod prune;
pub mod raw;
pub mod release;
pub mod searchlog_copy;
pub mod sink;
pub mod source;
pub mod telemetry;
pub mod text_agreement;
pub mod titles;
pub mod titles_archive;
pub mod worker;

// Pipeline state lives in its own crate so the API can read it without
// the pipeline's dependencies (Parquet, Quickwit, telemetry export).
// `owner_id` (leases and attempt paths) lives beside the leases.
pub use usnm_state::{cosmos, docs, owner_id, state};

// The Congress fetches and the local OCR pipeline are their own crates. The
// modules keep their paths here, for the stages above and for the crates
// that call them (`usnm-boinc`, `usnm-ocr-worker`).
pub use usnm_congress as congress;
pub use usnm_ocr_local as ocr_local;
// The curated page rows (`usnm-overlay` holds the Parquet schemas of a page's text).
pub use usnm_overlay::curated;
