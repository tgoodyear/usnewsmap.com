/**
 * The page for a path. The app has no router: `/` is the search, `/status`
 * the pipeline status, `/privacy` the privacy notice, and anything else not
 * found. The API answers the same paths with 200 and the rest with 404
 * (`is_app_page` in crates/usnm-api/src/site.rs). `/index.html` is the home
 * page's file, which the API serves with a 200, with or without a trailing
 * slash. The page names are also the routes `/v1/beacon` accepts (`ROUTES`
 * in crates/usnm-api/src/routes/beacon.rs).
 */
export type Page = "search" | "status" | "privacy" | "not-found";

export function pageFor(pathname: string): Page {
  switch (pathname.replace(/\/+$/, "")) {
    case "":
    case "/index.html":
      return "search";
    case "/status":
      return "status";
    case "/privacy":
      return "privacy";
    default:
      return "not-found";
  }
}

/** Each page's document title. None of them holds search text. */
export const TITLES: Record<Page, string> = {
  search: "US News Map",
  status: "Pipeline status · US News Map",
  privacy: "Privacy · US News Map",
  "not-found": "Page not found · US News Map",
};
