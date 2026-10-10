import { describe, expect, it } from "vitest";
import { searchQuery } from "./api/client";
import type { Example } from "./examples";
import {
  EXAMPLE_ORDER,
  EXAMPLES,
  EXAMPLES_MORE,
  EXAMPLES_SHOWN,
  examplesShown,
  mixByEra,
  setsOf,
  shuffle,
} from "./examples";
import raw from "./examples.json";
import { DEFAULTS, isIsoDate, parseView, searchParams, serializeView } from "./state/url";
import type { ViewState } from "./state/url";

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

// The shape of web/src/examples.json, which examples.ts imports with a cast
// and the API reads too (crates/usnm-api/src/prewarm.rs, which rejects
// unknown fields). Typed as a record of `Example`'s keys, so adding a field
// to the interface without listing it here fails to compile.
const FIELDS: Record<keyof Example, "string" | "view"> = {
  aggregate: "string",
  blurb: "string",
  era: "string",
  id: "string",
  title: "string",
  view: "view",
};

/** The `ViewState` keys whose default is null: their type isn't in DEFAULTS. */
type NullByDefault = { [K in keyof ViewState]: null extends ViewState[K] ? K : never }[keyof ViewState];
const isNumber = (v: unknown) => typeof v === "number" && Number.isFinite(v);
const NULLABLE: Record<NullByDefault, (v: unknown) => boolean> = {
  c: (v) => Array.isArray(v) && v.length === 2 && v.every(isNumber),
  win: isNumber,
  z: isNumber,
};

/** Whether `v` has the type of the view field `key`, judged by its default. */
function viewValueOk(key: keyof ViewState, v: unknown): boolean {
  const d = DEFAULTS[key];
  if (d === null) return v === null || NULLABLE[key as NullByDefault](v);
  if (Array.isArray(d)) return Array.isArray(v) && v.every((x) => typeof x === "string");
  return typeof v === typeof d;
}

/** What is wrong with the shape of one entry of examples.json. */
function shapeProblems(entry: unknown): string[] {
  if (typeof entry !== "object" || entry === null || Array.isArray(entry)) return ["not an object"];
  const out: string[] = [];
  const e = entry as Record<string, unknown>;
  for (const k of Object.keys(FIELDS)) if (!(k in e)) out.push(`missing ${k}`);
  for (const [k, v] of Object.entries(e)) {
    const kind = FIELDS[k as keyof Example];
    if (!kind) out.push(`unknown field ${k}`);
    else if (kind === "string" && typeof v !== "string") out.push(`${k} is not a string`);
    else if (kind === "view") {
      if (typeof v !== "object" || v === null || Array.isArray(v)) {
        out.push("view is not an object");
        continue;
      }
      for (const [vk, vv] of Object.entries(v)) {
        if (!Object.hasOwn(DEFAULTS, vk)) out.push(`view: unknown field ${vk}`);
        else if (!viewValueOk(vk as keyof ViewState, vv)) out.push(`view.${vk} has the wrong type`);
      }
    }
  }
  return out;
}

describe("examples.json", () => {
  it("holds exactly the Example fields, with views of ViewState fields", () => {
    const entries: unknown[] = raw;
    expect(entries.length).toBeGreaterThan(0);
    entries.forEach((entry, i) => {
      const id = (entry as { id?: unknown } | null)?.id;
      expect(shapeProblems(entry), `entry ${i} (${String(id)})`).toEqual([]);
    });
  });

  it("would catch a missing field, an extra one, or a wrong view", () => {
    const good = raw[0]!;
    expect(shapeProblems(good)).toEqual([]);
    const noEra: Record<string, unknown> = { ...good };
    delete noEra.era;
    expect(shapeProblems(noEra)).toEqual(["missing era"]);
    expect(shapeProblems({ ...good, note: "x" })).toEqual(["unknown field note"]);
    expect(shapeProblems({ ...good, title: 1 })).toEqual(["title is not a string"]);
    expect(shapeProblems({ ...good, view: [] })).toEqual(["view is not an object"]);
    expect(shapeProblems({ ...good, view: { query: "x" } })).toEqual(["view: unknown field query"]);
    expect(shapeProblems({ ...good, view: { lang: "ger" } })).toEqual(["view.lang has the wrong type"]);
    expect(shapeProblems({ ...good, view: { near: "5" } })).toEqual(["view.near has the wrong type"]);
    expect(shapeProblems({ ...good, view: { c: [1] } })).toEqual(["view.c has the wrong type"]);
    expect(shapeProblems({ ...good, view: { z: 4, c: [-90, 40], win: null } })).toEqual([]);
    expect(shapeProblems(null)).toEqual(["not an object"]);
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

  it("mixes eras, so the first three and every row of three after them span three", () => {
    for (let start = 1; start <= 50; start++) {
      let seed = start;
      const random = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
      const order = mixByEra(EXAMPLES, random);
      expect(order).toHaveLength(EXAMPLES.length);
      expect(new Set(order)).toEqual(new Set(EXAMPLES));
      setsOf(order).forEach((set, i) => {
        const eras = set.map((e) => e.era);
        expect(new Set(eras).size, `seed ${start}, set ${i}: ${eras.join(", ")}`).toBe(set.length);
      });
      expect(new Set(order.slice(0, EXAMPLES_SHOWN).map((e) => e.era)).size).toBe(EXAMPLES_SHOWN);
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
        setsOf(order).forEach((set, i) => {
          const eras = set.map((e) => e.era);
          expect(new Set(eras).size, `${count} examples, seed ${start}, set ${i}`).toBe(set.length);
        });
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

  it("cuts an order into sets from the start", () => {
    expect(setsOf(["a", "b", "c", "d"], 3)).toEqual([["a", "b", "c"], ["d"]]);
    expect(setsOf(["a", "b", "c"], 3)).toEqual([["a", "b", "c"]]);
    expect(setsOf([], 3)).toEqual([]);
  });
});

describe("examples shown", () => {
  it("starts with the first three, then adds ten at a time until all have shown", () => {
    expect(examplesShown(EXAMPLE_ORDER, 0)).toEqual(EXAMPLE_ORDER.slice(0, EXAMPLES_SHOWN));
    let before: readonly unknown[] = [];
    let clicks = 0;
    for (; ; clicks++) {
      const shown = examplesShown(EXAMPLE_ORDER, clicks);
      // Adds to what was shown, never replaces it.
      expect(shown.slice(0, before.length)).toEqual(before);
      // Ten more each time, until the last press shows what remains.
      const want = Math.min(EXAMPLES_SHOWN + clicks * EXAMPLES_MORE, EXAMPLE_ORDER.length);
      expect(shown, `after ${clicks} presses`).toHaveLength(want);
      expect(new Set(shown).size).toBe(shown.length);
      before = shown;
      if (shown.length === EXAMPLE_ORDER.length) break;
    }
    // 100 examples: 3, then 13, 23, ... 93, then the last 7.
    expect(clicks).toBe(Math.ceil((EXAMPLES.length - EXAMPLES_SHOWN) / EXAMPLES_MORE));
    expect(new Set(before)).toEqual(new Set(EXAMPLES));
    // Pressing on past the end changes nothing.
    expect(examplesShown(EXAMPLE_ORDER, clicks + 5)).toEqual(before);
  });

  it("handles short lists and custom steps", () => {
    expect(examplesShown(["a", "b"], 0)).toEqual(["a", "b"]);
    expect(examplesShown(["a", "b", "c", "d", "e"], 0, 2, 2)).toEqual(["a", "b"]);
    expect(examplesShown(["a", "b", "c", "d", "e"], 1, 2, 2)).toEqual(["a", "b", "c", "d"]);
    expect(examplesShown(["a", "b", "c", "d", "e"], 2, 2, 2)).toEqual(["a", "b", "c", "d", "e"]);
    expect(examplesShown(["a", "b", "c"], -1, 2, 2)).toEqual(["a", "b"]);
  });
});
