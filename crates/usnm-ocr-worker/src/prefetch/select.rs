//! Deterministic stratified sampling and manifest selection order.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use usnm_ocr_contract::prefetch::{self, ManifestRow};

use super::{priority, set_of, Options, Order, Set};

/// A stable 64-bit order for a doc_id (the first 8 bytes of its sha256).
fn rank(doc_id: &str) -> u64 {
    let d = Sha256::digest(doc_id.as_bytes());
    u64::from_be_bytes(d[..8].try_into().expect("8 bytes"))
}

/// About `n` rows stratified by (language, decade): one from each stratum
/// (the largest strata first, when `n` is smaller than their number), the
/// rest shared in proportion to the strata's sizes (largest remainders),
/// never more than a stratum holds. Within a stratum the rows with the
/// lowest [`rank`] are taken, so the same manifest gives the same sample.
/// Returned in issue order.
pub fn sample(rows: &[ManifestRow], n: usize) -> Vec<ManifestRow> {
    let mut strata: BTreeMap<(String, i32), Vec<&ManifestRow>> = BTreeMap::new();
    for r in rows {
        strata
            .entry((r.lang_or_und().to_owned(), r.decade()))
            .or_default()
            .push(r);
    }
    let mut keys: Vec<(String, i32)> = strata.keys().cloned().collect();
    // Largest first; ties in key order.
    keys.sort_by(|a, b| strata[b].len().cmp(&strata[a].len()).then(a.cmp(b)));
    let mut take: BTreeMap<(String, i32), usize> = BTreeMap::new();
    if n <= keys.len() {
        for k in keys.iter().take(n) {
            take.insert(k.clone(), 1);
        }
    } else {
        for k in &keys {
            take.insert(k.clone(), 1);
        }
        let total = rows.len() as f64;
        let rest = (n - keys.len()) as f64;
        let mut shares: Vec<(f64, &(String, i32))> = Vec::new();
        let mut given = 0usize;
        for k in &keys {
            let want = rest * strata[k].len() as f64 / total;
            let whole = (want.floor() as usize).min(strata[k].len() - 1);
            *take.get_mut(k).expect("set above") += whole;
            given += whole;
            shares.push((want - want.floor(), k));
        }
        shares.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)));
        let mut left = (n - keys.len()).saturating_sub(given);
        // Round after round in remainder order, until the sample is full or
        // every stratum is.
        while left > 0 {
            let mut gave = false;
            for (_, k) in &shares {
                if left == 0 {
                    break;
                }
                let t = take.get_mut(*k).expect("set above");
                if *t < strata[*k].len() {
                    *t += 1;
                    left -= 1;
                    gave = true;
                }
            }
            if !gave {
                break;
            }
        }
    }
    let mut out: Vec<ManifestRow> = Vec::new();
    for (k, rows) in &mut strata {
        rows.sort_by_key(|r| (rank(&r.doc_id), r.doc_id.clone()));
        let t = take.get(k).copied().unwrap_or(0);
        out.extend(rows.iter().take(t).map(|r| (*r).clone()));
    }
    prefetch::sort(&mut out);
    out
}

/// The rows a run works through: the manifest, sampled, ordered, then
/// limited. Keep pages come first. With [`Order::Priority`] they go by
/// issue, each issue in its best keep page's group, so an issue's pages stay
/// together and its item JSON is asked for once (even when they came from
/// different batches); buffer pages stay in issue order either way.
pub fn select(rows: Vec<ManifestRow>, opts: &Options) -> Vec<ManifestRow> {
    let awardee = |r: &ManifestRow| r.batch.split('_').next().unwrap_or_default().to_owned();
    let rows: Vec<ManifestRow> = rows
        .into_iter()
        .filter(|r| opts.sets.has(set_of(r)))
        .filter(|r| opts.awardees.is_empty() || opts.awardees.contains(&awardee(r)))
        .collect();
    let mut rows = match opts.sample {
        Some(n) => sample(&rows, n),
        None => rows,
    };
    type Issue = (String, chrono::NaiveDate, u16);
    let mut best: std::collections::HashMap<Issue, u8> = std::collections::HashMap::new();
    if opts.order == Order::Priority {
        for r in rows.iter().filter(|r| set_of(r) == Set::Keep) {
            let e = best
                .entry((r.lccn.clone(), r.date, r.edition))
                .or_insert(u8::MAX);
            *e = (*e).min(priority(r));
        }
    }
    rows.sort_by_cached_key(|r| {
        let set = set_of(r);
        let issue = (r.lccn.clone(), r.date, r.edition);
        match (set, opts.order) {
            (Set::Keep, Order::Priority) => (set, best[&issue], Some((issue, r.seq))),
            // Otherwise the manifest's order, which the stable sort keeps.
            _ => (set, 0, None),
        }
    });
    if let Some(l) = opts.limit {
        rows.truncate(l);
    }
    rows
}
