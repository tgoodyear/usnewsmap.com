/**
 * The API puts an example page's links to other examples of its era at the
 * end of <body> (`related_links` in crates/usnm-api/src/site.rs), outside the
 * app. They describe the page that was loaded, so they show only while the
 * address is still that page's path: a search or another example picked in
 * the app hides them, and Back shows them again.
 */
export function syncRelatedSearches(): () => void {
  const nav = document.getElementById("related-searches");
  if (!nav) return () => {};
  const path = window.location.pathname;
  const sync = () => {
    nav.hidden = window.location.pathname !== path;
  };
  window.addEventListener("popstate", sync);
  return () => window.removeEventListener("popstate", sync);
}
