//! Adapter for the cloud BOINC server (cloud-boinc), dev server only (not used by the running
//! bridge). `cloud pull` reads the server's assimilation API and writes the spool layout the
//! stock `batch_collect_assimilator` leaves, so `collect` and the hand-off code can read it
//! unchanged. See docs/boinc/server/bridge-steps.md, "Pull from the cloud BOINC server".
pub mod cli;
pub mod pull;
pub mod spool_out;
