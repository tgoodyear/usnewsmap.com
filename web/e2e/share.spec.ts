import { expect, test, type APIRequestContext } from "@playwright/test";

test("Share copies immediately and warms the actual current view before reporting readiness", async ({
  page,
  request,
}) => {
  test.skip(!process.env.PW_BASE_URL, "requires the API serving the built app and Chromium");
  test.setTimeout(60_000);
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "clipboard", {
      value: {
        writeText: async (text: string) => {
          document.documentElement.dataset.copiedLink = text;
        },
      },
    });
  });
  const requests: string[] = [];
  page.on("request", (request) => {
    if (new URL(request.url()).pathname === "/share-image.png") requests.push(request.url());
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&state=IL&bucket=week");
  const share = page.getByRole("button", { name: "Share", exact: true });
  await expect(share).toBeVisible();
  await share.evaluate((button) => {
    button.addEventListener(
      "click",
      () => {
        document.documentElement.dataset.shareClickUrl = window.location.href;
      },
      { once: true },
    );
  });
  expect(requests).toEqual([]);
  await share.click();
  await expect(page.getByRole("button", { name: "Link copied", exact: true })).toBeVisible();
  const link = await page.locator("html").getAttribute("data-share-click-url");
  await expect(page.locator("html")).toHaveAttribute("data-copied-link", link!);
  await expect(page.locator(".share-status")).toHaveText("Preview ready for copied link", { timeout: 50_000 });
  expect(requests.length).toBeGreaterThan(0);
  expect(new URL(requests[0]!).searchParams.get("state")).toBe("IL");
  const cached = await request.get(requests[0]!);
  expect(cached.status()).toBe(200);
  expect(cached.headers()["cache-control"]).toContain("max-age=3600");
  expect((await cached.body()).subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
});

async function screenshot(request: APIRequestContext, path: string): Promise<Buffer> {
  let png: Buffer = Buffer.alloc(0);
  await expect
    .poll(
      async () => {
        const response = await request.get(path, { timeout: 10_000 });
        expect(response.status()).toBe(200);
        expect(response.headers()["content-type"]).toBe("image/png");
        if (response.headers()["cache-control"] === "no-store") return false;
        png = await response.body();
        return true;
      },
      { timeout: 50_000, intervals: [500, 1000, 2000] },
    )
    .toBe(true);
  return png;
}

test("search previews are rendered PNGs of the linked view and cached", async ({ request, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(90_000);
  const search = "q=cross+of+gold&from=1896-01-01&to=1896-12-31";
  const html = await (await request.get(`/?${search}`)).text();
  const image = /property="og:image" content="([^"]+)"/.exec(html)?.[1];
  expect(image).toContain("/share-image.png?");
  const imagePath = new URL(image!.replaceAll("&amp;", "&")).pathname + new URL(image!.replaceAll("&amp;", "&")).search;
  const png = await screenshot(request, imagePath);
  expect(png.subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
  expect(png.readUInt32BE(16)).toBe(1200);
  expect(png.readUInt32BE(20)).toBe(630);
  expect(await (await request.get(imagePath)).body()).toEqual(png);
  expect(await screenshot(request, `${imagePath}&state=GA&t=1896-08-01`)).not.toEqual(png);
});

test("selected-place screenshots wait for page results and reject failed panels", async ({ page, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app");
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/v1/hits?**", async (route) => {
    await gate;
    await route.continue();
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&place=P00001");
  await expect(page.locator(".summary")).toContainText("places");
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "false");
  release();
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "true");
  await expect(page.locator(".hits .hit").first()).toBeVisible();
  await page.unroute("**/v1/hits?**");
  await page.route("**/v1/hits?**", (route) =>
    route.fulfill({
      status: 400,
      contentType: "application/problem+json",
      body: JSON.stringify({ type: "about:blank", title: "Page results unavailable", status: 400 }),
    }),
  );
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&place=P00002");
  await expect(page.locator("main.results")).toHaveAttribute("data-share-error", "true", { timeout: 15_000 });
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "false");
});

test("native states preview accepts a fully rendered ocean viewport", async ({ request, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(60_000);
  const png = await screenshot(
    request,
    "/share-image.png?page=%2F&q=cross+of+gold&from=1896-01-01&to=1896-12-31&layer=states&z=12&c=0,0",
  );
  expect(png.subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
});

test("baseline-dependent table previews wait for coverage and reject failures", async ({ page, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app");
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/v1/coverage?**", async (route) => {
    await gate;
    await route.continue();
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&tab=table");
  await expect(page.locator(".summary")).toContainText("places");
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "false");
  release();
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "true");
  await page.unroute("**/v1/coverage?**");
  await page.route("**/v1/coverage?**", (route) =>
    route.fulfill({
      status: 400,
      contentType: "application/problem+json",
      body: JSON.stringify({ type: "about:blank", title: "Coverage unavailable", status: 400 }),
    }),
  );
  await page.reload();
  await expect(page.locator("main.results")).toHaveAttribute("data-share-error", "true", { timeout: 15_000 });
  await expect(page.locator("main.results")).toHaveAttribute("data-share-ready", "false");
});

test("zero-result native states preview waits for its lazy map", async ({ request, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(60_000);
  const png = await screenshot(
    request,
    "/share-image.png?page=%2F&q=unmatchedfixtureword&from=1896-01-01&to=1896-12-31&layer=states",
  );
  expect(png.subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
});

test("capture waits beyond the interactive fallback for a real basemap", async ({ page, isMobile }) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app");
  await page.addInitScript(() => Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true }));
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(/\/(?:styles\/positron|basemap\/[^/]+\/style\.json)$/, async (route) => {
    await gate;
    await route.fulfill({
      json: {
        version: 8,
        sources: {
          land: {
            type: "geojson",
            attribution: "Synthetic basemap",
            data: {
              type: "Feature",
              properties: {},
              geometry: {
                type: "Polygon",
                coordinates: [
                  [
                    [-125, 25],
                    [-66, 25],
                    [-66, 50],
                    [-125, 50],
                    [-125, 25],
                  ],
                ],
              },
            },
          },
        },
        layers: [
          { id: "water", type: "background", paint: { "background-color": "#aaccdd" } },
          { id: "land", type: "fill", source: "land", paint: { "fill-color": "#eeddcc" } },
        ],
      },
    });
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31");
  const map = page.getByTestId("map");
  await expect(map).toBeVisible();
  await page.waitForTimeout(2500);
  expect(await map.getAttribute("data-basemap-ready")).not.toBe("true");
  await expect(map).toHaveAttribute("data-basemap-error", "false");
  release();
  await expect(map).toHaveAttribute("data-basemap-ready", "true");
  await expect(map.locator(".maplibregl-ctrl-attrib")).toContainText("Synthetic basemap");
});
