//! Geographic skew: does a place print a search term more or less often than
//! the other places did over the same dates? (docs/design/11-term-geographic-skew.md)
//!
//! 1. [`place_counts`]: each place's observed hits and its expected hits by
//!    indirect standardization. In every time bucket, the share of the
//!    **other** places' pages that matched, times the pages this place
//!    published in that bucket, summed over buckets. A place with many
//!    newspapers, or with more of its pages digitized, is expected to have
//!    more hits, and so is a place whose pages come from an epidemic year.
//! 2. [`dispersion_for`]: how much more the hits vary than independent pages
//!    would. One newspaper reprints the same item in issue after issue, so
//!    pages are not independent. Dividing observed and expected hits by
//!    `phi` treats a place as having `1 / phi` as much independent evidence.
//! 3. [`Prior::fit`]: an empirical-Bayes gamma prior on the ratio of observed
//!    to expected hits (the lift), with its mean and spread fitted to all
//!    places by maximizing the negative binomial marginal likelihood.
//! 4. [`Prior::score`]: each place's posterior for the lift,
//!    `Gamma(a + observed / phi, a / mean + expected / phi)`, where `a` is
//!    `alpha` widened by the uncertainty in the fitted mean: its
//!    median as the estimate, a central credible interval, and the
//!    probability that the lift is above 1. Places with little evidence stay
//!    near the prior, so a village with one page and one hit can't claim the
//!    top of the map.
//! 5. [`score_search`] and [`score_groups`]: the steps above for one search,
//!    as the map's relative-rate view runs them, per place and per state.
//!
//! The site runs a TypeScript port in the browser (`web/src/engine/skew.ts`).
//! `fixtures/skew-vectors.json` holds inputs and the scores this module gives
//! them; both test suites check it (`tests/skew_vectors.rs`), so the two
//! can't drift. Regenerate it after a deliberate change to the scoring with
//! `UPDATE_SKEW_VECTORS=1 cargo test -p usnm-core --test skew_vectors`.

use chrono::Datelike;

use crate::time::BucketSpec;

/// One place in one time bucket: pages it published and pages that matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub place: usize,
    pub bucket: usize,
    pub pages: u64,
    pub hits: u64,
}

/// A place's observed and expected hits over the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Counts {
    pub observed: u64,
    pub expected: f64,
}

/// Hits credited to the other places in a bucket where they have none but
/// this place has some (a continuity correction, as in a Jeffreys prior).
pub const ZERO_REFERENCE_HITS: f64 = 0.5;

/// The share of the other places' pages that matched in the cell's bucket;
/// `None` when the place published every page of that bucket (or the bucket
/// is outside the national series). When the others have no hits but this
/// place has some, the rate is [`ZERO_REFERENCE_HITS`] over the others'
/// pages instead of 0: a rate of 0 would make the place's hits impossible
/// under the model and give them no expected count to be compared with.
pub fn reference_rate(national_hits: &[u64], national_pages: &[u64], c: &Cell) -> Option<f64> {
    let (h, n) = (national_hits.get(c.bucket)?, national_pages.get(c.bucket)?);
    let other_pages = n.checked_sub(c.pages).filter(|&p| p > 0)?;
    let other_hits = h.saturating_sub(c.hits);
    let other_hits = if other_hits == 0 && c.hits > 0 {
        ZERO_REFERENCE_HITS
    } else {
        other_hits as f64
    };
    Some(other_hits / other_pages as f64)
}

/// Observed and expected hits per place (indexes `0..places`), comparing each
/// place with the others in the same buckets. Cells without a reference
/// (the place is the only publisher in that bucket) are left out of both.
pub fn place_counts(
    national_hits: &[u64],
    national_pages: &[u64],
    cells: &[Cell],
    places: usize,
) -> Vec<Counts> {
    let mut out = vec![
        Counts {
            observed: 0,
            expected: 0.0,
        };
        places
    ];
    for c in cells {
        let (Some(slot), Some(rate)) = (
            out.get_mut(c.place),
            reference_rate(national_hits, national_pages, c),
        ) else {
            continue;
        };
        slot.observed += c.hits;
        slot.expected += c.pages as f64 * rate;
    }
    out
}

/// Measured over-dispersion: `phi`, and how many places it was measured on.
/// `phi` is `None` when no place had enough expected hits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dispersion {
    pub phi: Option<f64>,
    pub places: usize,
}

/// Consecutive buckets are merged until a chunk expects this many hits.
pub const CHUNK_EXPECTED: f64 = 5.0;
/// A place needs this many chunks to contribute to [`dispersion`].
pub const MIN_CHUNKS: usize = 3;

/// Over-dispersion of the hits. For each place, its buckets (in order) are
/// merged into chunks that each expect at least [`CHUNK_EXPECTED`] hits at
/// the place's own overall lift. The Pearson chi-square of the chunks, with
/// binomial variance (hits are pages, at most one per page), per degree of
/// freedom, is that place's dispersion. `phi` is the median over places with
/// at least [`MIN_CHUNKS`] chunks, and never below 1.
///
/// Chunks of fixed expected size make `phi` depend on the data rather than
/// on the bucket unit the search asked for. A place whose interest in the
/// term rises and falls over the window still adds to it, so `phi` errs
/// toward wider intervals.
pub fn dispersion(national_hits: &[u64], national_pages: &[u64], cells: &[Cell]) -> Dispersion {
    let mut sorted: Vec<&Cell> = cells.iter().collect();
    sorted.sort_by_key(|c| (c.place, c.bucket));
    let mut per_place = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let place = sorted[i].place;
        let mut j = i;
        // (observed, expected at the reference rate, reference rate)
        let mut rows = Vec::new();
        while j < sorted.len() && sorted[j].place == place {
            let c = sorted[j];
            if let Some(r) = reference_rate(national_hits, national_pages, c) {
                if r > 0.0 {
                    rows.push((c.hits as f64, c.pages as f64 * r, r));
                }
            }
            j += 1;
        }
        i = j;
        let (o, e): (f64, f64) = rows.iter().fold((0.0, 0.0), |a, r| (a.0 + r.0, a.1 + r.1));
        if o <= 0.0 || e <= 0.0 {
            continue;
        }
        let theta = o / e;
        // Chunks: (observed, mean, variance).
        let mut chunks: Vec<(f64, f64, f64)> = Vec::new();
        let mut cur = (0.0, 0.0, 0.0);
        for &(o, e, r) in &rows {
            let mean = theta * e;
            cur.0 += o;
            cur.1 += mean;
            cur.2 += mean * (1.0 - (theta * r).min(1.0)).max(1e-3);
            if cur.1 >= CHUNK_EXPECTED {
                chunks.push(cur);
                cur = (0.0, 0.0, 0.0);
            }
        }
        if cur.1 > 0.0 {
            match chunks.last_mut() {
                Some(last) => {
                    last.0 += cur.0;
                    last.1 += cur.1;
                    last.2 += cur.2;
                }
                None => chunks.push(cur),
            }
        }
        if chunks.len() < MIN_CHUNKS {
            continue;
        }
        let chi: f64 = chunks.iter().map(|&(o, m, v)| (o - m).powi(2) / v).sum();
        per_place.push(chi / (chunks.len() - 1) as f64);
    }
    let places = per_place.len();
    if places == 0 {
        return Dispersion { phi: None, places };
    }
    Dispersion {
        phi: Some(median(&mut per_place).max(1.0)),
        places,
    }
}

/// Median of a non-empty slice (the mean of the middle pair for even lengths).
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Used when no place expects enough hits to measure dispersion: about the
/// middle of what was measured on real searches (doc 11, 11.5.7).
pub const FALLBACK_PHI: f64 = 4.0;

/// [`dispersion`] at a resolution that doesn't depend on the bucket unit the
/// search asked for: cells are merged into calendar years when the window
/// spans three years or more, and into calendar months otherwise (buckets
/// already coarser than that are used as they are). Returns the dispersion
/// and the `phi` to use, [`FALLBACK_PHI`] when none could be measured.
///
/// Each bucket goes to the period of its first day. Day, month and year
/// buckets fall wholly inside one period, so for them the result is the
/// same whichever unit was asked for. A week that crosses a month or year
/// boundary goes wholly to the earlier period, so with week buckets `phi`
/// can differ slightly from the same search by day (the cells only carry
/// weekly totals, so its pages can't be split exactly).
pub fn dispersion_for(
    spec: &BucketSpec,
    national_hits: &[u64],
    national_pages: &[u64],
    cells: &[Cell],
) -> (Dispersion, f64) {
    use std::collections::BTreeMap;
    // `to` is inclusive: 1880-01-01 to 1882-12-31 is three years.
    let by_year = spec.to + chrono::Days::new(1) >= spec.from + chrono::Months::new(36);
    let key = |b: usize| {
        let d = spec.bucket_start(b);
        if by_year {
            d.year() as i64
        } else {
            d.year() as i64 * 12 + i64::from(d.month0())
        }
    };
    let mut slot: BTreeMap<i64, usize> = BTreeMap::new();
    for b in 0..national_hits.len().min(national_pages.len()) {
        let next = slot.len();
        slot.entry(key(b)).or_insert(next);
    }
    let mut hits = vec![0u64; slot.len()];
    let mut pages = vec![0u64; slot.len()];
    for b in 0..national_hits.len().min(national_pages.len()) {
        hits[slot[&key(b)]] += national_hits[b];
        pages[slot[&key(b)]] += national_pages[b];
    }
    let mut merged: BTreeMap<(usize, usize), (u64, u64)> = BTreeMap::new();
    for c in cells {
        if c.bucket >= national_hits.len().min(national_pages.len()) {
            continue;
        }
        if let Some(&s) = slot.get(&key(c.bucket)) {
            let e = merged.entry((c.place, s)).or_default();
            e.0 += c.pages;
            e.1 += c.hits;
        }
    }
    let cells: Vec<Cell> = merged
        .into_iter()
        .map(|((place, bucket), (pages, hits))| Cell {
            place,
            bucket,
            pages,
            hits,
        })
        .collect();
    let d = dispersion(&hits, &pages, &cells);
    (d, d.phi.unwrap_or(FALLBACK_PHI))
}

/// The gamma prior on the lift, `Gamma(alpha, rate = alpha / mean)`: mean
/// `mean`, variance `mean^2 / alpha`. A large `alpha` means places barely
/// differ once corpus volume is accounted for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prior {
    pub alpha: f64,
    pub mean: f64,
    /// Sampling variance of `ln mean`: how uncertain the fitted mean is
    /// (the inverse of its Fisher information). See [`Prior::shape`].
    pub mean_var: f64,
}

/// One place's score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub observed: u64,
    pub expected: f64,
    /// `observed / expected`, unshrunk; `None` when nothing was expected.
    pub lift: Option<f64>,
    /// Posterior median of the lift.
    pub estimate: f64,
    /// Central credible interval for the lift at the requested level.
    pub lower: f64,
    pub upper: f64,
    /// Posterior probability that the lift is above 1.
    pub above: f64,
}

impl Score {
    /// The interval lies wholly above (`1`) or below (`-1`) 1, or includes it (`0`).
    pub fn direction(&self) -> i8 {
        if self.lower > 1.0 {
            1
        } else if self.upper < 1.0 {
            -1
        } else {
            0
        }
    }
}

/// Bounds of the prior search. `MAX_ALPHA` stands for "no detectable
/// difference between places".
pub const MIN_ALPHA: f64 = 1e-3;
pub const MAX_ALPHA: f64 = 1e6;
const MIN_MEAN: f64 = 1e-2;
const MAX_MEAN: f64 = 1e2;

impl Prior {
    /// Fit `alpha` and `mean` by maximum marginal likelihood: observed hits
    /// divided by `phi` are negative binomial with mean `mean * expected /
    /// phi` and size `alpha`. Places with no expected hits carry no
    /// information and are skipped. With no usable places, returns
    /// `MAX_ALPHA` and mean 1.
    pub fn fit(counts: &[Counts], phi: f64) -> Self {
        let phi = phi.max(1.0);
        let usable: Vec<(f64, f64)> = counts
            .iter()
            .filter(|c| c.expected > 0.0 && c.expected.is_finite())
            .map(|c| (c.observed as f64 / phi, c.expected / phi))
            .collect();
        if usable.is_empty() {
            return Self {
                alpha: MAX_ALPHA,
                mean: 1.0,
                mean_var: 0.0,
            };
        }
        let (so, se) = usable
            .iter()
            .fold((0.0, 0.0), |a, u| (a.0 + u.0, a.1 + u.1));
        let mut ln_mean = (so / se).clamp(MIN_MEAN, MAX_MEAN).ln();
        let mut ln_alpha = 0.0;
        // Coordinate ascent: alternate one-dimensional golden-section searches
        // (the log-likelihood is smooth and, in practice, unimodal in each).
        for _ in 0..30 {
            let before = (ln_alpha, ln_mean);
            ln_alpha = golden_max(MIN_ALPHA.ln(), MAX_ALPHA.ln(), |a| {
                nb_log_likelihood(&usable, a.exp(), ln_mean.exp())
            });
            ln_mean = golden_max(MIN_MEAN.ln(), MAX_MEAN.ln(), |m| {
                nb_log_likelihood(&usable, ln_alpha.exp(), m.exp())
            });
            if (ln_alpha - before.0).abs() < 1e-5 && (ln_mean - before.1).abs() < 1e-7 {
                break;
            }
        }
        let alpha = ln_alpha.exp().clamp(MIN_ALPHA, MAX_ALPHA);
        let mean = ln_mean.exp().clamp(MIN_MEAN, MAX_MEAN);
        // Fisher information for ln(mean): each place contributes
        // alpha m / (alpha + m), with m its mean count.
        let info: f64 = usable
            .iter()
            .map(|&(_, e)| {
                let m = mean * e;
                alpha * m / (alpha + m)
            })
            .sum();
        Self {
            alpha,
            mean,
            mean_var: if info > 0.0 { 1.0 / info } else { 0.0 },
        }
    }

    /// The shape the places are scored with: `alpha`, widened by the
    /// uncertainty in the fitted mean. The lift is the mean times a place's
    /// own factor, `Gamma(alpha, alpha)`; treating `ln mean` as normal with
    /// variance `v`, the product's squared coefficient of variation is
    /// `1/alpha + v (1 + 1/alpha)`, and the shape is its inverse. Without
    /// this, a search with few hits where the places don't differ (`alpha`
    /// at its bound) puts every place at the fitted mean with a very narrow
    /// interval, and a mean of 0.9 from 17 hits would mark every place as
    /// clearly below the others.
    pub fn shape(&self) -> f64 {
        1.0 / (1.0 / self.alpha + self.mean_var * (1.0 + 1.0 / self.alpha))
    }

    /// Score one place with a central credible interval at `level` (e.g.
    /// 0.9), treating its counts as `phi` times over-dispersed.
    pub fn score(&self, c: Counts, phi: f64, level: f64) -> Score {
        let phi = phi.max(1.0);
        let a = self.shape();
        let shape = a + c.observed as f64 / phi;
        let rate = a / self.mean + c.expected.max(0.0) / phi;
        let tail = (1.0 - level.clamp(0.0, 1.0)) / 2.0;
        Score {
            observed: c.observed,
            expected: c.expected,
            lift: (c.expected > 0.0).then(|| c.observed as f64 / c.expected),
            estimate: gamma_quantile(0.5, shape) / rate,
            lower: gamma_quantile(tail, shape) / rate,
            upper: gamma_quantile(1.0 - tail, shape) / rate,
            above: 1.0 - gamma_p(shape, rate),
        }
    }
}

/// Fit the prior to `counts` and score each place.
pub fn score_all(counts: &[Counts], phi: f64, level: f64) -> (Prior, Vec<Score>) {
    let prior = Prior::fit(counts, phi);
    let scores = counts.iter().map(|&c| prior.score(c, phi, level)).collect();
    (prior, scores)
}

/// One search scored as the map shows it (doc 11, 11.4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchScores {
    pub dispersion: Dispersion,
    /// The `phi` used: measured, or [`FALLBACK_PHI`].
    pub phi: f64,
    pub prior: Prior,
    /// One per place, in place order.
    pub scores: Vec<Score>,
}

/// Every step for one search: observed and expected hits per place,
/// dispersion at the calendar resolution, the prior fitted to the places
/// with `in_fit` true (a missing entry counts as true), and every place
/// scored with it. A place left out of the fit is still scored. The site
/// fits every place (doc 11, 11.14); the mask is there for a later
/// language-aware version (11.5.9) and is covered by the shared vectors.
pub fn score_search(
    spec: &BucketSpec,
    national_hits: &[u64],
    national_pages: &[u64],
    cells: &[Cell],
    places: usize,
    in_fit: &[bool],
    level: f64,
) -> SearchScores {
    let counts = place_counts(national_hits, national_pages, cells, places);
    let (dispersion, phi) = dispersion_for(spec, national_hits, national_pages, cells);
    let fit: Vec<Counts> = counts
        .iter()
        .enumerate()
        .filter(|&(i, _)| in_fit.get(i).copied().unwrap_or(true))
        .map(|(_, &c)| c)
        .collect();
    let prior = Prior::fit(&fit, phi);
    let scores = counts.iter().map(|&c| prior.score(c, phi, level)).collect();
    SearchScores {
        dispersion,
        phi,
        prior,
        scores,
    }
}

/// Cells summed per group (for example a state) and bucket, sorted by group
/// and bucket. `group_of[place]` names each place's group; cells of places
/// without one are dropped.
pub fn group_cells(cells: &[Cell], group_of: &[usize]) -> Vec<Cell> {
    let mut merged: std::collections::BTreeMap<(usize, usize), (u64, u64)> = Default::default();
    for c in cells {
        if let Some(&g) = group_of.get(c.place) {
            let e = merged.entry((g, c.bucket)).or_default();
            e.0 += c.pages;
            e.1 += c.hits;
        }
    }
    merged
        .into_iter()
        .map(|((place, bucket), (pages, hits))| Cell {
            place,
            bucket,
            pages,
            hits,
        })
        .collect()
}

/// Groups of places (states) scored as units: each group's cells are summed
/// per bucket and compared with the other groups' pages in that bucket, with
/// a prior fitted across the groups (doc 11, 11.4.1, "States"). `phi` is the
/// larger of the place-level `phi` and the groups' own: a state's hits vary
/// more than a place's (on the seven searches in doc 11 the states measured
/// 2 to 4 times the places' `phi`), and scoring them with the places' value
/// would make their intervals too narrow.
// The inputs mirror `score_search` plus the grouping and the places' phi.
#[allow(clippy::too_many_arguments)]
pub fn score_groups(
    spec: &BucketSpec,
    national_hits: &[u64],
    national_pages: &[u64],
    cells: &[Cell],
    group_of: &[usize],
    groups: usize,
    place_phi: f64,
    level: f64,
) -> SearchScores {
    let grouped = group_cells(cells, group_of);
    let counts = place_counts(national_hits, national_pages, &grouped, groups);
    let (dispersion, _) = dispersion_for(spec, national_hits, national_pages, &grouped);
    let phi = dispersion.phi.map_or(place_phi, |p| p.max(place_phi));
    let (prior, scores) = score_all(&counts, phi, level);
    SearchScores {
        dispersion,
        phi,
        prior,
        scores,
    }
}

fn golden_max(mut lo: f64, mut hi: f64, f: impl Fn(f64) -> f64) -> f64 {
    let g = (5f64.sqrt() - 1.0) / 2.0;
    let mut a = hi - g * (hi - lo);
    let mut b = lo + g * (hi - lo);
    let (mut fa, mut fb) = (f(a), f(b));
    for _ in 0..200 {
        if hi - lo < 1e-7 {
            break;
        }
        if fa < fb {
            lo = a;
            a = b;
            fa = fb;
            b = lo + g * (hi - lo);
            fb = f(b);
        } else {
            hi = b;
            b = a;
            fb = fa;
            a = hi - g * (hi - lo);
            fa = f(a);
        }
    }
    (lo + hi) / 2.0
}

fn nb_log_likelihood(counts: &[(f64, f64)], alpha: f64, mean: f64) -> f64 {
    let lg_alpha = ln_gamma(alpha);
    counts
        .iter()
        .map(|&(o, e)| {
            let m = mean * e;
            // ln Gamma(o + alpha) - ln Gamma(alpha) - ln o!
            //   + alpha ln(alpha / (alpha + m)) + o ln(m / (alpha + m))
            ln_gamma(o + alpha) - lg_alpha - ln_gamma(o + 1.0) - alpha * (m / alpha).ln_1p()
                + if o > 0.0 {
                    o * (m.ln() - (alpha + m).ln())
                } else {
                    0.0
                }
        })
        .sum()
}

/// `ln Gamma(x)` for `x > 0` (Lanczos, g = 7, n = 9; about 15 significant digits).
pub fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        // Reflection: Gamma(x) Gamma(1 - x) = pi / sin(pi x).
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut sum = C[0];
    for (i, c) in C.iter().enumerate().skip(1) {
        sum += c / (x + i as f64);
    }
    let t = x + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}

/// Regularized lower incomplete gamma function `P(a, x)`.
pub fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let front = (-x + a * x.ln() - ln_gamma(a)).exp();
    // Both expansions need on the order of sqrt(a) terms near x = a.
    let limit = 1_000 + (50.0 * a.sqrt()) as usize;
    if x < a + 1.0 {
        // Series.
        let (mut ap, mut sum, mut del) = (a, 1.0 / a, 1.0 / a);
        for _ in 0..limit {
            ap += 1.0;
            del *= x / ap;
            sum += del;
            if del.abs() < sum.abs() * 1e-16 {
                break;
            }
        }
        (sum * front).clamp(0.0, 1.0)
    } else {
        // Continued fraction for Q(a, x) (modified Lentz).
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..limit {
            let an = -(i as f64) * (i as f64 - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < tiny {
                d = tiny;
            }
            c = b + an / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            let delta = d * c;
            h *= delta;
            if (delta - 1.0).abs() < 1e-16 {
                break;
            }
        }
        (1.0 - front * h).clamp(0.0, 1.0)
    }
}

/// Inverse of the standard normal CDF (Acklam's rational approximation,
/// relative error below 1.2e-9).
pub fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    if p < 0.02425 {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - 0.02425 {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

/// Wilson and Hilferty's approximation to the `p` quantile of `Gamma(shape, 1)`.
fn wilson_hilferty(p: f64, shape: f64) -> f64 {
    let v = 1.0 / (9.0 * shape);
    shape * (1.0 - v + normal_quantile(p) * v.sqrt()).powi(3)
}

/// Above this shape the Wilson and Hilferty approximation is used as is
/// (relative error well below 1e-6 there).
const LARGE_SHAPE: f64 = 1e4;

/// The `p` quantile of `Gamma(shape, rate = 1)`: Newton's method on
/// [`gamma_p`], kept inside a bisection bracket, from a Wilson and Hilferty
/// start; the approximation alone for very large shapes.
pub fn gamma_quantile(p: f64, shape: f64) -> f64 {
    if p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if shape > LARGE_SHAPE {
        return wilson_hilferty(p, shape);
    }
    let ln_g = ln_gamma(shape);
    let (mut lo, mut hi) = (0.0, shape.max(1.0));
    while gamma_p(shape, hi) < p {
        lo = hi;
        hi *= 2.0;
    }
    // Start: Wilson and Hilferty, or for small shapes the leading term of the
    // series, P(a, x) ~ x^a / Gamma(a + 1).
    let mut x = if shape >= 1.0 {
        wilson_hilferty(p, shape)
    } else {
        let x = ((p.ln() + ln_gamma(shape + 1.0)) / shape).exp();
        if x < f64::MIN_POSITIVE {
            // Below the smallest normal f64: as close to 0 as it gets.
            return x;
        }
        x
    };
    if !(x > lo && x < hi) {
        x = (lo + hi) / 2.0;
    }
    for _ in 0..200 {
        let f = gamma_p(shape, x) - p;
        if f < 0.0 {
            lo = x;
        } else {
            hi = x;
        }
        let pdf = ((shape - 1.0) * x.ln() - x - ln_g).exp();
        let mut next = x - f / pdf;
        if !(next > lo && next < hi) {
            next = (lo + hi) / 2.0;
        }
        if (next - x).abs() <= 1e-13 * x || hi - lo <= 1e-13 * hi {
            return next;
        }
        x = next;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    /// splitmix64: a small deterministic generator, so the simulations are reproducible.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        }

        fn uniform(&mut self) -> f64 {
            ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
        }

        fn normal(&mut self) -> f64 {
            let (u, v) = (self.uniform(), self.uniform());
            (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
        }

        /// Poisson by inversion, in chunks of at most 500 (sums of Poissons are Poisson).
        fn poisson(&mut self, mut lambda: f64) -> u64 {
            let mut total = 0;
            while lambda > 0.0 {
                let l = lambda.min(500.0);
                lambda -= l;
                let u = self.uniform();
                let (mut k, mut p) = (0u64, (-l).exp());
                let mut cdf = p;
                while cdf < u && k < 100_000 {
                    k += 1;
                    p *= l / k as f64;
                    cdf += p;
                }
                total += k;
            }
            total
        }

        /// Gamma(shape, 1) by Marsaglia and Tsang (shape >= 1).
        fn gamma(&mut self, shape: f64) -> f64 {
            let d = shape - 1.0 / 3.0;
            let c = 1.0 / (9.0 * d).sqrt();
            loop {
                let x = self.normal();
                let v = (1.0 + c * x).powi(3);
                if v <= 0.0 {
                    continue;
                }
                if self.uniform().ln() < 0.5 * x * x + d - d * v + d * v.ln() {
                    return d * v;
                }
            }
        }
    }

    /// Heavy-tailed exposures like the corpus: most places publish few
    /// pages, a few publish very many (log-normal, median 20 expected hits).
    fn exposures(rng: &mut Rng, n: usize) -> Vec<f64> {
        (0..n).map(|_| (3.0 + 2.0 * rng.normal()).exp()).collect()
    }

    /// Poisson counts around `expected * lift`, in runs of `run` reprinted pages.
    fn draw(rng: &mut Rng, expected: &[f64], lifts: &[f64], run: u64) -> Vec<Counts> {
        expected
            .iter()
            .zip(lifts)
            .map(|(&e, &t)| Counts {
                observed: run * rng.poisson(e * t / run as f64),
                expected: e,
            })
            .collect()
    }

    /// Share of places whose interval contains the true lift.
    fn coverage(scores: &[Score], lifts: &[f64]) -> (usize, usize) {
        let hit = scores
            .iter()
            .zip(lifts)
            .filter(|(s, t)| s.lower <= **t && **t <= s.upper)
            .count();
        (hit, scores.len())
    }

    #[test]
    fn ln_gamma_matches_known_values() {
        assert!(close(ln_gamma(1.0), 0.0, 1e-13));
        assert!(close(ln_gamma(5.0), 24f64.ln(), 1e-13));
        assert!(close(
            ln_gamma(0.5),
            std::f64::consts::PI.sqrt().ln(),
            1e-13
        ));
        // ln(99!) = 359.1342053695754.
        assert!(close(ln_gamma(100.0), 359.134_205_369_575_4, 1e-13));
        assert!(close(ln_gamma(0.1), 2.252_712_651_734_206, 1e-12));
    }

    #[test]
    fn gamma_p_matches_known_values() {
        // P(1, x) = 1 - e^-x.
        for x in [0.1, 1.0, 3.0, 20.0] {
            assert!(close(gamma_p(1.0, x), 1.0 - (-x).exp(), 1e-12));
        }
        // Chi-square with k df: CDF(x) = P(k/2, x/2); 95th percentiles.
        assert!(close(gamma_p(0.5, 3.841_458_821 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(1.0, 5.991_464_547 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(5.0, 18.307_038_05 / 2.0), 0.95, 1e-9));
        assert!(close(gamma_p(50.0, 124.342_113_4 / 2.0), 0.95, 1e-7));
        assert!(close(gamma_p(500.0, 1_074.679_449 / 2.0), 0.95, 1e-6));
    }

    #[test]
    fn normal_quantile_matches_known_values() {
        assert!(close(normal_quantile(0.5), 0.0, 1e-12));
        assert!(close(normal_quantile(0.975), 1.959_963_985, 1e-8));
        assert!(close(normal_quantile(0.05), -1.644_853_627, 1e-8));
        assert!(close(normal_quantile(0.001), -3.090_232_306, 1e-8));
    }

    #[test]
    fn gamma_quantile_inverts_gamma_p() {
        for shape in [0.01, 0.05, 0.5, 1.0, 3.0, 40.0, 999.0, 9_999.0] {
            for p in [0.05, 0.5, 0.95] {
                let q = gamma_quantile(p, shape);
                assert!(close(gamma_p(shape, q), p, 1e-9), "shape {shape} p {p}");
            }
        }
        // Below that the lower quantiles underflow (the 5% point of
        // Gamma(0.001) is about 1e-1301); they come out as 0 or close to it.
        assert!(gamma_quantile(0.05, 0.001) < 1e-300);
        assert!(close(
            gamma_p(0.001, gamma_quantile(0.95, 0.001)),
            0.95,
            1e-9
        ));
        // Very large shapes use Wilson and Hilferty: the median of
        // Gamma(k) is k - 1/3 + 8 / (405 k) + O(1/k^2).
        for k in [2e4, 1e6, 1e7] {
            let m = k - 1.0 / 3.0 + 8.0 / (405.0 * k);
            assert!(close(gamma_quantile(0.5, k), m, 1e-9), "k {k}");
        }
        // ... and agree with the exact inversion just above the switch.
        let k = 1.5e4;
        let exact = {
            let (mut lo, mut hi) = (0.9 * k, 1.1 * k);
            for _ in 0..100 {
                let mid = (lo + hi) / 2.0;
                if gamma_p(k, mid) < 0.05 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            (lo + hi) / 2.0
        };
        assert!(close(gamma_quantile(0.05, k), exact, 1e-7));
    }

    #[test]
    fn compares_each_place_with_the_others_in_the_same_buckets() {
        // Bucket 0 is an epidemic year (10% of pages match), bucket 1 a quiet
        // one (0.1%). Place 0 published only in the epidemic year at that
        // rate; place 1 only in the quiet year; place 2 in both.
        let cells = [
            Cell {
                place: 0,
                bucket: 0,
                pages: 100,
                hits: 10,
            },
            Cell {
                place: 1,
                bucket: 1,
                pages: 90_000,
                hits: 90,
            },
            Cell {
                place: 2,
                bucket: 0,
                pages: 10_000,
                hits: 1_000,
            },
            Cell {
                place: 2,
                bucket: 1,
                pages: 1_000,
                hits: 1,
            },
            // Bucket 2: place 2 alone. No reference, so left out.
            Cell {
                place: 2,
                bucket: 2,
                pages: 50,
                hits: 50,
            },
        ];
        let hits = [1_010, 91, 50];
        let pages = [10_100, 91_000, 50];
        let c = place_counts(&hits, &pages, &cells, 3);
        // Place 0: 100 pages at the others' 10% = 10 expected, 10 observed.
        // Its share of all pages (100 of 101,150) would expect about 1.
        assert!(close(c[0].expected, 10.0, 1e-12));
        assert_eq!(c[0].observed, 10);
        assert!(close(c[1].expected, 90_000.0 * 1.0 / 1_000.0, 1e-12));
        assert_eq!(c[2].observed, 1_001);
        assert!(close(
            c[2].expected,
            10_000.0 * 0.1 + 1_000.0 * 0.001,
            1e-12
        ));
        // Out-of-range cells are ignored.
        let stray = [Cell {
            place: 9,
            bucket: 0,
            pages: 1,
            hits: 0,
        }];
        assert_eq!(place_counts(&hits, &pages, &stray, 1)[0].observed, 0);
    }

    #[test]
    fn hits_only_here_still_have_an_expected_count() {
        // The term appears only in place 0 (5 hits on 100 pages); the other
        // 10,000 pages have none. Without the correction place 0 would
        // expect 0 hits and have 5, which the model can't score.
        let cells = [
            Cell {
                place: 0,
                bucket: 0,
                pages: 100,
                hits: 5,
            },
            Cell {
                place: 1,
                bucket: 0,
                pages: 10_000,
                hits: 0,
            },
        ];
        let c = place_counts(&[5], &[10_100], &cells, 2);
        assert!(close(c[0].expected, 100.0 * 0.5 / 10_000.0, 1e-12));
        // Place 1 is compared with place 0's 5% and expects 500.
        assert!(close(c[1].expected, 500.0, 1e-12));
        let (_, scores) = score_all(&c, 1.0, 0.9);
        assert!(scores
            .iter()
            .all(|s| s.estimate.is_finite() && s.upper.is_finite()));
        assert!(scores[0].lift.unwrap() > 50.0);
        // The direction is clear; the size above 1 is set by the correction
        // (the map's colour scale stops at 8 times anyway).
        assert_eq!((scores[0].direction(), scores[1].direction()), (1, -1));
    }

    #[test]
    fn a_big_place_is_not_compared_with_itself() {
        // Place 0 has half the pages and matches at twice the rate of the rest.
        let cells = [
            Cell {
                place: 0,
                bucket: 0,
                pages: 50_000,
                hits: 1_000,
            },
            Cell {
                place: 1,
                bucket: 0,
                pages: 25_000,
                hits: 250,
            },
            Cell {
                place: 2,
                bucket: 0,
                pages: 25_000,
                hits: 250,
            },
        ];
        let c = place_counts(&[1_500], &[100_000], &cells, 3);
        // Against the others it is 2x; against a national rate that
        // includes itself it would be 1.33x.
        assert!(close(c[0].observed as f64 / c[0].expected, 2.0, 1e-12));
        assert!(close(1_000.0 / (50_000.0 * 0.015), 4.0 / 3.0, 1e-12));
    }

    #[test]
    fn one_page_one_hit_does_not_top_the_map() {
        let counts = [
            // A village: 0.01 expected, 1 hit.
            Counts {
                observed: 1,
                expected: 0.01,
            },
            // A city: 2,000 hits where 1,000 were expected.
            Counts {
                observed: 2_000,
                expected: 1_000.0,
            },
            // Places near the reference rate.
            Counts {
                observed: 500,
                expected: 520.0,
            },
            Counts {
                observed: 300,
                expected: 290.0,
            },
            Counts {
                observed: 40,
                expected: 45.0,
            },
        ];
        let (_, scores) = score_all(&counts, 1.0, 0.9);
        // Raw lift ranks the village first (100x vs 2x) ...
        assert!(scores[0].lift.unwrap() > scores[1].lift.unwrap());
        // ... the estimate doesn't, and only the city is clearly above 1.
        assert!(scores[1].estimate > scores[0].estimate);
        assert_eq!(scores[1].direction(), 1);
        assert_eq!(scores[0].direction(), 0);
        assert!(scores[1].lower > 1.8 && scores[1].upper < 2.2);
        assert!(scores[1].above > 0.999);
    }

    #[test]
    fn no_difference_between_places_means_no_skew() {
        // Every place prints the term at the reference rate (lift 1).
        let mut rng = Rng(7);
        let e = exposures(&mut rng, 400);
        let counts = draw(&mut rng, &e, &vec![1.0; 400], 1);
        let (prior, scores) = score_all(&counts, 1.0, 0.9);
        // The fit finds little or no spread between places, around 1 ...
        assert!(prior.alpha > 50.0, "alpha {}", prior.alpha);
        assert!(close(prior.mean, 1.0, 0.03), "mean {}", prior.mean);
        // ... so almost no place is flagged (a 90% interval without shrinkage
        // would exclude the true lift for about 40 of the 400).
        let flagged = scores.iter().filter(|s| s.direction() != 0).count();
        assert!(flagged <= 4, "{flagged} flagged");
        // The raw lift's top 10 are small places; every estimate stays near 1.
        let mut by_raw: Vec<&Score> = scores.iter().collect();
        by_raw.sort_by(|a, b| b.lift.partial_cmp(&a.lift).unwrap());
        let mut sorted = e.clone();
        sorted.sort_by(f64::total_cmp);
        let median_e = sorted[sorted.len() / 2];
        assert!(by_raw[..10].iter().all(|s| s.expected < median_e));
        assert!(by_raw[0].lift.unwrap() > 3.0);
        assert!(scores
            .iter()
            .all(|s| s.estimate < 1.05 && s.estimate > 0.95));
    }

    #[test]
    fn recovers_the_spread_between_places_and_calibrates_intervals() {
        // True lifts drawn from Gamma(4, 4): places genuinely differ, mean 1.
        let mut rng = Rng(11);
        let (mut hit, mut total) = (0, 0);
        let mut alphas = Vec::new();
        for _ in 0..20 {
            let e = exposures(&mut rng, 400);
            let lifts: Vec<f64> = e.iter().map(|_| rng.gamma(4.0) / 4.0).collect();
            let counts = draw(&mut rng, &e, &lifts, 1);
            let (prior, scores) = score_all(&counts, 1.0, 0.9);
            alphas.push(prior.alpha);
            let (h, t) = coverage(&scores, &lifts);
            hit += h;
            total += t;
        }
        alphas.sort_by(f64::total_cmp);
        let median = (alphas[9] + alphas[10]) / 2.0;
        assert!((3.0..=5.5).contains(&median), "median alpha {median}");
        let cov = hit as f64 / total as f64;
        assert!((0.87..=0.93).contains(&cov), "coverage {cov}");
    }

    #[test]
    fn stays_calibrated_when_the_prior_is_wrong() {
        // Not a gamma: most places log-normal around 1.3 (so the typical place
        // is above the page-weighted reference), 15% near zero (like a paper
        // printed in another language).
        let mut rng = Rng(13);
        let (mut hit, mut total) = (0, 0);
        let mut means = Vec::new();
        for _ in 0..20 {
            let e = exposures(&mut rng, 400);
            let lifts: Vec<f64> = e
                .iter()
                .map(|_| {
                    if rng.uniform() < 0.15 {
                        0.05
                    } else {
                        1.3 * (0.4 * rng.normal() - 0.08).exp()
                    }
                })
                .collect();
            let counts = draw(&mut rng, &e, &lifts, 1);
            let (prior, scores) = score_all(&counts, 1.0, 0.9);
            means.push(prior.mean);
            let (h, t) = coverage(&scores, &lifts);
            hit += h;
            total += t;
        }
        let cov = hit as f64 / total as f64;
        assert!((0.86..=0.92).contains(&cov), "coverage {cov}");
        // The fitted mean follows the places, not the fixed value 1.
        means.sort_by(f64::total_cmp);
        assert!(means[10] > 1.0, "median fitted mean {}", means[10]);
    }

    #[test]
    fn intervals_with_few_units() {
        // 42 units, like the states layer: the plug-in prior ignores its own
        // uncertainty, so coverage falls a little below nominal.
        let mut rng = Rng(17);
        let (mut hit, mut total) = (0, 0);
        for _ in 0..200 {
            let e: Vec<f64> = (0..42).map(|_| (5.0 + 1.5 * rng.normal()).exp()).collect();
            let lifts: Vec<f64> = e.iter().map(|_| rng.gamma(4.0) / 4.0).collect();
            let counts = draw(&mut rng, &e, &lifts, 1);
            let (_, scores) = score_all(&counts, 1.0, 0.9);
            let (h, t) = coverage(&scores, &lifts);
            hit += h;
            total += t;
        }
        let cov = hit as f64 / total as f64;
        assert!((0.86..=0.93).contains(&cov), "coverage {cov}");
    }

    #[test]
    fn ranks_by_size_of_skew_not_by_size_of_place() {
        // Big places slightly above the reference rate (1.15x), mid-size
        // places well above it (3x), the rest at the rate. A significance
        // score (z) puts the big places first; the lower credible bound puts
        // the 3x places first.
        let mut rng = Rng(3);
        let mut counts = Vec::new();
        let mut planted = Vec::new();
        for i in 0..300 {
            let (e, t) = match i {
                0..=9 => (20_000.0, 1.15),
                10..=19 => (40.0, 3.0),
                _ => ((2.0 + 1.5 * rng.normal()).exp(), 1.0),
            };
            planted.push(t);
            counts.push(Counts {
                observed: rng.poisson(e * t),
                expected: e,
            });
        }
        let (_, scores) = score_all(&counts, 1.0, 0.9);
        let top = |key: &dyn Fn(&Score) -> f64| {
            let mut idx: Vec<usize> = (0..scores.len()).collect();
            idx.sort_by(|&a, &b| key(&scores[b]).total_cmp(&key(&scores[a])));
            idx.truncate(10);
            idx
        };
        let z = |s: &Score| (s.observed as f64 - s.expected) / s.expected.sqrt();
        let by_z = top(&z);
        let by_lower = top(&|s: &Score| s.lower);
        let big = by_z.iter().filter(|&&i| planted[i] == 1.15).count();
        let strong = by_lower.iter().filter(|&&i| planted[i] == 3.0).count();
        assert!(big >= 9, "z top 10 has {big} big places");
        assert!(strong >= 9, "lower-bound top 10 has {strong} 3x places");
    }

    /// Cells for places at the reference rate whose interest drifts over
    /// time (plus or minus `drift`, one slow cycle over the window) and whose
    /// hits arrive in runs of `run` pages, over `buckets` buckets. Returns
    /// national hits, national pages and the cells.
    fn simulate_cells(
        rng: &mut Rng,
        places: usize,
        buckets: usize,
        drift: f64,
        run: u64,
    ) -> (Vec<u64>, Vec<u64>, Vec<Cell>) {
        let rate = 0.004;
        let mut cells = Vec::new();
        for p in 0..places {
            // Pages per year from about 50 to 20,000; buckets split a 20-year window.
            let per_year = (6.0 + 1.5 * rng.normal()).exp().clamp(20.0, 50_000.0);
            let pages = (per_year * 20.0 / buckets as f64).round().max(1.0) as u64;
            let phase = rng.uniform() * std::f64::consts::TAU;
            for b in 0..buckets {
                let t = (b as f64 + 0.5) / buckets as f64;
                let theta = 1.0 + drift * (std::f64::consts::TAU * t + phase).sin();
                let e = pages as f64 * rate * theta;
                let hits = (run * rng.poisson(e / run as f64)).min(pages);
                cells.push(Cell {
                    place: p,
                    bucket: b,
                    pages,
                    hits,
                });
            }
        }
        let mut hits = vec![0u64; buckets];
        let mut pages = vec![0u64; buckets];
        for c in &cells {
            hits[c.bucket] += c.hits;
            pages[c.bucket] += c.pages;
        }
        (hits, pages, cells)
    }

    /// Merge buckets `k` at a time (a coarser bucket unit over the same data).
    fn coarsen(cells: &[Cell], k: usize, buckets: usize) -> (Vec<u64>, Vec<u64>, Vec<Cell>) {
        let mut merged: std::collections::BTreeMap<(usize, usize), (u64, u64)> = Default::default();
        for c in cells {
            let e = merged.entry((c.place, c.bucket / k)).or_default();
            e.0 += c.pages;
            e.1 += c.hits;
        }
        let n = buckets.div_ceil(k);
        let (mut hits, mut pages) = (vec![0u64; n], vec![0u64; n]);
        let out: Vec<Cell> = merged
            .into_iter()
            .map(|((place, bucket), (p, h))| {
                hits[bucket] += h;
                pages[bucket] += p;
                Cell {
                    place,
                    bucket,
                    pages: p,
                    hits: h,
                }
            })
            .collect();
        (hits, pages, out)
    }

    #[test]
    fn dispersion_measures_reprinting_whatever_the_bucket() {
        let mut rng = Rng(5);
        // Independent pages, steady interest: phi near 1.
        let (h, n, cells) = simulate_cells(&mut rng, 300, 240, 0.0, 1);
        let d = dispersion(&h, &n, &cells);
        let phi = d.phi.unwrap();
        assert!((1.0..1.2).contains(&phi), "independent: phi {phi}");
        assert!(d.places > 100);
        // Items in runs of 4 and interest drifting by 30%, in 240 fine
        // buckets or 20 coarse ones: about 4 either way.
        let (h, n, cells) = simulate_cells(&mut rng, 300, 240, 0.3, 4);
        let fine = dispersion(&h, &n, &cells).phi.unwrap();
        let (h2, n2, coarse_cells) = coarsen(&cells, 12, 240);
        let coarse = dispersion(&h2, &n2, &coarse_cells).phi.unwrap();
        assert!((3.2..5.0).contains(&fine), "fine phi {fine}");
        assert!((3.2..5.0).contains(&coarse), "coarse phi {coarse}");
        assert!(
            (fine / coarse - 1.0).abs() < 0.15,
            "fine {fine} coarse {coarse}"
        );
        // Nothing to measure.
        assert_eq!(
            dispersion(&h, &n, &[]),
            Dispersion {
                phi: None,
                places: 0
            }
        );
    }

    #[test]
    fn median_averages_the_middle_pair() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn dispersion_for_ignores_the_bucket_unit() {
        use crate::time::BucketUnit;
        let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let mut rng = Rng(23);
        // 20 years by month, and the same data by year.
        let (h, n, cells) = simulate_cells(&mut rng, 200, 240, 0.3, 3);
        let months = BucketSpec::new(BucketUnit::Month, day("1880-01-01"), day("1899-12-31"));
        let years = BucketSpec::new(BucketUnit::Year, day("1880-01-01"), day("1899-12-31"));
        assert_eq!(months.len(), 240);
        let (h2, n2, yearly) = coarsen(&cells, 12, 240);
        let (a, phi_a) = dispersion_for(&months, &h, &n, &cells);
        let (b, phi_b) = dispersion_for(&years, &h2, &n2, &yearly);
        assert_eq!(a, b);
        assert_eq!(phi_a, phi_b);
        assert!((2.4..4.0).contains(&phi_a), "phi {phi_a}");
        // Exactly three years (inclusive end) is measured by year: the same
        // cells by month and by year give the same result.
        let three = BucketSpec::new(BucketUnit::Month, day("1880-01-01"), day("1882-12-31"));
        let three_years = BucketSpec::new(BucketUnit::Year, day("1880-01-01"), day("1882-12-31"));
        let (h3, n3, c3) = simulate_cells(&mut rng, 200, 36, 0.3, 3);
        let (h3y, n3y, c3y) = coarsen(&c3, 12, 36);
        assert_eq!(
            dispersion_for(&three, &h3, &n3, &c3),
            dispersion_for(&three_years, &h3y, &n3y, &c3y)
        );
        // A window under three years is measured by month; nothing
        // measurable falls back.
        let short = BucketSpec::new(BucketUnit::Week, day("1896-06-01"), day("1896-12-31"));
        let (d, phi) = dispersion_for(&short, &[0; 31], &[0; 31], &[]);
        assert_eq!((d.places, phi), (0, FALLBACK_PHI));
    }

    #[test]
    fn reprinting_with_real_differences_stays_calibrated_once_dispersion_is_used() {
        // Places differ (lifts from Gamma(4, 4)) and items run 6 pages at a
        // time. Scored with phi = 6; measuring phi is tested above.
        let mut rng = Rng(9);
        let (mut poisson_hit, mut adjusted_hit, mut total) = (0, 0, 0);
        for _ in 0..10 {
            let e = exposures(&mut rng, 300);
            let lifts: Vec<f64> = e.iter().map(|_| rng.gamma(4.0) / 4.0).collect();
            let counts = draw(&mut rng, &e, &lifts, 6);
            let (_, plain) = score_all(&counts, 1.0, 0.9);
            let (_, adjusted) = score_all(&counts, 6.0, 0.9);
            poisson_hit += coverage(&plain, &lifts).0;
            adjusted_hit += coverage(&adjusted, &lifts).0;
            total += lifts.len();
        }
        let (plain, adjusted) = (
            poisson_hit as f64 / total as f64,
            adjusted_hit as f64 / total as f64,
        );
        assert!(plain < 0.75, "poisson coverage {plain}");
        assert!(
            (0.86..=0.94).contains(&adjusted),
            "adjusted coverage {adjusted}"
        );
    }

    #[test]
    fn reprinting_alone_is_not_skew_once_dispersion_is_used() {
        // No place differs, items run 6 pages at a time.
        let mut rng = Rng(21);
        let (h, n, cells) = simulate_cells(&mut rng, 300, 20, 0.0, 6);
        let counts = place_counts(&h, &n, &cells, 300);
        let phi = dispersion(&h, &n, &cells).phi.unwrap();
        let flagged = |phi: f64| {
            let (_, scores) = score_all(&counts, phi, 0.9);
            scores.iter().filter(|s| s.direction() != 0).count()
        };
        let (poisson, adjusted) = (flagged(1.0), flagged(phi));
        assert!((4.5..7.5).contains(&phi), "phi {phi}");
        assert!(poisson >= 50, "poisson flags {poisson}");
        assert!(adjusted <= 6, "adjusted flags {adjusted}");
    }

    #[test]
    fn few_hits_and_no_difference_flags_nothing() {
        // 12 places at the reference rate with 17 hits between them: the
        // fitted mean is 0.87 by chance and alpha reaches its bound. Without
        // the mean's own uncertainty every place's interval is about 0.872
        // to 0.875, and all 12 would be flagged below 1.
        let counts: Vec<Counts> = [
            (5, 4.51),
            (3, 1.13),
            (0, 0.48),
            (1, 1.41),
            (1, 0.51),
            (3, 1.37),
            (2, 2.11),
            (0, 0.14),
            (1, 2.85),
            (1, 0.73),
            (0, 0.63),
            (0, 3.6),
        ]
        .iter()
        .map(|&(observed, expected)| Counts { observed, expected })
        .collect();
        let (prior, scores) = score_all(&counts, 4.0, 0.9);
        assert!(prior.alpha > 1e5, "alpha {}", prior.alpha);
        assert!(close(prior.mean, 17.0 / 19.47, 1e-3), "mean {}", prior.mean);
        // About 17 / 4 independent hits inform the mean.
        assert!(
            close(prior.mean_var, 4.0 / 17.0, 0.01),
            "{}",
            prior.mean_var
        );
        assert!(prior.shape() < 5.0);
        assert!(scores.iter().all(|s| s.direction() == 0));
    }

    #[test]
    fn places_left_out_of_the_fit_are_still_scored() {
        use crate::time::BucketUnit;
        let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let spec = BucketSpec::new(BucketUnit::Year, day("1880-01-01"), day("1880-12-31"));
        // Ten places at the reference rate, one (a paper in another
        // language) with almost no hits.
        let mut cells: Vec<Cell> = (0..10)
            .map(|p| Cell {
                place: p,
                bucket: 0,
                pages: 10_000,
                hits: 100 + p as u64,
            })
            .collect();
        cells.push(Cell {
            place: 10,
            bucket: 0,
            pages: 10_000,
            hits: 1,
        });
        let hits = [cells.iter().map(|c| c.hits).sum::<u64>()];
        let pages = [cells.iter().map(|c| c.pages).sum::<u64>()];
        let all = score_search(&spec, &hits, &pages, &cells, 11, &[], 0.9);
        let mut in_fit = vec![true; 11];
        in_fit[10] = false;
        let some = score_search(&spec, &hits, &pages, &cells, 11, &in_fit, 0.9);
        // Without it the places barely differ, so the prior is much tighter.
        assert!(some.prior.alpha > 10.0 * all.prior.alpha);
        // It is still scored, with the prior of the places it was left out
        // of, so it is pulled toward them more than when it was fitted.
        assert_eq!(some.scores.len(), 11);
        assert!(some.scores[10].estimate.is_finite());
        assert!(some.scores[10].estimate > all.scores[10].estimate);
        assert_eq!(some.scores[10].observed, 1);
        assert_eq!(some.phi, all.phi);
    }

    #[test]
    fn groups_are_compared_with_the_other_groups() {
        // Places 0 and 1 form group 0, place 2 group 1, in two buckets.
        let cells = [
            Cell {
                place: 0,
                bucket: 0,
                pages: 100,
                hits: 10,
            },
            Cell {
                place: 1,
                bucket: 0,
                pages: 100,
                hits: 30,
            },
            Cell {
                place: 1,
                bucket: 1,
                pages: 50,
                hits: 5,
            },
            Cell {
                place: 2,
                bucket: 0,
                pages: 400,
                hits: 20,
            },
            Cell {
                place: 2,
                bucket: 1,
                pages: 50,
                hits: 1,
            },
        ];
        let grouped = group_cells(&cells, &[0, 0, 1]);
        assert_eq!(grouped.len(), 4);
        assert_eq!((grouped[0].pages, grouped[0].hits), (200, 40));
        let c = place_counts(&[60, 6], &[600, 100], &grouped, 2);
        // Group 0 against group 1's 5% and 2%; group 1 against group 0's 20% and 10%.
        assert!(close(c[0].expected, 200.0 * 0.05 + 50.0 * 0.02, 1e-12));
        assert!(close(c[1].expected, 400.0 * 0.2 + 50.0 * 0.1, 1e-12));
        let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let spec = BucketSpec::new(
            crate::time::BucketUnit::Year,
            day("1880-01-01"),
            day("1881-12-31"),
        );
        let g = score_groups(
            &spec,
            &[60, 6],
            &[600, 100],
            &cells,
            &[0, 0, 1],
            2,
            1.5,
            0.9,
        );
        assert_eq!(g.scores[0].observed, 45);
        assert_eq!(g.scores[1].observed, 21);
        // Too few hits to measure the groups' own dispersion: the places' is used.
        assert_eq!((g.dispersion.phi, g.phi), (None, 1.5));
    }

    #[test]
    fn groups_use_their_own_dispersion_when_it_is_larger() {
        use crate::time::BucketUnit;
        let day = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        // Ten places in two groups, each place steady, but the groups'
        // interest swings in opposite directions from year to year.
        let spec = BucketSpec::new(BucketUnit::Year, day("1880-01-01"), day("1889-12-31"));
        let mut cells = Vec::new();
        for p in 0..10 {
            for b in 0..10 {
                let up = (b % 2 == 0) == (p < 5);
                cells.push(Cell {
                    place: p,
                    bucket: b,
                    pages: 10_000,
                    hits: if up { 130 } else { 70 },
                });
            }
        }
        let mut hits = vec![0u64; 10];
        let mut pages = vec![0u64; 10];
        for c in &cells {
            hits[c.bucket] += c.hits;
            pages[c.bucket] += c.pages;
        }
        let group_of: Vec<usize> = (0..10).map(|p| usize::from(p >= 5)).collect();
        let g = score_groups(&spec, &hits, &pages, &cells, &group_of, 2, 1.0, 0.9);
        assert!(g.phi > 10.0, "phi {}", g.phi);
        assert_eq!(Some(g.phi), g.dispersion.phi);
    }

    #[test]
    fn degenerate_inputs() {
        let fitted = Prior::fit(&[], 1.0);
        assert_eq!((fitted.alpha, fitted.mean), (MAX_ALPHA, 1.0));
        let zero = Counts {
            observed: 0,
            expected: 0.0,
        };
        assert_eq!(Prior::fit(&[zero], 1.0).alpha, MAX_ALPHA);
        let s = Prior {
            alpha: 2.0,
            mean: 1.0,
            mean_var: 0.0,
        }
        .score(zero, 1.0, 0.9);
        assert_eq!(s.lift, None);
        assert!(s.lower < 1.0 && s.upper > 1.0);
        let lone = Cell {
            place: 0,
            bucket: 0,
            pages: 5,
            hits: 5,
        };
        assert_eq!(reference_rate(&[5], &[5], &lone), None);
        assert_eq!(
            reference_rate(&[5], &[5], &Cell { bucket: 3, ..lone }),
            None
        );
    }
}
