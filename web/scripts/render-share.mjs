import { parseArgs } from "node:util";
import { chromium } from "playwright-core";

const { values } = parseArgs({ options: { url: { type: "string" } } });
const target = new URL(values.url);
if (
  target.protocol !== "http:" ||
  target.hostname !== "127.0.0.1" ||
  (!["/", "/index.html"].includes(target.pathname) && !target.pathname.startsWith("/search/"))
) {
  throw new Error("Screenshot target must be a local search permalink");
}

const browser = await chromium.launch({
  executablePath: process.env.PW_CHROMIUM_PATH,
  headless: true,
  timeout: 10_000,
  args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader", "--disable-dev-shm-usage"],
});
const deadline = setTimeout(() => void browser.close(), 30_000);
try {
  const context = await browser.newContext({
    viewport: { width: 1200, height: 630 },
    deviceScaleFactor: 1,
    colorScheme: "light",
    reducedMotion: "reduce",
    userAgent: "USNewsMap-Preview/1.0 (screenshot bot)",
    extraHTTPHeaders: { DNT: "1", "Sec-GPC": "1" },
  });
  await context.addInitScript(() => {
    Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true });
  });
  const page = await context.newPage();
  page.setDefaultTimeout(25_000);
  let basemapFailed = false;
  const isBasemapAsset = (value) => {
    const url = new URL(value);
    return (
      url.hostname === "tiles.openfreemap.org" || (url.origin === target.origin && url.pathname.startsWith("/basemap/"))
    );
  };
  page.on("requestfailed", (request) => {
    if (isBasemapAsset(request.url())) basemapFailed = true;
  });
  page.on("response", (response) => {
    if (isBasemapAsset(response.url()) && response.status() >= 400) {
      basemapFailed = true;
    }
  });
  // A search URL cannot turn this into an arbitrary URL screenshot service.
  await page.route("**/*", (route) => {
    const url = new URL(route.request().url());
    const local = url.origin === target.origin;
    const basemap = url.protocol === "https:" && url.hostname === "tiles.openfreemap.org";
    if ((local || basemap) && !["/share-image.png", "/search-map.png"].includes(url.pathname)) {
      return route.continue();
    }
    return route.abort();
  });
  const response = await page.goto(target.href, { waitUntil: "domcontentloaded" });
  if (!response?.ok()) throw new Error("Screenshot page failed to load");
  if (target.pathname.startsWith("/search/") || target.searchParams.get("q")?.trim()) {
    const results = page.locator('[data-share-ready="true"], [data-share-error="true"]');
    await results.waitFor();
    if ((await results.getAttribute("data-share-error")) === "true") {
      throw new Error("Search view results failed to load");
    }
    await page
      .locator('[data-testid="map"], .tables, .results:not(:has(.stage)) .notice:has-text("No pages match.")')
      .first()
      .waitFor();
    if (target.searchParams.get("tab") !== "table" && (await page.locator(".tables").count())) {
      throw new Error("WebGL map is unavailable");
    }
  } else {
    await page.locator("h1").waitFor();
  }
  const map = page.getByTestId("map");
  if (await map.count()) {
    await page
      .locator(
        '[data-testid="map"][data-basemap-error="true"], [data-testid="map"][data-basemap-ready="true"][data-overlay-ready="true"]',
      )
      .waitFor();
    if (basemapFailed || (await map.getAttribute("data-basemap-error")) === "true") {
      throw new Error("Basemap assets failed to load");
    }
  }
  await page.evaluate(() => document.fonts.ready);
  // Wait for the compositor to present both the WebGL overlay and basemap.
  await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
  const image = await page.screenshot({ type: "png", animations: "disabled" });
  process.stdout.write(image);
} finally {
  clearTimeout(deadline);
  await browser.close();
}
