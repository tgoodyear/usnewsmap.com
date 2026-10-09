//! Pages that ship in more than one batch archive (04 §4.7). LoC sometimes
//! puts the same issue in two batches (`wa_duwamish_ver01` and
//! `wa_lacamas_ver01` both carry pages of `sn87093109` from September 1875),
//! and every copy would otherwise become its own document and be counted in
//! the baselines twice.
//!
//! A release plans which copy of each such page to keep before it writes
//! anything:
//!
//! 1. Each batch's `counts.json` (pages per title and day) names the
//!    title-days that more than one batch has pages for. These files are small,
//!    so this costs about what the reference snapshot already reads.
//! 2. Only the batches with such a title-day have their page keys read
//!    (without the text), to find the pages they share.
//! 3. Of a page's copies, the kept one is a copy with text (`ok`) before one
//!    without, then the batch whose name sorts first. The order is total, so
//!    every release keeps the same copy of a page, full or delta.
//!
//! A copy that loses is not indexed if its batch is being indexed now, and
//! is hidden at search time if an index the version keeps already holds it
//! (a delta can't change a published index). Either way the page counts once
//! in the snapshot. An earlier index holds a losing copy if the previous
//! version kept that copy or hid it; versions released before this existed
//! indexed every copy. A copy without text of LoC's is a document too when
//! the version has American Stories' text (04 §4.9): it may have been
//! indexed with that text alone, so it is hidden like the others. One that
//! wasn't is hidden for nothing, which costs a clause in queries but
//! changes no count.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use usnm_core::ids::PageKey;
use usnm_core::text::TextStatus;
use usnm_core::time::day_number;
use usnm_store::ObjectStore;

use crate::curated::read_part;
use crate::state::{RunBatch, State};
use crate::worker::Counts;

/// A copy of a page that searches must not see: the document with this id
/// from this batch. Written to the snapshot's `duplicates.json`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hidden {
    pub doc_id: String,
    pub batch: String,
}

/// Which copy of each duplicated page a version keeps.
#[derive(Debug, Default)]
pub struct Plan {
    /// Pages with more than one copy: the batch whose copy is kept, and
    /// whether that copy has LoC's text.
    keep: HashMap<PageKey, (String, bool)>,
    /// Copies beyond the first, per (lccn, day), to take off the counts.
    excess: HashMap<(String, u32), u32>,
    /// Copies with text that won't be indexed, per batch.
    skipped_docs: HashMap<String, u64>,
    /// Losing copies already in an index the version keeps, sorted.
    pub hidden: Vec<Hidden>,
    /// Copies beyond the first of every page: what summing the batches'
    /// counts overstates.
    pub duplicate_pages: u64,
    /// Duplicate pages per (kept batch, other batch), for the report.
    pub pairs: BTreeMap<(String, String), u64>,
    /// Title-days that more than one batch has pages for.
    pub shared_days: u64,
}

impl Plan {
    /// Whether `batch`'s copy of `key` is the one the version keeps.
    pub fn keeps(&self, key: &PageKey, batch: &str) -> bool {
        self.keep.get(key).is_none_or(|(b, _)| b == batch)
    }

    /// Whether the copy of `key` the version keeps has LoC's text, when the
    /// page has more than one copy (`None` when it has one).
    pub fn kept_has_text(&self, key: &PageKey) -> Option<bool> {
        self.keep.get(key).map(|(_, ok)| *ok)
    }

    /// Pages the summed counts overstate: (lccn, day, pages).
    pub fn excess(&self) -> impl Iterator<Item = (&str, u32, u32)> {
        self.excess.iter().map(|((l, d), n)| (l.as_str(), *d, *n))
    }

    /// Pages with text in `batch` that the release won't index.
    pub fn skipped_docs(&self, batch: &str) -> u64 {
        self.skipped_docs.get(batch).copied().unwrap_or(0)
    }
}

/// A batch's `counts.json`.
pub async fn load_counts(curated: &dyn ObjectStore, b: &RunBatch) -> anyhow::Result<Counts> {
    let path = &b.curated.counts;
    let bytes = curated
        .get(path)
        .await?
        .with_context(|| format!("`{path}` is missing"))?;
    serde_json::from_slice(&bytes).context(path.clone())
}

/// Measure the duplicated pages across every curated batch, as a full
/// release would find them: a JSON report of the title-days and pages more
/// than one batch has, and which batches share them.
pub async fn report(state: &State, curated: &dyn ObjectStore) -> anyhow::Result<serde_json::Value> {
    let batches: Vec<RunBatch> = state
        .batches(&[])
        .await?
        .into_iter()
        .filter_map(|(b, _)| {
            b.curated.map(|c| RunBatch {
                batch: b.batch,
                curated: c,
            })
        })
        .collect();
    let pages: u64 = batches.iter().map(|b| b.curated.pages).sum();
    let p = plan(
        curated,
        &batches,
        &BTreeSet::new(),
        &Some(BTreeSet::new()),
        false,
    )
    .await?;
    Ok(serde_json::json!({
        "batches": batches.len(),
        "pages": pages,
        "distinct_pages": pages - p.duplicate_pages,
        "duplicate_pages": p.duplicate_pages,
        "shared_title_days": p.shared_days,
        "pairs": p.pairs.iter().map(|((kept, other), n)| serde_json::json!({
            "kept": kept, "other": other, "pages": n,
        })).collect::<Vec<_>>(),
    }))
}

/// What the previous version hid, if it recorded it: `None` for a version
/// released before duplicates were handled, whose indexes hold every copy.
pub type PreviouslyHidden = Option<BTreeSet<Hidden>>;

/// Plan the copies to keep among `batches` (every batch of the version).
/// `indexed` names the batches whose pages are already in an index the
/// version keeps (the previous version's batches); the others are about to
/// be indexed. `previously_hidden` is the previous version's hidden list.
/// `american_stories`: the version indexes pages with only American
/// Stories' text, so a published copy without LoC's text may be a document.
pub async fn plan(
    curated: &dyn ObjectStore,
    batches: &[RunBatch],
    indexed: &BTreeSet<&str>,
    previously_hidden: &PreviouslyHidden,
    american_stories: bool,
) -> anyhow::Result<Plan> {
    // Title-days by the first batch with pages on them, and the ones another
    // batch has too. Titles and batches are numbered to keep this small: a
    // full corpus has a few million title-days.
    let mut titles: HashMap<String, u32> = HashMap::new();
    let mut first: HashMap<(u32, u32), u32> = HashMap::new();
    let mut shared: HashMap<(u32, u32), BTreeSet<u32>> = HashMap::new();
    for (i, b) in batches.iter().enumerate() {
        let i = i as u32;
        for (lccn, days) in load_counts(curated, b).await? {
            let next = titles.len() as u32;
            let t = *titles.entry(lccn).or_insert(next);
            for day in days.into_keys() {
                let owner = *first.entry((t, day)).or_insert(i);
                if owner != i {
                    shared.entry((t, day)).or_default().extend([owner, i]);
                }
            }
        }
    }
    drop(first);
    let mut out = Plan {
        shared_days: shared.len() as u64,
        ..Plan::default()
    };
    if shared.is_empty() {
        return Ok(out);
    }
    let shared_ids: BTreeSet<u32> = shared.keys().map(|(t, _)| *t).collect();
    let shared_titles: HashMap<&str, u32> = titles
        .iter()
        .filter(|(_, t)| shared_ids.contains(t))
        .map(|(l, t)| (l.as_str(), *t))
        .collect();
    let involved: BTreeSet<u32> = shared.values().flatten().copied().collect();

    // Every copy of the pages on shared title-days: (batch, has text).
    let mut copies: BTreeMap<PageKey, Vec<(u32, bool)>> = BTreeMap::new();
    for i in involved {
        let b = &batches[i as usize];
        for path in &b.curated.parts {
            let bytes = curated
                .get(path)
                .await?
                .with_context(|| format!("curated part `{path}` is missing"))?;
            read_part(bytes.into(), false, |row| {
                let Some(&t) = shared_titles.get(row.key.lccn.as_str()) else {
                    return Ok(());
                };
                if shared
                    .get(&(t, day_number(row.key.date)))
                    .is_some_and(|s| s.contains(&i))
                {
                    let c = copies.entry(row.key).or_default();
                    // One copy per batch, however many times its archive has it.
                    if !c.iter().any(|(b, _)| *b == i) {
                        c.push((i, row.status == TextStatus::Ok));
                    }
                }
                Ok(())
            })
            .with_context(|| path.clone())?;
        }
    }

    let name = |i: u32| batches[i as usize].batch.as_str();
    let rank = |&(b, ok): &(u32, bool)| (!ok, name(b));
    for (key, c) in copies {
        if c.len() < 2 {
            continue;
        }
        let &(kept, kept_ok) = c
            .iter()
            .min_by_key(|c| rank(c))
            .expect("two or more copies");
        // The copy the previous version kept, of those it had.
        let kept_before = c
            .iter()
            .filter(|(b, _)| indexed.contains(name(*b)))
            .min_by_key(|c| rank(c))
            .map(|(b, _)| *b);
        for &(b, ok) in c.iter().filter(|(b, _)| *b != kept) {
            out.duplicate_pages += 1;
            *out.excess
                .entry((key.lccn.clone(), day_number(key.date)))
                .or_default() += 1;
            *out.pairs
                .entry((name(kept).to_owned(), name(b).to_owned()))
                .or_default() += 1;
            if indexed.contains(name(b)) {
                // Only copies with text are documents: LoC's, or American
                // Stories' when the version has it.
                if !ok && !american_stories {
                    continue;
                }
                let copy = Hidden {
                    doc_id: key.doc_id(),
                    batch: name(b).to_owned(),
                };
                // In an index only if it was kept, or already hidden.
                let in_index = match previously_hidden {
                    None => true,
                    Some(before) => kept_before == Some(b) || before.contains(&copy),
                };
                if in_index {
                    out.hidden.push(copy);
                }
            } else if ok {
                // Counted in the batch's `ok_pages`. A copy without LoC's
                // text isn't, so the release doesn't expect it.
                *out.skipped_docs.entry(name(b).to_owned()).or_default() += 1;
            }
        }
        out.keep.insert(key, (name(kept).to_owned(), kept_ok));
    }
    out.hidden.sort();
    Ok(out)
}
