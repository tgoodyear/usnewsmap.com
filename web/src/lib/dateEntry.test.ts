import { describe, expect, it } from "vitest";
import {
  DAY_WITHOUT_MONTH,
  FORMAT_ERROR,
  NO_SUCH_DATE,
  daysInMonth,
  parseDateEntry,
  rangeError,
  showDate,
  type Edge,
} from "./dateEntry";

const ok = (text: string, edge: Edge, bounds?: { from: string; to: string }) => {
  const r = parseDateEntry(text, edge, bounds);
  if (!r.ok) throw new Error(`${text}: ${r.error}`);
  return r.iso;
};
const error = (text: string, edge: Edge = "from", bounds?: { from: string; to: string }) => {
  const r = parseDateEntry(text, edge, bounds);
  if (r.ok) throw new Error(`${text} parsed as ${r.iso}`);
  return r.error;
};

describe("parseDateEntry", () => {
  it("leaves an empty box empty", () => {
    expect(ok("", "from")).toBe("");
    expect(ok("   ", "to")).toBe("");
  });

  it("fills in a year to January 1 in From and December 31 in To", () => {
    expect(ok("1827", "from")).toBe("1827-01-01");
    expect(ok("1827", "to")).toBe("1827-12-31");
  });

  it("fills in a month to its first day in From and its last in To", () => {
    expect(ok("03/1827", "from")).toBe("1827-03-01");
    expect(ok("3/1827", "from")).toBe("1827-03-01");
    expect(ok("04/1827", "to")).toBe("1827-04-30");
    expect(ok("12/1827", "to")).toBe("1827-12-31");
    expect(ok("1/1827", "to")).toBe("1827-01-31");
  });

  it("gets February right in leap years", () => {
    expect(ok("02/1860", "to")).toBe("1860-02-29");
    expect(ok("2/1861", "to")).toBe("1861-02-28");
    // Century years are leap years only every 400 years.
    expect(ok("02/1900", "to")).toBe("1900-02-28");
    expect(ok("02/2000", "to")).toBe("2000-02-29");
    expect(ok("02/1860", "from")).toBe("1860-02-01");
    expect(daysInMonth(1896, 2)).toBe(29);
  });

  it("reads a whole date the same in either box", () => {
    expect(ok("07/04/1876", "from")).toBe("1876-07-04");
    expect(ok("7/4/1876", "to")).toBe("1876-07-04");
    expect(ok("02/29/1860", "to")).toBe("1860-02-29");
    // As in the URL.
    expect(ok("1876-07-04", "from")).toBe("1876-07-04");
  });

  it("takes the gaps a segmented or placeholder entry leaves", () => {
    expect(ok("__/__/1827", "from")).toBe("1827-01-01");
    expect(ok("mm/dd/1827", "to")).toBe("1827-12-31");
    expect(ok("03/__/1827", "to")).toBe("1827-03-31");
    expect(ok("03//1827", "from")).toBe("1827-03-01");
    expect(ok("/1827", "to")).toBe("1827-12-31");
    expect(ok("1827-03", "to")).toBe("1827-03-31");
  });

  it("ignores spaces and takes dots and dashes as separators", () => {
    expect(ok("  1827 ", "from")).toBe("1827-01-01");
    expect(ok(" 03 / 1827", "to")).toBe("1827-03-31");
    expect(ok("7.4.1876", "from")).toBe("1876-07-04");
    expect(ok("7-4-1876", "from")).toBe("1876-07-04");
    expect(ok("7 4 1876", "from")).toBe("1876-07-04");
  });

  it("rejects a day and a year without a month", () => {
    expect(error("15/1860")).toBe(DAY_WITHOUT_MONTH);
    expect(error("31/1860", "to")).toBe(DAY_WITHOUT_MONTH);
    expect(error("/15/1860")).toBe(DAY_WITHOUT_MONTH);
    expect(error("__/15/1860", "to")).toBe(DAY_WITHOUT_MONTH);
    expect(error("mm/15/1860")).toBe(DAY_WITHOUT_MONTH);
    expect(error("15//1860")).toBe(DAY_WITHOUT_MONTH);
    expect(error("1860--15")).toBe(DAY_WITHOUT_MONTH);
  });

  it("rejects days that don't exist", () => {
    expect(error("02/30/1860")).toBe(NO_SUCH_DATE);
    expect(error("02/29/1861", "to")).toBe(NO_SUCH_DATE);
    expect(error("04/31/1860")).toBe(NO_SUCH_DATE);
    expect(error("04/00/1860")).toBe(NO_SUCH_DATE);
    expect(error("1860-02-30")).toBe(NO_SUCH_DATE);
  });

  it("rejects anything else", () => {
    for (const text of [
      "gold",
      "1860s",
      "60",
      "860",
      "18600",
      "03/60",
      "13/12/1860",
      "00/1860",
      "32/1860",
      "1860-13",
      "123/1860",
      "1/2/3/1860",
      "07/04/1876 12:00",
      "July 1876",
      "1876/07/04/",
    ]) {
      expect(error(text), text).toBe(FORMAT_ERROR);
    }
  });

  const bounds = { from: "1736-03-05", to: "1963-10-31" };

  it("keeps whole dates inside the index's first and last days", () => {
    expect(ok("03/05/1736", "from", bounds)).toBe("1736-03-05");
    expect(ok("10/31/1963", "to", bounds)).toBe("1963-10-31");
    expect(error("03/04/1736", "from", bounds)).toBe(rangeError(bounds));
    expect(error("11/01/1963", "to", bounds)).toBe(rangeError(bounds));
    expect(rangeError(bounds)).toBe("Enter a date between 03/05/1736 and 10/31/1963.");
  });

  it("cuts a year or month that overlaps the index to its first or last day", () => {
    expect(ok("1736", "from", bounds)).toBe("1736-03-05");
    expect(ok("1736", "to", bounds)).toBe("1736-12-31");
    expect(ok("1963", "to", bounds)).toBe("1963-10-31");
    expect(ok("1963", "from", bounds)).toBe("1963-01-01");
    expect(ok("03/1736", "from", bounds)).toBe("1736-03-05");
    expect(ok("10/1963", "to", bounds)).toBe("1963-10-31");
  });

  it("rejects years and months wholly outside the index", () => {
    expect(error("1735", "from", bounds)).toBe(rangeError(bounds));
    expect(error("1735", "to", bounds)).toBe(rangeError(bounds));
    expect(error("1964", "from", bounds)).toBe(rangeError(bounds));
    expect(error("02/1736", "to", bounds)).toBe(rangeError(bounds));
    expect(error("11/1963", "from", bounds)).toBe(rangeError(bounds));
  });
});

describe("showDate", () => {
  it("shows an ISO date as mm/dd/yyyy", () => {
    expect(showDate("1827-01-01")).toBe("01/01/1827");
    expect(showDate("")).toBe("");
  });
});
