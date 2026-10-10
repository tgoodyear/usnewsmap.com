import { describe, expect, it } from "vitest";
import { congressFeature } from "./congress";

describe("congressFeature", () => {
  it("places Congress at its own point, longitude first", () => {
    const f = congressFeature({ id: "PC001", name: "Congress (Washington, DC)", lat: 38.89, lon: -77.01 });
    expect(f.id).toBe("PC001");
    expect(f.geometry.coordinates).toEqual([-77.01, 38.89]);
    expect(f.properties.state).toBe("DC");
  });
});
