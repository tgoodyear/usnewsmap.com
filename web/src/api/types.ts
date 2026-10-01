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
  capabilities: {
    fuzzy: boolean;
    max_slop: number;
    nested_aggregations: boolean;
  };
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
  properties: {
    name: string;
    state: string;
    precision: string;
    titles: number;
    /** Languages of the place's titles (catalog codes such as "eng"). Absent from older APIs. */
    languages?: string[];
  };
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

/** A `/v1/status` section that needs the pipeline state: its data, or why there is none. */
export type Section<T> =
  ({ available: true } & T) | { available: false; reason: string };

export interface HourBin {
  start: string;
  batches: number;
  pages: number;
}

export interface Backfill {
  total: number;
  by_status: {
    queued: number;
    downloading: number;
    curated: number;
    failed: number;
  };
  in_progress: number;
  stale_leases: number;
  retrying: number;
  percent: number;
  pages: number;
  ok_pages: number;
  versions: Record<string, number>;
  newer_versions_pending: number;
  throughput: {
    hours: HourBin[];
    rate_window_hours: number;
    rate_per_hour: number;
    remaining: number;
    eta: string | null;
  };
  loc: {
    next_slot: string | null;
    blocked_until: string | null;
    throttled: boolean;
  };
  in_progress_batches: {
    batch: string;
    version: number;
    worker: string | null;
    since: string;
    lease_until: string | null;
    lease_expired: boolean;
    attempts: number;
  }[];
  recent: {
    batch: string;
    version: number;
    pages: number;
    curated_at: string;
  }[];
  failed_batches: {
    batch: string;
    version: number;
    attempts: number;
    error: string | null;
    updated_at: string;
  }[];
  listed_limit: number;
}

export type RunStatus = "building" | "published" | "failed";

export interface IndexRun {
  index_version: string;
  full: boolean;
  status: RunStatus;
  new_index: string;
  indexes: number;
  batches: number | null;
  docs: number;
  pages: number;
  started_at: string;
  published_at: string | null;
  duration_secs: number | null;
  previous_version: string | null;
  error: string | null;
}

export interface Indexing {
  current_version: string | null;
  writer: { held: boolean; holder: string | null; until: string | null };
  release: {
    index_version: string;
    docs_sent: number;
    docs_expected: number;
    percent: number;
    mb_sent: number;
    updated_at: string;
  } | null;
  last_published_at: string | null;
  failed_runs: number;
  /** Failed runs not yet superseded by a later successful publish. */
  failed_since_last_publish?: number;
  runs: IndexRun[];
}

export interface TitlesPipeline {
  curated_titles: number;
  awaiting_sync: number | null;
  batches_waiting_for_titles: number | null;
  unpublished_batches: number | null;
  ready_for_release: number | null;
  recurated_awaiting_full: number | null;
}

/** `GET /v1/status` (schema 1): the ingest pipeline's status. */
export interface Status {
  schema: number;
  generated_at: string;
  stale: boolean;
  error: string | null;
  pipeline: { available: boolean; read_at: string | null; reason?: string };
  published: {
    index_version: string;
    published_at: string;
    synthetic: boolean;
    pages: number;
    titles: number;
    places: number;
    bounds: { from: string; to: string };
    indexes: string[];
    deltas: number;
    max_deltas: number;
    next_release_full: boolean;
    batches: number | null;
  };
  backfill: Section<Backfill>;
  indexing: Section<Indexing>;
  titles: {
    catalog: Section<{ titles: number; places: number }>;
    published_titles: number;
    published_places: number;
    pipeline: Section<TitlesPipeline>;
  };
}
