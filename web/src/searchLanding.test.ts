import { afterEach, expect, test, vi } from "vitest";
import { enhanceSearchLanding } from "./searchLanding";

afterEach(() => {
  document.body.innerHTML = "";
});

test("listed searches retain readable results without mounting the API-dependent app", () => {
  document.body.innerHTML =
    '<div id="root"></div><div id="search-landing"><h1>Newspaper results for whigs</h1><button id="explore-map" hidden>Explore interactive map</button><section aria-label="Sample results"><a href="https://www.loc.gov/">A newspaper page</a></section></div>';
  const mount = vi.fn(() => {
    document.getElementById("root")!.textContent = "Could not load metadata";
  });
  document.getElementById("root")!.scrollIntoView = vi.fn();
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

test("unlisted searches and other pages use the dynamic app", () => {
  document.body.innerHTML = '<div id="root"><div id="site-fallback">Search newspapers</div></div>';
  expect(enhanceSearchLanding(vi.fn())).toBe(false);
});
