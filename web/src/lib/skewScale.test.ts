import { describe, expect, it } from "vitest";
import { formatTimes, skewColor, skewPosition, tickLabel } from "./skewScale";
import { formatRange, skewInfo, skewSentence, type SkewInfo } from "./skewText";
import { languageCounts, languageLabel, languageName } from "./languages";

describe("relative-rate scale", () => {
  it("is centred on 1× on a log scale and stops at 1/8× and 8×", () => {
    expect(skewPosition(1)).toBe(0.5);
    expect(skewPosition(2)).toBeCloseTo(4 / 6, 12);
    expect(skewPosition(0.5)).toBeCloseTo(2 / 6, 12);
    expect(skewPosition(8)).toBe(1);
    expect(skewPosition(100)).toBe(1);
    expect(skewPosition(1 / 100)).toBe(0);
    expect(skewPosition(0)).toBe(0);
    expect(skewColor(1)).toEqual([181, 178, 170, 230]);
    expect(skewColor(64)).toEqual(skewColor(8));
    // Below is blue, above is red.
    const [r1, , b1] = skewColor(1 / 4);
    const [r2, , b2] = skewColor(4);
    expect(b1).toBeGreaterThan(r1);
    expect(r2).toBeGreaterThan(b2);
  });

  it("labels rates plainly", () => {
    expect(formatTimes(6.14)).toBe("6.1×");
    expect(formatTimes(22.5)).toBe("23×");
    expect(formatTimes(0.26)).toBe("0.26×");
    expect(formatTimes(0.044)).toBe("0.044×");
    expect(formatTimes(0.004)).toBe("<0.01×");
    expect(tickLabel(1 / 8)).toBe("1/8×");
    expect(tickLabel(4)).toBe("4×");
  });
});

describe("relative-rate sentences", () => {
  const mobile: SkewInfo = {
    estimate: 6.14,
    lower: 5.23,
    upper: 7.15,
    observed: 336,
    expected: 47.3,
    pages: 1351,
    dir: 1,
    languages: null,
    languageCounts: null,
  };

  it("says what matched, what was expected and the range", () => {
    expect(formatRange(mobile)).toBe("5.2 to 7.2×");
    expect(skewSentence("Mobile, AL", mobile)).toBe(
      "Mobile, AL: 336 pages matched where 47 were expected from its 1,351 pages. About 6.1× the rate of the other places (5.2 to 7.2×).",
    );
  });

  it("says when it can't tell, and names papers in other languages", () => {
    const village = { ...mobile, observed: 1, expected: 0.04, pages: 1, estimate: 0.84, lower: 0.23, upper: 2.09, dir: 0 as const };
    expect(skewSentence("Little Rock Ark., AR", village)).toBe(
      "Little Rock Ark., AR: 1 page matched where less than 0.1 were expected from its 1 page. Not enough pages to say whether it differs from the other places (0.23 to 2.1×).",
    );
    expect(skewSentence("Pittsburg, PA", { ...mobile, languages: "Papers in Serbian and English" })).toMatch(
      /\(5\.2 to 7\.2×\)\. Papers in Serbian and English\.$/,
    );
    expect(skewSentence("X", mobile)).not.toMatch(/[—]/);
  });
});

describe("title languages", () => {
  it("names other languages first and English last, and nothing for English only", () => {
    expect(languageLabel(["ger"])).toBe("Papers in German");
    expect(languageLabel(["eng", "srp"])).toBe("Papers in Serbian and English");
    expect(languageLabel(["eng", "ger", "srp"])).toBe("Papers in German, Serbian and English");
    expect(languageLabel(["eng"])).toBeNull();
    expect(languageLabel([])).toBeNull();
    expect(languageLabel(undefined)).toBeNull();
    expect(languageLabel(["yoruba"])).toBe("Papers in Yoruba");
    expect(languageLabel(["pennsylvania german", "eng"])).toBe("Papers in Pennsylvania German and English");
    expect(languageName("old church slavonic")).toBe("Old Church Slavonic");
  });

  it("counts a place's papers in each language, most first, with each one's share", () => {
    expect(languageCounts({ eng: 1, fre: 3, ger: 1 }, 5)).toBe(
      "Of its 5 papers, 3 are in French (60%), 1 in English (20%) and 1 in German (20%).",
    );
    expect(languageCounts({ ger: 1 }, 3)).toBe("Of its 3 papers, 1 is in German (33%).");
    // A paper in two languages counts in both.
    expect(languageCounts({ eng: 2, ger: 1 }, 2)).toBe(
      "Of its 2 papers, 2 are in English (100%) and 1 in German (50%). A paper in more than one language counts in each.",
    );
    expect(languageCounts({ eng: 1, ger: 1 }, 1)).toBe("Its one paper is in English and German.");
    expect(languageCounts({ srp: 1, eng: 400 }, 401)).toBe(
      "Of its 401 papers, 400 are in English (100%) and 1 in Serbian (under 1%).",
    );
    expect(languageCounts(undefined, 5)).toBeNull();
    expect(languageCounts({}, 5)).toBeNull();
  });
});

describe("evidence", () => {
  it("never calls a place with no pages or nothing expected clearly different", () => {
    // A prior whose range alone excludes 1.
    const s = { observed: 0, expected: 0, lift: null, estimate: 1.2, lower: 1.06, upper: 1.38, above: 0.99 };
    expect(skewInfo(s, 500, null).dir).toBe(0);
    expect(skewInfo({ ...s, expected: 3, observed: 4 }, 0, null).dir).toBe(0);
    expect(skewInfo({ ...s, expected: 3, observed: 4 }, 500, null).dir).toBe(1);
  });
});
