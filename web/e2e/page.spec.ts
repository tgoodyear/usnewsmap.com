import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The page view (07 §7.14, #2026) against a mocked `GET /v1/pages/{doc_id}/layout`: the
// layout route has its own tests in the API.

const DOC = "loc:sn85055101_1917-04-03_ed-1_seq-3";
const PATH = `/page/${DOC}`;

const SCAN = `data:image/svg+xml,${encodeURIComponent(
  '<svg xmlns="http://www.w3.org/2000/svg" width="500" height="700"><rect width="500" height="700" fill="#e8dcc0"/></svg>',
)}`;

const WORDS = ["Liberty", "bell", "rung", "out", "from", "the", "old", "tower", "today"];

function layout(full: string | null = SCAN, granularity = "word", engine: string | null = "usnm-tesseract") {
  const line = (l: number) => [500, 700 + l * 300, 4000, 200];
  return {
    doc_id: DOC,
    engine,
    stamp: "20261101T120000Z",
    node: "n1",
    width: 5000,
    height: 7000,
    granularity,
    image: full ? { iiif_base: "https://tile.loc.gov/image-services/iiif/example", full } : null,
    lines: [0, 1, 2].map((l) => ({
      b: 0,
      l,
      rect: line(l),
      words: WORDS.slice(l * 3, l * 3 + 3).map((t, i) => ({
        i,
        t,
        rect: granularity === "word" ? [500 + i * 1300, 700 + l * 300, 1200, 200] : line(l),
        c: [0.95, 0.4, 0.7][i],
      })),
    })),
  };
}

async function mockLayout(page: Page, body: unknown, status = 200) {
  await page.route("**/v1/pages/**/layout", (route) =>
    route.fulfill({
      status,
      contentType: status === 200 ? "application/json" : "application/problem+json",
      body: JSON.stringify(body),
    }),
  );
}

async function expectAccessible(page: Page) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(results.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
}

const wordButtons = (page: Page) => page.getByRole("group", { name: "Words on the page" }).getByRole("button");

test("redraws the page, switches views, colours by confidence and zooms", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await mockLayout(page, layout());
  await page.goto(PATH);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Page 3, Apr 3, 1917");
  await expect(page).toHaveTitle(/^Page 3, Apr 3, 1917 · US News Map$/);
  const words = wordButtons(page);
  await expect(words).toHaveCount(9);

  // The first word sits at 10% of the page's width and is 1200 of 5000 wide.
  const layer = page.locator(".pv-layer");
  const box = (await layer.boundingBox())!;
  const first = (await words.first().boundingBox())!;
  expect(first.x - box.x).toBeCloseTo(box.width * 0.1, 0);
  expect(first.width).toBeCloseTo(box.width * 0.24, 0);
  expect(box.height / box.width).toBeCloseTo(1.4, 2);

  // Text over a faded scan is the default when a scan exists.
  await expect(page.locator(".pv-scan")).toHaveCount(1);
  await page.getByLabel("Scan", { exact: true }).check();
  await expect(words.first()).toBeHidden();
  // The words stay readable as plain text when the buttons are hidden.
  await expect(page.locator("p.visually-hidden")).toContainText("Liberty bell rung");
  await page.getByLabel("Text", { exact: true }).check();
  await expect(page.locator(".pv-scan")).toHaveCount(0);
  await expect(words.first()).toBeVisible();

  await expect(page.locator(".pv--conf")).toHaveCount(0);
  await page.getByLabel("Colour by confidence").check();
  await expect(page.locator(".pv--conf")).toHaveCount(1);
  await expectAccessible(page);

  await page.getByRole("button", { name: "Zoom in" }).click();
  await expect(page.locator(".pv-zoom output")).toHaveText("150%");
  const zoomed = (await words.first().boundingBox())!;
  expect(zoomed.width).toBeCloseTo(first.width * 1.5, 0);
  await page.getByRole("button", { name: "Reset" }).click();
  expect(errors).toEqual([]);
});

test("keyboard: one tab stop, arrows move between words, Enter marks a word and the mark is remembered", async ({
  page,
}) => {
  await mockLayout(page, layout());
  await page.goto(PATH);
  const words = wordButtons(page);
  await expect(words).toHaveCount(9);
  await expect(page.locator('.pv-word[tabindex="0"]')).toHaveCount(1);

  await words.first().focus();
  await page.keyboard.press("ArrowRight");
  await expect(words.nth(1)).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(words.nth(2)).toBeFocused();
  await page.keyboard.press("End");
  await expect(words.nth(8)).toBeFocused();
  await expect(page.locator('.pv-word[tabindex="0"]')).toHaveCount(1);
  await page.keyboard.press("Home");
  await page.keyboard.press("ArrowRight");

  await page.keyboard.press("Enter");
  const dialog = page.getByRole("dialog", { name: "Mark the word bell" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Right" })).toBeFocused();
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(words.nth(1)).toBeFocused();

  await page.keyboard.press("Enter");
  await dialog.getByRole("button", { name: "Wrong" }).click();
  await expect(dialog).toBeHidden();
  await expect(words.nth(1)).toHaveAttribute("data-verdict", "wrong");
  await expect(page.getByText("You have marked 1 on this page.")).toBeVisible();

  // Nothing is sent; the mark is this browser's own.
  await page.reload();
  await expect(words.nth(1)).toHaveAttribute("data-verdict", "wrong");
});

test("a page without a scan offers text only", async ({ page }) => {
  await mockLayout(page, layout(null));
  await page.goto(PATH);
  await expect(wordButtons(page)).toHaveCount(9);
  await expect(page.getByLabel("Text", { exact: true })).toBeChecked();
  await expect(page.getByLabel("Scan", { exact: true })).toHaveCount(0);
  await expect(page.getByLabel("Text over scan")).toHaveCount(0);
});

test("a page with null image and null engine still renders, with no scan modes", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await mockLayout(page, layout(null, "word", null));
  await page.goto(PATH);
  await expect(wordButtons(page)).toHaveCount(9);
  await expect(page.getByLabel("Scan", { exact: true })).toHaveCount(0);
  await expect(page.locator(".pv-hint")).toContainText("9 words.");
  await expect(page.locator(".pv-hint")).not.toContainText("read by");
  expect(errors).toEqual([]);
});

test("line-level boxes draw one span per line and turn word marking off", async ({ page }) => {
  await mockLayout(page, layout(SCAN, "line"));
  await page.goto(PATH);
  const words = wordButtons(page);
  await expect(words).toHaveCount(3);
  await expect(words.first()).toHaveText("Liberty bell rung");
  await expect(page.locator(".pv-hint")).toContainText("line-level boxes only");
  await words.first().click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expectAccessible(page);
});

test("a page with no layout yet says so and links to loc.gov", async ({ page }) => {
  await mockLayout(page, { type: "/errors/not-found", title: "Not found", status: 404 }, 404);
  await page.goto(PATH);
  await expect(page.getByRole("heading", { name: "No layout for this page yet" })).toBeVisible();
  await expect(page.getByRole("link", { name: "the Library of Congress", exact: true })).toHaveAttribute(
    "href",
    "https://www.loc.gov/resource/sn85055101/1917-04-03/ed-1/?sp=3",
  );
  await expectAccessible(page);
});

test("a failing API shows an error with a way to try again", async ({ page }) => {
  let calls = 0;
  await page.route("**/v1/pages/**/layout", (route) => {
    calls++;
    return calls <= 3
      ? route.fulfill({ status: 500, contentType: "application/problem+json", body: '{"title":"Broken","status":500}' })
      : route.fulfill({ json: layout() });
  });
  await page.goto(PATH);
  await expect(page.getByRole("alert")).toContainText("could not be loaded");
  await expectAccessible(page);
  await page.getByRole("button", { name: "Try again" }).click();
  await expect(wordButtons(page)).toHaveCount(9);
});
