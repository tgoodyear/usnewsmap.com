import { describe, expect, it } from "vitest";
import { DEFAULTS, isIsoDate, parseView, searchParams, serializeView } from "./url";

describe("view URL", () => {
  it("round-trips and writes only non-defaults", () => {
    const v = parseView(
      "?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31&bucket=week&t=1896-07-12&win=4&layer=heat&norm=skew&state=il,ny&lang=ger&place=P00412&sort=newest&z=4.2&c=-92.1,39.4",
    );
    expect(v.q).toBe('"cross of gold"');
    expect(v.state).toEqual(["IL", "NY"]);
    expect(v.lang).toEqual(["ger"]);
    expect(v.win).toBe(4);
    expect(v.c).toEqual([-92.1, 39.4]);
    expect(v.norm).toBe("skew");
    expect(v.sort).toBe("newest");
    expect(parseView(serializeView(v))).toEqual(v);
    expect(serializeView(DEFAULTS)).toBe("");
    expect(serializeView({ ...DEFAULTS, q: "fever" })).toBe("?q=fever");
    expect(parseView("?sort=relevant").sort).toBe("relevant");
  });

  it("drops invalid values instead of trusting them", () => {
    const v = parseView(
      "?mode=evil&bucket=hour&from=1896-6-1&win=-3&layer=3d&z=99&c=500,1&place=%3Cscript%3E&state=Illinois&lang=%3Cb%3E&sort=random",
    );
    expect(v).toEqual(DEFAULTS);
  });

  it("accepts only real calendar dates", () => {
    expect(isIsoDate("1896-02-29")).toBe(true);
    expect(isIsoDate("1897-02-29")).toBe(false);
    expect(isIsoDate("1896-02-31")).toBe(false);
    expect(isIsoDate("1896-13-01")).toBe(false);
    expect(parseView("?from=1896-02-31&to=1896-04-31&t=1896-06-31")).toEqual(DEFAULTS);
  });

  it("rejects blank coordinates and zooms the map can't show", () => {
    for (const q of ["?c=,", "?c=12,", "?c=,40", "?z=20", "?z=1", "?z="]) {
      const v = parseView(q);
      expect([v.c, v.z], q).toEqual([null, null]);
    }
    expect(parseView("?z=12&c=0,0")).toMatchObject({ z: 12, c: [0, 0] });
  });

  it("keeps the measure in the URL; the removed share of pages opens on Pages", () => {
    expect(parseView("?q=fever&norm=skew").norm).toBe("skew");
    expect(serializeView({ ...DEFAULTS, q: "fever", norm: "skew" })).toBe("?q=fever&norm=skew");
    // Old permalinks to the share of pages published fall back to Pages.
    expect(parseView("?q=fever&norm=rel").norm).toBe("raw");
    expect(serializeView(parseView("?q=fever&norm=rel"))).toBe("?q=fever");
    expect(parseView("?q=fever&norm=lift").norm).toBe("raw");
  });

  it("keeps the language filter in the API's canonical order", () => {
    const v = parseView("?q=fever&lang=SPA,ger,spa,%20eng");
    expect(v.lang).toEqual(["eng", "ger", "spa"]);
    expect(serializeView(v)).toBe("?q=fever&lang=eng%2Cger%2Cspa");
    expect(parseView(serializeView(v))).toEqual(v);
    // Unsorted or repeated codes set in code are written canonically too.
    expect(serializeView({ ...DEFAULTS, q: "fever", lang: ["spa", "ger", "spa"] })).toBe("?q=fever&lang=ger%2Cspa");
    expect(searchParams(v).lang).toEqual(["eng", "ger", "spa"]);
    // Anything but three letters is dropped; nothing left means no filter.
    expect(parseView("?q=fever&lang=german,e1g,,pennsylvania%20german").lang).toEqual([]);
    expect(serializeView(parseView("?q=fever&lang=xx"))).toBe("?q=fever");
    expect(parseView("?q=fever&lang=ger,english").lang).toEqual(["ger"]);
  });

  it("keeps near only in near mode", () => {
    expect(serializeView({ ...DEFAULTS, q: "a b", mode: "near", near: 8 })).toBe(
      "?q=a+b&mode=near&near=8",
    );
    expect(serializeView({ ...DEFAULTS, q: "a b", near: 8 })).toBe("?q=a+b");
  });
});
