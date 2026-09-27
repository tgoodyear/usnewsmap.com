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

export class ApiError extends Error {
  readonly problem: Problem;
  constructor(problem: Problem) {
    super(problem.detail ?? problem.title);
    this.problem = problem;
  }
}

async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    signal,
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
  meta: (signal?: AbortSignal) => getJson<Meta>("/v1/meta", signal),
  places: (version: string, signal?: AbortSignal) =>
    getJson<PlacesResponse>(`/v1/places?v=${encodeURIComponent(version)}`, signal),
  aggregate: (p: SearchParams, version: string, signal?: AbortSignal) =>
    getJson<AggregateResponse>(`/v1/aggregate?${searchQuery(p, version)}`, signal),
  /** `ref` is the response's `baseline_ref`, already canonical and versioned. */
  coverage: (ref: string, signal?: AbortSignal) =>
    getJson<CoverageResponse>(ref.replace(/^\/(api\/)?v1\//, "/v1/"), signal),
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
    return getJson<HitsResponse>(`/v1/hits?${s}`, signal);
  },
};
