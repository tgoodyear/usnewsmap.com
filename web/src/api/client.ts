import type {
  AggregateResponse,
  CoverageResponse,
  DaysResponse,
  HitSort,
  HitsResponse,
  Meta,
  PlacesResponse,
  Problem,
  Status,
} from "./types";

/** API origin: same-origin (the API serves the site; the dev server proxies) unless configured. */
export const API_BASE: string = (import.meta.env.VITE_API_BASE ?? "").replace(/\/$/, "");

/**
 * A version-scoped response came from a different index version than the
 * page is pinned to (the API redirects a stale `v`, and fetch follows it).
 * The app refreshes /v1/meta and every version-scoped query rather than
 * mixing snapshots (06 §6.5).
 */
export class VersionChangedError extends Error {
  readonly expected: string;
  readonly actual: string;
  constructor(expected: string, actual: string) {
    super(`index version changed from ${expected} to ${actual}`);
    this.expected = expected;
    this.actual = actual;
  }
}

export class ApiError extends Error {
  readonly problem: Problem;
  constructor(problem: Problem) {
    super(problem.detail ?? problem.title);
    this.problem = problem;
  }
}

/**
 * How long a request keeps asking about a search the API is still
 * computing, time queued for a slot included. The API gives a computation
 * at most 2 minutes, so a little more.
 */
export const MAX_COMPUTE_WAIT_MS = 150_000;

/** The longest one request waits on the API before a `202` (10 s, plus 2 s for its persistent cache). */
const REQUEST_WAIT_MS = 12_000;

/** Problem types the app handles by waiting, never by retrying at once. */
const BUSY = "/errors/busy";
const TIMEOUT = "/errors/backend-timeout";

/** The search ran out of time, on the API or while the page waited for it. */
const tookTooLong: Problem = {
  type: TIMEOUT,
  title: "Search took too long",
  status: 503,
  detail: "The search did not finish in time.",
  hint: "Narrow the date range or add filters, then try again.",
};

/**
 * Whether a failed query is worth retrying straight away: not a problem
 * with the request (4xx), not a version change, and not a search that ran
 * out of time or found the API busy (both were already waited out here).
 */
export function isRetryable(err: unknown): boolean {
  if (err instanceof VersionChangedError) return false;
  if (err instanceof ApiError) {
    const { status, type } = err.problem;
    return status >= 500 && type !== TIMEOUT && type !== BUSY;
  }
  return true;
}

/** Retry-After in milliseconds, within 1–30 s; 2 s when absent or unreadable. */
function retryAfter(resp: Response): number {
  const secs = Number(resp.headers.get("retry-after"));
  return Number.isFinite(secs) && secs > 0 ? Math.min(Math.max(secs, 1), 30) * 1000 : 2000;
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(signal.reason);
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", stop);
      resolve();
    }, ms);
    const stop = () => {
      clearTimeout(timer);
      reject(signal!.reason);
    };
    signal?.addEventListener("abort", stop, { once: true });
  });
}

async function problemOf(resp: Response): Promise<Problem> {
  let problem: Problem = {
    type: "about:blank",
    title: resp.statusText || "Request failed",
    status: resp.status,
  };
  if ((resp.headers.get("content-type") ?? "").includes("json")) {
    try {
      problem = (await resp.json()) as Problem;
    } catch {
      // keep the generic problem
    }
  }
  return problem;
}

/**
 * `fetch`, abandoned at `deadline` (ms since the epoch) with the timeout
 * problem, or when `signal` aborts.
 */
async function fetchBy(
  path: string,
  cache: RequestCache | undefined,
  signal: AbortSignal | undefined,
  deadline: number,
): Promise<Response> {
  const ctl = new AbortController();
  let late = false;
  const timer = setTimeout(() => {
    late = true;
    ctl.abort();
  }, Math.max(0, deadline - Date.now()));
  const stop = () => ctl.abort(signal!.reason);
  if (signal?.aborted) stop();
  else signal?.addEventListener("abort", stop, { once: true });
  try {
    return await fetch(`${API_BASE}${path}`, {
      signal: ctl.signal,
      cache,
      headers: { accept: "application/json" },
    });
  } catch (e) {
    if (late) throw new ApiError(tookTooLong);
    throw e;
  } finally {
    clearTimeout(timer);
    signal?.removeEventListener("abort", stop);
  }
}

export interface GetOptions {
  signal?: AbortSignal;
  cache?: RequestCache;
  /**
   * Called each time the API answers that the response is still being
   * computed, with how many searches are ahead of it while it is queued
   * for a slot, or `null` when it is running (or its place isn't known).
   */
  onComputing?: (ahead: number | null) => void;
}

/** How many searches are ahead of a `202`'s search in the API's queue (06 §6.3.5). */
async function aheadOf(resp: Response): Promise<number | null> {
  try {
    const body = (await resp.json()) as { status?: unknown; ahead?: unknown };
    return body.status === "queued" && typeof body.ahead === "number" ? body.ahead : null;
  } catch {
    return null;
  }
}

/**
 * GET a JSON resource. A search the API is still computing (`202`), or
 * one it is too busy to start (`503` busy), is asked for again after the
 * Retry-After wait, for at most {@link MAX_COMPUTE_WAIT_MS}; so is a rate
 * limit (`429`) met while waiting. Aborting `signal` stops the waiting.
 */
async function getJson<T>(path: string, opts: GetOptions = {}): Promise<T> {
  const { signal, cache, onComputing } = opts;
  const started = Date.now();
  let waiting = false;
  for (;;) {
    // Once waiting, every request is held to what is left of the longest wait.
    const resp = waiting
      ? await fetchBy(path, cache, signal, started + MAX_COMPUTE_WAIT_MS)
      : await fetch(`${API_BASE}${path}`, { signal, cache, headers: { accept: "application/json" } });
    let wait: number | null = null;
    let problem: Problem | null = null;
    if (resp.status === 202) {
      wait = retryAfter(resp);
      waiting = true;
      onComputing?.(await aheadOf(resp));
    } else if (!resp.ok) {
      problem = await problemOf(resp);
      if (problem.type === BUSY) {
        // The API's queue is full: the same wait for the visitor.
        wait = retryAfter(resp);
        waiting = true;
        onComputing?.(null);
      } else if (resp.status === 429 && waiting) {
        wait = retryAfter(resp);
      }
    }
    if (wait !== null) {
      // The next request may itself wait up to the API's 12 s before answering.
      if (Date.now() - started + wait + REQUEST_WAIT_MS > MAX_COMPUTE_WAIT_MS) throw new ApiError(tookTooLong);
      await sleep(wait, signal);
      continue;
    }
    if (problem) throw new ApiError(problem);
    return (await resp.json()) as T;
  }
}

/** Fetch a version-scoped resource and check it belongs to `version`. */
async function getPinned<T extends { index_version: string }>(
  path: string,
  version: string,
  signal?: AbortSignal,
  onComputing?: (ahead: number | null) => void,
): Promise<T> {
  const body = await getJson<T>(path, { signal, onComputing });
  if (body.index_version !== version) throw new VersionChangedError(version, body.index_version);
  return body;
}

/** Search parameters in the API's vocabulary; empty values are omitted. */
export interface SearchParams {
  q: string;
  mode?: string;
  near?: number;
  from?: string;
  to?: string;
  bucket?: string;
  state?: string[];
  lccn?: string[];
  /** Newspaper languages (catalog codes); any of them (07 §7.9). */
  lang?: string[];
  front?: boolean;
}

/**
 * Plain words, with no query syntax. Mirrors `is_plain` in
 * crates/usnm-core/src/query.rs: the API applies the phrase/any/near modes
 * only to plain input and parses anything else as query syntax.
 */
export function isPlain(q: string): boolean {
  return (
    !/["()~*:]/.test(q) &&
    !q.split(/\s+/).some((w) => w === "AND" || w === "OR" || w === "NOT" || w.startsWith("-"))
  );
}

export function searchQuery(p: SearchParams, version: string): URLSearchParams {
  const s = new URLSearchParams();
  s.set("q", p.q);
  // The selected mode applies to plain words ("cross of gold" as an exact
  // phrase). Query syntax such as quotes or OR already says what it means,
  // so it is sent without a mode, which the API reads as query syntax.
  const mode = p.mode ?? "phrase";
  if (isPlain(p.q) && mode !== "all") {
    s.set("mode", mode);
    if (mode === "near" && p.near) s.set("near", String(p.near));
  }
  if (p.from) s.set("from", p.from);
  if (p.to) s.set("to", p.to);
  if (p.bucket && p.bucket !== "auto") s.set("bucket", p.bucket);
  if (p.state?.length) s.set("state", p.state.join(","));
  if (p.lccn?.length) s.set("lccn", p.lccn.join(","));
  if (p.lang?.length) s.set("lang", p.lang.join(","));
  if (p.front) s.set("front", "true");
  // Pin to the version the page loaded, so every response comes from one snapshot.
  s.set("v", version);
  return s;
}

export const api = {
  // Always revalidate: /v1/meta is cacheable for 5 minutes, and after a
  // version change a cached copy would name the old version again.
  meta: (signal?: AbortSignal) => getJson<Meta>("/v1/meta", { signal, cache: "no-cache" }),
  /** The pipeline status page's data; the API recomputes it at most once a minute. */
  status: (signal?: AbortSignal) => getJson<Status>("/v1/status", { signal, cache: "no-cache" }),
  places: (version: string, signal?: AbortSignal) =>
    getPinned<PlacesResponse>(`/v1/places?v=${encodeURIComponent(version)}`, version, signal),
  /** `onComputing` is called while the API is still computing a large search. */
  aggregate: (
    p: SearchParams,
    version: string,
    signal?: AbortSignal,
    onComputing?: (ahead: number | null) => void,
  ) =>
    getPinned<AggregateResponse>(`/v1/aggregate?${searchQuery(p, version)}`, version, signal, onComputing),
  /** `ref` is the response's `baseline_ref`, already canonical and versioned. */
  coverage: (ref: string, version: string, signal?: AbortSignal) =>
    getPinned<CoverageResponse>(ref.replace(/^\/(api\/)?v1\//, "/v1/"), version, signal),
  hits: (
    p: SearchParams,
    version: string,
    place: string,
    sort: HitSort,
    cursor: string | null,
    signal?: AbortSignal,
  ) => {
    const s = searchQuery(p, version);
    s.delete("bucket");
    s.set("place", place);
    s.set("limit", "20");
    // Oldest first is the API's default, and leaving it out keeps one URL per list.
    if (sort !== "oldest") s.set("sort", sort);
    if (cursor) s.set("cursor", cursor);
    return getPinned<HitsResponse>(`/v1/hits?${s}`, version, signal);
  },
  /** Matching pages per day for up to 20 places, for exact median dates. */
  days: (p: SearchParams, version: string, places: string[], signal?: AbortSignal) => {
    const s = searchQuery(p, version);
    s.delete("bucket");
    s.set("place", places.join(","));
    return getJson<DaysResponse>(`/v1/days?${s}`, { signal });
  },
};
