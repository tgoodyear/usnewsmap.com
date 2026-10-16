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
const scratchBySums = new WeakMap<StateSums, { hits: Float64Array; pages: Float64Array }>();

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
    return prefixSums(cube, STATE_CODES.length, data.bucket.count, (p) => states[p] ?? STATE_CODES.length - 1);
  };
  return { hits: reduce(data.cube, data.places.id), pages: reduce(coverage.pages, coverage.places) };
}

/** Each frame is two prefix subtractions per state, not per place. */
export function stateTotals(sums: StateSums, t: number, window: number | null): StateTotal[] {
  let scratch = scratchBySums.get(sums);
  if (!scratch) {
    scratch = { hits: new Float64Array(sums.hits.places), pages: new Float64Array(sums.pages.places) };
    scratchBySums.set(sums, scratch);
  }
  const hits = windowValues(sums.hits, t, window, scratch.hits);
  const pages = windowValues(sums.pages, t, window, scratch.pages);
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
  const rates: number[] = [];
  for (const row of rows) {
    if (row.code && row.rate !== null && row.rate > 0) rates.push(row.rate);
  }
  rates.sort((a, b) => a - b);
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
  for (let i = 0; i < breaks.length; i++) {
    if (row.rate <= breaks[i]!) return STATE_COLORS[i]!;
  }
  return STATE_COLORS[4]!;
}
export const formatStateRate = (rate: number | null, digits = 3) =>
  rate === null ? "—" : rate.toLocaleString("en-US", { maximumSignificantDigits: digits });

/** Increase precision until unequal class thresholds have unequal labels. */
export function stateBreakLabels(breaks: number[]): string[] {
  let labels: string[] = [];
  for (let digits = 3; digits <= 17; digits++) {
    labels = breaks.map((rate) => formatStateRate(rate, digits));
    if (breaks.every((rate, i) => i === 0 || rate === breaks[i - 1] || labels[i] !== labels[i - 1])) break;
  }
  return labels;
}
export const stateDescription = (r: StateTotal) =>
  `${r.name} · ${r.hits.toLocaleString("en-US")} matching pages · ${r.pages.toLocaleString("en-US")} pages published · ${formatStateRate(r.rate)} per 1,000`;
