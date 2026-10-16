import { describe, expect, it } from "vitest";
import type { AggregateResponse, CoverageResponse, PlaceFeature } from "../api/types";
import {
  prepareStates,
  stateTotals,
  stateBreaks,
  stateColor,
  stateBreakLabels,
  formatStateRate,
  NO_MATCHES,
  NO_PAGES,
  type StateTotal,
} from "./states";
const feature = (state: string) => ({ properties: { state } }) as PlaceFeature;
const data = {
  index_version: "test",
  bucket: { unit: "month", from: "1896-01-01", to: "1896-02-28", count: 2 },
  places: { id: ["a", "b", "unknown", "missing"], hits: [4, 6, 2, 1] },
  cube: { p: [0, 1, 2, 3], b: [0, 1, 1, 0], h: [4, 6, 2, 1] },
} as AggregateResponse;
const coverage: CoverageResponse = {
  index_version: "test",
  bucket: "month",
  from: "1896-01-01",
  to: "1896-02-28",
  count: 2,
  places: ["b", "zero-hits", "a", "zero-state"],
  pages: { p: [0, 1, 2, 3], b: [1, 0, 0, 1], h: [60, 100, 40, 50] },
};
const features = new Map([
  ["a", feature("NY")],
  ["b", feature("NY")],
  ["zero-hits", feature("NY")],
  ["unknown", feature("XX")],
  ["zero-state", feature("DC")],
]);

describe("state window totals", () => {
  it("sums before dividing, includes coverage-only places, DC and unassigned hits", () => {
    const totals = stateTotals(prepareStates(data, coverage, features)!, 1, null);
    expect(totals.find((r) => r.code === "NY")).toMatchObject({ hits: 10, pages: 200, rate: 50 });
    expect(totals.find((r) => r.code === "DC")).toMatchObject({ hits: 0, pages: 50, rate: 0 });
    expect(totals.find((r) => r.code === "AK")).toMatchObject({ hits: 0, pages: 0, rate: null });
    expect(totals.find((r) => r.code === "")).toMatchObject({ hits: 3, pages: 0, rate: null });
    expect(totals.reduce((sum, r) => sum + r.hits, 0)).toBe(13);
    expect(totals.find((r) => r.code === "GU")).toBeDefined();
  });
  it("uses the same cumulative and trailing bucket window for numerator and denominator", () => {
    const sums = prepareStates(data, coverage, features)!;
    expect(stateTotals(sums, 0, null).find((r) => r.code === "NY")).toMatchObject({ hits: 4, pages: 140 });
    expect(stateTotals(sums, 1, 1).find((r) => r.code === "NY")).toMatchObject({ hits: 6, pages: 60, rate: 100 });
    expect(stateTotals(sums, 0, 1).reduce((n, r) => n + r.hits, 0)).toBe(5);
  });
  it("reuses private window scratch without mutating totals returned for previous windows", () => {
    const sums = prepareStates(data, coverage, features)!;
    const earlier = stateTotals(sums, 0, null);
    const snapshot = structuredClone(earlier);
    const later = stateTotals(sums, 1, 1);
    expect(later.find((row) => row.code === "NY")).toMatchObject({ hits: 6, pages: 60 });
    expect(earlier).toEqual(snapshot);
    expect(stateTotals(sums, 0, null)).toEqual(snapshot);
  });
  it("rejects stale or misaligned coverage instead of calling unknown baselines zero", () => {
    for (const patch of [
      { index_version: "old" },
      { count: 3 },
      { from: "1895-01-01" },
      { to: "1897-01-01" },
      { bucket: "year" as const },
    ]) {
      expect(prepareStates(data, { ...coverage, ...patch }, features)).toBeNull();
    }
  });
});
describe("five quantile classes", () => {
  it("preserves small positive rates and distinguishes nearby unequal thresholds", () => {
    expect(formatStateRate(0.001)).toBe("0.001");
    expect(stateBreakLabels([0.001, 0.002, 0.003, 0.004, 0.005])).toEqual([
      "0.001",
      "0.002",
      "0.003",
      "0.004",
      "0.005",
    ]);
    const close = [1.000001, 1.000002, 1.000003, 1.000004, 1.000005];
    expect(new Set(stateBreakLabels(close)).size).toBe(5);
    expect(new Set(stateBreakLabels([1, 1 + Number.EPSILON])).size).toBe(2);
  });
  const row = (rate: number | null): StateTotal => ({
    code: "NY",
    name: "New York",
    hits: rate ?? 0,
    pages: rate === null ? 0 : 1000,
    rate,
  });
  it("handles no states, all zeros, a single state and ties deterministically", () => {
    expect(stateBreaks([])).toEqual([0, 0, 0, 0, 0]);
    expect(stateBreaks([row(0)])).toEqual([0, 0, 0, 0, 0]);
    expect(stateBreaks([row(5)])).toEqual([5, 5, 5, 5, 5]);
    expect(stateBreaks([row(1), row(1), row(2), row(2), row(3)])).toEqual([1, 1, 2, 2, 3]);
    expect(stateColor(row(null), [])).toEqual(NO_PAGES);
    expect(stateColor(row(0), [])).toEqual(NO_MATCHES);
    expect(stateColor(row(1), [1, 1, 2, 2, 3])).not.toEqual(NO_MATCHES);
  });
});
