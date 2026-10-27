import { expect, test } from "@playwright/test";
import manifest from "../basemap.json" with { type: "json" };

test("the real overview basemap renders with all external requests blocked", async ({ page }) => {
  const external: string[] = [];
  const ranges: number[] = [];
  const sprites: number[] = [];
  await page.addInitScript(() => Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true }));
  await page.route("https://tiles.openfreemap.org/**", (route) => {
    external.push(route.request().url());
    return route.abort();
  });
  page.on("response", (response) => {
    if (response.url().endsWith("/overview.pmtiles.bin")) ranges.push(response.status());
    if (response.url().includes("/sprites/ofm")) sprites.push(response.status());
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31");
  const map = page.getByTestId("map");
  await expect(map).toHaveAttribute("data-basemap-ready", "true", { timeout: 25_000 });
  await expect(map).toHaveAttribute("data-basemap-error", "false");
  await expect(map.locator(".maplibregl-ctrl-attrib")).toContainText("OpenStreetMap");
  expect(external).toEqual([]);
  expect(ranges.length).toBeGreaterThan(1);
  expect(ranges.every((status) => status === 206)).toBe(true);
  expect(sprites.length).toBe(2);
  expect(sprites.every((status) => status === 200)).toBe(true);
});

test("the published archive honors cacheable byte ranges, including unsatisfiable ranges", async ({ request }) => {
  test.skip(!process.env.PW_BASE_URL, "requires the API's cache/range contract, not vite preview");
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

test("capture rejects a corrupt archive even when its HTTP range response succeeds", async ({ page }) => {
  await page.addInitScript(() => Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true }));
  await page.route("**/overview.pmtiles.bin", async (route) => {
    const response = await route.fetch();
    const body = await response.body();
    body.fill(0);
    await route.fulfill({ response, body });
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31");
  const map = page.getByTestId("map");
  await expect(map).toHaveAttribute("data-basemap-error", "true");
  await expect(map).toHaveAttribute("data-basemap-ready", "false");
});
