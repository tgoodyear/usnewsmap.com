import type { AggregateResponse, CoverageResponse, PlaceFeature, SparseCube } from "../api/types";
import { prefixSums, windowValues, type PrefixSums } from "./cube";
import names from "../lib/stateNames.json";

export interface StateTotal {
  code: string;
  name: string;
  hits: number;
  pages: number;
  /** Matching pages per 1,000 published; null when the denominator is zero. */
  rate: number | null;
}

export const STATE_NAMES: Readonly<Record<string, string>> = names;
export const STATE_CODES = [...Object.keys(STATE_NAMES), ""];
export interface StateSums {
  hits: PrefixSums;
  pages: PrefixSums;
}

/** Reduce once, retaining coverage-only places and missing/unknown place states. */
export function prepareStates(
  data: AggregateResponse,
  coverage: CoverageResponse,
  features: ReadonlyMap<string, PlaceFeature>,
): StateSums | null {
  if (
    coverage.index_version !== data.index_version ||
    coverage.count !== data.bucket.count ||
    coverage.bucket !== data.bucket.unit ||
    coverage.from !== data.bucket.from ||
    coverage.to !== data.bucket.to
  )
    return null;
  const byCode = new Map(STATE_CODES.map((code, i) => [code, i]));
  const reduce = (cube: SparseCube, ids: string[]): PrefixSums => {
    const states = ids.map((id) => byCode.get(features.get(id)?.properties.state ?? "") ?? STATE_CODES.length - 1);
    return prefixSums(
      { ...cube, p: cube.p.map((p) => states[p] ?? STATE_CODES.length - 1) },
      STATE_CODES.length,
      data.bucket.count,
    );
  };
  return { hits: reduce(data.cube, data.places.id), pages: reduce(coverage.pages, coverage.places) };
}

/** Each frame is two prefix subtractions per state, not per place. */
export function stateTotals(sums: StateSums, t: number, window: number | null): StateTotal[] {
  const hits = windowValues(sums.hits, t, window);
  const pages = windowValues(sums.pages, t, window);
  return STATE_CODES.map((code, i) => ({
    code,
    name: STATE_NAMES[code] ?? "Unassigned",
    hits: hits[i]!,
    pages: pages[i]!,
    rate: pages[i]! > 0 ? (1000 * hits[i]!) / pages[i]! : null,
  }));
}

/** Five positive-rate quantiles; zero is a separate neutral class. Ties stay together. */
export function stateBreaks(rows: StateTotal[]): number[] {
  const rates = rows.flatMap((r) => (r.code && r.rate !== null && r.rate > 0 ? [r.rate] : [])).sort((a, b) => a - b);
  return Array.from({ length: 5 }, (_, i) => rates[Math.max(0, Math.ceil(((i + 1) * rates.length) / 5) - 1)] ?? 0);
}

export const STATE_COLORS: [number, number, number, number][] = [
  [198, 219, 239, 145],
  [158, 202, 225, 145],
  [107, 174, 214, 145],
  [49, 130, 189, 145],
  [8, 81, 156, 145],
];
export const NO_MATCHES: [number, number, number, number] = [247, 247, 239, 145];
export const NO_PAGES: [number, number, number, number] = [142, 142, 142, 145];
export function stateColor(row: StateTotal | undefined, breaks: number[]): [number, number, number, number] {
  if (!row || row.rate === null) return NO_PAGES;
  if (row.hits === 0) return NO_MATCHES;
  const index = breaks.findIndex((b) => row.rate! <= b);
  return STATE_COLORS[index < 0 ? 4 : index]!;
}
export const formatStateRate = (rate: number | null) =>
  rate === null ? "—" : rate.toLocaleString("en-US", { maximumFractionDigits: 2 });
export const stateDescription = (r: StateTotal) =>
  `${r.name} · ${r.hits.toLocaleString("en-US")} matching pages · ${r.pages.toLocaleString("en-US")} pages published · ${formatStateRate(r.rate)} per 1,000`;
