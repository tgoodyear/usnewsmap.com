import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// Runs against the API serving the synthetic fixtures (6 places; "cross of
// gold" first appears in Chicago on 1896-07-10 and reaches Nebraska last).

async function expectAccessible(page: Page) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    // The WebGL canvas is decorative; the table is its accessible equivalent.
    .exclude(".maplibregl-canvas-container")
    .exclude(".maplibregl-ctrl-attrib")
    .analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
}

test("example search maps, plays, drills down and keeps a permalink", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("newspapers");
  await expect(page.getByRole("note")).toContainText("synthetic");
  await expectAccessible(page);

  await page.getByRole("button", { name: /Cross of Gold, 1896/ }).click();
  await expect(page).toHaveURL(/q=%22cross\+of\+gold%22/);
  await expect(page.locator(".summary")).toContainText("6 places");

  // Keyboard playback: Home goes to the first week, where nothing has matched yet.
  await page.locator("body").press("Home");
  await expect(page.locator(".dock__label")).toHaveText("Week of Jun 1, 1896");
  await expect(page.locator(".summary")).toContainText("0 places");
  await page.locator("body").press("End");
  await expect(page.locator(".summary")).toContainText("6 places");

  // Scrub to mid-July: only the eastern cities have printed it.
  const url = new URL(page.url());
  url.searchParams.set("t", "1896-07-13");
  await page.goto(url.toString());
  const early = Number((await page.locator(".summary strong").first().textContent()) ?? "0");
  expect(early).toBeGreaterThan(0);
  expect(early).toBeLessThan(6);

  await page.getByRole("tab", { name: "Table" }).click();
  await expect(page).toHaveURL(/tab=table/);
  const rows = page.locator("table.places tbody tr");
  await expect(rows).toHaveCount(early);
  await expectAccessible(page);

  await rows.first().getByRole("button").click();
  const panel = page.getByRole("complementary");
  await expect(panel.locator(".hit").first()).toBeVisible();
  await expect(panel.locator("mark").first()).toBeVisible();
  const link = panel.getByRole("link", { name: /Library of Congress/ }).first();
  await expect(link).toHaveAttribute("href", /^https:\/\/www\.loc\.gov\/resource\/sn99/);
  await expect(link).toHaveAttribute("rel", /noopener/);

  // The permalink restores the whole view.
  await page.reload();
  await expect(page.getByRole("tab", { name: "Table" })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".dock__label")).toContainText("1896");
  await expect(page.getByRole("complementary")).toBeVisible();
});

test("the map renders on the map tab", async ({ page, browserName }) => {
  test.skip(browserName !== "chromium");
  await page.goto("/?q=%22cross+of+gold%22&bucket=month");
  await expect(page.getByTestId("map")).toBeVisible();
  await expect(page.locator(".maplibregl-canvas")).toBeVisible();
  await expect(page.locator(".legend")).toContainText("Pages containing the match");
});

test("syntax errors show the API's hint", async ({ page }) => {
  await page.goto("/?q=%22cross+of");
  const alert = page.getByRole("alert");
  await expect(alert).toContainText("Query syntax error");
  await expect(alert).toContainText("Use quotes for phrases");
});

test("no results suggests what to try", async ({ page }) => {
  await page.goto("/?q=zyzzyva");
  await expect(page.getByRole("status")).toContainText("No pages match");
});
