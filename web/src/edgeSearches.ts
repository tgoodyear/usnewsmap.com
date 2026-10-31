import { optedOut, postBeacon } from "./pageview";

/** The `cf-cache-status` values that mean Cloudflare answered without asking the API. */
const FROM_EDGE = new Set(["HIT", "STALE", "UPDATING"]);

const reported = new Set<string>();

/**
 * Cloudflare keeps searches (06 §6.5), and one it answers never reaches the
 * API to be counted in the search log (06 §6.8). This reports it to the API,
 * once per page load, and the API applies the log's own checks.
 */
export function reportEdgeSearch(
  resp: Pick<Response, "headers" | "url">,
  nav: Pick<Navigator, "sendBeacon"> & Parameters<typeof optedOut>[0] = navigator,
): void {
  if (optedOut(nav, window as { doNotTrack?: string | null })) return;
  if (!FROM_EDGE.has(resp.headers.get("cf-cache-status") ?? "")) return;
  const query = new URL(resp.url).search.slice(1);
  if (!query || reported.has(query)) return;
  reported.add(query);
  postBeacon("/v1/search-report", JSON.stringify({ query }), nav);
}
