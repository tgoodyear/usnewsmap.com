//! Georgia Historic Newspapers (#1193): page images through GHN's IIIF service, for our own
//! OCR.
//!
//! The maintainer's decision (1 November 2026) is to take GHN's IIIF page images and run our
//! own OCR on them, starting with public-domain pages up to 1930 on titles we lack, once the
//! one queue (#1413) and native OCR (#1418, #1415) can carry them. GHN's search JSON, its OCR
//! and anything `robots.txt` disallows are never asked for, and its Turnstile challenge is
//! never passed.
//!
//! - [`catalog`] picks the issues, as a dry run: GHN's titles, less those we already have, the
//!   ones elsewhere in hand (the Wiregrass Farmer, #1419) and issues after 1930. It writes
//!   lists and enqueues nothing.
//! - [`fetch`] takes those issues' manifests and full-size page images through the fetcher
//!   (#1413) under the `ghn` policy (`policies/ghn.toml`): paced, one at a time, within
//!   `robots.txt` and an allow-list of paths. Each page gets its `ghn:` id (ADR-0026) and a
//!   sidecar with its provenance and SHA-256. It enqueues no OCR.
//!
//! Before any run beyond a sample, the issue lists the open steps: a rights filter from DLG's
//! records (only NoC-US is a public-domain claim), a page-level duplicate check where a title
//! is shared with Chronicling America, and a comparison of our OCR with GHN's.

use std::path::Path;

use anyhow::Context;
use chrono::NaiveDate;

use crate::fetcher::{Engine, EngineOptions, FileBudgets, Policies, GHN};

pub mod catalog;
pub mod fetch;
pub mod iiif;
#[cfg(test)]
mod tests;

pub use iiif::Hosts;

pub const SITE: &str = "https://gahistoricnewspapers.galileo.usg.edu";
pub const IMAGES: &str = "https://iiif-ha.galib.uga.edu/iiif/2/newspapers/";

/// Titles taken some other way, and where.
pub const ELSEWHERE: &[(&str, &str)] = &[(
    "sn90052158",
    "the Wiregrass Farmer comes from LoC's API text (#1419)",
)];

/// The last day of the latest year whose publications are public domain in the US now:
/// 1930's entered it on 1 January 2026. Going later is the maintainer's decision.
pub fn latest() -> NaiveDate {
    NaiveDate::from_ymd_opt(1930, 12, 31).expect("a date")
}

/// The rights statement that is a public-domain claim: No Copyright - United States.
pub fn is_noc_us(statement: &str) -> bool {
    let s = statement.trim().trim_end_matches('/');
    ["http://", "https://"]
        .iter()
        .any(|p| s.strip_prefix(p) == Some("rightsstatements.org/vocab/NoC-US/1.0"))
}

/// An engine with the `ghn` policy alone, its budget in `pacer`, so nothing else can be asked,
/// and the lock that keeps it the only one on that pacer while it's held. The pacer spaces
/// requests across processes, but the one-transfer limit and byte cap are each engine's own,
/// so a second run (`ghn-fetch` beside `ghn-catalog --fetch-metadata`, or a fetch into
/// another output) is refused rather than let it overlap transfers.
pub(crate) fn engine(
    policies: &Policies,
    pacer: &Path,
    who: &str,
) -> anyhow::Result<(Engine, std::fs::File)> {
    if let Some(dir) = pacer.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Not the pacer file itself: its own lock is taken and dropped around each request.
    let mut run = pacer.as_os_str().to_owned();
    run.push(".run");
    let held = crate::bulk::hold_lock(Path::new(&run))
        .with_context(|| format!("{who}: one GHN run at a time per pacer"))?;
    let ghn = policies
        .iter()
        .find(|p| p.name == GHN)
        .context("no ghn policy")?
        .clone();
    let budgets = FileBudgets::new(None).with_file(GHN, pacer.to_owned(), None);
    let engine = Engine::new(
        Policies::new(vec![ghn])?,
        Box::new(budgets),
        EngineOptions {
            who: who.to_owned(),
            rate_file: None,
        },
    )?;
    Ok((engine, held))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    usnm_core::digest::hex(&sha2::Sha256::digest(bytes))
}

/// `body` at `path`, whole or not at all.
pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// A TSV field: no tabs or line breaks.
pub(crate) fn field(s: &str) -> String {
    s.replace(['\t', '\n', '\r'], " ")
}
