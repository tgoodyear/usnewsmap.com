import { describe, expect, it } from "vitest";
import { indexSummary } from "./indexSummary";

describe("indexSummary", () => {
  it("gives the page and newspaper counts and the year range", () => {
    expect(
      indexSummary({
        pages: 5263258,
        titles: 1094,
        bounds: { from: "1819-11-20", to: "1963-12-31" },
      }),
    ).toBe("5,263,258 pages from 1,094 newspapers, 1819–1963.");
  });

  it("names a single year once", () => {
    expect(
      indexSummary({
        pages: 1,
        titles: 1,
        bounds: { from: "1896-01-01", to: "1896-12-31" },
      }),
    ).toBe("1 page from 1 newspaper, 1896.");
  });

  it("leaves out the page count when the API has none", () => {
    expect(
      indexSummary({
        titles: 6,
        bounds: { from: "1895-01-01", to: "1897-12-31" },
      }),
    ).toBe("6 newspapers, 1895–1897.");
  });
});
