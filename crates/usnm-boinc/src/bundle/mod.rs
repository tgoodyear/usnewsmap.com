//! Several `usnm_ocr` pages in one BOINC workunit (#1059, docs/boinc-ocr-bundles.md).
//!
//! Only the BOINC unit is shared. Each page keeps its own claim, export, ledger reservation,
//! dispatch record, hand-off spool, trust and gold check, and publication:
//!
//! - [`submit`] (server) stages one unit for 2 to 32 reserved pages and marks each page's
//!   server record with the bundle's id;
//! - [`work`] (worker container) reads the pages one by one, checkpointing after each;
//! - [`spool`] (server, `spool-out`) turns the bundle's assimilated result back into each
//!   page's standard spool files, or puts a page that failed back to `reserved`, to be
//!   submitted alone.
pub mod spool;
pub mod submit;
pub mod work;

/// The physical name of a bundle input in the download tree.
pub(crate) fn physical(bundle: &str, logical: &str) -> String {
    format!("{bundle}_{}", logical.replace('.', "_"))
}
