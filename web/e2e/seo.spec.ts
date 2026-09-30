import { readdirSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { expect, test, type Page } from "@playwright/test";

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

test("robots.txt keeps crawlers off the API and names the sitemap", async ({ request }) => {
  const res = await request.get("/robots.txt");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/^text\/plain/);
  const body = await res.text();
  expect(body).toMatch(/^User-agent: \*$/m);
  expect(body).toMatch(/^Disallow: \/v1\/$/m);
  expect(body).toMatch(new RegExp(`^Sitemap: ${SITE}/sitemap\\.xml$`, "m"));
});

test("sitemap.xml lists the home page and the privacy page", async ({ request }) => {
  const res = await request.get("/sitemap.xml");
  expect(res.status()).toBe(200);
  expect(res.headers()["content-type"]).toMatch(/xml/);
  const locs = [...(await res.text()).matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
  expect(locs).toEqual([`${SITE}/`, `${SITE}/privacy`]);
});

test("the privacy page renders, with links to it from the other pages", async ({ page }) => {
  await page.route("https://tiles.openfreemap.org/**", (route) => route.fulfill({ status: 404 }));
  await page.goto("/privacy");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Privacy");
  await expect(page.locator("h1")).toHaveCount(1);
  await expect(page).toHaveTitle("Privacy · US News Map");
  for (const path of ["/", "/status", "/does-not-exist"]) {
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
    expect((await request.get("/status")).status()).toBe(200);
  });

  test("search permalinks and the status page are noindex; the home page is not", async ({ request }) => {
    for (const url of ["/?q=x", "/status"]) {
      const res = await request.get(url);
      expect(res.status(), url).toBe(200);
      expect(res.headers()["x-robots-tag"], url).toBe("noindex");
    }
    const home = await request.get("/");
    expect(home.headers()["x-robots-tag"]).toBeUndefined();
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
