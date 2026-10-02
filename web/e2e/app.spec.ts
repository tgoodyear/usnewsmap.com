import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

// The app uses the default basemap style. Serve a local style in its place,
// with a GeoJSON source so MapLibre's worker must load, and no third-party
// tile service is contacted.
const STYLE = {
  version: 8,
  sources: {
    outline: {
      type: "geojson",
      data: {
        type: "Feature",
        properties: {},
        geometry: { type: "Polygon", coordinates: [[[-125, 25], [-67, 25], [-67, 49], [-125, 49], [-125, 25]]] },
      },
    },
  },
  layers: [
    { id: "bg", type: "background", paint: { "background-color": "#e8e4dc" } },
    { id: "us", type: "fill", source: "outline", paint: { "fill-color": "#f7f5f0" } },
  ],
};

test.beforeEach(async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/**", (route) =>
    route.request().url().includes("/styles/")
      ? route.fulfill({ json: STYLE })
      : route.fulfill({ status: 404 }),
  );
});

/** Console errors, which fail the test (e.g. "Worker failed to load"). */
function watchErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("console", (m) => {
    if (m.type() === "error") errors.push(m.text());
  });
  page.on("pageerror", (e) => errors.push(e.message));
  return errors;
}

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

/** The home page's example cards. */
const exampleCards = (page: Page) => page.locator("ul.examples button.example");

/** Page through "Show other examples" until the card named `name` shows. */
async function findExample(page: Page, name: RegExp) {
  const card = exampleCards(page).filter({ hasText: name });
  // 17 sets of 3 cover 50 examples; allow for a longer list.
  for (let i = 0; i < 40 && (await card.count()) === 0; i++) {
    await page.getByRole("button", { name: "Show other examples" }).click();
  }
  return card;
}

test("the home page shows three examples, and others on request", async ({ page }) => {
  await page.goto("/");
  const cards = exampleCards(page);
  await expect(cards).toHaveCount(3);
  const first = await cards.allTextContents();

  const more = page.getByRole("button", { name: "Show other examples" });
  await more.focus();
  await page.keyboard.press("Enter");
  await expect(cards).toHaveCount(3);
  await expect.poll(() => cards.allTextContents()).not.toEqual(first);
  await expect(more).toBeFocused();
  await expectAccessible(page);

  // A card runs its search.
  const title = (await cards.first().locator("strong").textContent()) ?? "";
  await cards.first().click();
  await expect(page).toHaveURL(/[?&]q=/);
  // The search ran (most examples match nothing in the fixtures, which still shows a summary).
  await expect(page.locator(".summary")).toContainText("pages");
  // Back returns to the same cards.
  await page.goBack();
  await expect(cards.first()).toContainText(title);
});

test("example search maps, plays, drills down and keeps a permalink", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { level: 1 })).toContainText("newspapers");
  await expect(page.getByRole("note")).toContainText("synthetic");
  await expectAccessible(page);

  await (await findExample(page, /Cross of Gold, 1896/)).click();
  await expect(page).toHaveURL(/q=%22cross\+of\+gold%22/);
  await expect(page.locator(".summary")).toContainText("6 places");

  // Keyboard playback: Home goes to the first week, where nothing has matched yet.
  await page.locator("body").press("Home");
  await expect(page.locator(".dock__label")).toHaveText("Week of Jun 1, 1896");
  // The position reaches the URL once movement pauses.
  await expect(page).toHaveURL(/[?&]t=1896-06-01/);
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

  await page.getByRole("button", { name: "Table" }).click();
  await expect(page).toHaveURL(/tab=table/);
  const rows = page.locator("table.places tbody tr");
  await expect(rows).toHaveCount(early);
  await expectAccessible(page);

  await rows.first().getByRole("button").click();
  const panel = page.getByRole("complementary");
  await expect(panel.locator(".hit").first()).toBeVisible();
  await expect(panel.locator("mark").first()).toBeVisible();
  // The fixtures' LCCNs are invented, so no Library of Congress link is offered.
  await expect(panel.getByRole("link", { name: /Library of Congress/ })).toHaveCount(0);
  await expect(panel.locator(".hit__demo").first()).toContainText("not a real Library of Congress page");

  // The permalink restores the whole view.
  await page.reload();
  await expect(page.getByRole("button", { name: "Table" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".dock__label")).toContainText("1896");
  await expect(page.getByRole("complementary")).toBeVisible();
});

test("the map renders with its worker and no console errors", async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto("/?q=%22cross+of+gold%22&bucket=month");
  await expect(page.getByTestId("map")).toBeVisible();
  await expect(page.locator(".maplibregl-canvas")).toBeVisible();
  await expect(page.locator(".legend")).toContainText("Pages containing the match");
  // The GeoJSON source is parsed in the worker; the map is idle only once it has.
  await page.waitForFunction(() => document.querySelector(".maplibregl-canvas") !== null);
  await page.waitForTimeout(1500);
  expect(errors.filter((e) => !/WebGL|GPU|WEBGL_debug/i.test(e))).toEqual([]);
});

test("space on a focused button activates it, not playback", async ({ page }) => {
  await page.goto("/?q=%22cross+of+gold%22&bucket=month");
  await page.getByRole("button", { name: "Table" }).focus();
  await page.keyboard.press("Space");
  await expect(page.getByRole("button", { name: "Table" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("button", { name: /Play/ })).toHaveAttribute("aria-pressed", "false");
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

test("exact phrase applies to plain words, like a quoted phrase", async ({ page }) => {
  const pages = async (query: string) => {
    await page.goto(`/?${query}&from=1895-01-01&to=1897-12-31&bucket=month&tab=table`);
    const summary = page.locator(".summary");
    await expect(summary).toContainText("pages");
    return Number(((await summary.locator("strong").nth(1).textContent()) ?? "").replace(/,/g, ""));
  };
  const quoted = await pages("q=%22cross+of+gold%22");
  const plain = await pages("q=cross+of+gold");
  const any = await pages("q=cross+of+gold&mode=any");
  expect(plain).toBe(quoted);
  expect(any).toBeGreaterThan(plain);

  // Word order matters for a phrase: typed with the default "Exact phrase"
  // mode, "standard gold" matches nothing (as all words it would match 201).
  await page.goto("/");
  await page.getByRole("searchbox").fill("standard gold");
  await page.getByRole("button", { name: "Search" }).click();
  await expect(page.getByRole("status").filter({ hasText: "No pages match" })).toBeVisible();
  expect(await pages("q=standard+gold&mode=all")).toBeGreaterThan(0);
});

test("the API serves the site: app routes, security headers, cached assets", async ({ page, request }) => {
  test.skip(!process.env.PW_BASE_URL, "only when the API serves the build (CI)");
  const shell = await request.get("/status/");
  expect(shell.status()).toBe(200);
  expect(shell.headers()["content-type"]).toContain("text/html");
  expect(shell.headers()["cache-control"]).toBe("no-cache");
  expect(shell.headers()["content-security-policy"]).toContain("connect-src 'self'");
  expect((await request.get("/assets/does-not-exist.js")).status()).toBe(404);

  // The page loads under that CSP, and its assets are cached for a year.
  const errors = watchErrors(page);
  const script = page.waitForResponse((r) => /\/assets\/index-.*\.js$/.test(r.url()));
  await page.goto("/");
  const js = await script;
  expect(js.headers()["cache-control"]).toBe("public, max-age=31536000, immutable");
  expect(["br", "gzip"]).toContain(js.headers()["content-encoding"]);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("newspapers");
  expect(errors).toEqual([]);
});

test("the search options open with an extra tap on phones", async ({ page, isMobile }) => {
  await page.goto("/");
  const match = page.getByLabel("Match");
  const toggle = page.getByRole("button", { name: /^Options/ });
  if (!isMobile) {
    // Wide screens show the options inline and no toggle.
    await expect(match).toBeVisible();
    await expect(toggle).toBeHidden();
    return;
  }
  await expect(page.getByRole("searchbox")).toBeVisible();
  await expect(match).toBeHidden();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(match).toBeVisible();
  await match.selectOption("all");
  await expect(toggle).toHaveText("Options (1)");
  await expectAccessible(page);

  // A search that uses an option opens with the options showing.
  await page.goto("/?q=gold&mode=any");
  await expect(page.getByLabel("Match")).toBeVisible();
});

test("the status page shows the published version without the pipeline state", async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto("/");
  await page.getByRole("link", { name: "Pipeline status" }).click();
  await expect(page).toHaveURL(/\/status$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Pipeline status");
  // CI's API serves the fixtures with no Cosmos: the reference sections
  // work and the pipeline sections say so.
  await expect(page.getByRole("heading", { name: "Indexing and releases" })).toBeVisible();
  await expect(page.getByText("fixture-v1", { exact: true })).toBeVisible();
  await expect(page.getByText("1 of 8 deltas used", { exact: false }).first()).toBeVisible();
  await expect(page.getByText(/^Not available\./)).toHaveCount(3);
  await expect(page.locator(".health")).toContainText("Pipeline state not available");
  await expect(page.getByRole("link", { name: "/v1/status" })).toHaveAttribute("href", "/v1/status");
  await page.getByText("Raw response").click();
  await expect(page.locator(".status-json")).toContainText('"schema": 1');
  await expectAccessible(page);
  expect(errors).toEqual([]);

  const json = await page.request.get("/v1/status");
  expect(json.headers()["cache-control"]).toBe("public, max-age=30");
  expect((await json.json()).backfill.available).toBe(false);
});

test("the status page lists published pages by state and by language", async ({ page }) => {
  const errors = watchErrors(page);
  await page.goto("/status");
  // The fixtures: six places in six states, 312 pages each, all in English.
  const states = page.getByRole("table", { name: "Published pages by state" });
  await expect(page.getByRole("heading", { level: 2, name: "Pages by state" })).toBeVisible();
  await expect(states.locator("tbody tr")).toHaveCount(6);
  await expect(states.locator("tbody tr").first()).toHaveText(/California\s*1\s*1\s*312\s*16\.7%/);
  await expect(states.locator("tfoot tr")).toHaveText(/Total\s*6\s*6\s*1,872\s*100\.0%/);
  await expect(page.getByText("6 states and territories have pages", { exact: false })).toBeVisible();
  // Sorting by name, then reversing it.
  const byName = states.getByRole("button", { name: "State" });
  await byName.click();
  await byName.click();
  await expect(states.getByRole("columnheader", { name: "State" })).toHaveAttribute("aria-sort", "descending");
  await expect(states.locator("tbody th").first()).toHaveText("South Carolina");

  const languages = page.getByRole("table", { name: "Published pages by language" });
  await expect(page.getByRole("heading", { level: 2, name: "Pages by language" })).toBeVisible();
  await expect(languages.locator("tbody tr")).toHaveCount(1);
  await expect(languages.locator("tbody tr")).toHaveText(/English\s*6\s*1,872\s*100\.0%/);

  // On a phone the page itself never scrolls sideways; a wide table scrolls in its own box.
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth);
  expect(overflow).toBeLessThanOrEqual(0);
  await expectAccessible(page);
  expect(errors).toEqual([]);
});

test("About opens from the footer and keeps the search", async ({ page }) => {
  await page.goto("/?q=gold");
  const dialog = page.getByRole("dialog", { name: "About US News Map" });
  // About is only in the footer, not the header.
  await expect(page.getByRole("banner").getByRole("button", { name: "About" })).toHaveCount(0);
  const open = page.getByRole("contentinfo").getByRole("button", { name: "About" });
  await open.click();
  await expect(dialog).toBeVisible();
  // The GTRI credit links to LinkedIn; the maintainer line links to his site.
  const trevor = dialog.getByRole("link", { name: "Trevor Goodyear" });
  await expect(trevor).toHaveCount(2);
  await expect(trevor.nth(0)).toHaveAttribute("href", "https://www.linkedin.com/in/goodyear/");
  await expect(trevor.nth(1)).toHaveAttribute("href", "https://goodyeartechnical.com/");
  const links: [string, string][] = [
    ["Claudio Saunt", "https://claudiosaunt.com/"],
    ["Steve Berry", "https://history.uga.edu/directory/people/stephen-berry"],
    ["Slate", "https://web.archive.org/web/20190307100233/http://www.slate.com/blogs/the_vault/2016/03/07/us_news_map_interactive_lets_you_map_how_historical_newspapers_digitized.html"],
    ["The Washington Post", "https://web.archive.org/web/20160616154229/https://www.washingtonpost.com/news/the-intersect/wp/2016/03/17/the-secret-pre-internet-history-of-viral-memes/"],
    ["Chronicling America Data Challenge", "https://web.archive.org/web/20170126055934/https://www.neh.gov/news/press-release/2016-07-25"],
  ];
  for (const [name, href] of links) {
    const link = dialog.getByRole("link", { name, exact: true });
    await expect(link).toHaveAttribute("href", href);
    await expect(link).toHaveAttribute("rel", "noopener noreferrer");
  }
  // Named twice: the collection (first paragraph) and who ran the data challenge.
  const ndnp = dialog.getByRole("link", { name: "NEH and Library of Congress", exact: true });
  await expect(ndnp).toHaveCount(2);
  for (const link of await ndnp.all()) {
    await expect(link).toHaveAttribute("href", "https://www.loc.gov/ndnp/");
    await expect(link).toHaveAttribute("rel", "noopener noreferrer");
  }
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();
  await expect(open).toBeFocused();
  await expect(page).toHaveURL(/\?q=gold/);

  await open.click();
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
  await expect(open).toBeFocused();
});

test("About closes on a backdrop click but not on a click inside its box", async ({ page }) => {
  // A short window, so the dialog scrolls and has a scrollbar.
  await page.setViewportSize({ width: 390, height: 400 });
  await page.goto("/");
  const dialog = page.getByRole("dialog", { name: "About US News Map" });
  await page.getByRole("contentinfo").getByRole("button", { name: "About" }).click();
  await expect(dialog).toBeVisible();
  const box = (await dialog.boundingBox())!;
  // The dialog's own edge (border, scrollbar): stays open.
  await page.mouse.click(box.x + box.width - 2, box.y + box.height / 2);
  await expect(dialog).toBeVisible();
  // The backdrop, left of the dialog: closes.
  await page.mouse.click(Math.max(1, box.x / 2), box.y + box.height / 2);
  await expect(dialog).toBeHidden();
});

test("the footer credits link to the NEH and Library of Congress newspaper program", async ({ page }) => {
  await page.goto("/");
  const footer = page.getByRole("contentinfo");
  await expect(footer.getByRole("link", { name: "NEH and Library of Congress" })).toHaveAttribute("href", "https://www.loc.gov/ndnp/");
});
