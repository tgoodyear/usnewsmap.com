import { afterEach, expect, test } from "vitest";
import { syncRelatedSearches } from "./relatedSearches";

let stop = () => {};

afterEach(() => {
  stop();
  window.history.replaceState(null, "", "/");
  document.body.innerHTML = "";
});

const navigate = (url: string) => {
  window.history.pushState(null, "", url);
  window.dispatchEvent(new PopStateEvent("popstate"));
};

test("shows the links only while the loaded example's path is current", () => {
  window.history.replaceState(null, "", "/examples/cross-of-gold");
  document.body.innerHTML = '<div id="root"></div><nav id="related-searches"><a href="/examples/populists">P</a></nav>';
  stop = syncRelatedSearches();
  const nav = document.getElementById("related-searches")!;
  expect(nav.hidden).toBe(false);
  navigate("/examples/cross-of-gold?place=P00001");
  expect(nav.hidden).toBe(false);
  navigate("/examples/whigs");
  expect(nav.hidden).toBe(true);
  navigate("/search/gold");
  expect(nav.hidden).toBe(true);
  window.history.replaceState(null, "", "/examples/cross-of-gold");
  window.dispatchEvent(new PopStateEvent("popstate"));
  expect(nav.hidden).toBe(false);
});

test("does nothing on a page without the links", () => {
  document.body.innerHTML = '<div id="root"></div>';
  stop = syncRelatedSearches();
  navigate("/search/gold");
  expect(document.getElementById("related-searches")).toBeNull();
});
