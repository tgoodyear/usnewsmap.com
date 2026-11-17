import { describe, expect, it } from "vitest";
import { docIdForPath, pageFor, pagePath, queryForPath, searchPath } from "./route";

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
    expect(pageFor("/examples/whigs")).toBe("search");
    expect(pageFor("/examples/whigs/")).toBe("search");
    expect(pageFor("/search/%22cross%20of%20gold%22/")).toBe("search");
    expect(pageFor("/page/loc:sn85055101_1917-04-03_ed-1_seq-3")).toBe("page");
    expect(pageFor("/page/loc%3Asn85055101_1917-04-03_ed-1_seq-3/")).toBe("page");
  });

  it("reads a page's document id from /page/<doc_id>", () => {
    const id = "loc:sn85055101_1917-04-03_ed-1_seq-3";
    expect(docIdForPath(`/page/${id}`)).toBe(id);
    expect(docIdForPath(`/page/${encodeURIComponent(id)}/`)).toBe(id);
    expect(docIdForPath("/page/sn85055101_1917-04-03_ed-1_seq-3")).toBe("sn85055101_1917-04-03_ed-1_seq-3");
    for (const path of [
      "/page",
      "/page/",
      "/page/a/b",
      "/page/a%2Fb",
      "/page/a%20b",
      "/page/%FF",
      "/page/..",
      "/pages/x",
    ]) {
      expect(docIdForPath(path), path).toBeNull();
      expect(pageFor(path), path).toBe("not-found");
    }
    expect(docIdForPath(`/page/${"a".repeat(201)}`)).toBeNull();
    expect(pagePath(id)).toBe(`/page/${id}`);
    expect(docIdForPath(pagePath(id))).toBe(id);
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
      "/examples/",
      "/examples/missing-example",
      "/examples/whigs/extra",
      "/examples/Whigs",
      "/examples/%77higs",
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
    expect(searchPath('"anthracite strike"~5')).toBe("/search/%22anthracite%20strike%22~5");
    expect(queryForPath(searchPath('"anthracite strike"~5'))).toBe('"anthracite strike"~5');
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
