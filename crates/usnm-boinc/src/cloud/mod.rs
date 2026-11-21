//! Adapter for the cloud BOINC server (cloud-boinc), dev server only (not used by the running
//! bridge). `cloud pull` reads the server's assimilation API and writes the spool layout the
//! stock `batch_collect_assimilator` leaves, so `collect` and the hand-off code can read it
//! unchanged. See docs/boinc/server/bridge-steps.md, "Pull from the cloud BOINC server".
//! `cloud pull-results` writes the same spool from a results bucket the server exported to.
pub mod cli;
pub mod pull;
pub mod pull_results;
pub mod spool_out;
pub mod submit;
#[cfg(any(test, feature = "cloud-validatord"))]
pub mod validatord;
