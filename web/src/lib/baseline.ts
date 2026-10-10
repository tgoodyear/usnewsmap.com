import type { AggregateResponse } from "../api/types";
import { languageName } from "./languages";

// Which pages a search is compared with (#237, 06 §6.3.3): the relative
// rate and the share of pages published divide by them. The API decides;
// these say so on the page.

type Baseline = NonNullable<AggregateResponse["baseline"]>;

const joined = (names: string[]) =>
  names.length === 1 ? names[0]! : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;

/**
 * The sentence in the relative rate's explanation saying which pages each
 * place's matches are compared with, or null when the API doesn't say (an
 * older API) or the search has no baselines (a newspaper or front-page filter).
 */
export function baselineNote(baseline: Baseline | null | undefined): string | null {
  if (!baseline) return null;
  const names = joined(baseline.languages.map((c) => languageName(c)));
  if (baseline.why === "query_language") {
    if (baseline.languages.length === 1 && baseline.languages[0] === "eng") {
      return (
        "Each place's matching pages are compared with the pages of its English-language newspapers, since this " +
        "search's words are English. The few matches in newspapers catalogued in other languages, mostly bilingual " +
        "papers, still count."
      );
    }
    return `Each place's matching pages are compared with the pages of its ${names}-language newspapers, since this search's words are ${names}.`;
  }
  if (baseline.why === "filter") {
    const chosen = baseline.languages.length === 1 ? "the language chosen" : "the languages chosen";
    return `Each place's matching pages are compared with the pages of its newspapers in ${names}, ${chosen}.`;
  }
  return (
    "Each place's matching pages are compared with all its newspapers' pages, in every language. Choose a language " +
    "under Languages to compare with that language's newspapers only."
  );
}
