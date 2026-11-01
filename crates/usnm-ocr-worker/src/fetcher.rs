//! One way to fetch outside content (#1413).
//!
//! A [`Job`] says what's wanted: a name, a size and SHA-256, a destination,
//! and sources in preference order. The bytes are the same from any of them,
//! so a mirror and the original are one code path. Asking a host anything goes
//! through its [`Policy`], and a URL no policy covers is refused.
//!
//! A download goes to `<name>.part`, its size and SHA-256 are checked, and only
//! then does it get its final name. [`Engine`] does one download; [`run::run`]
//! works through a [`JobStore`] with a set number of workers.
//!
//! Two things are behind interfaces, so they can move off local files without
//! this code changing: request budgets ([`Budget`], from [`Budgets`]) and job
//! and per-file state ([`JobStore`]). Paths, hosts and limits all come from
//! the caller's options and the policies.

mod budget;
mod engine;
mod job;
mod policy;
pub mod run;
#[cfg(test)]
mod tests;

pub use budget::{Budget, Budgets, FileBudgets};
pub use engine::{Engine, EngineOptions, Fail, Got};
pub use job::{Attempt, FileJobs, HeldBack, Job, JobStore, Status};
pub use policy::{Policies, Policy, PushBack, ServingProbe, LOC};

/// A URL for logs and errors: no query, which on a CDN redirect holds a signature.
pub(crate) fn plain(url: &reqwest::Url) -> String {
    let mut u = url.clone();
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}
