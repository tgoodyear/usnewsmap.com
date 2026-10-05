// Response shapes of the public API (06 §6.3). Kept in step with
// crates/usnm-api/src/routes; generated from OpenAPI once that exists.

export type BucketUnit = "year" | "month" | "week" | "day";
export type Mode = "phrase" | "all" | "any" | "near";
/**
 * Order of a `/v1/hits` list: by date, then title, edition and page (reversed
 * for newest), or `relevant`: the pages that mention the search most first (#126).
 */
export type HitSort = "oldest" | "newest" | "relevant";

export interface Meta {
  index_version: string;
  bounds: { from: string; to: string };
  published_at: string;
  synthetic: boolean;
  places: number;
  titles: number;
  /** Every page in the published version. Absent from APIs older than this field. */
  pages?: number;
  /**
   * The language filter's choices (07 §7.9): catalog languages the `lang`
   * parameter accepts, most pages first. Absent from older APIs.
   */
  languages?: MetaLanguage[];
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

export interface MetaLanguage {
  code: string;
  name: string;
  /** Titles that list it; a title in several languages counts in each. */
  titles: number;
  /** Null when the snapshot doesn't record pages per title. */
  pages: number | null;
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
    /** Titles that list each language; a title in several counts in each. Absent from older APIs. */
    language_titles?: Record<string, number>;
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
  total: {
    hits: number;
    places: number;
    baseline_pages: number;
    /** Earliest and latest matching day (days since 1700-01-01); null when nothing matches. Absent from older APIs. */
    first_day?: number | null;
    last_day?: number | null;
    /** The earliest and latest matching page; null when nothing matches. Absent from older APIs. */
    first?: HitItem | null;
    last?: HitItem | null;
    /** Newspapers with at least one matching page (#121). Absent from older APIs. */
    papers?: number;
  };
  series: { hits: number[]; baseline: number[] | null };
  places: {
    id: string[];
    hits: number[];
    first_day: number[];
    /** Absent from older APIs. */
    last_day?: number[];
  };
  cube: SparseCube & { calls: number; baseline_ref: string | null };
  /**
   * Matching pages per newspaper, most first, at most 500 (`total.papers`
   * counts them all); title and place from the catalog (#121). Absent from older APIs.
   */
  papers?: { lccn: string[]; hits: number[]; title: (string | null)[]; place_id: (string | null)[] };
  /**
   * Matching pages per title language, most first. A page of a paper in
   * several languages counts in each, so these can add up to more than
   * `total.hits` (#121). Absent from older APIs.
   */
  languages?: { code: string[]; hits: number[] };
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
  /** Pages also in another batch, left out of `pages` (04 §4.7). Absent from older APIs. */
  duplicate_pages?: number;
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

/** One state's (or territory's) share of the published pages. */
export interface StatePages {
  /** Postal code, e.g. "IL". */
  state: string;
  /** LoC's name for it, or the code when it isn't a known state. */
  name: string;
  places: number;
  titles: number;
  pages: number;
  /** Percent of every published page, to one decimal. */
  percent: number;
}

/** One language's newspapers and, when the snapshot records them, pages. */
export interface LanguagePages {
  /** The catalog's code (e.g. "eng"); null for newspapers with no language recorded. */
  code: string | null;
  name: string;
  titles: number;
  pages: number | null;
  percent: number | null;
}

/**
 * Pages by language. A newspaper that lists several languages counts in each
 * one's row, so the rows can add up to more than the version's pages.
 */
export interface ByLanguage {
  /** False for versions published before releases recorded pages per newspaper. */
  pages_known: boolean;
  multilingual_titles: number;
  multilingual_pages: number | null;
  rows: LanguagePages[];
}

/** What the pipeline is doing at this moment (`activity.now`). */
export type Now =
  | "listing"
  | "downloading"
  | "titles"
  | "indexing"
  | "merging"
  | "publishing"
  | "idle";

export type LastOutcome =
  | "published"
  | "nothing_new"
  | "titles_left"
  | "failed"
  | "stopped";

/** `activity`: the "Right now" line's data (06 §6.3.6). */
export interface Activity {
  now: Now;
  /** "job": the ingest execution reports it; "inferred": worked out from other state; "none" when idle. */
  source: "job" | "inferred" | "none";
  since: string | null;
  run_started_at: string | null;
  run: string | null;
  reported_at: string | null;
  done: number | null;
  total: number | null;
  percent: number | null;
  eta: string | null;
  /** loc.gov rate limited the title lookups; nothing is sent until then. */
  paused_until: string | null;
  index_version: string | null;
  merge: {
    step: string;
    splits: number;
    merges_running: number;
    merges_queued: number;
  } | null;
  last: {
    outcome: LastOutcome;
    ended_at: string;
    step: Now | null;
    error: string | null;
    index_version: string | null;
  } | null;
  /** The ingest job's next scheduled start; null when it is started by hand. */
  next_run: string | null;
}

/** `GET /v1/status` (schema 1): the ingest pipeline's status. */
export interface OcrCount {
  pages: number;
  issues: number;
}

/** Japanese pages LoC ships without text, which our OCR job reads (#139). */
export interface OcrJa {
  /** Reported recently with pages left. */
  running: boolean;
  targets: OcrCount;
  done: OcrCount;
  /** Of the target pages, to one decimal, never rounded up to 100. */
  percent: number;
  engine: string | null;
  started_at: string | null;
  updated_at: string;
  /** Only while running. */
  eta: string | null;
}

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
    /**
     * Copies of pages that also ship in another batch, counted once in
     * `pages`; null when the snapshot doesn't record them. Absent from older APIs.
     */
    duplicate_pages?: number | null;
    /** Added after schema 1 shipped; older APIs leave them out. */
    by_state?: StatePages[];
    by_language?: ByLanguage;
    /** The Japanese index built from our own OCR; null or absent when the version has none. */
    ja?: { indexes: string[]; fold: number; pages: number } | null;
  };
  /** Added after schema 1 shipped; older APIs leave it out. */
  activity?: Section<Activity>;
  backfill: Section<Backfill>;
  indexing: Section<Indexing>;
  /** The Japanese OCR job's progress. Absent from older APIs. */
  ocr_ja?: Section<OcrJa>;
  titles: {
    catalog: Section<{ titles: number; places: number }>;
    published_titles: number;
    published_places: number;
    pipeline: Section<TitlesPipeline>;
  };
}
