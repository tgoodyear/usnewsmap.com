import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page, type TestInfo } from "@playwright/test";

// The fixture API makes any search containing "slowsearch" take 4 s while a
// visitor waits 1 s (scripts/ci/start-api-fixtures.sh), so it answers 202
// and the app asks again, as for a cold search on the real corpus. Each
// test searches a word of its own, so no other test's result is cached.

function slowWord(info: TestInfo): string {
  return `slowsearch${info.project.name}${info.title.length}${Date.now()}`;
}

const working = (page: Page) => page.getByRole("status").filter({ hasText: "Large search, still working" });

test("a large search says it is still working, then shows its results", async ({ page }, info) => {
  const word = slowWord(info);
  const statuses: number[] = [];
  page.on("response", (r) => {
    if (r.url().includes("/v1/aggregate") && r.url().includes(word)) statuses.push(r.status());
  });
  await page.goto(`/?q=${encodeURIComponent(`gold OR ${word}`)}&tab=table`);
  await expect(working(page)).toBeVisible();
  const axe = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"])
    .analyze();
  expect(axe.violations.map((v) => `${v.id}: ${v.help}`)).toEqual([]);

  await expect(page.locator(".summary")).toContainText("places", { timeout: 20_000 });
  await expect(working(page)).toHaveCount(0);
  expect(statuses[0]).toBe(202);
  expect(statuses.at(-1)).toBe(200);
  expect(statuses.filter((s) => s !== 202 && s !== 200)).toEqual([]);
});

test("a new search stops the wait for a large one", async ({ page }, info) => {
  const word = slowWord(info);
  let asked = 0;
  page.on("request", (r) => {
    if (r.url().includes("/v1/aggregate") && r.url().includes(word)) asked += 1;
  });
  await page.goto(`/?q=${encodeURIComponent(`gold OR ${word}`)}&tab=table`);
  await expect(working(page)).toBeVisible();

  // In the page, not a new page load: the app itself must stop asking.
  await page.getByRole("searchbox").fill('"cross of gold"');
  await page.getByRole("button", { name: "Search" }).click();
  await expect(page.locator(".summary")).toContainText("6 places");
  await expect(working(page)).toHaveCount(0);
  const before = asked;
  // Longer than the 2 s Retry-After: nothing asks about the old search.
  await page.waitForTimeout(3000);
  expect(asked).toBe(before);
});
