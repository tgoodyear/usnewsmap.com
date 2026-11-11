//! The held-pages lists the batch controller writes to the hand-off container (#1751, #1758):
//! the pages whose work is settled for good, as `<prefix><plan>.<NNNN>.json`. `claim pull`
//! deletes a settled page's queue message when it comes back, and the seeder leaves such
//! pages out of its window and its retries (#1754), so both read them here.

use std::collections::{BTreeSet, HashMap};

use anyhow::{ensure, Context};
use serde::Deserialize;
use usnm_store::ObjectStore;

/// Each plan's list of pages whose held result is settled (#835): archived as held, by the
/// plan or by one that shared its worker (`archived_held`), each with the sha256 of the
/// message its job came from. `<prefix><plan>.json` in the hand-off container, written by
/// that plan's controller before it claims; a claim reads every plan's list.
pub const HELD_PAGES_PREFIX: &str = "scheduler/held-pages/";
/// The most one held-pages list may be: 200,000 entries (the controller's cap) of about
/// 150 bytes, with room to spare.
pub const HELD_PAGES_MAX_BYTES: usize = 64 << 20;
/// The most held-pages lists a claim reads (one per plan).
pub const HELD_PAGES_MAX_LISTS: usize = 1024;

/// Doc id -> the sha256s of the messages whose held results are settled ([`HELD_PAGES_PREFIX`]).
pub type HeldPages = HashMap<String, BTreeSet<String>>;

/// Every plan's held-pages list under [`HELD_PAGES_PREFIX`], merged; `None` when there is none.
pub async fn load_held_pages(handoff: &dyn ObjectStore) -> anyhow::Result<Option<HeldPages>> {
    let mut names: Vec<String> = handoff
        .list(HELD_PAGES_PREFIX.trim_end_matches('/'))
        .await?
        .into_iter()
        .filter(|n| n.starts_with(HELD_PAGES_PREFIX) && n.ends_with(".json"))
        .collect();
    names.sort();
    ensure!(
        names.len() <= HELD_PAGES_MAX_LISTS,
        "{} held-pages lists under {HELD_PAGES_PREFIX}, more than {HELD_PAGES_MAX_LISTS}",
        names.len()
    );
    let mut merged: Option<HeldPages> = None;
    for name in names {
        let Some(bytes) = handoff.get(&name).await? else {
            continue;
        };
        let pages = parse_held_pages(&bytes).with_context(|| format!("held-pages list {name}"))?;
        let all = merged.get_or_insert_with(HeldPages::new);
        for (doc_id, digests) in pages {
            all.entry(doc_id).or_default().extend(digests);
        }
    }
    Ok(merged)
}

/// Read one held-pages list: `{"schema_version": 1, "pages": {"<doc id>": ["<sha256>", ...]}}`.
pub fn parse_held_pages(bytes: &[u8]) -> anyhow::Result<HeldPages> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        schema_version: u32,
        pages: HashMap<String, BTreeSet<String>>,
    }
    ensure!(
        bytes.len() <= HELD_PAGES_MAX_BYTES,
        "a held-pages list is over {HELD_PAGES_MAX_BYTES} bytes"
    );
    let file: File = serde_json::from_slice(bytes).context("invalid held-pages list")?;
    ensure!(
        file.schema_version == 1,
        "unsupported held-pages list version {}",
        file.schema_version
    );
    for (doc_id, digests) in &file.pages {
        ensure!(
            usnm_store::is_safe_segment(doc_id)
                && !digests.is_empty()
                && digests.iter().all(|d| {
                    d.len() == 64
                        && d.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                }),
            "invalid held-pages entry for {doc_id}"
        );
    }
    Ok(file.pages)
}
