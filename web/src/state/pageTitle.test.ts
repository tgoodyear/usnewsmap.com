// @vitest-environment node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { EXAMPLES } from "../examples";
import { pageFor, TITLES } from "../route";
import { DEFAULTS, parseViewUrl, searchTitle } from "./url";

// The cases crates/usnm-api/src/site/head.rs checks too, so a page keeps the
// title the API wrote when the app starts.
const cases: { title: string; url: string }[] = JSON.parse(
  readFileSync(resolve(__dirname, "../../../fixtures/page-titles.json"), "utf8"),
);

describe("searchTitle", () => {
  it.each(cases)("matches the API for $url", ({ title, url }) => {
    const page = pageFor(url.split("?")[0]!);
    expect(page === "search" ? searchTitle(parseViewUrl(url)) : TITLES[page]).toBe(title);
  });

  it("gives every example its own title", () => {
    const titles = EXAMPLES.map((ex) => searchTitle({ ...DEFAULTS, q: ex.view.q ?? "" }));
    expect(titles).toEqual(EXAMPLES.map((ex) => `${ex.title} · US News Map`));
    expect(new Set(titles).size).toBe(EXAMPLES.length);
  });

  it("matches an example with its filters", () => {
    for (const ex of EXAMPLES) {
      expect(searchTitle({ ...DEFAULTS, ...ex.view }), ex.id).toBe(`${ex.title} · US News Map`);
    }
  });

  it("keeps the home title when there are no search terms", () => {
    expect(searchTitle({ ...DEFAULTS, from: "1900-01-01" })).toBe(TITLES.search);
  });

  it("keeps long and non-ASCII searches in the limit, in characters", () => {
    for (const q of ["word ".repeat(50), "x".repeat(256), "日本語".repeat(80), "𝒳".repeat(80)]) {
      expect([...searchTitle({ ...DEFAULTS, q })].length).toBeLessThanOrEqual(60);
    }
  });
});
