import { expect, test } from "@playwright/test";

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
