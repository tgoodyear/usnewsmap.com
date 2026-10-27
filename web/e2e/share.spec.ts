import { expect, test, type APIRequestContext } from "@playwright/test";

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
