/**
 * The page for a path. The app has no router: `/` is the search, `/status`
 * the pipeline status, and anything else not found. The API answers the same
 * paths with 200 and the rest with 404 (`is_app_page` in
 * crates/usnm-api/src/site.rs).
 */
export type Page = "search" | "status" | "not-found";

export function pageFor(pathname: string): Page {
  switch (pathname.replace(/\/+$/, "")) {
    case "":
      return "search";
    case "/status":
      return "status";
    default:
      return "not-found";
  }
}
