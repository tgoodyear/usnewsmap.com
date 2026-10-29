import { afterEach, expect, test, vi } from "vitest";
import { enhanceSearchLanding, focusInteractiveSearch } from "./searchLanding";

afterEach(() => {
  document.body.innerHTML = "";
  window.history.replaceState(null, "", "/");
});

test("listed searches retain readable results without mounting the API-dependent app", () => {
  document.body.innerHTML =
    '<div id="root"></div><div id="search-landing"><h1>Newspaper results for whigs</h1><button id="explore-map" hidden>Explore interactive map</button><section aria-label="Sample results"><a href="https://www.loc.gov/">A newspaper page</a></section></div>';
  const mount = vi.fn(() => {
    document.getElementById("root")!.textContent = "Could not load metadata";
  });
  expect(enhanceSearchLanding(mount)).toBe(true);
  expect(mount).not.toHaveBeenCalled();
  const button = document.querySelector<HTMLButtonElement>("button")!;
  expect(button.hidden).toBe(false);
  button.click();
  expect(mount).toHaveBeenCalledOnce();
  expect(document.querySelector("#search-landing a")?.textContent).toBe("A newspaper page");
  expect(document.querySelector("#search-landing [inert], #search-landing .visually-hidden")).toBeNull();
  expect(button.hidden).toBe(true);
  button.click();
  expect(mount).toHaveBeenCalledOnce();
});

test("changing searches or filters hides stale results, and Back restores them", () => {
  window.history.replaceState(null, "", "/search/whigs");
  document.body.innerHTML =
    '<div id="root"></div><div id="search-landing"><button id="explore-map" hidden>Explore map</button></div>';
  enhanceSearchLanding(vi.fn());
  const landing = document.getElementById("search-landing")!;
  for (const [url, hidden] of [
    ["/search/other-query", true],
    ["/search/whigs?state=NY", true],
    ["/search/whigs?bucket=year", false],
    ["/search/whigs", false],
  ] as const) {
    window.history.replaceState(null, "", url);
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(landing.hidden, url).toBe(hidden);
  }
});

test("mounting the app moves focus and leaves only one main landmark", () => {
  document.body.innerHTML =
    '<div id="root"><main><input type="search" aria-label="Search"></main></div><div id="search-landing"><main>Original results</main></div>';
  focusInteractiveSearch();
  expect(document.activeElement).toBe(document.querySelector("input"));
  expect(document.querySelector("#search-landing main")).toBeNull();
  expect(document.querySelector("#search-landing section")?.getAttribute("aria-label")).toBe(
    "Original newspaper search results",
  );
});

test("unlisted searches and other pages use the dynamic app", () => {
  document.body.innerHTML = '<div id="root"><div id="site-fallback">Search newspapers</div></div>';
  expect(enhanceSearchLanding(vi.fn())).toBe(false);
});
