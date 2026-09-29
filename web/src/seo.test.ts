import { describe, expect, it } from "vitest";
import html from "../index.html?raw";

// The head that search engines and link previews read without running the
// app. The e2e suite (e2e/seo.spec.ts) checks the same tags in a browser.
const doc = new DOMParser().parseFromString(html, "text/html");
const meta = (selector: string) => doc.querySelector(selector)?.getAttribute("content");

describe("index.html", () => {
  it("has a title, a description and a canonical URL", () => {
    expect(doc.title).toBe("US News Map");
    expect(meta('meta[name="description"]')).toBeTruthy();
    expect(doc.querySelector('link[rel="canonical"]')?.getAttribute("href")).toBe("https://usnewsmap.com/");
  });

  it("has Open Graph and Twitter card tags", () => {
    expect(meta('meta[property="og:type"]')).toBe("website");
    expect(meta('meta[property="og:url"]')).toBe("https://usnewsmap.com/");
    for (const p of ["og:site_name", "og:title", "og:description", "og:image:alt"]) {
      expect(meta(`meta[property="${p}"]`), p).toBeTruthy();
    }
    expect(meta('meta[property="og:image"]')).toBe("https://usnewsmap.com/og-image.png");
    expect(meta('meta[property="og:image:width"]')).toBe("1200");
    expect(meta('meta[property="og:image:height"]')).toBe("630");
    expect(meta('meta[name="twitter:card"]')).toBe("summary_large_image");
    expect(meta('meta[name="twitter:image"]')).toBe(meta('meta[property="og:image"]'));
  });

  it("has JSON-LD that parses, with the schema.org context", () => {
    const blocks = [...doc.querySelectorAll('script[type="application/ld+json"]')];
    expect(blocks.length).toBeGreaterThan(0);
    for (const block of blocks) {
      const data = JSON.parse(block.textContent ?? "");
      expect(data["@context"]).toBe("https://schema.org");
      expect(data["@type"]).toBeTruthy();
    }
    const site = JSON.parse(blocks[0]!.textContent ?? "");
    expect(site).toMatchObject({
      "@type": "WebSite",
      name: "US News Map",
      url: "https://usnewsmap.com/",
      publisher: { "@type": "Person", name: "Trevor Goodyear", url: "https://goodyeartechnical.com/" },
    });
    expect(site.description).toBeTruthy();
    // Google retired the sitelinks search box.
    expect(JSON.stringify(site)).not.toContain("SearchAction");
  });

  it("has text in the root for clients that don't run the app, and no h1", () => {
    const root = doc.getElementById("root")!;
    expect(root.textContent).toContain("Chronicling America");
    expect(root.querySelector("a[href='https://goodyeartechnical.com/']")).not.toBeNull();
    expect(root.querySelector("h1")).toBeNull();
  });
});
