import { describe, expect, it } from "vitest";
import { formatTimes, skewColor, skewPosition, tickLabel } from "./skewScale";
import { formatRange, skewSentence, type SkewInfo } from "./skewText";

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
    nonEnglish: false,
  };

  it("says what matched, what was expected and the range", () => {
    expect(formatRange(mobile)).toBe("5.2 to 7.2×");
    expect(skewSentence("Mobile, AL", mobile)).toBe(
      "Mobile, AL: 336 pages matched where 47 were expected from its 1,351 pages. About 6.1× the rate of the other places (5.2 to 7.2×).",
    );
  });

  it("says when it can't tell, and when the papers aren't in English", () => {
    const village = { ...mobile, observed: 1, expected: 0.04, pages: 1, estimate: 0.84, lower: 0.23, upper: 2.09, dir: 0 as const };
    expect(skewSentence("Little Rock Ark., AR", village)).toBe(
      "Little Rock Ark., AR: 1 page matched where less than 0.1 were expected from its 1 page. Can't tell whether it differs from the other places (0.23 to 2.1×).",
    );
    expect(skewSentence("San Diego, CA", { ...mobile, nonEnglish: true })).toContain("not in English");
    expect(skewSentence("X", mobile)).not.toMatch(/[—]/);
  });
});
