// Geographic skew (docs/design/11-term-geographic-skew.md): does a place
// print a search term more or less often than the other places did over the
// same dates? A port of crates/usnm-core/src/skew.rs, the reference
// implementation. fixtures/skew-vectors.json holds inputs and the scores the
// Rust gives them; skew.test.ts checks this file against it, so the two
// can't drift. Keep the arithmetic in the same order as the Rust.

import type { BucketUnit } from "../api/types";
import { bucketStart, dateFromDay, dayNumber } from "../lib/time";

/** Pages published and pages that matched, per (place, bucket), as parallel arrays. */
export interface Cells {
  p: ArrayLike<number>;
  b: ArrayLike<number>;
  pages: ArrayLike<number>;
  hits: ArrayLike<number>;
}

export interface Counts {
  observed: number;
  expected: number;
}

export interface Dispersion {
  /** null when no place had enough expected hits to measure it. */
  phi: number | null;
  places: number;
}

/** Gamma prior on the lift: mean `mean`, shape `alpha`; `meanVar` is the variance of ln(mean). */
export interface Prior {
  alpha: number;
  mean: number;
  meanVar: number;
}

export interface Score {
  observed: number;
  expected: number;
  /** observed / expected, unshrunk; null when nothing was expected. */
  lift: number | null;
  /** Posterior median of the lift. */
  estimate: number;
  lower: number;
  upper: number;
  /** Posterior probability that the lift is above 1. */
  above: number;
}

export interface BucketSpec {
  unit: BucketUnit;
  from: string;
  to: string;
}

export const ZERO_REFERENCE_HITS = 0.5;
export const CHUNK_EXPECTED = 5;
export const MIN_CHUNKS = 3;
export const FALLBACK_PHI = 4;
export const MIN_ALPHA = 1e-3;
export const MAX_ALPHA = 1e6;
const MIN_MEAN = 1e-2;
const MAX_MEAN = 1e2;
const LARGE_SHAPE = 1e4;
/** f64::MIN_POSITIVE: the smallest normal double (Number.MIN_VALUE is subnormal). */
const MIN_POSITIVE = 2.2250738585072014e-308;

/** 1 above, -1 below, 0 when the interval includes 1. */
export function direction(s: Score): -1 | 0 | 1 {
  return s.lower > 1 ? 1 : s.upper < 1 ? -1 : 0;
}

/**
 * The share of the other places' pages that matched in the bucket; null when
 * the place published every page of it (or the bucket is out of range). When
 * the others have no hits but this place has some, they are credited with
 * ZERO_REFERENCE_HITS.
 */
export function referenceRate(
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  bucket: number,
  pages: number,
  hits: number,
): number | null {
  if (bucket >= nationalHits.length || bucket >= nationalPages.length) return null;
  const h = nationalHits[bucket]!;
  const n = nationalPages[bucket]!;
  if (n < pages) return null;
  const otherPages = n - pages;
  if (otherPages <= 0) return null;
  let otherHits = Math.max(h - hits, 0);
  if (otherHits === 0 && hits > 0) otherHits = ZERO_REFERENCE_HITS;
  return otherHits / otherPages;
}

/** Observed and expected hits per place, each compared with the others in the same buckets. */
export function placeCounts(
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
  places: number,
): Counts[] {
  const out: Counts[] = Array.from({ length: places }, () => ({ observed: 0, expected: 0 }));
  for (let i = 0; i < cells.p.length; i++) {
    const slot = out[cells.p[i]!];
    if (!slot) continue;
    const rate = referenceRate(nationalHits, nationalPages, cells.b[i]!, cells.pages[i]!, cells.hits[i]!);
    if (rate === null) continue;
    slot.observed += cells.hits[i]!;
    slot.expected += cells.pages[i]! * rate;
  }
  return out;
}

function median(v: number[]): number {
  v.sort((a, b) => a - b);
  const n = v.length;
  return n % 2 === 1 ? v[(n - 1) / 2]! : (v[n / 2 - 1]! + v[n / 2]!) / 2;
}

/** Cell indexes sorted by (place, bucket). */
function order(cells: Cells): number[] {
  const idx = Array.from({ length: cells.p.length }, (_, i) => i);
  idx.sort((a, b) => cells.p[a]! - cells.p[b]! || cells.b[a]! - cells.b[b]! || a - b);
  return idx;
}

/** Over-dispersion of the hits: the median per-place Pearson chi-square per degree of freedom. */
export function dispersion(
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
): Dispersion {
  const sorted = order(cells);
  const perPlace: number[] = [];
  let i = 0;
  while (i < sorted.length) {
    const place = cells.p[sorted[i]!]!;
    let j = i;
    const ro: number[] = [];
    const re: number[] = [];
    const rr: number[] = [];
    while (j < sorted.length && cells.p[sorted[j]!] === place) {
      const k = sorted[j]!;
      const r = referenceRate(nationalHits, nationalPages, cells.b[k]!, cells.pages[k]!, cells.hits[k]!);
      if (r !== null && r > 0) {
        ro.push(cells.hits[k]!);
        re.push(cells.pages[k]! * r);
        rr.push(r);
      }
      j++;
    }
    i = j;
    let o = 0;
    let e = 0;
    for (let k = 0; k < ro.length; k++) {
      o += ro[k]!;
      e += re[k]!;
    }
    if (o <= 0 || e <= 0) continue;
    const theta = o / e;
    const chunks: [number, number, number][] = [];
    let cur: [number, number, number] = [0, 0, 0];
    for (let k = 0; k < ro.length; k++) {
      const mean = theta * re[k]!;
      cur[0] += ro[k]!;
      cur[1] += mean;
      cur[2] += mean * Math.max(1 - Math.min(theta * rr[k]!, 1), 1e-3);
      if (cur[1] >= CHUNK_EXPECTED) {
        chunks.push(cur);
        cur = [0, 0, 0];
      }
    }
    if (cur[1] > 0) {
      const last = chunks[chunks.length - 1];
      if (last) {
        last[0] += cur[0];
        last[1] += cur[1];
        last[2] += cur[2];
      } else {
        chunks.push(cur);
      }
    }
    if (chunks.length < MIN_CHUNKS) continue;
    let chi = 0;
    for (const [co, m, v] of chunks) chi += (co - m) ** 2 / v;
    perPlace.push(chi / (chunks.length - 1));
  }
  const places = perPlace.length;
  if (places === 0) return { phi: null, places };
  return { phi: Math.max(median(perPlace), 1), places };
}

/** chrono's `date + Months(n)`: the day is clamped to the end of a shorter month. */
function addMonths(iso: string, n: number): string {
  const [y, m, d] = iso.split("-").map(Number) as [number, number, number];
  const ym = y * 12 + (m - 1) + n;
  const ny = Math.floor(ym / 12);
  const nm = (ym % 12) + 1;
  const last = new Date(Date.UTC(ny, nm, 0)).getUTCDate();
  return `${String(ny).padStart(4, "0")}-${String(nm).padStart(2, "0")}-${String(Math.min(d, last)).padStart(2, "0")}`;
}

/**
 * dispersion() at a resolution that doesn't depend on the bucket unit:
 * calendar years when the window spans three years or more, calendar months
 * otherwise. Each bucket goes to the period of its first day. Returns the
 * dispersion and the phi to use (FALLBACK_PHI when none could be measured).
 */
export function dispersionFor(
  spec: BucketSpec,
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
): { dispersion: Dispersion; phi: number } {
  // `to` is inclusive: 1880-01-01 to 1882-12-31 is three years.
  const byYear = dateFromDay(dayNumber(spec.to) + 1) >= addMonths(spec.from, 36);
  const n = Math.min(nationalHits.length, nationalPages.length);
  const keyOf = new Int32Array(n);
  for (let b = 0; b < n; b++) {
    const iso = bucketStart(spec.unit, spec.from, b);
    const y = Number(iso.slice(0, 4));
    keyOf[b] = byYear ? y : y * 12 + Number(iso.slice(5, 7)) - 1;
  }
  // Keys rise with the bucket, so slots are numbered in order.
  const slot = new Map<number, number>();
  for (let b = 0; b < n; b++) if (!slot.has(keyOf[b]!)) slot.set(keyOf[b]!, slot.size);
  const hits = new Array<number>(slot.size).fill(0);
  const pages = new Array<number>(slot.size).fill(0);
  for (let b = 0; b < n; b++) {
    const s = slot.get(keyOf[b]!)!;
    hits[s]! += nationalHits[b]!;
    pages[s]! += nationalPages[b]!;
  }
  const merged = new Map<number, [number, number, number, number]>();
  const stride = slot.size;
  for (let i = 0; i < cells.p.length; i++) {
    const b = cells.b[i]!;
    if (b >= n) continue;
    const s = slot.get(keyOf[b]!)!;
    const key = cells.p[i]! * stride + s;
    const e = merged.get(key);
    if (e) {
      e[2] += cells.pages[i]!;
      e[3] += cells.hits[i]!;
    } else {
      merged.set(key, [cells.p[i]!, s, cells.pages[i]!, cells.hits[i]!]);
    }
  }
  const rows = [...merged.values()];
  const d = dispersion(hits, pages, {
    p: rows.map((r) => r[0]),
    b: rows.map((r) => r[1]),
    pages: rows.map((r) => r[2]),
    hits: rows.map((r) => r[3]),
  });
  return { dispersion: d, phi: d.phi ?? FALLBACK_PHI };
}

/** The shape places are scored with: alpha widened by the uncertainty in the fitted mean. */
export function priorShape(p: Prior): number {
  return 1 / (1 / p.alpha + p.meanVar * (1 + 1 / p.alpha));
}

function goldenMax(lo: number, hi: number, f: (x: number) => number): number {
  const g = (Math.sqrt(5) - 1) / 2;
  let a = hi - g * (hi - lo);
  let b = lo + g * (hi - lo);
  let fa = f(a);
  let fb = f(b);
  for (let k = 0; k < 200; k++) {
    if (hi - lo < 1e-7) break;
    if (fa < fb) {
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
  return (lo + hi) / 2;
}

function nbLogLikelihood(o: Float64Array, e: Float64Array, alpha: number, mean: number): number {
  const lgAlpha = lnGamma(alpha);
  let sum = 0;
  for (let i = 0; i < o.length; i++) {
    const oi = o[i]!;
    const m = mean * e[i]!;
    sum +=
      lnGamma(oi + alpha) -
      lgAlpha -
      lnGamma(oi + 1) -
      alpha * Math.log1p(m / alpha) +
      (oi > 0 ? oi * (Math.log(m) - Math.log(alpha + m)) : 0);
  }
  return sum;
}

/** Fit alpha and the mean by maximum negative binomial marginal likelihood (Prior::fit). */
export function fitPrior(counts: Counts[], phiIn: number): Prior {
  const phi = Math.max(phiIn, 1);
  const usable = counts.filter((c) => c.expected > 0 && Number.isFinite(c.expected));
  if (usable.length === 0) return { alpha: MAX_ALPHA, mean: 1, meanVar: 0 };
  const o = Float64Array.from(usable, (c) => c.observed / phi);
  const e = Float64Array.from(usable, (c) => c.expected / phi);
  let so = 0;
  let se = 0;
  for (let i = 0; i < o.length; i++) {
    so += o[i]!;
    se += e[i]!;
  }
  let lnMean = Math.log(Math.min(Math.max(so / se, MIN_MEAN), MAX_MEAN));
  let lnAlpha = 0;
  for (let k = 0; k < 30; k++) {
    const before = [lnAlpha, lnMean] as const;
    lnAlpha = goldenMax(Math.log(MIN_ALPHA), Math.log(MAX_ALPHA), (a) => nbLogLikelihood(o, e, Math.exp(a), Math.exp(lnMean)));
    lnMean = goldenMax(Math.log(MIN_MEAN), Math.log(MAX_MEAN), (m) => nbLogLikelihood(o, e, Math.exp(lnAlpha), Math.exp(m)));
    if (Math.abs(lnAlpha - before[0]) < 1e-5 && Math.abs(lnMean - before[1]) < 1e-7) break;
  }
  const alpha = Math.min(Math.max(Math.exp(lnAlpha), MIN_ALPHA), MAX_ALPHA);
  const mean = Math.min(Math.max(Math.exp(lnMean), MIN_MEAN), MAX_MEAN);
  let info = 0;
  for (let i = 0; i < e.length; i++) {
    const m = mean * e[i]!;
    info += (alpha * m) / (alpha + m);
  }
  return { alpha, mean, meanVar: info > 0 ? 1 / info : 0 };
}

/** One place's posterior: median, central interval at `level`, P(lift > 1). */
export function scorePlace(prior: Prior, c: Counts, phiIn: number, level: number): Score {
  const phi = Math.max(phiIn, 1);
  const a = priorShape(prior);
  const shape = a + c.observed / phi;
  const rate = a / prior.mean + Math.max(c.expected, 0) / phi;
  const tail = (1 - Math.min(Math.max(level, 0), 1)) / 2;
  return {
    observed: c.observed,
    expected: c.expected,
    lift: c.expected > 0 ? c.observed / c.expected : null,
    estimate: gammaQuantile(0.5, shape) / rate,
    lower: gammaQuantile(tail, shape) / rate,
    upper: gammaQuantile(1 - tail, shape) / rate,
    above: 1 - gammaP(shape, rate),
  };
}

export function scoreAll(counts: Counts[], phi: number, level: number): { prior: Prior; scores: Score[] } {
  const prior = fitPrior(counts, phi);
  return { prior, scores: counts.map((c) => scorePlace(prior, c, phi, level)) };
}

export interface SearchScores {
  dispersion: Dispersion;
  phi: number;
  prior: Prior;
  scores: Score[];
}

/** Every step for one search (score_search): places with inFit false are scored but left out of the fit. */
export function scoreSearch(
  spec: BucketSpec,
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
  places: number,
  inFit: ArrayLike<boolean>,
  level: number,
): SearchScores {
  const counts = placeCounts(nationalHits, nationalPages, cells, places);
  const { dispersion: d, phi } = dispersionFor(spec, nationalHits, nationalPages, cells);
  const prior = fitPrior(
    counts.filter((_, i) => i >= inFit.length || inFit[i]),
    phi,
  );
  return { dispersion: d, phi, prior, scores: counts.map((c) => scorePlace(prior, c, phi, level)) };
}

/** Cells summed per group and bucket, sorted by group and bucket (group_cells). */
export function groupCells(cells: Cells, groupOf: ArrayLike<number>): Cells {
  let stride = 1;
  for (let i = 0; i < cells.b.length; i++) stride = Math.max(stride, cells.b[i]! + 1);
  const merged = new Map<number, [number, number, number, number]>();
  for (let i = 0; i < cells.p.length; i++) {
    const p = cells.p[i]!;
    if (p >= groupOf.length) continue;
    const g = groupOf[p]!;
    const key = g * stride + cells.b[i]!;
    const e = merged.get(key);
    if (e) {
      e[2] += cells.pages[i]!;
      e[3] += cells.hits[i]!;
    } else {
      merged.set(key, [g, cells.b[i]!, cells.pages[i]!, cells.hits[i]!]);
    }
  }
  const rows = [...merged.values()].sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  return {
    p: rows.map((r) => r[0]),
    b: rows.map((r) => r[1]),
    pages: rows.map((r) => r[2]),
    hits: rows.map((r) => r[3]),
  };
}

/** Groups (states) scored as units with the place-level phi and their own prior (score_groups). */
export function scoreGroups(
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
  groupOf: ArrayLike<number>,
  groups: number,
  phi: number,
  level: number,
): { prior: Prior; scores: Score[] } {
  const counts = placeCounts(nationalHits, nationalPages, groupCells(cells, groupOf), groups);
  return scoreAll(counts, phi, level);
}

// Special functions (the same algorithms and constants as the Rust).

const LANCZOS = [
  0.9999999999998099, 676.5203681218851, -1259.1392167224028, 771.3234287776531, -176.6150291621406,
  12.507343278686905, -0.13857109526572012, 9.984369578019572e-6, 1.5056327351493116e-7,
];

/** ln Gamma(x) for x > 0 (Lanczos, g = 7, n = 9). */
export function lnGamma(x: number): number {
  if (x < 0.5) return Math.log(Math.PI / Math.sin(Math.PI * x)) - lnGamma(1 - x);
  const y = x - 1;
  let sum = LANCZOS[0]!;
  for (let i = 1; i < 9; i++) sum += LANCZOS[i]! / (y + i);
  const t = y + 7 + 0.5;
  return 0.5 * Math.log(2 * Math.PI) + (y + 0.5) * Math.log(t) - t + Math.log(sum);
}

/** Regularized lower incomplete gamma function P(a, x). */
export function gammaP(a: number, x: number): number {
  if (x <= 0) return 0;
  const front = Math.exp(-x + a * Math.log(x) - lnGamma(a));
  const limit = 1000 + Math.floor(50 * Math.sqrt(a));
  if (x < a + 1) {
    let ap = a;
    let sum = 1 / a;
    let del = 1 / a;
    for (let k = 0; k < limit; k++) {
      ap += 1;
      del *= x / ap;
      sum += del;
      if (Math.abs(del) < Math.abs(sum) * 1e-16) break;
    }
    return Math.min(Math.max(sum * front, 0), 1);
  }
  const tiny = 1e-300;
  let b = x + 1 - a;
  let c = 1 / tiny;
  let d = 1 / b;
  let h = d;
  for (let i = 1; i < limit; i++) {
    const an = -i * (i - a);
    b += 2;
    d = an * d + b;
    if (Math.abs(d) < tiny) d = tiny;
    c = b + an / c;
    if (Math.abs(c) < tiny) c = tiny;
    d = 1 / d;
    const delta = d * c;
    h *= delta;
    if (Math.abs(delta - 1) < 1e-16) break;
  }
  return Math.min(Math.max(1 - front * h, 0), 1);
}

const NA = [-3.969683028665376e1, 2.209460984245205e2, -2.759285104469687e2, 1.38357751867269e2, -3.066479806614716e1, 2.506628277459239];
const NB = [-5.447609879822406e1, 1.615858368580409e2, -1.556989798598866e2, 6.680131188771972e1, -1.328068155288572e1];
const NC = [-7.784894002430293e-3, -3.223964580411365e-1, -2.400758277161838, -2.549732539343734, 4.374664141464968, 2.938163982698783];
const ND = [7.784695709041462e-3, 3.224671290700398e-1, 2.445134137142996, 3.754408661907416];

/** Inverse of the standard normal CDF (Acklam's rational approximation). */
export function normalQuantile(p: number): number {
  const tail = (q: number) =>
    (((((NC[0]! * q + NC[1]!) * q + NC[2]!) * q + NC[3]!) * q + NC[4]!) * q + NC[5]!) /
    ((((ND[0]! * q + ND[1]!) * q + ND[2]!) * q + ND[3]!) * q + 1);
  if (p < 0.02425) return tail(Math.sqrt(-2 * Math.log(p)));
  if (p > 1 - 0.02425) return -tail(Math.sqrt(-2 * Math.log(1 - p)));
  const q = p - 0.5;
  const r = q * q;
  return (
    ((((((NA[0]! * r + NA[1]!) * r + NA[2]!) * r + NA[3]!) * r + NA[4]!) * r + NA[5]!) * q) /
    (((((NB[0]! * r + NB[1]!) * r + NB[2]!) * r + NB[3]!) * r + NB[4]!) * r + 1)
  );
}

function wilsonHilferty(p: number, shape: number): number {
  const v = 1 / (9 * shape);
  return shape * (1 - v + normalQuantile(p) * Math.sqrt(v)) ** 3;
}

/** The p quantile of Gamma(shape, rate 1): Newton inside a bisection bracket. */
export function gammaQuantile(p: number, shape: number): number {
  if (p <= 0) return 0;
  if (p >= 1) return Infinity;
  if (shape > LARGE_SHAPE) return wilsonHilferty(p, shape);
  const lnG = lnGamma(shape);
  let lo = 0;
  let hi = Math.max(shape, 1);
  while (gammaP(shape, hi) < p) {
    lo = hi;
    hi *= 2;
  }
  let x: number;
  if (shape >= 1) {
    x = wilsonHilferty(p, shape);
  } else {
    x = Math.exp((Math.log(p) + lnGamma(shape + 1)) / shape);
    if (x < MIN_POSITIVE) return x;
  }
  if (!(x > lo && x < hi)) x = (lo + hi) / 2;
  for (let k = 0; k < 200; k++) {
    const f = gammaP(shape, x) - p;
    if (f < 0) lo = x;
    else hi = x;
    const pdf = Math.exp((shape - 1) * Math.log(x) - x - lnG);
    let next = x - f / pdf;
    if (!(next > lo && next < hi)) next = (lo + hi) / 2;
    if (Math.abs(next - x) <= 1e-13 * x || hi - lo <= 1e-13 * hi) return next;
    x = next;
  }
  return x;
}
