//! A full release built on an on-prem host (#283, #917): the Quickwit writer
//! works on the host's own disk (`file://` metastore and index root), the
//! reference store is a local mirror of the lake's `reference`, and pipeline
//! state is a local file. Nothing in Azure changes while it builds. Two steps
//! go with it:
//!
//! - [`seed_state`] writes the state file's batch records from a published
//!   version's batch list, since the state file has none of Cosmos's.
//! - [`publish_staged`] then copies what the staged release wrote into Azure:
//!   each index of `index-storage-pending.json` into the index account (its
//!   published splits, then its `metastore.json` pointed at the account, then
//!   the account's metastore manifest), the version's reference snapshot into
//!   the lake, and the pending pointer last. A rerun skips what is already
//!   there with the same size, so an interrupted copy continues.
//!
//! The copy rides out a flaky uplink: each request is tried again after a
//! failure that may pass (a dropped or hung connection, a 408, 429 or 5xx),
//! a split whose blocks ran out of tries is sent again after a wait, and a
//! split sent again (in this run or a rerun) sends only the blocks that
//! weren't staged already. A refusal (403, a different object of the same
//! name) stops it at once.
//!
//! Promotion stays the operator step of docs/operations/rebuild-and-index-storage.md, "Index storage
//! cutover".
//!
//! A restore (`Staged::restore`, #1061) is the same copy for a version the
//! lake already serves, which is the state after serving moves on-prem
//! (ADR-0027, Phase 4): `current.json` names the release's own version, not
//! its rollback, so the plain publish refuses it. The restore refuses unless
//! the lake serves exactly this release's version and indexes, copies what
//! the account and the lake lack, and writes neither the pending pointer nor
//! `current.json`.

use std::collections::BTreeSet;

use anyhow::{bail, ensure, Context};
use serde_json::Value;

mod publish;
mod seed;

pub use publish::{publish_staged, IndexCopy, Published, Staged};
pub use seed::{
    import_staged_run, mirror, record_current, reserve_names, seed_state, Every, Mirrored, Seeded,
};

/// The staged pointer a release with `--stage-index-storage` writes.
pub const PENDING_POINTER: &str = "index-storage-pending.json";
/// The file-backed metastore's list of indexes, at the metastore's root.
pub const METASTORE_MANIFEST: &str = "manifest.json";
/// The staged run's record in its version's snapshot (`{version}/index_run.json`),
/// for [`import_staged_run`]: the host's state is a local file, so this is how
/// its run reaches Cosmos (the release script's publish step, or the ingest job).
pub const RUN_FILE: &str = "index_run.json";
/// Every index id a pointer names: its `indexes`, and those of the blocks
/// that carry their own (`ja`, `congress`).
pub fn pointer_indexes(pointer: &Value) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut add = |v: &Value, at: &str| -> anyhow::Result<()> {
        let Some(list) = v.get("indexes") else {
            return Ok(());
        };
        for id in list
            .as_array()
            .with_context(|| format!("{at}.indexes isn't a list"))?
        {
            let id = id
                .as_str()
                .with_context(|| format!("{at}.indexes holds a non-string"))?;
            if !usnm_store::is_safe_segment(id) {
                bail!("`{id}` isn't an index id");
            }
            out.push(id.to_owned());
        }
        Ok(())
    };
    add(pointer, "pointer")?;
    for block in ["ja", "congress"] {
        if let Some(b) = pointer.get(block).filter(|b| b.is_object()) {
            add(b, block)?;
        }
    }
    let unique: BTreeSet<&String> = out.iter().collect();
    ensure!(
        unique.len() == out.len(),
        "the pointer names an index twice"
    );
    ensure!(!out.is_empty(), "the pointer names no index");
    Ok(out)
}

/// One split of a file-backed `metastore.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub id: String,
    pub state: String,
    pub docs: u64,
}

/// The splits a file-backed `metastore.json` lists, with their states.
/// Quickwit 0.9 keeps them as a list of `{split_state, split_metadata}`.
pub fn metastore_splits(meta: &Value) -> anyhow::Result<Vec<Split>> {
    let splits = meta
        .get("splits")
        .and_then(Value::as_array)
        .context("metastore.json has no splits list")?;
    splits
        .iter()
        .map(|s| {
            let m = s.get("split_metadata").unwrap_or(s);
            let id = m["split_id"]
                .as_str()
                .context("a split without split_id")?
                .to_owned();
            if !usnm_store::is_safe_segment(&id) {
                bail!("`{id}` isn't a split id");
            }
            Ok(Split {
                state: s["split_state"]
                    .as_str()
                    .with_context(|| format!("split {id} has no split_state"))?
                    .to_owned(),
                docs: m["num_docs"].as_u64().unwrap_or(0),
                id,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
