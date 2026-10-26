/**
 * The page for a path. `/` and `/search/<query>` are search, `/datasets`
 * the dataset details, `/privacy` the privacy notice, `/credits` the OCR credits, and anything else not
 * found. The API answers the same paths with 200 and the rest with 404
 * (`is_app_page` in crates/usnm-api/src/site.rs). `/index.html` is the home
 * page's file, which the API serves with a 200, with or without a trailing
 * slash. `/status`, the dataset details' old path, is redirected to
 * `/datasets` by the API; the app shows the same page there too, for a
 * server that doesn't redirect (the dev server). The page names are also the
 * routes `/v1/beacon` accepts (`ROUTES` in crates/usnm-api/src/routes/beacon.rs).
 */
export type Page = "search" | "datasets" | "privacy" | "credits" | "not-found";

/** The dataset details page's path, and its old one (#294). */
export const DATASETS_PATH = "/datasets";
export const OLD_DATASETS_PATH = "/status";

const WORD = /^[\p{L}\p{M}\p{N}]+$/u;
const SLUG = /^[\p{L}\p{M}\p{N}]+(?:-[\p{L}\p{M}\p{N}]+)*$/u;

/**
 * The readable path segment for a plain exact-phrase search: lowercase words
 * joined by hyphens, with any surrounding quotes dropped. Search is
 * case-insensitive and a hyphen inside a word is a word break, so this means
 * what the query did. Null when the query has syntax (operators, wildcards,
 * other punctuation) or the mode is not the default, where quotes matter.
 * Mirrored by `slug_for` in crates/usnm-api/src/site.rs.
 */
export function slugFor(q: string, mode = "phrase"): string | null {
  if (mode !== "phrase") return null;
  let text = q.trim();
  const inner = text.slice(1, -1);
  if (text.length >= 2 && text.startsWith('"') && text.endsWith('"') && !inner.includes('"')) {
    text = inner;
  } else if (text.split(/\s+/).some((w) => w === "AND" || w === "OR" || w === "NOT" || w.startsWith("-"))) {
    return null;
  }
  const words = text.split(/[\s-]+/).filter(Boolean);
  if (words.length === 0 || !words.every((w) => WORD.test(w))) return null;
  return words.map((w) => w.toLowerCase()).join("-");
}

/** A single encoded segment preserves spaces, punctuation and search syntax. */
export function searchPath(q: string, mode = "phrase"): string {
  // Browsers normalize dot-only path segments, even when percent-encoded.
  if (!q || q === "." || q === "..") return "/";
  const slug = slugFor(q, mode);
  if (slug) return `/search/${slug}`;
  return `/search/${new URLSearchParams({ q }).toString().slice(2).replace(/\+/g, "%20")}`;
}

export function queryForPath(pathname: string): string | null {
  const match = /^\/search\/([^/]+)\/?$/.exec(pathname);
  if (!match) return null;
  let q: string;
  try {
    q = decodeURIComponent(match[1]!);
  } catch {
    return null;
  }
  // A lowercase hyphenated segment is a readable slug (see `slugFor`).
  if (SLUG.test(q) && q === q.toLowerCase()) q = q.replace(/-/g, " ");
  return q.length > 0 && q.length <= 256 && q !== "." && q !== ".." ? q : null;
}

export function pageFor(pathname: string): Page {
  if (queryForPath(pathname) !== null) return "search";
  switch (pathname.replace(/\/+$/, "")) {
    case "":
    case "/index.html":
      return "search";
    case DATASETS_PATH:
    case OLD_DATASETS_PATH:
      return "datasets";
    case "/credits":
      return "credits";
    case "/privacy":
      return "privacy";
    default:
      return "not-found";
  }
}

/** Each page's document title. None of them holds search text. */
export const TITLES: Record<Page, string> = {
  search: "US News Map",
  datasets: "Dataset details · US News Map",
  privacy: "Privacy · US News Map",
  credits: "OCR credits · US News Map",
  "not-found": "Page not found · US News Map",
};
