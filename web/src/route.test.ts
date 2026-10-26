import { describe, expect, it } from "vitest";
import { pageFor, queryForPath, searchPath } from "./route";

describe("pageFor", () => {
  it("knows the app's pages, with or without a trailing slash", () => {
    expect(pageFor("/")).toBe("search");
    expect(pageFor("//")).toBe("search");
    expect(pageFor("/index.html")).toBe("search");
    expect(pageFor("/index.html/")).toBe("search");
    expect(pageFor("/datasets")).toBe("datasets");
    expect(pageFor("/datasets/")).toBe("datasets");
    // The old path, for a server that doesn't redirect it (#294).
    expect(pageFor("/status")).toBe("datasets");
    expect(pageFor("/status/")).toBe("datasets");
    expect(pageFor("/credits")).toBe("credits");
    expect(pageFor("/credits/")).toBe("credits");
    expect(pageFor("/privacy")).toBe("privacy");
    expect(pageFor("/privacy/")).toBe("privacy");
    expect(pageFor("/search/gold")).toBe("search");
    expect(pageFor("/search/%22cross%20of%20gold%22/")).toBe("search");
  });

  it("finds nothing anywhere else", () => {
    for (const path of [
      "/does-not-exist",
      "/statuses",
      "/status/x",
      "/datasets/x",
      "/dataset",
      "/search/",
      "/search/gold/extra",
      "/search/%FF",
      "/search/%",
      "/privacy/x",
      "/credits/x",
    ]) {
      expect(pageFor(path)).toBe("not-found");
    }
  });
});

describe("readable search paths", () => {
  it("slugs plain exact phrases, dropping quotes and case", () => {
    expect(searchPath('"Fifteenth Amendment"')).toBe("/search/fifteenth-amendment");
    expect(searchPath("Know-Nothing")).toBe("/search/know-nothing");
    expect(searchPath("gold")).toBe("/search/gold");
  });

  it("keeps the encoded form where syntax or the mode matters", () => {
    expect(searchPath("gold OR silver")).toBe("/search/gold%20OR%20silver");
    expect(searchPath("-gold")).toBe("/search/-gold");
    expect(searchPath('"cross of gold"', "all")).toBe("/search/%22cross%20of%20gold%22");
    expect(searchPath("copperhead*")).toBe("/search/copperhead*");
  });

  it("reads slugs and legacy paths back to a query", () => {
    expect(queryForPath("/search/fifteenth-amendment")).toBe("fifteenth amendment");
    expect(queryForPath("/search/%22cross%20of%20gold%22")).toBe('"cross of gold"');
    expect(queryForPath("/search/Know-Nothing")).toBe("Know-Nothing");
  });

  it("keeps hyphens outside phrase mode and encodes non-ASCII slugs", () => {
    expect(queryForPath("/search/well-known", "all")).toBe("well-known");
    expect(searchPath("well-known", "all")).toBe("/search/well-known");
    expect(searchPath("日本")).toBe("/search/%E6%97%A5%E6%9C%AC");
    expect(queryForPath(searchPath("日本"))).toBe("日本");
    expect(searchPath("İstanbul")).toBe("/search/%C4%B0stanbul");
  });
});
