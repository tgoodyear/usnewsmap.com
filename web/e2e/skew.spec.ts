import AxeBuilder from "@axe-core/playwright";
import { readFileSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

// The relative-rate view (doc 11) against the API serving the synthetic
// fixtures: 6 places in 6 states, so the view is available, and the fixture
// corpus has no real differences between places.

const STYLE = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
};

test.beforeEach(async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.request().url().includes("/styles/") ? route.fulfill({ json: STYLE }) : route.fulfill({ status: 404 }),
  );
});

async function expectAccessible(page: Page) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .exclude(".maplibregl-canvas-container")
    .exclude(".maplibregl-ctrl-attrib")
    .analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
}

const measure = (page: Page) => page.getByRole("group", { name: "Measure" });

test("the relative rate: toggle, legend, lists, table, export and permalink", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/?q=%22cross+of+gold%22&bucket=month");
  await expect(measure(page).getByRole("button", { name: "Pages" })).toHaveAttribute("aria-pressed", "true");

  // From the keyboard.
  const rate = measure(page).getByRole("button", { name: "Relative rate" });
  await rate.focus();
  await page.keyboard.press("Enter");
  await expect(rate).toHaveAttribute("aria-pressed", "true");
  await expect(page).toHaveURL(/[?&]norm=skew/);

  const legend = page.locator(".legend");
  await expect(legend).toContainText("Relative rate");
  await expect(legend).toContainText("compared with the other 5 places with pages in this window, in 6 states, over the same months");
  await expect(legend).toContainText("Index fixture-v1");
  await expect(page.getByRole("complementary", { name: "Places that differ most clearly" })).toBeVisible();
  // Points only: the heat layer isn't offered in this view.
  await expect(page.getByRole("combobox", { name: "Map layer" })).toHaveCount(0);
  await expectAccessible(page);

  // The explanation opens and closes from the keyboard.
  const info = page.getByRole("button", { name: "About the relative rate" });
  await info.focus();
  await page.keyboard.press("Enter");
  await expect(info).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByText("pulled toward the typical rate")).toBeVisible();
  // The explanation fits on the screen, phones included.
  const box = await page.locator(".infotip__body").boundingBox();
  const width = page.viewportSize()!.width;
  expect(box!.x).toBeGreaterThanOrEqual(0);
  expect(box!.x + box!.width).toBeLessThanOrEqual(width);
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(info).toHaveAttribute("aria-expanded", "false");

  await page.getByRole("button", { name: "Table" }).click();
  const places = page.getByRole("table", { name: /Relative rate of each place/ });
  await expect(places.getByRole("columnheader", { name: "Expected" })).toBeVisible();
  await expect(places.locator("tbody tr")).toHaveCount(6);
  await expect(page.getByRole("table", { name: "By state, each compared with the other states" })).toBeVisible();
  await expectAccessible(page);

  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download CSV" }).click();
  const file = await (await download).path();
  const lines = readFileSync(file, "utf8").trim().split("\n");
  expect(lines[0]).toBe("place_id,name,state,pages,hits,expected,estimate,lower,upper,languages");
  expect(lines).toHaveLength(7);

  // The permalink restores the view.
  await page.reload();
  await expect(measure(page).getByRole("button", { name: "Relative rate" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("table", { name: /Relative rate of each place/ })).toBeVisible();

  await measure(page).getByRole("button", { name: "Pages" }).click();
  await expect(page).not.toHaveURL(/norm=/);
  await expect(page.getByRole("table", { name: /Places with matching pages/ })).toBeVisible();
  expect(errors).toEqual([]);
});

test("a selected place says how its rate compares", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&norm=skew&tab=table");
  await page.getByRole("table", { name: /Relative rate of each place/ }).locator("tbody th button").first().click();
  const panel = page.locator(".panel__skew");
  await expect(panel).toContainText(/pages? matched where .* expected from its .* pages?\./);
  await expect(panel).toContainText(/the other places \(.* to .*×\)/);
});

test("an older share-of-pages permalink opens on Pages", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&norm=rel");
  await expect(measure(page).getByRole("button", { name: "Pages" })).toHaveAttribute("aria-pressed", "true");
  await expect(measure(page).getByRole("button", { name: "Share of pages" })).toHaveCount(0);
  await expect(measure(page).getByRole("button")).toHaveCount(2);
  await expect(page.locator(".legend")).toContainText("Pages containing the match");
  await expectAccessible(page);
  await measure(page).getByRole("button", { name: "Relative rate" }).click();
  await expect(page).toHaveURL(/[?&]norm=skew/);
});

test("the relative rate needs five places with pages", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&state=IL,NY&norm=skew");
  await expect(page.getByRole("status").filter({ hasText: "at least 5 places" })).toBeVisible();
  // Page counts are shown meanwhile.
  await expect(page.locator(".legend")).toContainText("Pages containing the match");
});

test("playback keeps the relative rate and its scale", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&norm=skew&win=3");
  await expect(page.locator(".legend")).toContainText("Relative rate");
  await page.locator("body").press("Home");
  await expect(page).toHaveURL(/[?&]t=1895-01-01/);
  // Nothing printed yet: no place is clearly different.
  await expect(page.getByText("No place is clearly above 1× in this window.")).toBeVisible();
  await page.locator("body").press("End");
  await expect(page.locator(".legend")).toContainText("Relative rate");
  // The live region describes the trailing window, not everything up to the date.
  await expect(page.locator("p[aria-live=polite]")).toContainText("in the window ending Dec 1897", { timeout: 5000 });
  await expect(measure(page).getByRole("button", { name: "Relative rate" })).toHaveAttribute("aria-pressed", "true");
  expect(errors).toEqual([]);
});

test("a heat layer in the link gives way to points in the relative rate", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&norm=skew&layer=heat&state=IL,NY");
  await expect(page.getByRole("status").filter({ hasText: "at least 5 places" })).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Map layer" })).toHaveCount(0);
});
