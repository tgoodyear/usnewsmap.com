// The relative-rate view's model (doc 11, 11.6): fit once per search, then
// score any playback frame from prefix sums in O(places). phi and the priors
// stay at their full-window values, so colours don't jump because of a refit.

import {
  dispersionFor,
  fitPrior,
  groupCells,
  groupPhi,
  referenceRate,
  scorePlace,
  type BucketSpec,
  type Cells,
  type Counts,
  type Dispersion,
  type Prior,
  type Score,
} from "./skew";

export interface SkewInput {
  spec: BucketSpec;
  nationalHits: ArrayLike<number>;
  nationalPages: ArrayLike<number>;
  cells: Cells;
  places: number;
  /** false leaves a place out of the prior fit (its titles are all in other languages). */
  inFit: ArrayLike<boolean>;
  stateOf: ArrayLike<number>;
  states: number;
  /** Credible level of the interval, e.g. 0.9. */
  level: number;
}

/**
 * Per-unit running totals, stored sparsely (one entry per cell, not per
 * bucket), so memory follows the cells rather than places × buckets. Unit
 * u's entries are `start[u]` to `start[u + 1]`, in bucket order; each holds
 * the totals over buckets up to and including its own.
 */
export interface UnitSums {
  units: number;
  buckets: number;
  start: Int32Array;
  bucket: Int32Array;
  /** Hits in buckets that have a reference (another unit published). */
  observed: Float64Array;
  /** Hits expected at the other units' rate. */
  expected: Float64Array;
  /** Pages published, every bucket. */
  pages: Float64Array;
}

export interface SkewModel {
  buckets: number;
  level: number;
  dispersion: Dispersion;
  phi: number;
  prior: Prior;
  /** States' phi: the larger of the places' and their own. */
  statePhi: number;
  statePrior: Prior;
  places: UnitSums;
  states: UnitSums;
}

export function unitSums(
  nationalHits: ArrayLike<number>,
  nationalPages: ArrayLike<number>,
  cells: Cells,
  units: number,
  buckets: number,
): UnitSums {
  const idx: number[] = [];
  for (let i = 0; i < cells.p.length; i++)
    if (cells.p[i]! < units && cells.b[i]! < buckets && cells.p[i]! >= 0) idx.push(i);
  idx.sort((a, b) => cells.p[a]! - cells.p[b]! || cells.b[a]! - cells.b[b]! || a - b);
  const start = new Int32Array(units + 1);
  const bucket = new Int32Array(idx.length);
  const observed = new Float64Array(idx.length);
  const expected = new Float64Array(idx.length);
  const pages = new Float64Array(idx.length);
  let n = 0;
  let unit = -1;
  for (const i of idx) {
    const u = cells.p[i]!;
    const b = cells.b[i]!;
    const fresh = u !== unit || bucket[n - 1] !== b;
    if (u !== unit) {
      for (let k = unit + 1; k <= u; k++) start[k] = n;
      unit = u;
    }
    if (fresh) {
      bucket[n] = b;
      const prev = n > start[u]! ? n - 1 : -1;
      observed[n] = prev >= 0 ? observed[prev]! : 0;
      expected[n] = prev >= 0 ? expected[prev]! : 0;
      pages[n] = prev >= 0 ? pages[prev]! : 0;
      n++;
    }
    const at = n - 1;
    pages[at]! += cells.pages[i]!;
    const rate = referenceRate(nationalHits, nationalPages, b, cells.pages[i]!, cells.hits[i]!);
    if (rate === null) continue;
    observed[at]! += cells.hits[i]!;
    expected[at]! += cells.pages[i]! * rate;
  }
  for (let k = unit + 1; k <= units; k++) start[k] = n;
  return {
    units,
    buckets,
    start,
    bucket: bucket.slice(0, n),
    observed: observed.slice(0, n),
    expected: expected.slice(0, n),
    pages: pages.slice(0, n),
  };
}

/** Index of unit u's last entry in a bucket before `b`, or -1. */
function before(s: UnitSums, u: number, b: number): number {
  let lo = s.start[u]!;
  let hi = s.start[u + 1]!;
  // First entry with bucket >= b.
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (s.bucket[mid]! < b) lo = mid + 1;
    else hi = mid;
  }
  return lo > s.start[u]! ? lo - 1 : -1;
}

const at = (a: Float64Array, k: number) => (k < 0 ? 0 : a[k]!);

/** Bucket range [lo, hi) of the frame at `t`: cumulative, or the trailing `win` buckets. */
function range(buckets: number, t: number, win: number | null): [number, number] {
  const hi = Math.min(Math.max(t, 0), buckets - 1) + 1;
  return [win === null ? 0 : Math.max(0, hi - win), hi];
}

export function windowCounts(s: UnitSums, t: number, win: number | null): Counts[] {
  const [lo, hi] = range(s.buckets, t, win);
  const out: Counts[] = new Array(s.units);
  for (let u = 0; u < s.units; u++) {
    const a = lo === 0 ? -1 : before(s, u, lo);
    const b = before(s, u, hi);
    out[u] = { observed: at(s.observed, b) - at(s.observed, a), expected: at(s.expected, b) - at(s.expected, a) };
  }
  return out;
}

export function windowPages(s: UnitSums, t: number, win: number | null): Float64Array {
  const [lo, hi] = range(s.buckets, t, win);
  const out = new Float64Array(s.units);
  for (let u = 0; u < s.units; u++) {
    out[u] = at(s.pages, before(s, u, hi)) - (lo === 0 ? 0 : at(s.pages, before(s, u, lo)));
  }
  return out;
}

/** A unit's expected hits over the whole search. */
export function totalExpected(s: UnitSums, u: number): number {
  const end = s.start[u + 1]!;
  return end > s.start[u]! ? s.expected[end - 1]! : 0;
}

/**
 * The largest expected count the playback will reach, so circles keep one
 * scale: the full window when cumulative, else the biggest trailing window.
 * A trailing window's total is largest when it ends on a cell, so only those
 * ends are tried; O(cells).
 */
export function maxWindowExpected(s: UnitSums, win: number | null): number {
  let max = 1e-9;
  for (let u = 0; u < s.units; u++) {
    if (win === null) {
      max = Math.max(max, totalExpected(s, u));
      continue;
    }
    let j = s.start[u]!;
    for (let k = s.start[u]!; k < s.start[u + 1]!; k++) {
      // Entries before j end before this window starts.
      while (s.bucket[j]! <= s.bucket[k]! - win) j++;
      max = Math.max(max, s.expected[k]! - (j > s.start[u]! ? s.expected[j - 1]! : 0));
    }
  }
  return max;
}

/** Fit the search: dispersion, the place and state priors, and the prefix sums for playback. */
export function buildSkew(input: SkewInput): SkewModel {
  const { spec, nationalHits, nationalPages, cells, places, inFit, stateOf, states, level } = input;
  const buckets = Math.min(nationalHits.length, nationalPages.length);
  const placeSums = unitSums(nationalHits, nationalPages, cells, places, buckets);
  const { dispersion, phi } = dispersionFor(spec, nationalHits, nationalPages, cells);
  const counts = windowCounts(placeSums, buckets - 1, null);
  const prior = fitPrior(
    counts.filter((_, i) => i >= inFit.length || inFit[i]),
    phi,
  );
  const grouped = groupCells(cells, stateOf);
  const stateSums = unitSums(nationalHits, nationalPages, grouped, states, buckets);
  const statePhi = groupPhi(spec, nationalHits, nationalPages, grouped, phi).phi;
  const statePrior = fitPrior(windowCounts(stateSums, buckets - 1, null), statePhi);
  return { buckets, level, dispersion, phi, prior, statePhi, statePrior, places: placeSums, states: stateSums };
}

export interface Frame {
  places: Score[];
  states: Score[];
  /** Pages published per place and per state in the frame. */
  placePages: Float64Array;
  statePages: Float64Array;
}

/** Scores for the frame at bucket `t` (cumulative when `win` is null). */
export function scoreFrame(m: SkewModel, t: number, win: number | null): Frame {
  return {
    places: windowCounts(m.places, t, win).map((c) => scorePlace(m.prior, c, m.phi, m.level)),
    states: windowCounts(m.states, t, win).map((c) => scorePlace(m.statePrior, c, m.statePhi, m.level)),
    placePages: windowPages(m.places, t, win),
    statePages: windowPages(m.states, t, win),
  };
}
