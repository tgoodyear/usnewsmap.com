// Words for the relative-rate view (doc 11, 11.6): tooltips, the place
// panel, the lists and the screen-reader summary share these sentences.

import { formatTimes } from "./skewScale";
import { direction, type Score } from "../engine/skew";

export interface SkewInfo {
  estimate: number;
  lower: number;
  upper: number;
  /** Pages that matched, in buckets with other places to compare with. */
  observed: number;
  /** Pages expected to match at the other places' rate. */
  expected: number;
  /** Pages published in the window. */
  pages: number;
  /** 1 clearly above, -1 clearly below, 0 can't tell. */
  dir: -1 | 0 | 1;
  /** "Papers in German and English" when any of its titles isn't in English, else null. */
  languages: string | null;
}

export function formatExpected(x: number): string {
  if (x >= 100) return Math.round(x).toLocaleString("en-US");
  if (x >= 10) return x.toFixed(0);
  if (x >= 0.1) return x.toFixed(1);
  return x > 0 ? "less than 0.1" : "0";
}

/** "5.2 to 7.2" */
export function formatRange(s: Pick<SkewInfo, "lower" | "upper">): string {
  return `${formatTimes(s.lower).replace("×", "")} to ${formatTimes(s.upper)}`;
}

const pagesWord = (n: number) => (n === 1 ? "page" : "pages");

/** "Mobile, AL: 336 pages matched where 47 were expected from its 1,351 pages. About 6.1× the rate of the other places (5.2 to 7.2×)." */
export function skewSentence(name: string, s: SkewInfo): string {
  const counts = `${name}: ${s.observed.toLocaleString("en-US")} ${pagesWord(s.observed)} matched where ${formatExpected(
    s.expected,
  )} ${s.expected >= 0.95 && s.expected < 1.05 ? "was" : "were"} expected from its ${s.pages.toLocaleString("en-US")} ${pagesWord(
    s.pages,
  )}.`;
  const rate =
    s.dir === 0
      ? `Not enough pages to say whether it differs from the other places (${formatRange(s)}).`
      : `About ${formatTimes(s.estimate)} the rate of the other places (${formatRange(s)}).`;
  const lang = s.languages ? ` ${s.languages}.` : "";
  return `${counts} ${rate}${lang}`;
}

/**
 * A score as the view shows it. A place with no pages or nothing expected in
 * the frame has no evidence of its own, so it is never "clearly" above or
 * below, even when the prior alone excludes 1.
 */
export function skewInfo(s: Score, pages: number, languages: string | null): SkewInfo {
  return {
    estimate: s.estimate,
    lower: s.lower,
    upper: s.upper,
    observed: s.observed,
    expected: s.expected,
    pages,
    dir: pages > 0 && s.expected > 0 ? direction(s) : 0,
    languages,
  };
}
