import { describe, expect, it } from "vitest";
import { topPlace } from "./SampleResults";

describe("sample result city", () => {
  it("chooses the most hits across the whole search, breaking ties by place ID", () => {
    const places = { id: ["P3", "P2", "P1"], hits: [1, 8, 8], first_day: [0, 0, 0] };
    expect(topPlace(places)).toBe("P1");
    expect(topPlace({ ...places, hits: [9, 8, 8] })).toBe("P3");
    expect(topPlace({ ...places, hits: [0, 0, 0] })).toBeNull();
    expect(topPlace({ id: [], hits: [], first_day: [] })).toBeNull();
  });
});
