import { describe, expect, it } from "vitest";
import { searchQuery } from "./api/client";
import { EXAMPLE_ORDER, EXAMPLES, EXAMPLES_SHOWN, examplesAt, mixByEra, shuffle } from "./examples";
import { DEFAULTS, isIsoDate, parseView, searchParams, serializeView } from "./state/url";

/** Sorted `key=value` pairs, so parameter order doesn't matter. */
function pairs(query: URLSearchParams | string): string[] {
  return [...new URLSearchParams(query).entries()].map(([k, v]) => `${k}=${v}`).sort();
}

describe("examples", () => {
  it("have unique ids and views that survive the URL", () => {
    expect(new Set(EXAMPLES.map((e) => e.id)).size).toBe(EXAMPLES.length);
    for (const ex of EXAMPLES) {
      const view = { ...DEFAULTS, ...ex.view };
      expect(parseView(serializeView(view))).toEqual(view);
    }
  });

  // The API warms each example's `aggregate` query before serving a new
  // index version; it only helps if that is exactly what the app sends.
  it("name the aggregate query the app sends when one is clicked", () => {
    for (const ex of EXAMPLES) {
      // A click from the home page: the example over the defaults, through the URL.
      const view = parseView(serializeView({ ...DEFAULTS, ...ex.view }));
      // The search App.tsx sends for the view.
      const sent = searchQuery(searchParams(view), "VERSION");
      sent.delete("v");
      expect(pairs(ex.aggregate), ex.id).toEqual(pairs(sent));
    }
  });
});

// The query language's limits (06 §6.4, crates/usnm-core/src/query.rs, and
// `limits` in /v1/meta). The API's own parser checks each example too
// (prewarm.rs `examples_are_valid_aggregate_queries`); this mirrors the limits
// so a bad example fails here first.
const LIMITS = { maxQueryChars: 256, maxTerms: 12, maxOrBranches: 4 };
// What Chronicling America can hold: the corpus's published bounds today are
// 1751-05-09 to 1963-12-31 (/v1/meta), and LoC digitizes nothing later.
const CORPUS = { from: "1751-05-09", to: "1963-12-31" };
const OPERATORS = new Set(["AND", "OR", "NOT"]);
const MAX_TITLE = 32;
const MAX_BLURB = 90;

describe("example queries and copy", () => {
  it("are within the query language's limits", () => {
    for (const ex of EXAMPLES) {
      const q = ex.view.q ?? "";
      expect(q.trim(), ex.id).not.toBe("");
      expect(q.length, ex.id).toBeLessThanOrEqual(LIMITS.maxQueryChars);
      expect(q.split('"').length % 2, `${ex.id}: balanced quotes`).toBe(1);
      const words = q.split(/\s+/);
      const terms = words
        .filter((w) => !OPERATORS.has(w))
        .flatMap((w) => w.split(/[^\p{L}\p{N}]+/u))
        .filter(Boolean);
      expect(terms.length, ex.id).toBeGreaterThan(0);
      expect(terms.length, ex.id).toBeLessThanOrEqual(LIMITS.maxTerms);
      expect(words.filter((w) => w === "OR").length + 1, ex.id).toBeLessThanOrEqual(LIMITS.maxOrBranches);
    }
  });

  it("search windows inside the corpus", () => {
    for (const ex of EXAMPLES) {
      const { from = "", to = "" } = ex.view;
      for (const d of [from, to].filter(Boolean)) {
        expect(isIsoDate(d), `${ex.id}: ${d}`).toBe(true);
        expect(d >= CORPUS.from && d <= CORPUS.to, `${ex.id}: ${d}`).toBe(true);
      }
      if (from && to) expect(from < to, ex.id).toBe(true);
    }
  });

  it("have unique, short titles and blurbs without em dashes", () => {
    const titles = EXAMPLES.map((e) => e.title.toLowerCase());
    const blurbs = EXAMPLES.map((e) => e.blurb.toLowerCase());
    expect(new Set(titles).size).toBe(EXAMPLES.length);
    expect(new Set(blurbs).size).toBe(EXAMPLES.length);
    for (const ex of EXAMPLES) {
      expect(ex.id, ex.id).toMatch(/^[a-z0-9]+(-[a-z0-9]+)*$/);
      expect(ex.title.length, ex.id).toBeLessThanOrEqual(MAX_TITLE);
      expect(ex.blurb.length, ex.id).toBeLessThanOrEqual(MAX_BLURB);
      expect(ex.blurb, ex.id).toMatch(/\.$/);
      expect(`${ex.title} ${ex.blurb}`, ex.id).not.toMatch(/\u2014/);
    }
  });
});

describe("example rotation", () => {
  it("shuffles into a permutation", () => {
    let seed = 1;
    const random = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
    const order = shuffle(EXAMPLES, random);
    expect(order).toHaveLength(EXAMPLES.length);
    expect(new Set(order.map((e) => e.id))).toEqual(new Set(EXAMPLES.map((e) => e.id)));
    expect(new Set(EXAMPLE_ORDER)).toEqual(new Set(EXAMPLES));
  });

  it("mixes eras, so every set in a pass spans three", () => {
    for (let start = 1; start <= 50; start++) {
      let seed = start;
      const random = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
      const order = mixByEra(EXAMPLES, random);
      expect(order).toHaveLength(EXAMPLES.length);
      expect(new Set(order)).toEqual(new Set(EXAMPLES));
      // One pass: every set until each example has shown, including the one
      // that wraps around to the start.
      for (let page = 0; page < Math.ceil(order.length / EXAMPLES_SHOWN); page++) {
        const eras = examplesAt(order, page).map((e) => e.era);
        expect(new Set(eras).size, `seed ${start}, set ${page}: ${eras.join(", ")}`).toBe(EXAMPLES_SHOWN);
      }
    }
  });

  it("mixes any count of examples in nearly equal eras", () => {
    for (let count = 21; count <= 110; count++) {
      // Seven eras whose sizes differ by at most one.
      const items = Array.from({ length: count }, (_, i) => ({ era: `era-${i % 7}`, i }));
      for (let start = 1; start <= 10; start++) {
        let seed = start * 7919 + count;
        const random = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
        const order = mixByEra(items, random);
        expect(new Set(order).size, `${count} examples`).toBe(count);
        for (let page = 0; page < Math.ceil(count / EXAMPLES_SHOWN); page++) {
          const eras = examplesAt(order, page).map((e) => e.era);
          expect(new Set(eras).size, `${count} examples, seed ${start}, set ${page}`).toBe(EXAMPLES_SHOWN);
        }
      }
    }
  });

  it("are spread across eras", () => {
    const sizes = new Map<string, number>();
    for (const ex of EXAMPLES) {
      expect(ex.era, ex.id).toMatch(/^\d{4}-\d{4}$/);
      sizes.set(ex.era, (sizes.get(ex.era) ?? 0) + 1);
    }
    expect(sizes.size).toBeGreaterThanOrEqual(EXAMPLES_SHOWN);
    for (const [era, n] of sizes) expect(n, era).toBeGreaterThanOrEqual(5);
  });

  it("shows full sets that reach every example", () => {
    const seen = new Set<string>();
    const pages = Math.ceil(EXAMPLE_ORDER.length / EXAMPLES_SHOWN);
    for (let page = 0; page < pages; page++) {
      const shown = examplesAt(EXAMPLE_ORDER, page);
      expect(shown).toHaveLength(EXAMPLES_SHOWN);
      expect(new Set(shown).size).toBe(EXAMPLES_SHOWN);
      for (const ex of shown) seen.add(ex.id);
    }
    expect(seen.size).toBe(EXAMPLES.length);
    // Paging on past a pass starts it again: the same, checked, sets.
    for (let page = 0; page < pages; page++) {
      expect(examplesAt(EXAMPLE_ORDER, page + pages)).toEqual(examplesAt(EXAMPLE_ORDER, page));
      expect(examplesAt(EXAMPLE_ORDER, page + 3 * pages)).toEqual(examplesAt(EXAMPLE_ORDER, page));
    }
    expect(examplesAt(["a", "b"], 5)).toEqual(["a", "b"]);
    expect(examplesAt(["a", "b", "c", "d"], 1, 3)).toEqual(["d", "a", "b"]);
    expect(examplesAt(["a", "b", "c", "d"], 2, 3)).toEqual(["a", "b", "c"]);
  });
});
