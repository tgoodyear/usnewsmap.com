import { describe, expect, it } from "vitest";
import { searchQuery, type SearchParams } from "./api/client";
import { EXAMPLES } from "./examples";
import { DEFAULTS, parseView, serializeView } from "./state/url";

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
      // The search parameters App.tsx derives from the view.
      const params: SearchParams = {
        q: view.q,
        mode: view.mode,
        near: view.near,
        from: view.from,
        to: view.to,
        bucket: view.bucket,
        state: view.state,
      };
      const sent = searchQuery(params, "VERSION");
      sent.delete("v");
      expect(pairs(ex.aggregate), ex.id).toEqual(pairs(sent));
    }
  });
});
