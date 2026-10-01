import type { ViewState } from "./state/url";
import examples from "./examples.json";

export interface Example {
  id: string;
  title: string;
  blurb: string;
  view: Partial<ViewState>;
  /**
   * The `/v1/aggregate` query string the app sends for `view`, without `v`.
   * The API warms exactly these searches before it serves a new index
   * version (crates/usnm-api/src/prewarm.rs); examples.test.ts checks they
   * match what the app sends.
   */
  aggregate: string;
  /**
   * The span of years the example belongs to, such as "1860-1877". Only used
   * to mix the examples shown together, so a set spans several eras.
   */
  era: string;
}

// Preset searches (F-30). Each was checked against the real corpus for enough
// pages and places to map (see the pull request that added it). The API reads
// the same file at build time.
export const EXAMPLES: Example[] = examples as Example[];

/** How many examples the home page shows at a time. */
export const EXAMPLES_SHOWN = 3;

/** A copy of `items` in random order (Fisher–Yates), drawing from `random`. */
export function shuffle<T>(items: readonly T[], random: () => number): T[] {
  const out = [...items];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(random() * (i + 1));
    [out[i], out[j]] = [out[j]!, out[i]!];
  }
  return out;
}

/**
 * The `page`th set of `n` examples from `order`, wrapping around the end, so
 * every set is full and paging through shows every example.
 */
export function examplesAt<T>(order: readonly T[], page: number, n = EXAMPLES_SHOWN): T[] {
  const count = Math.min(n, order.length);
  const start = ((page * n) % order.length + order.length) % order.length;
  return Array.from({ length: count }, (_, i) => order[(start + i) % order.length]!);
}

/**
 * A random order that takes one example from each era in turn (each era's
 * examples shuffled), so neighbours come from different eras. The eras go in
 * a random order, smallest first: the biggest then supply the leftovers at
 * the end, and the set that wraps around to the start begins with a
 * different era. With eras of nearly equal size, every set of three in one
 * pass through the examples spans three eras.
 */
export function mixByEra<T extends { era: string }>(items: readonly T[], random: () => number): T[] {
  const groups = new Map<string, T[]>();
  for (const item of items) groups.set(item.era, [...(groups.get(item.era) ?? []), item]);
  const queues = shuffle([...groups.values()], random)
    .sort((a, b) => a.length - b.length)
    .map((g) => shuffle(g, random));
  const out: T[] = [];
  while (out.length < items.length) {
    for (const q of queues) {
      const next = q.shift();
      if (next) out.push(next);
    }
  }
  return out;
}

/** This page load's order: random per visit, the same until the page reloads. */
export const EXAMPLE_ORDER: readonly Example[] = mixByEra(EXAMPLES, Math.random);
