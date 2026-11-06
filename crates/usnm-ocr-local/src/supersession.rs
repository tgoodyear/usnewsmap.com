//! Pinned supersession of future bulk recognition by a verified, selected
//! third-party reading (#811).
//!
//! A list names exact pages, each bound to the batch copy it replaces, the
//! scan and baseline of its decision history ([`alternate_decisions`]), the
//! selected candidate and the `select` record that chose it. Storing a list
//! changes nothing. The seeder consults one only when
//! `USNM_OCR_SEED_SUPERSESSION` pins its SHA-256, and then leaves out a listed
//! page's **new** bulk work only while that exact selection still verifies.
//! Retries, `ocr-enqueue` and queued, leased or in-flight work are never
//! touched. A missing, invalid, retained-baseline, rejected or revoked
//! selection schedules the page normally.
//!
//! The scheduler has no scan hash of its own: that the listed scan is the
//! copy the batch publishes rests on the staged registration attestation.

use std::collections::{BTreeSet, HashMap};

use anyhow::{ensure, Context};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::message::is_base_batch_name;
use usnm_store::ObjectStore;

use crate::alternate::{digest, put_exact};
use crate::alternate_decisions::{self, hash_ok, Action, Subject};
use crate::ROOT;

pub const FORMAT: &str = "usnm-ocr-alternate-supersession/1";
/// Entries a list may hold; each is verified at most once per seeding run.
pub const MAX_ENTRIES: usize = 256;
pub const MAX_BYTES: usize = 256 << 10;
const MAX_NOTE: usize = 2048;

/// One page whose future bulk recognition a verified selection replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub doc_id: String,
    /// The base batch and curated version of the copy the seeder would queue.
    pub batch: String,
    pub batch_version: u16,
    pub source_image_sha256: String,
    pub baseline_sha256: String,
    pub candidate: String,
    /// The id of the exact `select` decision record.
    pub selection: String,
}

impl Entry {
    pub fn subject(&self) -> Subject {
        Subject {
            doc_id: self.doc_id.clone(),
            source_image_sha256: self.source_image_sha256.clone(),
            baseline_sha256: self.baseline_sha256.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    pub format: String,
    pub reason: String,
    pub evidence: String,
    pub entries: Vec<Entry>,
}

/// Where a list is stored, by the SHA-256 of its exact bytes.
pub fn path(sha256: &str) -> String {
    format!("{ROOT}/alternate-supersession/{sha256}.json")
}

/// Check a list's shape. Errors name page ids, never other list content.
pub fn parse(bytes: &[u8]) -> anyhow::Result<List> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_BYTES,
        "supersession list: size bound"
    );
    let list: List = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("supersession list refused: invalid JSON"))?;
    ensure!(
        list.format == FORMAT,
        "supersession list: format is not {FORMAT}"
    );
    ensure!(
        !list.reason.trim().is_empty()
            && list.reason.len() <= MAX_NOTE
            && !list.evidence.trim().is_empty()
            && list.evidence.len() <= MAX_NOTE,
        "supersession list requires bounded reason and evidence"
    );
    ensure!(
        !list.entries.is_empty() && list.entries.len() <= MAX_ENTRIES,
        "supersession list holds 1 to {MAX_ENTRIES} entries"
    );
    let mut pages = BTreeSet::new();
    for e in &list.entries {
        ensure!(
            PageKey::from_doc_id(&e.doc_id).is_ok_and(|k| k.doc_id() == e.doc_id),
            "supersession list: noncanonical page identity"
        );
        ensure!(
            is_base_batch_name(&e.batch),
            "supersession entry {}: not a base batch name",
            e.doc_id
        );
        ensure!(
            [
                &e.source_image_sha256,
                &e.baseline_sha256,
                &e.candidate,
                &e.selection
            ]
            .iter()
            .all(|h| hash_ok(h)),
            "supersession entry {}: invalid SHA-256",
            e.doc_id
        );
        ensure!(
            pages.insert(e.doc_id.as_str()),
            "supersession entry {}: listed twice",
            e.doc_id
        );
    }
    Ok(list)
}

/// Read the list pinned by `sha256`, refusing bytes that don't hash to it.
pub async fn load(curated: &dyn ObjectStore, sha256: &str) -> anyhow::Result<List> {
    ensure!(hash_ok(sha256), "supersession list: invalid SHA-256");
    let path = path(sha256);
    let mut chunks = curated
        .get_stream(&path)
        .await?
        .with_context(|| format!("no supersession list at {path}"))?;
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.next().await {
        bytes.extend_from_slice(&chunk?);
        ensure!(
            bytes.len() <= MAX_BYTES,
            "supersession list {path}: over {MAX_BYTES} bytes"
        );
    }
    ensure!(
        digest(&bytes) == sha256,
        "supersession list {path}: checksum mismatch"
    );
    parse(&bytes)
}

/// Succeeds only while the entry's exact `select` record is the current
/// selection and its candidate, rights and comparison revalidate.
pub async fn verify(curated: &dyn ObjectStore, entry: &Entry) -> anyhow::Result<()> {
    let selection = alternate_decisions::selection(curated, &entry.subject())
        .await?
        .context("no current selection: baseline retained, revoked or never selected")?;
    ensure!(
        selection.id == entry.selection
            && selection.selected.as_ref() == Some(&entry.candidate)
            && matches!(
                &selection.decision.action,
                Action::Select { candidate, .. } if *candidate == entry.candidate
            ),
        "the current selection is not the listed decision"
    );
    Ok(())
}

/// `usnm-ingest ocr-alternate-supersession`: check a list and every entry's
/// selection now, then store it create-only under its SHA-256. This alone
/// changes no scheduling; pinning the returned hash does.
pub async fn publish(curated: &dyn ObjectStore, bytes: &[u8]) -> anyhow::Result<String> {
    let list = parse(bytes)?;
    for e in &list.entries {
        verify(curated, e)
            .await
            .with_context(|| format!("supersession entry {} does not verify", e.doc_id))?;
    }
    let sha256 = digest(bytes);
    put_exact(curated, &path(&sha256), bytes, "application/json").await?;
    Ok(sha256)
}

/// What a pinned list says about a page the seeder would queue as new work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Not listed.
    Unlisted,
    /// Listed and verified this run: leave it out.
    Superseded,
    /// Listed, but the selection didn't verify: queue it normally.
    Unverified,
    /// Listed for another batch copy or version: queue it normally.
    OtherCopy,
}

/// A pinned list for one seeding run, verifying each entry at most once.
pub struct Pinned<'a> {
    curated: &'a dyn ObjectStore,
    pub sha256: String,
    entries: HashMap<String, Entry>,
    verified: HashMap<String, bool>,
}

impl<'a> Pinned<'a> {
    pub async fn load(curated: &'a dyn ObjectStore, sha256: &str) -> anyhow::Result<Self> {
        let list = load(curated, sha256).await?;
        Ok(Self {
            curated,
            sha256: sha256.to_owned(),
            entries: list
                .entries
                .into_iter()
                .map(|e| (e.doc_id.clone(), e))
                .collect(),
            verified: HashMap::new(),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub async fn check(&mut self, doc_id: &str, batch: &str, batch_version: u16) -> Check {
        let Some(entry) = self.entries.get(doc_id) else {
            return Check::Unlisted;
        };
        if entry.batch != batch || entry.batch_version != batch_version {
            return Check::OtherCopy;
        }
        let ok = match self.verified.get(doc_id) {
            Some(&ok) => ok,
            None => {
                let ok = match verify(self.curated, entry).await {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(
                            doc_id,
                            error = %format!("{e:#}"),
                            "listed alternate selection does not verify; scheduling the page normally"
                        );
                        false
                    }
                };
                self.verified.insert(doc_id.to_owned(), ok);
                ok
            }
        };
        if ok {
            Check::Superseded
        } else {
            Check::Unverified
        }
    }
}
