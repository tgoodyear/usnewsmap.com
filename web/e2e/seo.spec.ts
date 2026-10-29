import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";
import examples from "../src/examples.json" with { type: "json" };
import { searchPath } from "../src/route";
import AxeBuilder from "@axe-core/playwright";

// SEO smoke tests: the head that search engines and link previews read, the
// page structure once the app renders, and what the API sends crawlers.
// Adapted from goodyeartechnical.com's tests/e2e/seo.spec.js.

const SITE = "https://usnewsmap.com";
const withApi = !!process.env.PW_BASE_URL;
const PUBLIC = fileURLToPath(new URL("../public/", import.meta.url));

async function content(page: Page, selector: string) {
  return page.locator(selector).getAttribute("content");
}

test.describe("home page head and structure", () => {
  test.beforeEach(async ({ page }) => {
    await page.goto("/");
  });

  test("title and meta description", async ({ page }) => {
    await expect(page).toHaveTitle("US News Map");
    expect(await content(page, 'meta[name="description"]')).toBeTruthy();
  });

  test("canonical link points at the home page", async ({ page }) => {
    await expect(page.locator('link[rel="canonical"]')).toHaveAttribute("href", `${SITE}/`);
  });

  test("Open Graph and Twitter card tags", async ({ page }) => {
    expect(await content(page, 'meta[property="og:type"]')).toBe("website");
    expect(await content(page, 'meta[property="og:site_name"]')).toBe("US News Map");
    expect(await content(page, 'meta[property="og:title"]')).toBeTruthy();
    expect(await content(page, 'meta[property="og:description"]')).toBeTruthy();
    expect(await content(page, 'meta[property="og:url"]')).toBe(`${SITE}/`);
    expect(await content(page, 'meta[property="og:image"]')).toBe(`${SITE}/og-image.png`);
    expect(await content(page, 'meta[property="og:image:width"]')).toBe("1200");
    expect(await content(page, 'meta[property="og:image:height"]')).toBe("630");
    expect(await content(page, 'meta[property="og:image:alt"]')).toBeTruthy();
    expect(await content(page, 'meta[name="twitter:card"]')).toBe("summary_large_image");
  });

  test("the share image is served", async ({ request }) => {
    const image = await request.get("/og-image.png");
    expect(image.status()).toBe(200);
    expect(image.headers()["content-type"]).toBe("image/png");
  });

  test("exactly one h1 once the app renders, and the static text is gone", async ({ page }) => {
    await expect(page.getByRole("heading", { level: 1 })).toContainText("newspapers");
    await expect(page.locator("h1")).toHaveCount(1);
    // The static description's <noscript> is gone with the rest of it.
    await expect(page.locator("#root noscript")).toHaveCount(0);
  });

  test("every JSON-LD block parses and declares the schema.org context", async ({ page }) => {
    const blocks = await page
      .locator('script[type="application/ld+json"]')
      .evaluateAll((scripts) => scripts.map((s) => s.textContent ?? ""));
    expect(blocks.length).toBeGreaterThan(0);
    for (const raw of blocks) {
      const data = JSON.parse(raw);
      expect(data["@context"]).toBe("https://schema.org");
      expect(data["@type"]).toBeTruthy();
    }
  });
});

test("the static description is in the HTML before the app runs", async ({ request }) => {
  const html = await (await request.get("/")).text();
  expect(html).toContain("Chronicling America");
  expect(html).toContain('href="https://goodyeartechnical.com/"');
});

test("curated searches expose top-city results and homepage links without JavaScript", async ({ browser }) => {
  test.skip(!withApi, "the API renders the search preview");
  test.setTimeout(60_000);
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  try {
    await page.goto(`${process.env.PW_BASE_URL}/`);
    const home = page.getByRole("navigation", { name: "Common newspaper searches" });
    await expect(home.getByRole("link", { name: "cross of gold", exact: true })).toHaveAttribute(
      "href",
      "/search/cross-of-gold",
    );
    await home.getByRole("link", { name: "cross of gold", exact: true }).click();
    const samples = page.getByRole("region", { name: "Sample results" });
    await expect(samples).toContainText("has the most hits");
    await expect(samples.locator("ol > li")).toHaveCount(10);
    await expect(samples).toContainText("cross of gold");
    await expect(samples).toContainText("1896-07-18");
    const map = page.getByRole("img", { name: "Map and timeline of newspaper matches for cross of gold" });
    await expect(map).toBeVisible();
    await expect
      .poll(() => map.evaluate((image: HTMLImageElement) => image.complete && image.naturalWidth === 1200), {
        timeout: 50_000,
      })
      .toBe(true);
  } finally {
    await context.close();
  }
});

test("sitemap searches remain readable with JavaScript enabled and API access blocked", async ({ page }) => {
  test.skip(!withApi, "the API renders the search landing page");
  const apiRequests: string[] = [];
  await page.route("**/v1/**", (route) => {
    apiRequests.push(route.request().url());
    return route.abort();
  });
  await page.goto("/search/cross-of-gold");
  const landing = page.locator("#search-landing");
  const samples = landing.getByRole("region", { name: "Sample results" });
  await expect(landing.getByRole("heading", { level: 1 })).toHaveText("Newspaper results for cross of gold");
  await expect(samples).toBeVisible();
  await expect(samples.locator("ol > li")).toHaveCount(10);
  await expect(samples).toContainText("1896-07-18");
  await expect(page.locator("#root")).toBeHidden();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await expect(landing.getByRole("img", { name: /Map and timeline/ })).toBeVisible();
  await expect(page.getByRole("main")).toHaveCount(1);
  await expect.poll(() => apiRequests.some((url) => new URL(url).pathname === "/v1/meta")).toBe(true);
  await expect(samples).toBeVisible();
  await expect(samples.locator("ol > li")).toHaveCount(10);
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});

test("the static map upgrades automatically only after the matching interactive map paints", async ({ page }) => {
  test.skip(!withApi, "the API renders the static map");
  test.setTimeout(60_000);
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/v1/meta", async (route) => {
    await gate;
    await route.continue();
  });
  await page.goto("/search/cross-of-gold", { waitUntil: "domcontentloaded" });
  const landing = page.locator("#search-landing");
  await expect(landing).toBeVisible();
  await expect(page.locator("#root")).toBeHidden();
  await expect(landing.getByRole("region", { name: "Sample results" })).toContainText("1896-07-18");
  release();
  await expect(page.getByTestId("map")).toBeVisible({ timeout: 50_000 });
  await expect(landing).toHaveCount(0);
  await expect(page.getByTestId("map")).toHaveAttribute("data-basemap-ready", "true");
  await expect(page.getByTestId("map")).toHaveAttribute("data-overlay-ready", "true");
  await expect(page.getByRole("main")).toHaveCount(1);
  await expect(page.getByRole("searchbox", { name: "Word or phrase" })).toHaveValue("cross of gold");
});

test("an unknown path shows the not-found page", async ({ page }) => {
  const res = await page.goto("/does-not-exist");
  // vite preview answers every path with the shell and a 200.
  if (withApi) expect(res?.status()).toBe(404);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Page not found");
  await expect(page.locator("h1")).toHaveCount(1);
  await expect(page).toHaveTitle("Page not found · US News Map");
  await page.getByRole("link", { name: "Search the newspapers" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("newspapers");
});

/** robots.txt groups (RFC 9309): consecutive `User-agent` lines share the rules after them. */
function robotsGroups(body: string): Map<string, string[]> {
  const groups = new Map<string, string[]>();
  let agents: string[] = [];
  let inRules = false;
  for (const raw of body.split("\n")) {
    const line = raw.replace(/#.*/, "").trim();
    const m = /^([A-Za-z-]+):\s*(.*)$/.exec(line);
    if (!m) continue;
    const key = m[1] ?? "";
    const value = m[2] ?? "";
    if (key.toLowerCase() === "user-agent") {
      if (inRules) agents = [];
      inRules = false;
      agents.push(value.toLowerCase());
      groups.set(value.toLowerCase(), []);
    } else if (/^(allow|disallow)$/i.test(key) && agents.length) {
      inRules = true;
      for (const a of agents) groups.get(a)?.push(`${key}: ${value}`);
    }
  }
  return groups;
}

// The user-initiated fetchers of AI assistants, acting for a person (not crawlers).
const AI_USER_AGENTS = ["Claude-User", "ChatGPT-User", "Perplexity-User", "MistralAI-User", "Google-Agent"];

test("robots.txt keeps crawlers off the API and names the sitemap", async ({ request }) => {
  const res = await request.get("/robots.txt");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/^text\/plain/);
  const body = await res.text();
  const groups = robotsGroups(body);
  expect(groups.get("*")).toEqual(["Allow: /", "Disallow: /v1/", "Disallow: /api/v1/"]);
  expect(body).toMatch(new RegExp(`^Sitemap: ${SITE}/sitemap\\.xml$`, "m"));
});

test("robots.txt lets AI assistants' user-initiated fetchers use the API", async ({ request }) => {
  const groups = robotsGroups(await (await request.get("/robots.txt")).text());
  for (const agent of AI_USER_AGENTS) {
    // Groups don't inherit from `*`, so each needs its own rules.
    expect(groups.get(agent.toLowerCase()), agent).toEqual(["Allow: /"]);
  }
});

test("llms.txt is plain text that describes the site and the API", async ({ request }) => {
  const res = await request.get("/llms.txt");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/^text\/plain/);
  expect(res.headers()["x-robots-tag"]).toBeUndefined();
  const body = await res.text();
  expect(body).toMatch(/^# US News Map\n/);
  expect(body).toMatch(/^> /m);
  expect(body).toContain("https://api.usnewsmap.com/v1");
  for (const route of ["meta", "aggregate", "hits", "places", "coverage", "status", "openapi.json"]) {
    expect(body).toContain(`/v1/${route}`);
  }
});

test("the API's OpenAPI document is served on the site's origin too", async ({ request }) => {
  const res = await request.get("/v1/openapi.json");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/^application\/json/);
  const doc = await res.json();
  expect(doc.openapi).toBe("3.1.0");
  expect(doc.servers.map((s: { url: string }) => s.url)).toContain(SITE);
  expect(Object.keys(doc.paths)).toContain("/v1/aggregate");
});

test("sitemap.xml lists canonical site pages and curated base searches", async ({ request }) => {
  const res = await request.get("/sitemap.xml");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/xml/);
  const locs = [...(await res.text()).matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
  expect(locs).toEqual([
    `${SITE}/`,
    `${SITE}/privacy`,
    `${SITE}/datasets`,
    `${SITE}/credits`,
    ...[...new Set(examples.map(({ view }) => searchPath(view.q)))].map((path) => `${SITE}${path}`),
  ]);
  if (withApi) {
    for (const loc of locs) {
      const page = await request.get(loc!.replace(SITE, ""));
      expect(page.status(), loc).toBe(200);
      expect(page.headers()["x-robots-tag"], loc).toBeUndefined();
      expect(await page.text(), loc).toContain(`<link rel="canonical" href="${loc}" />`);
    }
  }
});

test("the privacy page renders, with links to it from the other pages", async ({ page }) => {
  await page.route(/tiles\.openfreemap\.org\/|\/basemap\/[^/]+\/style\.json$/, (route) =>
    route.fulfill({ status: 404 }),
  );
  await page.goto("/privacy");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Privacy");
  await expect(page.locator("h1")).toHaveCount(1);
  await expect(page).toHaveTitle("Privacy · US News Map");
  for (const path of ["/", "/datasets", "/does-not-exist"]) {
    await page.goto(path);
    await expect(page.getByRole("link", { name: "Privacy" }), path).toHaveAttribute("href", "/privacy");
  }
});

test("the IndexNow key file holds the key", async ({ request }) => {
  const names = readdirSync(PUBLIC).filter((f) => /^[0-9a-f]{32}\.txt$/.test(f));
  expect(names).toHaveLength(1);
  const key = names[0]!.replace(/\.txt$/, "");
  expect(readFileSync(`${PUBLIC}${key}.txt`, "utf8")).toBe(key);
  const res = await request.get(`/${key}.txt`);
  expect(res.status()).toBe(200);
  expect(await res.text()).toBe(key);
});

test.describe("what the API tells crawlers", () => {
  test.skip(!withApi, "only when the API serves the build (CI)");

  test("unknown paths are 404s with the app shell", async ({ request }) => {
    const res = await request.get("/does-not-exist");
    expect(res.status()).toBe(404);
    expect(res.headers()["content-type"]).toContain("text/html");
    expect(await res.text()).toContain('id="root"');
    expect((await request.get("/datasets")).status()).toBe(200);
  });

  test("the old status path redirects to the dataset details (#294)", async ({ request }) => {
    for (const [url, location] of [
      ["/status", "/datasets"],
      ["/status/?x=1", "/datasets?x=1"],
    ] as const) {
      const res = await request.get(url, { maxRedirects: 0 });
      expect(res.status(), url).toBe(301);
      expect(res.headers()["location"], url).toBe(location);
      expect(res.headers()["x-robots-tag"], url).toBe("noindex");
    }
    expect((await request.get("/status")).url()).toMatch(/\/datasets$/);
  });

  test("uncurated searches and filtered permalinks are noindex; site pages are not", async ({ request }) => {
    for (const url of ["/search/x", "/search/whigs?tab=table"]) {
      const res = await request.get(url);
      expect(res.status(), url).toBe(200);
      expect(res.headers()["x-robots-tag"], url).toBe("noindex");
    }
    for (const url of ["/", "/datasets", "/credits", "/privacy"]) {
      const page = await request.get(url);
      expect(page.headers()["x-robots-tag"], url).toBeUndefined();
    }
  });

  test("legacy searches redirect without losing filters", async ({ request }) => {
    const res = await request.get("/?q=%22cross+of+gold%22&tab=table", { maxRedirects: 0 });
    expect(res.status()).toBe(301);
    expect(res.headers()["location"]).toBe("/search/cross-of-gold?tab=table");
  });

  test("the privacy page is indexable and names itself in the head", async ({ request }) => {
    const res = await request.get("/privacy");
    expect(res.status()).toBe(200);
    expect(res.headers()["x-robots-tag"]).toBeUndefined();
    const html = await res.text();
    expect(html).toContain("<title>Privacy · US News Map</title>");
    expect(html).toContain(`<link rel="canonical" href="${SITE}/privacy" />`);
    expect(html).toContain(`<meta property="og:url" content="${SITE}/privacy" />`);
  });
});
