import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { expect, it } from "vitest";
import { STATE_NAMES } from "./states";

it("ships a <=100 KB gzip Census asset matching all 56 state/territory names", () => {
  const bytes = readFileSync("src/assets/states.geojson");
  const asset = JSON.parse(bytes.toString());
  expect(gzipSync(bytes, { level: 9 }).length).toBeLessThanOrEqual(100_000);
  expect(asset.type).toBe("FeatureCollection");
  expect(asset.features).toHaveLength(56);
  expect(
    Object.fromEntries(
      asset.features.map((f: { properties: { STUSPS: string; NAME: string } }) => [
        f.properties.STUSPS,
        f.properties.NAME,
      ]),
    ),
  ).toEqual(STATE_NAMES);
  for (const f of asset.features) {
    expect(Object.keys(f.properties).sort()).toEqual(["NAME", "STUSPS"]);
    expect(["Polygon", "MultiPolygon"]).toContain(f.geometry.type);
  }
});
