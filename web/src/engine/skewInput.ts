// Assemble the relative-rate view's inputs from responses the site already
// has (doc 11, 11.4.3): the aggregate (hits per place and bucket, and the
// national series), the coverage cube its `baseline_ref` names (pages
// published per place and bucket) and the places list. No new request.

import type { AggregateResponse, CoverageResponse, PlaceFeature } from "../api/types";
import type { SkewInput } from "./skewModel";
import { languageLabel } from "../lib/languages";

/** The view is off below this many places with pages in the window (doc 11, 11.4.2). */
export const MIN_PLACES = 5;
/** Credible level of the intervals. */
export const LEVEL = 0.9;

export type Unavailable =
  /** No baselines: lccn or front filters, or lang on a version without per-language counts. */
  | "filters"
  /** The coverage cube doesn't match the aggregate's buckets. */
  | "mismatch"
  /** Fewer than MIN_PLACES places published pages in the window. */
  | "few-places";

export interface Prepared {
  /** The index version the inputs came from. */
  version: string;
  /** The search (version, query and buckets) the inputs belong to. */
  search: string;
  input: SkewInput;
  /** Place ids in the model's order (the coverage response's). */
  placeIds: string[];
  /** State codes in the model's state order. */
  stateCodes: string[];
  /** "Papers in German and English" where any title isn't in English, else null. */
  languages: (string | null)[];
  /** Places with pages in the window. */
  placesWithPages: number;
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
  // Places without a known state are left out of the states (-1).
  const stateOf = placeIds.map((id) => {
    const s = features.get(id)?.properties.state ?? "";
    if (!s) return -1;
    let g = stateIndex.get(s);
    if (g === undefined) {
      g = stateCodes.length;
      stateIndex.set(s, g);
      stateCodes.push(s);
    }
    return g;
  });
  // Every place is scored and fitted the same way. Without a language filter a
  // place's languages are named, since they may explain a low rate (doc 11,
  // 11.14); with one the baselines count only those languages' pages, so
  // there is nothing to explain.
  const filtered = new URLSearchParams(agg.query.canonical).has("lang");
  const languages = placeIds.map((id) => (filtered ? null : languageLabel(features.get(id)?.properties.languages)));
  return {
    version: agg.index_version,
    search: [agg.index_version, agg.query.canonical, agg.bucket.unit, agg.bucket.from, agg.bucket.to, buckets].join("|"),
    input: {
      spec: { unit: agg.bucket.unit, from: agg.bucket.from, to: agg.bucket.to },
      nationalHits: agg.series.hits,
      nationalPages: agg.series.baseline,
      cells,
      places: placeIds.length,
      inFit: [],
      stateOf,
      states: stateCodes.length,
      level: LEVEL,
    },
    placeIds,
    stateCodes,
    languages,
    placesWithPages,
  };
}
