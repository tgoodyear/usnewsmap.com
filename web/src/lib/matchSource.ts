import type { HitItem } from "../api/types";

// Which OCR a search matched (#218). The API says so only for a version
// that searches American Stories' text too; without it these say nothing.

/** The badge on a page that matched only in American Stories' text. */
export const AMERICAN_STORIES_BADGE = "American Stories OCR";

/**
 * Why a page carries the American Stories badge, or null when it doesn't:
 * the page matched in American Stories' text and not in LoC's. The snippet's
 * source decides the middle sentence.
 */
export function americanStoriesNote(h: Pick<HitItem, "matched_in" | "snippet_source">): string | null {
  const m = h.matched_in;
  if (!m || m.length !== 1 || m[0] !== "american_stories") return null;
  const snippet =
    h.snippet_source === "american_stories"
      ? "The snippet comes from that text."
      : "The snippet comes from the Library of Congress text, which matches only part of the search.";
  return (
    "The Library of Congress text for this page doesn't contain the match. " +
    "It was found in American Stories (Dell et al. 2023), a second machine reading of Chronicling America's scans. " +
    `${snippet} The page image at the Library of Congress shows what was printed.`
  );
}

/**
 * The note on the search's page count: how many of the matching pages
 * matched only in American Stories' text. Null when the API doesn't say, or
 * none did, or the count shown isn't every matching page (playback stopped
 * short of the end, or a trailing window): the number is for the whole search.
 */
export function americanStoriesOnlyNote(only: number | undefined, total: number, shown: number): string | null {
  if (only === undefined || only <= 0 || shown !== total) return null;
  const n = only.toLocaleString("en-US");
  return `${n} of the ${total.toLocaleString("en-US")} pages in this search ${only === 1 ? "matches" : "match"} only in American Stories' text (Dell et al. 2023), not in the Library of Congress text.`;
}
