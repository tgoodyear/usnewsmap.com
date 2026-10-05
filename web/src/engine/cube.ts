// Client-side temporal engine (07 §7.3). The aggregate response carries a
// sparse place × bucket cube once; every playback frame is computed locally
// from per-place prefix sums in O(places).

import type { SparseCube } from "../api/types";

export interface PrefixSums {
  places: number;
  buckets: number;
  /** Row p holds S[p][0..=B]: S[p][b+1] = hits in buckets 0..=b. */
  sums: Float64Array;
}

export function prefixSums(cube: SparseCube, places: number, buckets: number): PrefixSums {
  const stride = buckets + 1;
  const sums = new Float64Array(places * stride);
  const n = Math.min(cube.p.length, cube.b.length, cube.h.length);
  for (let i = 0; i < n; i++) {
    const p = cube.p[i]!;
    const b = cube.b[i]!;
    if (p < places && b < buckets) sums[p * stride + b + 1]! += cube.h[i]!;
  }
  for (let p = 0; p < places; p++) {
    const row = p * stride;
    for (let b = 1; b <= buckets; b++) sums[row + b]! += sums[row + b - 1]!;
  }
  return { places, buckets, sums };
}

/**
 * Per-place totals for the view at bucket `t`: cumulative (buckets 0..=t)
 * when `window` is null, else the trailing `window` buckets ending at `t`.
 */
export function windowValues(
  ps: PrefixSums,
  t: number,
  window: number | null,
  out: Float64Array = new Float64Array(ps.places),
): Float64Array {
  const stride = ps.buckets + 1;
  const hi = Math.min(Math.max(t, 0), ps.buckets - 1) + 1;
  const lo = window === null ? 0 : Math.max(0, hi - window);
  for (let p = 0; p < ps.places; p++) {
    const row = p * stride;
    out[p] = ps.sums[row + hi]! - ps.sums[row + lo]!;
  }
  return out;
}

/**
 * Re-index a coverage cube (pages published, keyed by the coverage
 * response's place list) onto the aggregate's place order. Places with no
 * hits are dropped; places with no coverage get no cells.
 */
export function alignCube(
  cube: SparseCube,
  cubePlaces: string[],
  targetPlaces: string[],
): SparseCube {
  const index = new Map(targetPlaces.map((id, i) => [id, i]));
  const map = cubePlaces.map((id) => index.get(id) ?? -1);
  const out: SparseCube = { p: [], b: [], h: [] };
  for (let i = 0; i < cube.p.length; i++) {
    const target = map[cube.p[i]!] ?? -1;
    if (target >= 0) {
      out.p.push(target);
      out.b.push(cube.b[i]!);
      out.h.push(cube.h[i]!);
    }
  }
  return out;
}

/** hits / pages per place; 0 where nothing was published. */
export function relative(
  hits: Float64Array,
  pages: Float64Array,
  out: Float64Array = new Float64Array(hits.length),
): Float64Array {
  for (let i = 0; i < hits.length; i++) {
    const d = pages[i]!;
    out[i] = d > 0 ? hits[i]! / d : 0;
  }
  return out;
}

/**
 * Per place, the bucket where the `q` quantile of its matching pages falls in
 * the view at bucket `t` (the same window as `windowValues`): the first
 * bucket by which at least `q` of them have appeared. NaN for a place with
 * no pages in the window. `q` = 0.5 is the median mention date (#127);
 * O(places × log buckets) by binary search on the prefix sums.
 */
export function windowQuantile(
  ps: PrefixSums,
  t: number,
  window: number | null,
  q: number,
  out: Float64Array = new Float64Array(ps.places),
): Float64Array {
  const stride = ps.buckets + 1;
  const hi = Math.min(Math.max(t, 0), ps.buckets - 1) + 1;
  const lo = window === null ? 0 : Math.max(0, hi - window);
  for (let p = 0; p < ps.places; p++) {
    const row = p * stride;
    const base = ps.sums[row + lo]!;
    const total = ps.sums[row + hi]! - base;
    if (total <= 0) {
      out[p] = Number.NaN;
      continue;
    }
    const need = Math.max(q * total, Number.MIN_VALUE);
    // The smallest b in [lo, hi) with sums[b + 1] - base >= need.
    let a = lo;
    let z = hi - 1;
    while (a < z) {
      const mid = (a + z) >> 1;
      if (ps.sums[row + mid + 1]! - base >= need) z = mid;
      else a = mid + 1;
    }
    out[p] = a;
  }
  return out;
}
