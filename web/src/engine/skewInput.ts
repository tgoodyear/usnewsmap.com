// Assemble the relative-rate view's inputs from responses the site already
// has (doc 11, 11.4.3): the aggregate (hits per place and bucket, and the
// national series), the coverage cube its `baseline_ref` names (pages
// published per place and bucket) and the places list. No new request.

import type { AggregateResponse, CoverageResponse, PlaceFeature } from "../api/types";
import type { SkewInput } from "./skewModel";

/** The view is off below this many places with pages in the window (doc 11, 11.4.2). */
export const MIN_PLACES = 5;
/** Credible level of the intervals. */
export const LEVEL = 0.9;

export type Unavailable =
  /** lccn, lang or front filters: the API sends no baselines. */
  | "filters"
  /** The coverage cube doesn't match the aggregate's buckets. */
  | "mismatch"
  /** Fewer than MIN_PLACES places published pages in the window. */
  | "few-places";

export interface Prepared {
  input: SkewInput;
  /** Place ids in the model's order (the coverage response's). */
  placeIds: string[];
  /** State codes in the model's state order. */
  stateCodes: string[];
  /** Places whose titles are all in languages other than English. */
  nonEnglish: boolean[];
  /** Places with pages in the window. */
  placesWithPages: number;
}

/** Titles all in other languages; unknown (no languages listed) counts as English. */
export function isNonEnglish(f: PlaceFeature | undefined): boolean {
  const langs = f?.properties.languages ?? [];
  return langs.length > 0 && !langs.includes("eng");
}

export function prepareSkew(
  agg: AggregateResponse,
  coverage: CoverageResponse,
  features: Map<string, PlaceFeature>,
): Prepared | Unavailable {
  if (agg.series.baseline === null || agg.cube.baseline_ref === null) return "filters";
  const buckets = agg.bucket.count;
  if (coverage.count !== buckets || agg.series.baseline.length !== buckets) return "mismatch";
  const placeIds = coverage.places;
  const index = new Map(placeIds.map((id, i) => [id, i]));
  // Hits keyed by (coverage place, bucket).
  const hits = new Map<number, number>();
  for (let i = 0; i < agg.cube.p.length; i++) {
    const at = index.get(agg.places.id[agg.cube.p[i]!]!);
    if (at === undefined) continue;
    const key = at * buckets + agg.cube.b[i]!;
    hits.set(key, (hits.get(key) ?? 0) + agg.cube.h[i]!);
  }
  const n = coverage.pages.p.length;
  const cells = { p: new Int32Array(n), b: new Int32Array(n), pages: new Float64Array(n), hits: new Float64Array(n) };
  const withPages = new Uint8Array(placeIds.length);
  for (let i = 0; i < n; i++) {
    const p = coverage.pages.p[i]!;
    const b = coverage.pages.b[i]!;
    cells.p[i] = p;
    cells.b[i] = b;
    cells.pages[i] = coverage.pages.h[i]!;
    cells.hits[i] = hits.get(p * buckets + b) ?? 0;
    if (coverage.pages.h[i]! > 0) withPages[p] = 1;
  }
  const placesWithPages = withPages.reduce((a, x) => a + x, 0);
  if (placesWithPages < MIN_PLACES) return "few-places";
  const stateCodes: string[] = [];
  const stateIndex = new Map<string, number>();
  const stateOf = placeIds.map((id) => {
    const s = features.get(id)?.properties.state ?? "";
    let g = stateIndex.get(s);
    if (g === undefined) {
      g = stateCodes.length;
      stateIndex.set(s, g);
      stateCodes.push(s);
    }
    return g;
  });
  const nonEnglish = placeIds.map((id) => isNonEnglish(features.get(id)));
  return {
    input: {
      spec: { unit: agg.bucket.unit, from: agg.bucket.from, to: agg.bucket.to },
      nationalHits: agg.series.hits,
      nationalPages: agg.series.baseline,
      cells,
      places: placeIds.length,
      inFit: nonEnglish.map((x) => !x),
      stateOf,
      states: stateCodes.length,
      level: LEVEL,
    },
    placeIds,
    stateCodes,
    nonEnglish,
    placesWithPages,
  };
}
