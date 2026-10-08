import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The From and To boxes against the API serving the synthetic fixtures,
// whose pages run from 1895-01-01 to 1897-12-31. A year or a month alone is
// a date range: From fills in to its first day and To to its last.

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

/** Type a search and its dates, opening the options on phones, and run it. */
async function search(page: Page, q: string, from: string, to: string) {
  await page.getByRole("searchbox").fill(q);
  const toggle = page.getByRole("button", { name: /^Options/ });
  if ((await toggle.isVisible()) && (await toggle.getAttribute("aria-expanded")) !== "true") await toggle.click();
  await page.getByRole("textbox", { name: "From" }).fill(from);
  await page.getByRole("textbox", { name: "To" }).fill(to);
  await page.getByRole("button", { name: "Search" }).click();
}

test("a year alone searches from January 1 to December 31", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await search(page, "gold", "1896", "1896");
  await expect(page).toHaveURL(/[?&]from=1896-01-01(&|$)/);
  expect(new URL(page.url()).searchParams.get("to")).toBe("1896-12-31");
  await expect(page.locator(".summary")).toContainText("pages");
  // The boxes show the dates searched.
  await expect(page.getByRole("textbox", { name: "From" })).toHaveValue("01/01/1896");
  await expect(page.getByRole("textbox", { name: "To" })).toHaveValue("12/31/1896");
  await expectAccessible(page);
  expect(errors).toEqual([]);
});

test("a month alone in To searches to its last day", async ({ page }) => {
  await page.goto("/");
  await search(page, "gold", "1896", "02/1896");
  await expect(page).toHaveURL(/[?&]to=1896-02-29(&|$)/);
  expect(new URL(page.url()).searchParams.get("from")).toBe("1896-01-01");
  await expect(page.getByRole("textbox", { name: "To" })).toHaveValue("02/29/1896");
});

test("a day and a year without a month is refused next to the box", async ({ page }) => {
  await page.goto("/?q=gold");
  await expect(page.locator(".summary")).toContainText("pages");
  await search(page, "gold", "", "/15/1896");
  const to = page.getByRole("textbox", { name: "To" });
  await expect(to).toHaveAttribute("aria-invalid", "true");
  await expect(to).toBeFocused();
  const message = page.getByText("To: Add the month, or enter just the year.");
  await expect(message).toBeVisible();
  expect((await to.getAttribute("aria-describedby"))?.split(" ")).toContain(await message.getAttribute("id"));
  // Nothing was searched.
  expect(new URL(page.url()).searchParams.get("to")).toBeNull();
  await expectAccessible(page);

  // A date outside the index is refused too, as the date picker's limits did.
  await to.fill("1700");
  await expect(to).not.toHaveAttribute("aria-invalid");
  await to.press("Enter");
  await expect(page.getByText("To: Enter a date between 01/01/1895 and 12/31/1897.")).toBeVisible();
});

test("the calendar button opens the browser's date picker", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/?q=gold&from=1896-03-01");
  const button = page.getByRole("button", { name: "Choose the From date on a calendar" });
  await expect(button).toBeVisible();
  await button.click();
  // The hidden date input behind it starts on the box's date.
  await expect(page.locator(".date-field__native").first()).toHaveValue("1896-03-01");
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});
