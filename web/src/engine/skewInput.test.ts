import { describe, expect, it } from "vitest";
import type { AggregateResponse, CoverageResponse, PlaceFeature } from "../api/types";
import { isNonEnglish, prepareSkew } from "./skewInput";

const feature = (id: string, state: string, languages?: string[]): PlaceFeature => ({
  type: "Feature",
  id,
  geometry: { type: "Point", coordinates: [0, 0] },
  properties: { name: id, state, precision: "city", titles: 1, ...(languages ? { languages } : {}) },
});

// Six places with pages in two months; hits in three of them.
const ids = ["A", "B", "C", "D", "E", "F"];
const coverage: CoverageResponse = {
  index_version: "v",
  bucket: "month",
  from: "1896-01-01",
  to: "1896-02-29",
  count: 2,
  places: ids,
  pages: { p: [0, 1, 2, 3, 4, 5, 0], b: [0, 0, 0, 0, 0, 1, 1], h: [10, 20, 30, 40, 50, 60, 5] },
};
const agg: AggregateResponse = {
  index_version: "v",
  synthetic: false,
  query: { canonical: "q=x", ast: "x" },
  bucket: { unit: "month", from: "1896-01-01", to: "1896-02-29", count: 2 },
  coarsened: false,
  total: { hits: 9, places: 3, baseline_pages: 215 },
  series: { hits: [7, 2], baseline: [150, 65] },
  // The aggregate lists only places with hits, in its own order.
  places: { id: ["F", "A", "C"], hits: [1, 4, 4], first_day: [0, 0, 0] },
  cube: { p: [1, 2, 1, 0], b: [0, 0, 1, 1], h: [3, 4, 1, 1], calls: 1, baseline_ref: "/v1/coverage?x" },
};
const features = new Map(
  [
    feature("A", "IL"),
    feature("B", "IL"),
    feature("C", "NY", ["ger"]),
    feature("D", "NY", ["ger", "eng"]),
    feature("E", "CA", []),
    feature("F", "CA"),
  ].map((f) => [f.id, f]),
);

describe("prepareSkew", () => {
  it("joins hits onto the coverage cells in the coverage's place order", () => {
    const p = prepareSkew(agg, coverage, features);
    if (typeof p === "string") throw new Error(p);
    expect(p.placeIds).toEqual(ids);
    expect(Array.from(p.input.cells.hits)).toEqual([3, 0, 4, 0, 0, 1, 1]);
    expect(Array.from(p.input.cells.pages)).toEqual([10, 20, 30, 40, 50, 60, 5]);
    expect(p.input.nationalPages).toEqual([150, 65]);
    expect(p.placesWithPages).toBe(6);
    expect(p.stateCodes).toEqual(["IL", "NY", "CA"]);
    expect(Array.from(p.input.stateOf)).toEqual([0, 0, 1, 1, 2, 2]);
    // Only C prints in other languages only; it is left out of the fit.
    expect(p.nonEnglish).toEqual([false, false, true, false, false, false]);
    expect(Array.from(p.input.inFit)).toEqual([true, true, false, true, true, true]);
  });

  it("is unavailable when filters remove the baselines, buckets differ or places are few", () => {
    expect(prepareSkew({ ...agg, series: { ...agg.series, baseline: null } }, coverage, features)).toBe("filters");
    expect(prepareSkew(agg, { ...coverage, count: 3 }, features)).toBe("mismatch");
    const few = { ...coverage, pages: { p: [0, 1, 2, 3], b: [0, 0, 0, 0], h: [1, 1, 1, 1] } };
    expect(prepareSkew(agg, few, features)).toBe("few-places");
  });

  it("treats a place with no listed languages as English", () => {
    expect(isNonEnglish(feature("X", "TX"))).toBe(false);
    expect(isNonEnglish(feature("X", "TX", []))).toBe(false);
    expect(isNonEnglish(feature("X", "TX", ["spa"]))).toBe(true);
    expect(isNonEnglish(undefined)).toBe(false);
  });
});
