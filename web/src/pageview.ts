import { API_BASE } from "./api/client";
import { TITLES, type Page } from "./route";

/**
 * Page views (07 §7.8). Each page the app shows reports one page view to the
 * API (`POST /v1/beacon`), which forwards it to Application Insights. The
 * report holds the page's name and title and, for the first page of a page
 * load, the referring site's origin and the landing URL's `utm_*` tags.
 * Never the path, the query string or the search: the API refuses any other
 * field. Nothing is stored in the browser.
 *
 * Only the production site at https://usnewsmap.com reports, so development
 * and end-to-end runs send nothing (e2e/pageview.spec.ts loads the build as
 * that origin to check what it sends) (`VITE_PAGE_VIEWS=1` turns it on anywhere,
 * `0` off). Do Not Track and Global Privacy Control turn it off.
 */

export const SITE_ORIGIN = "https://usnewsmap.com";
export const MAX_UTM = 64;
const UTM_KEYS = ["utm_source", "utm_medium", "utm_campaign"] as const;
type UtmKey = (typeof UTM_KEYS)[number];

/** The body of `POST /v1/beacon`. */
export type PageViewReport = {
  route: Page;
  title: string;
  referrer_origin: string;
} & Partial<Record<UtmKey, string>>;

type PrivacySignals = {
  doNotTrack?: string | null;
  globalPrivacyControl?: boolean;
};

/** Whether the visitor asked not to be tracked (DNT or GPC). */
export function optedOut(nav: PrivacySignals, win?: { doNotTrack?: string | null }): boolean {
  return nav.doNotTrack === "1" || win?.doNotTrack === "1" || nav.globalPrivacyControl === true;
}

/** Whether this page load reports page views. */
export function enabled(opts: {
  prod: boolean;
  flag: string | undefined;
  origin: string;
  nav: PrivacySignals;
  win?: { doNotTrack?: string | null };
}): boolean {
  if (optedOut(opts.nav, opts.win)) return false;
  if (opts.flag === "1") return true;
  if (opts.flag === "0") return false;
  return opts.prod && opts.origin === SITE_ORIGIN;
}

/**
 * The referrer reduced to its origin: `internal` for this site, empty for
 * none (or anything that isn't an http(s) URL).
 */
export function referrerOrigin(referrer: string, ownOrigin: string): string {
  if (!referrer) return "";
  let url: URL;
  try {
    url = new URL(referrer);
  } catch {
    return "";
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return "";
  return url.origin === ownOrigin ? "internal" : url.origin;
}

/** The `utm_*` tags of a query string, lowercased and cut to 64 characters. */
export function utmTags(search: string): Partial<Record<UtmKey, string>> {
  const params = new URLSearchParams(search);
  const tags: Partial<Record<UtmKey, string>> = {};
  for (const key of UTM_KEYS) {
    const value = [...(params.get(key) ?? "")]
      .filter((c) => !/\p{Cc}/u.test(c))
      .join("")
      .trim()
      .toLowerCase();
    const capped = [...value].slice(0, MAX_UTM).join("").trim();
    if (capped) tags[key] = capped;
  }
  return tags;
}

/**
 * The report for a page. The first page of a page load carries the referrer
 * and the landing URL's tags; later ones say `internal`.
 */
export function report(
  page: Page,
  first: boolean,
  landing: { referrer: string; search: string; origin: string },
): PageViewReport {
  if (!first) return { route: page, title: TITLES[page], referrer_origin: "internal" };
  return {
    route: page,
    title: TITLES[page],
    referrer_origin: referrerOrigin(landing.referrer, landing.origin),
    ...utmTags(landing.search),
  };
}

/** Send a report without delaying the page or its unload. */
export function send(body: PageViewReport, nav: Pick<Navigator, "sendBeacon"> = navigator): void {
  const url = `${API_BASE}/v1/beacon`;
  const json = JSON.stringify(body);
  try {
    // A string goes as text/plain, which needs no CORS preflight.
    if (typeof nav.sendBeacon === "function" && nav.sendBeacon(url, json)) return;
  } catch {
    // Fall through to fetch.
  }
  void fetch(url, {
    method: "POST",
    body: json,
    keepalive: true,
    credentials: "omit",
    headers: { "content-type": "application/json" },
  }).catch(() => undefined);
}

let reported = 0;
// Read once, when the app loads: the landing page's referrer and tags.
const landing =
  typeof window === "undefined"
    ? { referrer: "", search: "", origin: "" }
    : { referrer: document.referrer, search: window.location.search, origin: window.location.origin };

/** Report that the app is showing `page`. Call once per page shown. */
export function trackPageView(page: Page): void {
  const on = enabled({
    prod: import.meta.env.PROD,
    flag: import.meta.env.VITE_PAGE_VIEWS,
    origin: landing.origin,
    nav: navigator as Navigator & PrivacySignals,
    win: window as Window & { doNotTrack?: string | null },
  });
  if (!on) return;
  send(report(page, reported === 0, landing));
  reported += 1;
}
