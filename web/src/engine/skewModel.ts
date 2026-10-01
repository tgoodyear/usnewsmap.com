// The relative-rate view's model (doc 11, 11.6): fit once per search, then
// score any playback frame from prefix sums in O(places). phi and the priors
// stay at their full-window values, so colours don't jump because of a refit.

import {
  dispersionFor,
  fitPrior,
  groupCells,
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

/** Per-unit prefix sums over buckets: S[u][b+1] = total over buckets 0..=b. */
export interface UnitSums {
  units: number;
  buckets: number;
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
  const stride = buckets + 1;
  const observed = new Float64Array(units * stride);
  const expected = new Float64Array(units * stride);
  const pages = new Float64Array(units * stride);
  for (let i = 0; i < cells.p.length; i++) {
    const u = cells.p[i]!;
    const b = cells.b[i]!;
    if (u >= units || b >= buckets) continue;
    const at = u * stride + b + 1;
    pages[at]! += cells.pages[i]!;
    const rate = referenceRate(nationalHits, nationalPages, b, cells.pages[i]!, cells.hits[i]!);
    if (rate === null) continue;
    observed[at]! += cells.hits[i]!;
    expected[at]! += cells.pages[i]! * rate;
  }
  for (const s of [observed, expected, pages]) {
    for (let u = 0; u < units; u++) {
      const row = u * stride;
      for (let b = 1; b <= buckets; b++) s[row + b]! += s[row + b - 1]!;
    }
  }
  return { units, buckets, observed, expected, pages };
}

/** Bucket range [lo, hi) of the frame at `t`: cumulative, or the trailing `win` buckets. */
function range(buckets: number, t: number, win: number | null): [number, number] {
  const hi = Math.min(Math.max(t, 0), buckets - 1) + 1;
  return [win === null ? 0 : Math.max(0, hi - win), hi];
}

export function windowCounts(s: UnitSums, t: number, win: number | null): Counts[] {
  const stride = s.buckets + 1;
  const [lo, hi] = range(s.buckets, t, win);
  const out: Counts[] = new Array(s.units);
  for (let u = 0; u < s.units; u++) {
    const row = u * stride;
    out[u] = {
      observed: s.observed[row + hi]! - s.observed[row + lo]!,
      expected: s.expected[row + hi]! - s.expected[row + lo]!,
    };
  }
  return out;
}

export function windowPages(s: UnitSums, t: number, win: number | null): Float64Array {
  const stride = s.buckets + 1;
  const [lo, hi] = range(s.buckets, t, win);
  const out = new Float64Array(s.units);
  for (let u = 0; u < s.units; u++) out[u] = s.pages[u * stride + hi]! - s.pages[u * stride + lo]!;
  return out;
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
  const stateSums = unitSums(nationalHits, nationalPages, groupCells(cells, stateOf), states, buckets);
  const statePrior = fitPrior(windowCounts(stateSums, buckets - 1, null), phi);
  return { buckets, level, dispersion, phi, prior, statePrior, places: placeSums, states: stateSums };
}

/**
 * The largest expected count the playback will reach, so circles keep one
 * scale: the full window when cumulative, else the biggest trailing window.
 */
export function maxWindowExpected(s: UnitSums, win: number | null): number {
  const stride = s.buckets + 1;
  let max = 1e-9;
  for (let u = 0; u < s.units; u++) {
    const row = u * stride;
    if (win === null) {
      max = Math.max(max, s.expected[row + s.buckets]!);
      continue;
    }
    for (let hi = 1; hi <= s.buckets; hi++) {
      max = Math.max(max, s.expected[row + hi]! - s.expected[row + Math.max(0, hi - win)]!);
    }
  }
  return max;
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
    states: windowCounts(m.states, t, win).map((c) => scorePlace(m.statePrior, c, m.phi, m.level)),
    placePages: windowPages(m.places, t, win),
    statePages: windowPages(m.states, t, win),
  };
}
