import { expect, test } from "@playwright/test";
import manifest from "../basemap.json" with { type: "json" };

test("the real overview basemap renders with all external requests blocked", async ({ page }) => {
  const external: string[] = [];
  const ranges: number[] = [];
  await page.addInitScript(() => Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true }));
  await page.route("https://tiles.openfreemap.org/**", (route) => {
    external.push(route.request().url());
    return route.abort();
  });
  page.on("response", (response) => {
    if (response.url().endsWith("/overview.pmtiles.bin")) ranges.push(response.status());
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31");
  const map = page.getByTestId("map");
  await expect(map).toHaveAttribute("data-basemap-ready", "true", { timeout: 25_000 });
  await expect(map).toHaveAttribute("data-basemap-error", "false");
  await expect(map.locator(".maplibregl-ctrl-attrib")).toContainText("OpenStreetMap");
  expect(external).toEqual([]);
  expect(ranges.length).toBeGreaterThan(1);
  expect(ranges.every((status) => status === 206)).toBe(true);
});

test("the published archive honors cacheable byte ranges, including unsatisfiable ranges", async ({ request }) => {
  const path = `/basemap/${manifest.version}/overview.pmtiles.bin`;
  const response = await request.get(path, { headers: { Range: "bytes=0-126", "Accept-Encoding": "gzip, br" } });
  expect(response.status()).toBe(206);
  expect(response.headers()["content-range"]).toMatch(/^bytes 0-126\/\d+$/);
  expect(response.headers()["cache-control"]).toContain("immutable");
  expect(response.headers()["content-encoding"]).toBeUndefined();
  expect((await response.body()).subarray(0, 7).toString()).toBe("PMTiles");
  const invalid = await request.get(path, { headers: { Range: "bytes=999999999-" } });
  expect(invalid.status()).toBe(416);
});
