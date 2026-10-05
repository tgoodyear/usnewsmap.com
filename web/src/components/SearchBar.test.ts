import { describe, expect, it } from "vitest";
import { DEFAULTS } from "../state/url";
import { activeOptions, searchKey } from "./SearchBar";

describe("activeOptions", () => {
  it("counts only options that differ from the defaults", () => {
    expect(activeOptions(DEFAULTS)).toBe(0);
    expect(activeOptions({ ...DEFAULTS, mode: "all" })).toBe(1);
    expect(activeOptions({ ...DEFAULTS, from: "1890-01-01", to: "1900-12-31", state: ["GA", "SC"] })).toBe(3);
    // Any number of languages is one option.
    expect(activeOptions({ ...DEFAULTS, lang: ["ger", "spa"] })).toBe(1);
  });
});

describe("searchKey", () => {
  it("changes with the language filter, so the form shows the URL's choice", () => {
    expect(searchKey({ ...DEFAULTS, q: "gold", lang: ["ger"] })).not.toBe(searchKey({ ...DEFAULTS, q: "gold" }));
    // The newspaper filter is part of the search too (#121).
    expect(searchKey({ ...DEFAULTS, q: "gold", lccn: ["sn99000001"] })).not.toBe(searchKey({ ...DEFAULTS, q: "gold" }));
  });
});
