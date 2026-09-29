// Response shapes of the public API (06 §6.3). Kept in step with
// crates/usnm-api/src/routes; generated from OpenAPI once that exists.

export type BucketUnit = "year" | "month" | "week" | "day";
export type Mode = "phrase" | "all" | "any" | "near";

export interface Meta {
  index_version: string;
  bounds: { from: string; to: string };
  published_at: string;
  synthetic: boolean;
  places: number;
  titles: number;
  /** Every page in the published version. Absent from APIs older than this field. */
  pages?: number;
  capabilities: { fuzzy: boolean; max_slop: number; nested_aggregations: boolean };
  limits: {
    max_query_chars: number;
    max_terms: number;
    max_or_branches: number;
    max_slop: number;
    max_fuzzy: number;
    min_prefix_chars: number;
  };
}

export interface PlaceFeature {
  type: "Feature";
  id: string;
  geometry: { type: "Point"; coordinates: [number, number] };
  properties: { name: string; state: string; precision: string; titles: number };
}

export interface PlacesResponse {
  type: "FeatureCollection";
  index_version: string;
  features: PlaceFeature[];
}

/** Sparse (place index, bucket index, count) triplets. */
export interface SparseCube {
  p: number[];
  b: number[];
  h: number[];
}

export interface AggregateResponse {
  index_version: string;
  synthetic: boolean;
  query: { canonical: string; ast: string };
  bucket: { unit: BucketUnit; from: string; to: string; count: number };
  coarsened: boolean;
  total: { hits: number; places: number; baseline_pages: number };
  series: { hits: number[]; baseline: number[] | null };
  places: { id: string[]; hits: number[]; first_day: number[] };
  cube: SparseCube & { calls: number; baseline_ref: string | null };
}

export interface CoverageResponse {
  index_version: string;
  bucket: BucketUnit;
  from: string;
  to: string;
  count: number;
  places: string[];
  pages: SparseCube;
}

export interface HitItem {
  doc_id: string;
  date: string;
  lccn: string;
  title: string | null;
  place_id: string;
  edition: number;
  seq: number;
  front_page: boolean;
  /** HTML-escaped text with `<mark>` around matched terms. */
  snippets: string[];
  links: { viewer: string | null };
}

export interface HitsResponse {
  index_version: string;
  place: { id: string; name: string; state: string } | null;
  title: { lccn: string; name: string } | null;
  total: number;
  items: HitItem[];
  next_cursor: string | null;
}

/** RFC 9457 problem details. */
export interface Problem {
  type: string;
  title: string;
  status: number;
  detail?: string;
  hint?: string;
  position?: number;
}
