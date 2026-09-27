import type {
  AggregateResponse,
  CoverageResponse,
  HitsResponse,
  Meta,
  PlacesResponse,
  Problem,
} from "./types";

/** API origin: same-origin (dev proxy, SWA linked backend) unless configured. */
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

async function getJson<T>(path: string, signal?: AbortSignal, cache?: RequestCache): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    signal,
    cache,
    headers: { accept: "application/json" },
  });
  if (!resp.ok) {
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
    throw new ApiError(problem);
  }
  return (await resp.json()) as T;
}

/** Fetch a version-scoped resource and check it belongs to `version`. */
async function getPinned<T extends { index_version: string }>(
  path: string,
  version: string,
  signal?: AbortSignal,
): Promise<T> {
  const body = await getJson<T>(path, signal);
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
  front?: boolean;
}

export function searchQuery(p: SearchParams, version: string): URLSearchParams {
  const s = new URLSearchParams();
  s.set("q", p.q);
  if (p.mode && p.mode !== "phrase") s.set("mode", p.mode);
  if (p.mode === "near" && p.near) s.set("near", String(p.near));
  if (p.from) s.set("from", p.from);
  if (p.to) s.set("to", p.to);
  if (p.bucket && p.bucket !== "auto") s.set("bucket", p.bucket);
  if (p.state?.length) s.set("state", p.state.join(","));
  if (p.lccn?.length) s.set("lccn", p.lccn.join(","));
  if (p.front) s.set("front", "true");
  // Pin to the version the page loaded, so every response comes from one snapshot.
  s.set("v", version);
  return s;
}

export const api = {
  // Always revalidate: /v1/meta is cacheable for 5 minutes, and after a
  // version change a cached copy would name the old version again.
  meta: (signal?: AbortSignal) => getJson<Meta>("/v1/meta", signal, "no-cache"),
  places: (version: string, signal?: AbortSignal) =>
    getPinned<PlacesResponse>(`/v1/places?v=${encodeURIComponent(version)}`, version, signal),
  aggregate: (p: SearchParams, version: string, signal?: AbortSignal) =>
    getPinned<AggregateResponse>(`/v1/aggregate?${searchQuery(p, version)}`, version, signal),
  /** `ref` is the response's `baseline_ref`, already canonical and versioned. */
  coverage: (ref: string, version: string, signal?: AbortSignal) =>
    getPinned<CoverageResponse>(ref.replace(/^\/(api\/)?v1\//, "/v1/"), version, signal),
  hits: (
    p: SearchParams,
    version: string,
    place: string,
    cursor: string | null,
    signal?: AbortSignal,
  ) => {
    const s = searchQuery(p, version);
    s.delete("bucket");
    s.set("place", place);
    s.set("limit", "20");
    if (cursor) s.set("cursor", cursor);
    return getPinned<HitsResponse>(`/v1/hits?${s}`, version, signal);
  },
};
