import { afterEach, expect, test, vi } from "vitest";
import { enhanceSearchLanding } from "./searchLanding";

afterEach(() => {
  document.body.innerHTML = "";
});

function shell() {
  document.body.innerHTML =
    '<div id="root"></div><div id="search-landing"><main><h1>Newspaper results for whigs</h1><img alt="Map for whigs"><a href="https://www.loc.gov/">A newspaper page</a></main></div>';
}

async function mutations() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

test("automatically mounts offscreen and retains the static map when API requests fail", async () => {
  shell();
  const mount = vi.fn(() => {
    document.getElementById("root")!.innerHTML = '<p role="alert">Could not load metadata</p>';
  });
  expect(enhanceSearchLanding(mount)).toBe(true);
  expect(mount).toHaveBeenCalledOnce();
  await mutations();
  expect(document.querySelector("#search-landing img")).not.toBeNull();
  expect(document.querySelector("#search-landing a")?.textContent).toBe("A newspaper page");
  expect(document.getElementById("root")!.inert).toBe(true);
  expect(document.getElementById("root")!.getAttribute("aria-hidden")).toBe("true");
  expect(document.querySelector('[role="status"]')?.textContent).toContain("unavailable");
});

test("replaces all static elements only after the actual basemap and result overlay are ready", async () => {
  shell();
  const frames: FrameRequestCallback[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  try {
    const root = document.getElementById("root")!;
    enhanceSearchLanding(() => {
      root.innerHTML =
        '<main data-share-ready="false"><div data-testid="map" data-basemap-ready="false" data-overlay-ready="false"></div></main>';
    });
    const results = root.querySelector("main")!;
    const map = root.querySelector("[data-testid]")!;
    results.dataset.shareReady = "true";
    await mutations();
    expect(frames).toHaveLength(0);
    map.setAttribute("data-basemap-ready", "true");
    map.setAttribute("data-overlay-ready", "true");
    await mutations();
    expect(frames).toHaveLength(1);
    expect(document.getElementById("search-landing")).not.toBeNull();
    frames.shift()!(0);
    expect(document.getElementById("search-landing")).not.toBeNull();
    frames.shift()!(0);
    expect(document.getElementById("search-landing")).toBeNull();
    expect(root.inert).toBe(false);
    expect(root.hasAttribute("aria-hidden")).toBe(false);
    expect(root.classList.contains("search-hydrating")).toBe(false);
  } finally {
    vi.unstubAllGlobals();
  }
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
])("reveals search controls only after %s are ready", async (_, content) => {
  shell();
  const frames: FrameRequestCallback[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  try {
    const root = document.getElementById("root")!;
    enhanceSearchLanding(() => {
      root.innerHTML = `<main data-share-ready="false"><input type="search">${content}</main>`;
    });
    await mutations();
    expect(frames).toHaveLength(0);
    root.querySelector("main")!.dataset.shareReady = "true";
    await mutations();
    frames.shift()!(0);
    frames.shift()!(0);
    expect(document.getElementById("search-landing")).toBeNull();
    expect(root.inert).toBe(false);
    expect(root.classList.contains("search-hydrating")).toBe(false);
  } finally {
    vi.unstubAllGlobals();
  }
});

test("keeps the static view if readiness regresses before the replacement paints", async () => {
  shell();
  const frames: FrameRequestCallback[] = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  try {
    const root = document.getElementById("root")!;
    enhanceSearchLanding(() => {
      root.innerHTML = '<main data-share-ready="true"><div class="tables"></div></main>';
    });
    await mutations();
    frames.shift()!(0);
    root.querySelector("main")!.dataset.shareReady = "false";
    frames.shift()!(0);
    expect(document.getElementById("search-landing")).not.toBeNull();
    expect(root.inert).toBe(true);
    await mutations();
    expect(frames).toHaveLength(0);
    root.querySelector("main")!.dataset.shareReady = "true";
    await mutations();
    frames.shift()!(0);
    frames.shift()!(0);
    expect(document.getElementById("search-landing")).toBeNull();
    expect(root.inert).toBe(false);
  } finally {
    vi.unstubAllGlobals();
  }
});
