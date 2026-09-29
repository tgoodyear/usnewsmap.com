import { describe, expect, it } from "vitest";
import { indexSummary } from "./indexSummary";

describe("indexSummary", () => {
  it("gives the page and newspaper counts with the version", () => {
    expect(indexSummary({ index_version: "pages-v20260929-2", pages: 2747479, titles: 687 })).toBe(
      "2,747,479 pages from 687 newspapers. Index pages-v20260929-2.",
    );
  });

  it("falls back to the version alone when the API has no page count", () => {
    expect(indexSummary({ index_version: "fixture-v1", titles: 6 })).toBe("Index fixture-v1.");
  });
});
