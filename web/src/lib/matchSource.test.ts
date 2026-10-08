import { describe, expect, it } from "vitest";
import { americanStoriesNote, americanStoriesOnlyNote } from "./matchSource";

describe("americanStoriesNote", () => {
  it("explains a page that matched only in American Stories' text, with its snippet", () => {
    expect(americanStoriesNote({ matched_in: ["american_stories"], snippet_source: "american_stories" })).toBe(
      "The Library of Congress text for this page doesn't contain the match. " +
        "It was found in American Stories (Dell et al. 2023), a second machine reading of Chronicling America's scans. " +
        "The snippet comes from that text. The page image at the Library of Congress shows what was printed.",
    );
  });

  it("says when the snippet is still from the Library of Congress text", () => {
    expect(americanStoriesNote({ matched_in: ["american_stories"] })).toMatch(
      /scans\. The snippet comes from the Library of Congress text, which matches only part of the search\. The page image/,
    );
  });

  it("says nothing for a page that matched in LoC's text, in both, or without the field", () => {
    expect(americanStoriesNote({ matched_in: ["loc"] })).toBeNull();
    expect(americanStoriesNote({ matched_in: ["loc", "american_stories"] })).toBeNull();
    expect(americanStoriesNote({})).toBeNull();
  });

  it("never uses an em dash", () => {
    for (const snippet_source of ["american_stories", undefined] as const) {
      expect(americanStoriesNote({ matched_in: ["american_stories"], snippet_source })).not.toMatch(/—/);
    }
  });
});

describe("americanStoriesOnlyNote", () => {
  it("counts the pages only American Stories' text matched, out of every matching page", () => {
    expect(americanStoriesOnlyNote(1234, 20000, 20000)).toBe(
      "1,234 of the 20,000 pages in this search match only in American Stories' text (Dell et al. 2023), not in the Library of Congress text.",
    );
    expect(americanStoriesOnlyNote(1, 3, 3)).toMatch(/^1 of the 3 pages in this search matches only/);
  });

  it("says nothing when no page or no count", () => {
    expect(americanStoriesOnlyNote(0, 20, 20)).toBeNull();
    expect(americanStoriesOnlyNote(undefined, 20, 20)).toBeNull();
  });

  it("says nothing while the count shown is only part of the search (playback, a trailing window)", () => {
    expect(americanStoriesOnlyNote(5, 20, 12)).toBeNull();
    expect(americanStoriesOnlyNote(5, 20, 0)).toBeNull();
  });
});
