import { afterEach, describe, expect, it, vi } from "vitest";
import {
  api,
  ApiError,
  isPlain,
  isRetryable,
  MAX_COMPUTE_WAIT_MS,
  searchQuery,
  VersionChangedError,
} from "./client";

function respond(body: unknown, status = 200, type = "application/json") {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(body), { status, headers: { "content-type": type } })),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const computing = () =>
  new Response(JSON.stringify({ status: "computing", retry_after: 2 }), {
    status: 202,
    headers: { "content-type": "application/json", "retry-after": "2" },
  });
const problem = (status: number, type: string, retry?: string) =>
  new Response(JSON.stringify({ type, title: "t", status }), {
    status,
    headers: { "content-type": "application/problem+json", ...(retry ? { "retry-after": retry } : {}) },
  });
const ok = () =>
  new Response(JSON.stringify({ index_version: "v1", total: { hits: 1 } }), {
    status: 200,
    headers: { "content-type": "application/json" },
  });

/** Answer each fetch with the next response in `seq`. */
function sequence(...seq: (() => Response)[]) {
  const fn = vi.fn(async () => seq.shift()!());
  vi.stubGlobal("fetch", fn);
  return fn;
}

describe("searches the API is still computing", () => {
  it("asks again after Retry-After until the result arrives", async () => {
    vi.useFakeTimers();
    const fetch = sequence(computing, computing, ok);
    const onComputing = vi.fn();
    const result = api.aggregate({ q: "radio" }, "v1", undefined, onComputing);
    await vi.advanceTimersByTimeAsync(1999);
    expect(fetch).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(fetch).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    await expect(result).resolves.toMatchObject({ index_version: "v1" });
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(onComputing).toHaveBeenCalledTimes(2);
    // The same URL each time.
    const urls = fetch.mock.calls.map((c) => String((c as unknown[])[0]));
    expect(new Set(urls).size).toBe(1);
  });

  it("stops asking when the search changes (the signal aborts)", async () => {
    vi.useFakeTimers();
    const fetch = sequence(computing, computing, ok);
    const ctl = new AbortController();
    const result = api.aggregate({ q: "radio" }, "v1", ctl.signal).catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(500);
    ctl.abort(new DOMException("changed", "AbortError"));
    expect((await result as DOMException).name).toBe("AbortError");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("gives up after the longest wait, and that isn't retried", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn(async () => computing());
    vi.stubGlobal("fetch", fetch);
    const result = api.aggregate({ q: "radio" }, "v1").catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(MAX_COMPUTE_WAIT_MS + 5000);
    const err = (await result) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.problem.type).toBe("/errors/backend-timeout");
    expect(fetch.mock.calls.length).toBeLessThanOrEqual(MAX_COMPUTE_WAIT_MS / 2000 + 1);
    expect(isRetryable(err)).toBe(false);
  });

  it("holds a request made while waiting to the longest wait", async () => {
    vi.useFakeTimers();
    let calls = 0;
    // The first answer is a 202; the next request never gets an answer.
    vi.stubGlobal(
      "fetch",
      vi.fn((_: unknown, init?: RequestInit) => {
        calls += 1;
        if (calls === 1) return Promise.resolve(computing());
        return new Promise<Response>((_, reject) =>
          init?.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError"))),
        );
      }),
    );
    const result = api.aggregate({ q: "radio" }, "v1").catch((e: unknown) => e);
    await vi.advanceTimersByTimeAsync(MAX_COMPUTE_WAIT_MS + 1000);
    const err = (await result) as ApiError;
    expect(err).toBeInstanceOf(ApiError);
    expect(err.problem.type).toBe("/errors/backend-timeout");
  });

  it("waits out a busy API, and a rate limit met while waiting", async () => {
    vi.useFakeTimers();
    const fetch = sequence(
      () => problem(503, "/errors/busy", "5"),
      computing,
      () => problem(429, "/errors/rate-limited", "3"),
      ok,
    );
    const result = api.aggregate({ q: "radio" }, "v1");
    await vi.advanceTimersByTimeAsync(5000 + 2000 + 3000);
    await expect(result).resolves.toMatchObject({ index_version: "v1" });
    expect(fetch).toHaveBeenCalledTimes(4);
  });

  it("a rate limit before any 202 is an error, as before", async () => {
    sequence(() => problem(429, "/errors/rate-limited", "3"));
    await expect(api.aggregate({ q: "radio" }, "v1")).rejects.toBeInstanceOf(ApiError);
  });

  it("retries only failures worth retrying", () => {
    const err = (status: number, type: string) => new ApiError({ type, title: "t", status });
    expect(isRetryable(err(503, "/errors/backend"))).toBe(true);
    expect(isRetryable(new TypeError("network"))).toBe(true);
    expect(isRetryable(err(503, "/errors/backend-timeout"))).toBe(false);
    expect(isRetryable(err(503, "/errors/busy"))).toBe(false);
    expect(isRetryable(err(400, "/errors/query-syntax"))).toBe(false);
    expect(isRetryable(new VersionChangedError("a", "b"))).toBe(false);
  });
});

describe("api client", () => {
  it("rejects a response from another index version (a followed 307)", async () => {
    respond({ index_version: "v2", features: [] });
    await expect(api.places("v1")).rejects.toBeInstanceOf(VersionChangedError);
    respond({ index_version: "v1", type: "FeatureCollection", features: [] });
    await expect(api.places("v1")).resolves.toMatchObject({ index_version: "v1" });
  });

  it("turns problem+json into ApiError", async () => {
    respond({ type: "/errors/query-syntax", title: "Query syntax error", status: 400, hint: "h" }, 400, "application/problem+json");
    const err = await api.aggregate({ q: '"x' }, "v1").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).problem.hint).toBe("h");
  });

  it("revalidates /v1/meta instead of trusting the HTTP cache", async () => {
    respond({ index_version: "v2" });
    await api.meta();
    expect(vi.mocked(fetch).mock.calls[0]![1]).toMatchObject({ cache: "no-cache" });
  });

  it("pins every search to the version and omits defaults", async () => {
    respond({ index_version: "v1" });
    await api.aggregate({ q: "a b", mode: "all", bucket: "auto", state: ["GA"] }, "v1");
    const url = String(vi.mocked(fetch).mock.calls[0]![0]);
    expect(url).toBe("/v1/aggregate?q=a+b&state=GA&v=v1");
  });

  it("sends the mode for plain words and query syntax as-is", () => {
    const q = (p: Parameters<typeof searchQuery>[0]) => searchQuery(p, "v").toString();
    expect(q({ q: "cross of gold", mode: "phrase" })).toBe("q=cross+of+gold&mode=phrase&v=v");
    expect(q({ q: "cross of gold" })).toBe("q=cross+of+gold&mode=phrase&v=v");
    expect(q({ q: "yellow fever", mode: "near", near: 5 })).toBe("q=yellow+fever&mode=near&near=5&v=v");
    expect(q({ q: "free silver", mode: "any" })).toBe("q=free+silver&mode=any&v=v");
    expect(q({ q: '"cross of gold"', mode: "phrase" })).toBe("q=%22cross+of+gold%22&v=v");
    expect(q({ q: "fever OR influenza", mode: "phrase" })).toBe("q=fever+OR+influenza&v=v");
    expect(isPlain("gold -silver")).toBe(false);
    expect(isPlain("gold-standard")).toBe(true);
  });
});
