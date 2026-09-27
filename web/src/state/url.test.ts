import { describe, expect, it } from "vitest";
import { DEFAULTS, isIsoDate, parseView, serializeView } from "./url";

describe("view URL", () => {
  it("round-trips and writes only non-defaults", () => {
    const v = parseView(
      "?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31&bucket=week&t=1896-07-12&win=4&layer=heat&norm=rel&state=il,ny&place=P00412&z=4.2&c=-92.1,39.4",
    );
    expect(v.q).toBe('"cross of gold"');
    expect(v.state).toEqual(["IL", "NY"]);
    expect(v.win).toBe(4);
    expect(v.c).toEqual([-92.1, 39.4]);
    expect(parseView(serializeView(v))).toEqual(v);
    expect(serializeView(DEFAULTS)).toBe("");
    expect(serializeView({ ...DEFAULTS, q: "fever" })).toBe("?q=fever");
  });

  it("drops invalid values instead of trusting them", () => {
    const v = parseView(
      "?mode=evil&bucket=hour&from=1896-6-1&win=-3&layer=3d&z=99&c=500,1&place=%3Cscript%3E&state=Illinois",
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

  it("keeps near only in near mode", () => {
    expect(serializeView({ ...DEFAULTS, q: "a b", mode: "near", near: 8 })).toBe(
      "?q=a+b&mode=near&near=8",
    );
    expect(serializeView({ ...DEFAULTS, q: "a b", near: 8 })).toBe("?q=a+b");
  });
});
