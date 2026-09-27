import { afterEach, describe, expect, it, vi } from "vitest";
import { api, ApiError, isPlain, searchQuery, VersionChangedError } from "./client";

function respond(body: unknown, status = 200, type = "application/json") {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(body), { status, headers: { "content-type": type } })),
  );
}

afterEach(() => vi.unstubAllGlobals());

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
