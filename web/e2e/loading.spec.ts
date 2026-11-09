import { expect, test } from "@playwright/test";

// A route callback that calls `route.fetch()` can still be running when the test's last
// assertion passes (a background request from the page). Wait for it, or Playwright fails
// the test with "Test ended" / "Response has been disposed" while it runs.
test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "wait" });
});

test("the static fallback does not flash while application assets are unavailable", async ({ page }) => {
  await page.route("**/*", (route) =>
    ["script", "stylesheet"].includes(route.request().resourceType()) ? route.abort() : route.continue(),
  );
  await page.goto("/", { waitUntil: "domcontentloaded" });
  const fallback = page.locator("#site-fallback");
  await expect(fallback).toContainText("US News Map searches");
  await expect(fallback).toBeHidden();
});

test("the mounted application replaces the hidden fallback", async ({ page }) => {
  await page.goto("/");
  await expect(page.locator("#site-fallback")).toHaveCount(0);
  await expect(page.locator("#root .app")).toBeVisible();
});

// uBlock Origin's "Disable JavaScript" adds this CSP and blocks script
// requests, but the browser still matches `@media (scripting: enabled)`.
test("the static page remains readable when an extension blocks scripts", async ({ page }) => {
  await page.route("**/*", async (route) => {
    const request = route.request();
    if (request.resourceType() === "script") return route.abort();
    if (!request.isNavigationRequest()) return route.continue();
    const response = await route.fetch();
    await route.fulfill({
      response,
      headers: { ...response.headers(), "content-security-policy": "script-src http: https:" },
    });
  });
  await page.goto("/");
  const fallback = page.locator("#site-fallback");
  await expect(fallback).toBeVisible();
  await expect(fallback).toContainText("US News Map searches");
});

test.describe("without JavaScript", () => {
  test.use({ javaScriptEnabled: false });

  test("the static page remains readable", async ({ page }) => {
    await page.goto("/");
    const fallback = page.locator("#site-fallback");
    await expect(fallback).toBeVisible();
    await expect(fallback).toContainText("US News Map searches");
    await expect(fallback.getByRole("link", { name: "Chronicling America" })).toBeVisible();
  });
});
