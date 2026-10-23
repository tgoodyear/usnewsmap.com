//! The `usnm-ingest` command line below the top level. Each command group keeps its clap arguments
//! and the functions that run them in a module here, as `usnm-boinc` keeps each app's in its own
//! `cli` module; `main.rs` holds only the top-level `Cli`, the list of commands, start-up and the
//! dispatch. What several groups use is in `common`.
pub(crate) mod batches;
pub(crate) mod common;
pub(crate) mod congress;
pub(crate) mod curate;
pub(crate) mod ocr;
pub(crate) mod onprem;
pub(crate) mod prune;
pub(crate) mod release;
pub(crate) mod run;
pub(crate) mod titles;
