import { postBeacon } from "./pageview";

/** The `cf-cache-status` values that mean Cloudflare answered without asking the API. */
const FROM_EDGE = new Set(["HIT", "STALE", "UPDATING"]);

const reported = new Set<string>();

/**
 * Cloudflare keeps the example searches (06 §6.5), and a search it answers
 * never reaches the API to be counted in the search log (06 §6.8). This tells
 * the API which example it was, once per page load; the example's id is all
 * it sends, and the API applies the log's own checks.
 */
export function reportEdgeExample(
  resp: Pick<Response, "headers">,
  nav: Pick<Navigator, "sendBeacon"> = navigator,
): void {
  const example = resp.headers.get("x-usnm-example");
  if (!example || reported.has(example)) return;
  if (!FROM_EDGE.has(resp.headers.get("cf-cache-status") ?? "")) return;
  reported.add(example);
  postBeacon("/v1/example-search", JSON.stringify({ example }), nav);
}
