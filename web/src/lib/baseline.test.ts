import { describe, expect, it } from "vitest";
import { baselineNote } from "./baseline";

describe("baselineNote", () => {
  it("says an English search is compared with English-language pages, its other matches still counted", () => {
    const note = baselineNote({ languages: ["eng"], why: "query_language" })!;
    expect(note).toContain("compared with the pages of its English-language newspapers");
    expect(note).toContain("matches in newspapers catalogued in other languages, mostly bilingual papers, still count");
  });

  it("names a Japanese search's pages", () => {
    expect(baselineNote({ languages: ["jpn"], why: "query_language" })).toBe(
      "Each place's matching pages are compared with the pages of its Japanese-language newspapers, since this search's words are Japanese.",
    );
  });

  it("names the languages chosen", () => {
    expect(baselineNote({ languages: ["ger"], why: "filter" })).toBe(
      "Each place's matching pages are compared with the pages of its newspapers in German, the language chosen.",
    );
    expect(baselineNote({ languages: ["ger", "spa"], why: "filter" })).toBe(
      "Each place's matching pages are compared with the pages of its newspapers in German and Spanish, the languages chosen.",
    );
  });

  it("says when every page counts", () => {
    expect(baselineNote({ languages: [], why: "all" })).toContain(
      "compared with all its newspapers' pages, in every language",
    );
  });

  it("says nothing without baselines or from an older API", () => {
    expect(baselineNote(null)).toBeNull();
    expect(baselineNote(undefined)).toBeNull();
  });
});
