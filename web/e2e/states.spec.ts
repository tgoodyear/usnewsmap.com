import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.fulfill({
      json: {
        version: 8,
        sources: {},
        layers: [{ id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } }],
      },
    }),
  );
});
const query = "/?q=%22cross+of+gold%22&bucket=month&from=1896-06-01&to=1896-12-31&c=-89,40&z=5";

test("States is lazy, permalinked, accessible and exports the exact window numbers without new API queries", async ({
  page,
}) => {
  const assets: string[] = [];
  const requests: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes(".geojson")) assets.push(r.url());
    if (/\/v1\/(aggregate|coverage)/.test(r.url())) requests.push(r.url());
  });
  await page.goto(query);
  const layer = page.getByRole("combobox", { name: "Map layer" });
  await expect(layer).toBeVisible();
  // Wait for the already-requested baseline before counting requests.
  await expect.poll(() => requests.filter((r) => r.includes("/coverage")).length).toBe(1);
  expect(assets).toHaveLength(0);
  const before = requests.length;
  const geometry = page.waitForResponse((r) => r.url().includes(".geojson"));
  await layer.selectOption("states");
  expect((await geometry).ok()).toBe(true);
  await expect(page).toHaveURL(/layer=states/);
  await expect(page.getByRole("region", { name: "States legend" })).toContainText("per 1,000 pages published");
  const table = page.getByRole("region", { name: "States table" });
  await expect(table.getByRole("row")).toHaveCount(58); // 56 boundaries + unassigned + header.
  const sum = await table
    .locator("tbody tr")
    .evaluateAll((rows) =>
      rows.reduce((n, row) => n + Number((row.children[1]?.textContent ?? "0").replace(/,/g, "")), 0),
    );
  expect(sum).toBe(Number((await page.locator(".summary strong").nth(1).textContent())!.replace(/,/g, "")));
  await page.getByRole("button", { name: "Per 1,000 pages", exact: true }).click();
  const csvDownload = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download states CSV" }).click();
  const download = await csvDownload;
  expect(download.suggestedFilename()).toBe("usnewsmap-states-window.csv");
  const stream = await download.createReadStream();
  let csv = "";
  for await (const chunk of stream!) csv += chunk.toString();
  const csvRows = csv.trim().split("\r\n").slice(1);
  expect(csvRows).toHaveLength(57);
  expect(csvRows.reduce((n, row) => n + Number(row.split(",")[2]!.replaceAll('"', "")), 0)).toBe(sum);
  const violations = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .exclude(".maplibregl-canvas-container")
    .exclude(".maplibregl-ctrl-attrib")
    .analyze();
  expect(violations.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
  expect(requests).toHaveLength(before);
  await page.reload();
  await expect(layer).toHaveValue("states");
  await expect(table.getByRole("rowheader", { name: "District of Columbia", exact: true })).toBeVisible();
  await page.locator("body").press("Home");
  await expect(table.getByRole("row").filter({ hasText: "Illinois" })).toContainText("0");
  await page.locator("body").press("End");
  await table.getByRole("button", { name: "Add state filter: Illinois", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/state=IL/);
});

test("state polygon hover/click and mobile pinned tap provide state details and filtering", async ({
  page,
  isMobile,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto(`${query}&layer=states`);
  await expect(page.getByRole("region", { name: "States legend" })).toBeVisible();
  const map = page.getByTestId("map");
  await expect(map).toBeVisible();
  // Illinois is at the URL viewport centre; wait for the overlay's first draw.
  await page.waitForTimeout(1000);
  const box = (await map.boundingBox())!;
  const point = { x: box.x + box.width / 2, y: box.y + box.height / 2 };
  if (isMobile) {
    await page.touchscreen.tap(point.x, point.y);
    const details = page.getByRole("group", { name: "Illinois details" });
    await expect(details).toContainText("matching pages");
    await expect(page).not.toHaveURL(/state=IL/);
    await page.locator("body").press("Escape");
    await expect(details).toHaveCount(0);
    // A rapid repeat at the same map position is MapLibre's double-tap zoom.
    await page.waitForTimeout(350);
    await map.scrollIntoViewIfNeeded();
    const again = (await map.boundingBox())!;
    await page.touchscreen.tap(again.x + again.width / 2, again.y + again.height / 2);
    await details.getByRole("button", { name: "Add state filter: Illinois" }).click();
  } else {
    await page.mouse.move(point.x, point.y);
    const tooltip = page.getByRole("tooltip").filter({ hasText: "Illinois" });
    await expect(tooltip).toContainText("pages published");
    await expect(tooltip).toContainText("per 1,000");
    await page.locator("body").press("Escape");
    await expect(tooltip).toHaveCount(0);
    await page.mouse.move(point.x + 2, point.y + 2);
    await expect(tooltip).toBeVisible();
    await page.mouse.click(point.x, point.y);
  }
  await expect(page).toHaveURL(/state=IL/);
  expect(errors).toEqual([]);
});

test("States is hidden in unsupported contexts, including shared links", async ({ page }) => {
  // Supply a successful synthetic response for the Congress display context:
  // this fixture API has no Congress corpus of its own.
  await page.route("**/v1/aggregate?**", async (route) => {
    const url = new URL(route.request().url());
    url.searchParams.delete("source");
    await route.fulfill({ response: await route.fetch({ url: url.toString() }) });
  });
  for (const suffix of ["&norm=skew", "&norm=when", "&tab=table", "&source=congress", "&lccn=sn99000001"]) {
    await page.goto(`${query}&layer=states${suffix}`);
    await expect(page.locator(".summary")).toBeVisible();
    await expect(page.locator('select option[value="states"]')).toHaveCount(0);
    await expect(page.getByRole("region", { name: "States legend" })).toHaveCount(0);
  }
});

test("without WebGL a States permalink uses the accessible table and hides the layer selector", async ({ page }) => {
  await page.addInitScript(() => {
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (
      this: HTMLCanvasElement,
      ...args: Parameters<typeof getContext>
    ) {
      if (String(args[0]).startsWith("webgl")) return null;
      return getContext.apply(this, args);
    } as typeof getContext;
  });

  await page.goto(`${query}&layer=states`);
  await expect(page.getByText(/The map needs WebGL2/)).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Map layer" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "States table" })).toBeVisible();
});

test("States legend and table remain accessible in the dark theme", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto(`${query}&layer=states`);
  await expect(page.getByRole("region", { name: "States table" })).toBeVisible();
  const scan = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .exclude(".maplibregl-canvas-container")
    .exclude(".maplibregl-ctrl-attrib")
    .analyze();
  expect(scan.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);
});

test("a search with no matches still distinguishes published pages from no coverage", async ({ page }) => {
  await page.goto("/?q=zyzzyva&layer=states&bucket=month");
  await expect(page.getByRole("status").filter({ hasText: "No pages match" })).toBeVisible();
  const table = page.getByRole("region", { name: "States table" });
  await expect(table.getByRole("row")).toHaveCount(58);
  const counts = await table.locator("tbody tr").evaluateAll((rows) =>
    rows.map((row) => ({
      hits: Number((row.children[1]?.textContent ?? "0").replaceAll(",", "")),
      pages: Number((row.children[2]?.textContent ?? "0").replaceAll(",", "")),
    })),
  );
  expect(counts.every((r) => r.hits === 0)).toBe(true);
  expect(counts.some((r) => r.pages > 0)).toBe(true);
  expect(counts.some((r) => r.pages === 0)).toBe(true);
  await expect(page.getByRole("region", { name: "States legend" })).toContainText("No positive rates");
});

test("warns about truncated place totals and retains the table when geometry fails", async ({ page }) => {
  await page.route("**/v1/aggregate?**", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.total.places = 5001;
    await route.fulfill({ response, json: data });
  });
  await page.route("**/*.geojson", (route) => route.fulfill({ status: 503 }));
  await page.goto(`${query}&layer=states`);
  await expect(page.getByRole("note").filter({ hasText: "State matching-page totals are incomplete" })).toBeVisible();
  await expect(page.getByRole("alert")).toContainText("State boundaries could not be loaded");
  await expect(page.getByRole("button", { name: "Download states CSV" })).toBeVisible();
});

test("unavailable coverage does not draw state fills as zero published pages", async ({ page }) => {
  await page.route("**/v1/coverage?**", (route) => route.fulfill({ status: 503 }));
  await page.goto(`${query}&layer=states`);
  await expect(page.getByRole("alert").filter({ hasText: "Pages-published counts could not be loaded" })).toBeVisible({
    timeout: 15_000,
  });
  await expect(page.getByRole("region", { name: "States legend" })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "States table" })).toHaveCount(0);
});
