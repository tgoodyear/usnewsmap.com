import { afterEach, expect, test, vi } from "vitest";
import { enhanceSearchLanding } from "./searchLanding";

afterEach(() => {
  window.dispatchEvent(new PopStateEvent("popstate"));
  document.body.innerHTML = "";
});

function shell() {
  document.body.innerHTML =
    '<div id="root"></div><div id="search-landing"><main><h1>Newspaper results for whigs</h1><img alt="Map for whigs"><a href="https://www.loc.gov/">A newspaper page</a></main></div>';
}

async function mutations() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("mounts the normal visible app immediately without waiting for results or the static image", async () => {
  shell();
  const root = document.getElementById("root")!;
  const mount = vi.fn(() => {
    root.innerHTML =
      '<main data-share-ready="false"><input type="search"><div data-testid="map" data-basemap-ready="false" data-overlay-ready="false"></div></main>';
  });
  expect(enhanceSearchLanding(mount)).toBe(true);
  expect(mount).toHaveBeenCalledOnce();
  await mutations();
  expect(root.hidden).toBe(false);
  expect(root.inert).not.toBe(true);
  expect(root.hasAttribute("aria-hidden")).toBe(false);
  expect(document.getElementById("search-landing")!.hidden).toBe(true);
});

test("reveals the static map and excerpts when API requests fail, then recovers", async () => {
  shell();
  const mount = vi.fn(() => {
    document.getElementById("root")!.innerHTML =
      '<div data-search-error="true"><p role="alert">Could not load metadata</p></div>';
  });
  expect(enhanceSearchLanding(mount)).toBe(true);
  expect(mount).toHaveBeenCalledOnce();
  await mutations();
  expect(document.querySelector("#search-landing img")).not.toBeNull();
  expect(document.querySelector("#search-landing a")?.textContent).toBe("A newspaper page");
  const root = document.getElementById("root")!;
  expect(root.hidden).toBe(true);
  expect(document.getElementById("search-landing")!.hidden).toBe(false);
  expect(document.getElementById("search-landing")!.classList.contains("search-static-fallback")).toBe(true);
  expect(document.querySelector('[role="status"]')?.textContent).toContain("unavailable");
  root.innerHTML = '<main data-share-ready="false">Retrying</main>';
  await mutations();
  expect(root.hidden).toBe(false);
  expect(document.getElementById("search-landing")!.hidden).toBe(true);
});

test("keeps the normal app usable when optional newspaper or sample requests fail", async () => {
  shell();
  const root = document.getElementById("root")!;
  enhanceSearchLanding(() => {
    root.innerHTML =
      '<main data-share-ready="false"><input type="search"><p role="alert">Newspaper list unavailable</p><section class="visually-hidden" inert><p role="alert">Sample results unavailable</p></section></main>';
  });
  await mutations();
  expect(root.hidden).toBe(false);
  expect(document.getElementById("search-landing")!.hidden).toBe(true);
});

test("discards the hidden static elements once the basemap and result overlay are ready", async () => {
  shell();
  const root = document.getElementById("root")!;
  enhanceSearchLanding(() => {
    root.innerHTML =
      '<main data-share-ready="false"><div data-testid="map" data-basemap-ready="false" data-overlay-ready="false"></div></main>';
  });
  const results = root.querySelector("main")!;
  const map = root.querySelector("[data-testid]")!;
  results.dataset.shareReady = "true";
  await mutations();
  expect(document.getElementById("search-landing")!.hidden).toBe(true);
  expect(root.hidden).toBe(false);
  map.setAttribute("data-basemap-ready", "true");
  map.setAttribute("data-overlay-ready", "true");
  await mutations();
  expect(document.getElementById("search-landing")).toBeNull();
  expect(root.hidden).toBe(false);
});

test("unlisted searches and other pages use the dynamic app directly", () => {
  document.body.innerHTML = '<div id="root"><div id="site-fallback">Search newspapers</div></div>';
  const mount = vi.fn();
  expect(enhanceSearchLanding(mount)).toBe(false);
  expect(mount).not.toHaveBeenCalled();
});

test.each([
  ["empty results", '<div class="notice" role="status"><strong>No pages match.</strong> Try widening the dates.</div>'],
  ["non-WebGL tables", '<div class="tables"><table><tbody><tr><td>Washington</td></tr></tbody></table></div>'],
])("shows the controls immediately and discards the fallback when %s are ready", async (_, content) => {
  shell();
  const root = document.getElementById("root")!;
  enhanceSearchLanding(() => {
    root.innerHTML = `<main data-share-ready="false"><input type="search">${content}</main>`;
  });
  await mutations();
  expect(root.hidden).toBe(false);
  expect(document.getElementById("search-landing")!.hidden).toBe(true);
  root.querySelector("main")!.dataset.shareReady = "true";
  await mutations();
  expect(document.getElementById("search-landing")).toBeNull();
  expect(root.hidden).toBe(false);
});

test("navigation prevents the original fallback from appearing for a different query or filters", async () => {
  shell();
  const root = document.getElementById("root")!;
  enhanceSearchLanding(() => {
    root.innerHTML = '<main data-share-ready="false"><input type="search"></main>';
  });
  window.dispatchEvent(new PopStateEvent("popstate"));
  root.innerHTML = '<p role="alert">New query failed</p>';
  await mutations();
  expect(document.getElementById("search-landing")).toBeNull();
  expect(root.hidden).toBe(false);
});
