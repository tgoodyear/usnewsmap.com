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
 * The `page`th set of `n` examples from `order`. A pass is every set until
 * each example has shown; the last set of a pass wraps around to the start, so
 * every set is full, and the next page begins the pass again, so paging on
 * shows the same sets `mixByEra` checked.
 */
export function examplesAt<T>(order: readonly T[], page: number, n = EXAMPLES_SHOWN): T[] {
  const count = Math.min(n, order.length);
  const sets = Math.max(Math.ceil(order.length / n), 1);
  const start = (((page % sets) + sets) % sets) * n;
  return Array.from({ length: count }, (_, i) => order[(start + i) % order.length]!);
}

/**
 * A random order in which every set of `n` (as `examplesAt` shows them, the
 * last wrapping around to the start) spans `n` eras. The eras take turns,
 * the biggest first (ties in random order, each era's examples shuffled), so
 * any `n` in a row come from different eras while there are at least `n`;
 * then the whole order starts at a random point where the set that wraps
 * around to the start does too.
 */
export function mixByEra<T extends { era: string }>(
  items: readonly T[],
  random: () => number,
  n = EXAMPLES_SHOWN,
): T[] {
  const groups = new Map<string, T[]>();
  for (const item of items) groups.set(item.era, [...(groups.get(item.era) ?? []), item]);
  const queues = shuffle([...groups.values()], random)
    .sort((a, b) => b.length - a.length)
    .map((g) => shuffle(g, random));
  const turns: T[] = [];
  while (turns.length < items.length) {
    for (const q of queues) {
      const next = q.shift();
      if (next) turns.push(next);
    }
  }
  const spans = (order: T[]) =>
    Array.from({ length: Math.ceil(order.length / n) }, (_, page) => examplesAt(order, page, n)).every(
      (set) => new Set(set.map((e) => e.era)).size === set.length,
    );
  for (const k of shuffle([...turns.keys()], random)) {
    const order = [...turns.slice(k), ...turns.slice(0, k)];
    if (spans(order)) return order;
  }
  return turns;
}

/** This page load's order: random per visit, the same until the page reloads. */
export const EXAMPLE_ORDER: readonly Example[] = mixByEra(EXAMPLES, Math.random);
