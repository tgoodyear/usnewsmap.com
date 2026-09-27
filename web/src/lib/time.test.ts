import { describe, expect, it } from "vitest";
import { bucketIndex, bucketLabel, bucketStart, dateFromDay, dayNumber } from "./time";

describe("day numbers", () => {
  it("match the server epoch (1700-01-01 = 0)", () => {
    expect(dayNumber("1700-01-01")).toBe(0);
    // From the fixture API: first_day 71779 is 1896-07-11.
    expect(dateFromDay(71779)).toBe("1896-07-11");
    expect(dayNumber("1896-07-11")).toBe(71779);
    for (const d of ["1756-02-29", "1900-03-01", "1963-12-31"]) {
      expect(dateFromDay(dayNumber(d))).toBe(d);
    }
  });
});

describe("buckets", () => {
  it("start where the server's BucketSpec does", () => {
    expect(bucketStart("year", "1895-06-15", 0)).toBe("1895-06-15");
    expect(bucketStart("year", "1895-06-15", 1)).toBe("1896-01-01");
    expect(bucketStart("month", "1895-01-01", 18)).toBe("1896-07-01");
    expect(bucketStart("month", "1895-11-20", 2)).toBe("1896-01-01");
    expect(bucketStart("week", "1896-06-01", 1)).toBe("1896-06-08");
    expect(bucketStart("day", "1896-12-31", 1)).toBe("1897-01-01");
  });

  it("index dates and clamp to the range", () => {
    expect(bucketIndex("month", "1895-01-01", 36, "1896-07-12")).toBe(18);
    expect(bucketIndex("week", "1896-06-01", 31, "1896-06-08")).toBe(1);
    expect(bucketIndex("year", "1895-01-01", 3, "1999-01-01")).toBe(2);
    expect(bucketIndex("day", "1896-01-01", 10, "1800-01-01")).toBe(0);
  });

  it("label", () => {
    expect(bucketLabel("month", "1896-07-01")).toBe("Jul 1896");
    expect(bucketLabel("week", "1896-07-12")).toBe("Week of Jul 12, 1896");
    expect(bucketLabel("year", "1896-01-01")).toBe("1896");
  });
});
