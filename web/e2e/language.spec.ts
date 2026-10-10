import { readFileSync } from "node:fs";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The language filter (07 §7.9) against the API serving the synthetic
// fixtures: six places, one title each. Four titles are in English (New
// York's is also in German), Nebraska's is in German and California's in
// Spanish, so "German" finds New York and Nebraska.

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

/** On phones the filters sit behind "Options"; open it if it's shown. */
async function openOptions(page: Page) {
  const toggle = page.getByRole("button", { name: /^Options/ });
  if (await toggle.isVisible()) {
    if ((await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
  }
}

const placeRows = (page: Page) =>
  page.getByRole("table", { name: "Places with matching pages up to the current date" }).locator("tbody tr");

test("choose a language: URL, results, reload and accessibility", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/?q=%22cross+of+gold%22&tab=table");
  await expect(placeRows(page)).toHaveCount(6);
  const before = await page.locator(".summary").textContent();

  await openOptions(page);
  const button = page.getByRole("button", { name: "Languages: any" });
  await button.click();
  await expect(button).toHaveAttribute("aria-expanded", "true");
  const list = page.getByRole("group", { name: "Newspaper languages" });
  // Most pages first, with each language's newspapers.
  await expect(list.getByRole("checkbox")).toHaveCount(3);
  await expect(list.locator("label").first()).toHaveText("English (4 newspapers)");
  await expectAccessible(page);

  // From the keyboard: check German, then Escape closes the list.
  const german = list.getByRole("checkbox", { name: "German (2 newspapers)" });
  await german.focus();
  await page.keyboard.press("Space");
  await expect(german).toBeChecked();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Languages: German" })).toBeFocused();
  // On phones the language choice counts as one option.
  const toggle = page.getByRole("button", { name: /^Options/ });
  if (await toggle.isVisible()) await expect(toggle).toHaveText("Options (1)");
  await page.getByRole("button", { name: "Search", exact: true }).click();

  await expect(page).toHaveURL(/[?&]lang=ger(&|$)/);
  await expect(placeRows(page)).toHaveCount(2);
  const names = await placeRows(page).locator("th").allTextContents();
  expect(names.map((n) => n.replace(/^Fixture \w+ \w+ \((.*)\)$/, "$1")).sort()).toEqual(["Nebraska", "New York area"]);
  await expect(page.locator(".summary")).not.toHaveText(before ?? "");
  // Baselines count only German-language pages, so the share of pages published stays.
  await expect(page.getByRole("columnheader", { name: "Share of pages published" })).toBeVisible();
  await expectAccessible(page);

  // The permalink restores the filter.
  await page.reload();
  await expect(placeRows(page)).toHaveCount(2);
  await openOptions(page);
  await page.getByRole("button", { name: "Languages: German" }).click();
  await expect(page.getByRole("checkbox", { name: "German (2 newspapers)" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "English (4 newspapers)" })).not.toBeChecked();
  await page.keyboard.press("Escape");

  // The relative rate needs 5 places with pages, and German has 2: it says so
  // instead of comparing against every language.
  await page.getByRole("group", { name: "Measure" }).getByRole("button", { name: "Relative rate" }).click();
  await expect(page.getByRole("status").filter({ hasText: "needs at least 5 places" })).toBeVisible();
  await expect(placeRows(page)).toHaveCount(2);
  await expectAccessible(page);
  expect(errors).toEqual([]);
});

test("relative rate under a language filter compares only those languages' pages", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  // English and German together reach five places (everything but California).
  await page.goto("/?q=%22cross+of+gold%22&bucket=month&lang=eng,ger&norm=skew&tab=table");
  const places = page.getByRole("table", { name: /Relative rate of each place/ });
  await expect(places.locator("tbody tr")).toHaveCount(5);
  await expect(page.getByText("isn't available")).toHaveCount(0);
  // No "Papers in … and English" line: the baselines already compare like with like.
  await expect(places.getByRole("columnheader", { name: "Languages" })).toBeVisible();
  await expect(places.getByText(/^Papers in /)).toHaveCount(0);
  await expectAccessible(page);

  const download = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download CSV" }).click();
  const lines = readFileSync(await (await download).path(), "utf8")
    .trim()
    .split("\n");
  expect(lines[0]).toBe("place_id,name,state,pages,hits,expected,estimate,lower,upper,languages,first_seen,last_seen");
  expect(lines).toHaveLength(6);
  expect(errors).toEqual([]);
});

test("a title in two languages is found by either", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&tab=table&lang=eng");
  await expect(placeRows(page)).toHaveCount(4);
  const names = await placeRows(page).locator("th").allTextContents();
  expect(names.some((n) => n.includes("New York"))).toBe(true);
  await page.goto("/?q=%22cross+of+gold%22&tab=table&lang=spa,ger");
  await expect(placeRows(page)).toHaveCount(3);
});

test("no match under a language filter, and a code the catalog doesn't have", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&lang=fre");
  await expect(page.getByText("No pages match.")).toBeVisible();
  await expect(page.getByText("remove language or state filters")).toBeVisible();
  await openOptions(page);
  await page.getByRole("button", { name: "Languages: French" }).click();
  const french = page.getByRole("checkbox", { name: "French (no newspapers)" });
  await expect(french).toBeChecked();
  await french.uncheck();
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page).not.toHaveURL(/lang=/);
  await expect(page.getByText("No pages match.")).toHaveCount(0);
  await expectAccessible(page);
});
