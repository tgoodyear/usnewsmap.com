import { expect, test, type APIRequestContext } from "@playwright/test";

const revisionedPreview = /^\/search-preview\/[0-9a-f]{16}\/[a-z0-9]+(?:-[a-z0-9]+)*\.png$/;

function advertisedImage(html: string, name = "og:image"): string {
  const image = new RegExp(`(?:property|name)="${name}" content="([^"]+)"`).exec(html)?.[1];
  expect(image).toBeDefined();
  const url = new URL(image!.replaceAll("&amp;", "&"));
  const path = url.pathname + url.search;
  expect(url.hash).toBe("");
  if (url.pathname === "/share-image.png") expect(url.search).not.toBe("");
  else expect(path).toMatch(revisionedPreview);
  return path;
}

test("Share copies immediately and warms the actual current view without preview status text", async ({
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
    const url = new URL(request.url());
    if (url.pathname === "/share-image.png" || revisionedPreview.test(url.pathname)) requests.push(request.url());
  });
  await page.goto("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&state=IL&bucket=week");
  const share = page.getByRole("button", { name: "Share", exact: true });
  await expect(share).toBeVisible();
  await expect(page.locator(".share-status")).toBeEmpty();
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
  await expect(page.locator(".share-status")).toBeEmpty();
  await expect(page.getByRole("button", { name: "Link copied", exact: true })).toBeEnabled({ timeout: 50_000 });
  await expect(page.locator(".share-status")).toBeEmpty();
  expect(requests.length).toBeGreaterThan(0);
  const requested = new URL(requests[0]!);
  if (requested.pathname === "/share-image.png") expect(requested.searchParams.get("state")).toBe("IL");
  else expect(requested.search).toBe("");
  const cached = await request.get(requests[0]!);
  expect(cached.status()).toBe(200);
  expect(cached.headers()["cache-control"]).toContain("max-age=86400");
  expect(cached.headers()["cloudflare-cdn-cache-control"]).toBe("public, max-age=1209600");
  expect((await cached.body()).subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
  const warmHtml = await (await request.get(link!)).text();
  expect(advertisedImage(warmHtml)).toBe(new URL(cached.url()).pathname);
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

test("filtered search previews publish immutable PNGs and legacy redirects after rendering", async ({
  request,
  isMobile,
}) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(90_000);
  const filters = "from=1896-01-01&to=1896-12-31&state=IL";
  const search = `q=cross+of+gold&${filters}`;
  const pagePath = `/search/cross-of-gold?${filters}`;
  const html = await (await request.get(pagePath)).text();
  const imagePath = advertisedImage(html);
  const png = await screenshot(request, imagePath);
  expect(png.subarray(0, 8)).toEqual(Buffer.from("\x89PNG\r\n\x1a\n", "binary"));
  expect(png.readUInt32BE(16)).toBe(1200);
  expect(png.readUInt32BE(20)).toBe(630);
  const generic = await (await request.get("/og-image.png")).body();
  expect(png).not.toEqual(generic);

  const legacyPath = `/share-image.png?page=%2F&${search}&v=2`;
  const legacy = await request.get(legacyPath, { maxRedirects: 0 });
  expect(legacy.status()).toBe(307);
  expect(legacy.headers()["cache-control"]).toBe("no-store");
  const preview = legacy.headers()["location"] ?? "";
  expect(preview).toMatch(revisionedPreview);
  const published = await request.get(preview);
  expect(published.status()).toBe(200);
  expect(published.headers()["content-type"]).toBe("image/png");
  expect(published.headers()["cache-control"]).toBe("public, max-age=86400");
  expect(published.headers()["cloudflare-cdn-cache-control"]).toBe("public, max-age=1209600");
  expect(await published.body()).toEqual(png);
  const followed = await request.get(legacyPath);
  expect(followed.status()).toBe(200);
  expect(new URL(followed.url()).pathname).toBe(preview);
  expect(followed.headers()["cache-control"]).toBe("public, max-age=86400");
  expect(followed.headers()["cloudflare-cdn-cache-control"]).toBe("public, max-age=1209600");
  expect(await followed.body()).toEqual(png);

  const warmHtml = await (await request.get(pagePath)).text();
  expect(advertisedImage(warmHtml)).toBe(preview);
  expect(advertisedImage(warmHtml, "twitter:image")).toBe(preview);
  expect(advertisedImage(await (await request.get(`/?${search}`)).text())).toBe(preview);
  const changedHtml = await (
    await request.get("/search/cross-of-gold?from=1896-01-01&to=1896-12-31&state=GA&t=1896-08-01&z=6&c=-83,33")
  ).text();
  expect(await screenshot(request, advertisedImage(changedHtml))).not.toEqual(png);
});

test("the no-JavaScript map uses the same rendered search view, never the generic share image", async ({
  request,
  isMobile,
}) => {
  test.skip(!process.env.PW_BASE_URL || isMobile, "requires the API serving the built app and Chromium");
  test.setTimeout(60_000);
  const legacy = await request.get("/search-map.png?example=cross-of-gold", { maxRedirects: 0 });
  expect(legacy.status()).toBe(307);
  expect(legacy.headers()["cache-control"]).toBe("no-store");
  const preview = legacy.headers()["location"] ?? "";
  expect(preview).toMatch(/^\/search-preview\/[0-9a-f]{16}\/cross-of-gold\.png$/);
  const html = await (await request.get("/examples/cross-of-gold")).text();
  expect(html).toContain(`<img src="${preview}"`);
  const map = await request.get(preview, { timeout: 50_000 });
  expect(map.status()).toBe(200);
  expect(map.headers()["content-type"]).toBe("image/png");
  expect(map.headers()["cache-control"]).toBe("public, max-age=86400");
  expect(map.headers()["cloudflare-cdn-cache-control"]).toBe("public, max-age=1209600");
  const png = await map.body();
  expect(png.readUInt32BE(16)).toBe(1200);
  expect(png.readUInt32BE(20)).toBe(630);
  const shared = await screenshot(request, "/share-image.png?page=%2Fexamples%2Fcross-of-gold&v=2");
  expect(png).toEqual(shared);
  const generic = await (await request.get("/og-image.png")).body();
  expect(png).not.toEqual(generic);
  expect((await request.get("/search-map.png?q=not-a-listed-search")).status()).toBe(404);
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
