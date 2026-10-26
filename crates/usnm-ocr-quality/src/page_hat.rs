//! The page-level three-cornered hat, version 2 (#962): solved on the
//! passage-matched disagreement of [`agreement`] instead of the whole-page
//! `cer_sym`.
//!
//! The whole-page `cer_sym` between two readers counts reading order and
//! text one reader lacks as edits. LoC's text is in ALTO block order,
//! American Stories' in article order and Tesseract's in its own
//! segmentation order, so on real pages most of the whole-page distance is
//! order and coverage, and the hat charged it to the readers as misreading.
//! Version 2 takes, for every pair of readers, `local_cer_sym`: edits over
//! the characters the local alignment placed, whatever the order. One kind
//! of `d` for every pair keeps `d_ij = e_i + e_j` about misreading only.
//!
//! A pair falls back to the whole-page `cer_sym` ([`Basis::Page`]) when the
//! local alignment can't measure it: nothing placed, or neither reader has
//! [`MIN_MATCHED`] of its characters placed in the other. That is garbage
//! on one side or different text, where order is not what separates the
//! two, and the local rate over the few chunks that did place would read
//! as agreement.
//!
//! What placed is coverage, not error, and stays out of `d`: each pair
//! keeps both `matched` shares, and each reader gets the mean share of its
//! characters placed in the other readers' texts.
//!
//! Version 1 is the unit's own hat ([`crate::report::analyse`]), which the
//! volunteer result still carries; this one is computed on the server from
//! the same texts.
use crate::{agreement, hat};
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::quality::round6;

/// The hat's version: 1 is the unit's whole-page hat, 2 this one.
pub const VERSION: u32 = 2;

/// A pair is measured locally when at least one of the two readers has
/// this share of its characters placed in the other.
pub const MIN_MATCHED: f64 = 0.5;

/// Which disagreement a pair's `d` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// `local_cer_sym`: edits over the placed characters.
    Local,
    /// The whole-page `cer_sym`, where the local alignment can't measure
    /// the pair.
    Page,
}

/// One pair's disagreement, with what it was chosen from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PairD {
    pub a: String,
    pub b: String,
    pub basis: Basis,
    /// The disagreement the hat is solved on.
    pub d: f64,
    pub cer_sym: f64,
    pub local_cer_sym: Option<f64>,
    /// Shares of `a`'s and `b`'s characters placed in the other's text.
    pub matched_a: Option<f64>,
    pub matched_b: Option<f64>,
}

/// One reader's estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReaderHat {
    pub name: String,
    /// Clipped at 0; none when the hat is underdetermined.
    pub est_cer: Option<f64>,
    pub est_cer_raw: Option<f64>,
    /// Mean share of this reader's characters placed in the other readers'
    /// texts, over the pairs with this reader that have one.
    pub matched: Option<f64>,
}

/// The hat over one page's readers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageHat {
    pub v: u32,
    pub determined: bool,
    pub n_readers: usize,
    pub pairs: Vec<PairD>,
    pub residual: Option<f64>,
    pub readers: Vec<ReaderHat>,
    /// `1 - min est_cer`, in 0..1; none when underdetermined.
    pub ceiling_est: Option<f64>,
}

/// The pair's `d`: local where the alignment measured it, else whole-page.
pub fn pair_d(a: &str, b: &str, name_a: &str, name_b: &str) -> PairD {
    let g = agreement::compare(a, b);
    let placed_enough = g
        .matched_a
        .into_iter()
        .chain(g.matched_b)
        .any(|m| m >= MIN_MATCHED);
    let (basis, d) = match g.local_cer_sym {
        Some(l) if placed_enough => (Basis::Local, l),
        _ => (Basis::Page, g.pair.cer_sym),
    };
    PairD {
        a: name_a.to_owned(),
        b: name_b.to_owned(),
        basis,
        d,
        cer_sym: g.pair.cer_sym,
        local_cer_sym: g.local_cer_sym,
        matched_a: g.matched_a,
        matched_b: g.matched_b,
    }
}

/// Solves the hat for `readers` as `(name, text)`. A pair where both texts
/// are empty after normalisation is left out, as the unit's hat leaves it.
pub fn solve(readers: &[(&str, &str)]) -> PageHat {
    let n = readers.len();
    let mut pairs = Vec::new();
    let mut eqs = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            let p = pair_d(readers[i].1, readers[j].1, readers[i].0, readers[j].0);
            if p.matched_a.is_none() && p.matched_b.is_none() {
                continue;
            }
            eqs.push((i, j, p.d));
            pairs.push(p);
        }
    }
    let h = hat::solve(n, &eqs);
    let mut matched: Vec<Vec<f64>> = vec![Vec::new(); n];
    for (&(i, j, _), p) in eqs.iter().zip(&pairs) {
        matched[i].extend(p.matched_a);
        matched[j].extend(p.matched_b);
    }
    let out: Vec<ReaderHat> = readers
        .iter()
        .enumerate()
        .map(|(i, (name, _))| ReaderHat {
            name: (*name).to_owned(),
            est_cer: h.determined.then(|| round6(h.estimates[i])),
            est_cer_raw: h.determined.then(|| round6(h.raw[i])),
            matched: (!matched[i].is_empty())
                .then(|| round6(matched[i].iter().sum::<f64>() / matched[i].len() as f64)),
        })
        .collect();
    let ceiling_est = h
        .determined
        .then(|| h.estimates.iter().copied().fold(f64::INFINITY, f64::min))
        .filter(|m| m.is_finite())
        .map(|m| round6((1.0 - m).clamp(0.0, 1.0)));
    PageHat {
        v: VERSION,
        determined: h.determined,
        n_readers: n,
        pairs,
        residual: h.determined.then(|| round6(h.residual)),
        readers: out,
        ceiling_est,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// `n` random words of 2 to 9 letters.
    fn words(n: usize, r: &mut Lcg) -> Vec<String> {
        (0..n)
            .map(|_| {
                let len = 2 + r.next() % 8;
                (0..len)
                    .map(|_| (b'a' + (r.next() % 26) as u8) as char)
                    .collect()
            })
            .collect()
    }

    /// Every `every`-th letter from `phase` on replaced by `q`.
    fn misread(text: &str, every: usize, phase: usize) -> String {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                if c != ' ' && i % every == phase {
                    'q'
                } else {
                    c
                }
            })
            .collect()
    }

    fn v1(readers: &[(&str, &str)]) -> hat::Hat {
        let mut eqs = Vec::new();
        for i in 0..readers.len() {
            for j in i + 1..readers.len() {
                eqs.push((i, j, align::pair(readers[i].1, readers[j].1).cer_sym));
            }
        }
        hat::solve(readers.len(), &eqs)
    }

    fn join(cols: &[String], order: &[usize]) -> String {
        order
            .iter()
            .map(|&k| cols[k].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn reordered_columns_give_low_disagreement_and_misreads_high() {
        // Four columns of one page. LoC reads them in order with a misread
        // every 40 letters; American Stories in another order, near clean;
        // our OCR in a third order with a misread every 9 letters.
        let mut r = Lcg(962);
        let cols: Vec<String> = (0..4).map(|_| words(300, &mut r).join(" ")).collect();
        let truth = join(&cols, &[0, 1, 2, 3]);
        let loc = misread(&join(&cols, &[0, 1, 2, 3]), 40, 3);
        let as_ = misread(&join(&cols, &[2, 0, 3, 1]), 200, 7);
        let ours = misread(&join(&cols, &[1, 3, 0, 2]), 9, 1);
        let readers = [
            ("loc", loc.as_str()),
            ("as", as_.as_str()),
            ("ours", ours.as_str()),
        ];
        let h = solve(&readers);
        assert!(h.determined);
        assert!(h.pairs.iter().all(|p| p.basis == Basis::Local));
        // The pairs' whole-page rates are mostly order; the local ones
        // aren't.
        for p in &h.pairs {
            assert!(p.cer_sym > 0.3, "{p:?}");
        }
        let d = |a: &str, b: &str| {
            h.pairs
                .iter()
                .find(|p| p.a == a && p.b == b)
                .map(|p| p.d)
                .unwrap()
        };
        assert!(d("loc", "as") < 0.05, "{}", d("loc", "as"));
        assert!(d("loc", "ours") > 0.1, "{}", d("loc", "ours"));
        // The estimates near each reader's true rate within the columns,
        // and in the right order.
        let true_cer = |x: &str, order: &[usize]| align::pair(x, &join(&cols, order)).cer_a;
        let truths = [
            true_cer(&loc, &[0, 1, 2, 3]),
            true_cer(&as_, &[2, 0, 3, 1]),
            true_cer(&ours, &[1, 3, 0, 2]),
        ];
        let est: Vec<f64> = h.readers.iter().map(|x| x.est_cer.unwrap()).collect();
        for (k, (e, t)) in est.iter().zip(truths).enumerate() {
            assert!((e - t).abs() < 0.02, "reader {k}: estimated {e}, true {t}");
        }
        assert!(est[1] < est[0] && est[0] < est[2], "{est:?}");
        // The whole-page hat charges the order to the readers.
        let old = v1(&readers);
        assert!(old.estimates[1] > 0.1, "{:?}", old.estimates);
        assert!(old.estimates[1] > 5.0 * est[1].max(0.01));
        assert_eq!(truth.len(), loc.len());
        for x in &h.readers {
            assert!(x.matched.unwrap() > 0.9, "{x:?}");
        }
    }

    #[test]
    fn missing_text_is_coverage_not_error() {
        // American Stories leaves out half the page (the ads); what it has
        // it read cleanly.
        let mut r = Lcg(5);
        let page = words(800, &mut r);
        let full = page.join(" ");
        let half = page[..400].join(" ");
        let ours = misread(&full, 12, 2);
        let h = solve(&[("loc", &full), ("as", &half), ("ours", &ours)]);
        assert!(h.determined);
        let as_ = &h.readers[1];
        assert!(as_.est_cer.unwrap() < 0.02, "{as_:?}");
        // LoC's text is only half placed in American Stories', fully in
        // ours: a mean near 0.75.
        let m = h.readers[0].matched.unwrap();
        assert!((m - 0.75).abs() < 0.03, "{m}");
        assert_eq!(as_.matched, Some(1.0));
    }

    #[test]
    fn garbage_falls_back_to_the_whole_page() {
        let mut r = Lcg(11);
        let page = words(600, &mut r).join(" ");
        let garbage = words(600, &mut r).join(" ");
        let p = pair_d(&page, &garbage, "loc", "ours");
        assert_eq!(p.basis, Basis::Page);
        assert_eq!(p.d, p.cer_sym);
        assert!(p.d > 0.5);
        // A page where two readers agree and a third is garbage: the third
        // gets the high estimate, the two the low ones.
        let other = misread(&page, 30, 0);
        let h = solve(&[("loc", &page), ("as", &other), ("ours", &garbage)]);
        let est: Vec<f64> = h.readers.iter().map(|x| x.est_cer.unwrap()).collect();
        assert!(est[2] > 0.5 && est[0] < 0.05 && est[1] < 0.05, "{est:?}");
    }

    #[test]
    fn empty_pairs_and_too_few_readers() {
        let h = solve(&[("loc", ""), ("as", ""), ("ours", "")]);
        assert!(h.pairs.is_empty());
        assert!(!h.determined);
        assert!(h
            .readers
            .iter()
            .all(|x| x.est_cer.is_none() && x.matched.is_none()));
        let h = solve(&[("loc", "one two three four"), ("as", "one two three four")]);
        assert!(!h.determined);
        assert_eq!(h.pairs.len(), 1);
        assert_eq!(h.ceiling_est, None);
    }

    #[test]
    fn deterministic() {
        let mut r = Lcg(3);
        let a = words(500, &mut r).join(" ");
        let b = misread(&a, 17, 4);
        let c = misread(&a, 23, 5);
        let readers = [
            ("loc", a.as_str()),
            ("as", b.as_str()),
            ("ours", c.as_str()),
        ];
        let one = serde_json::to_string(&solve(&readers)).unwrap();
        assert_eq!(one, serde_json::to_string(&solve(&readers)).unwrap());
    }
}
