import { describe, expect, it } from "vitest";
import { DEFAULTS } from "../state/url";
import { activeOptions } from "./SearchBar";

describe("activeOptions", () => {
  it("counts only options that differ from the defaults", () => {
    expect(activeOptions(DEFAULTS)).toBe(0);
    expect(activeOptions({ ...DEFAULTS, mode: "all" })).toBe(1);
    expect(activeOptions({ ...DEFAULTS, from: "1890-01-01", to: "1900-12-31", state: ["GA", "SC"] })).toBe(3);
  });
});
