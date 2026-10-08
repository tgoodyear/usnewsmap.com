// What the search form's From and To boxes accept: a whole date, a month and
// a year, or just a year. A partial date fills in to the start of the period
// in From and to its end in To, so "1860" is 1860-01-01 to 1860-12-31 and
// "2/1860" in To is 1860-02-29.

/** Which end of the range a box sets. */
export type Edge = "from" | "to";

/** The corpus's first and last days (ISO), when /v1/meta has loaded. */
export interface DateBounds {
  from: string;
  to: string;
}

/** An ISO date ("" for an empty box), or what's wrong with the entry. */
export type DateEntry = { ok: true; iso: string } | { ok: false; error: string };

export const FORMAT_ERROR = "Enter a date as mm/dd/yyyy, mm/yyyy or yyyy.";
export const DAY_WITHOUT_MONTH = "Add the month, or enter just the year.";
export const NO_SUCH_DATE = "That day doesn't exist. Check the month and day.";

/** The range message, e.g. "Enter a date between 01/01/1736 and 12/31/1963." */
export function rangeError(bounds: DateBounds): string {
  return `Enter a date between ${showDate(bounds.from)} and ${showDate(bounds.to)}.`;
}

const pad = (n: number) => String(n).padStart(2, "0");

function isLeap(y: number): boolean {
  return (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
}

/** Days in a month (1-12) of a year, leap years included. */
export function daysInMonth(y: number, m: number): number {
  return m === 2 ? (isLeap(y) ? 29 : 28) : [4, 6, 9, 11].includes(m) ? 30 : 31;
}

const iso = (y: number, m: number, d: number) => `${String(y).padStart(4, "0")}-${pad(m)}-${pad(d)}`;

/** An ISO date as the box shows it: "1827-01-01" is "01/01/1827". "" stays "". */
export function showDate(isoDate: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(isoDate);
  return m ? `${m[2]}/${m[3]}/${m[1]}` : "";
}

/**
 * A part the visitor left out: nothing, underscores, or the placeholder's
 * own letters ("mm/dd/1860").
 */
function blank(part: string): boolean {
  return /^(_*|mm?|dd?|yyyy)$/i.test(part);
}

type Parts = { y: number; m: number | null; d: number | null } | { error: string };

/** Split an entry into year, month and day, before any filling in. */
function parts(text: string): Parts {
  // Slashes as typed, dashes from ISO dates, dots and spaces as some people
  // write them. One separator per gap, so "03//1860" keeps its empty day.
  const raw = text.split(/\s*[/.-]\s*|\s+/).map((p) => (blank(p) ? "" : p));
  if (raw.length > 3 || raw.some((p) => !/^\d*$/.test(p))) return { error: FORMAT_ERROR };
  const num = (p: string) => (p === "" ? null : Number(p));
  const short = (p: string) => p.length <= 2;

  // yyyy, yyyy-mm or yyyy-mm-dd, as in the URL.
  if (raw[0]!.length === 4) {
    const [y, m = "", d = ""] = raw as [string, string?, string?];
    if (!short(m) || !short(d)) return { error: FORMAT_ERROR };
    if (m === "" && d !== "") return { error: DAY_WITHOUT_MONTH };
    const month = num(m);
    if (month !== null && (month < 1 || month > 12)) return { error: FORMAT_ERROR };
    return { y: Number(y), m: month, d: num(d) };
  }
  // mm/yyyy or mm/dd/yyyy; either of mm and dd may be left empty.
  if (raw.length === 1 || raw[raw.length - 1]!.length !== 4) return { error: FORMAT_ERROR };
  const [m, d = ""] = raw.length === 2 ? [raw[0]!] : [raw[0]!, raw[1]!];
  if (!short(m) || !short(d)) return { error: FORMAT_ERROR };
  const y = Number(raw[raw.length - 1]);
  if (m === "" && d !== "") return { error: DAY_WITHOUT_MONTH };
  const month = num(m);
  if (month !== null && (month < 1 || month > 12)) {
    // 13 to 31 with no day after it can only be a day: "15/1860" has no month.
    return { error: d === "" && month > 12 && month <= 31 ? DAY_WITHOUT_MONTH : FORMAT_ERROR };
  }
  return { y, m: month, d: num(d) };
}

/**
 * Read a From or To box. Empty is fine (no limit). A year or a month fills
 * in to the start of the period for From and its end for To. A whole date
 * outside the corpus is an error, as the date picker's limits made it
 * before; a year or month that overlaps the corpus is cut to its first or
 * last day, and one wholly outside it is an error.
 */
export function parseDateEntry(text: string, edge: Edge, bounds?: DateBounds): DateEntry {
  const t = text.trim();
  if (t === "") return { ok: true, iso: "" };
  const p = parts(t);
  if ("error" in p) return { ok: false, error: p.error };
  const { y, m, d } = p;
  if (m !== null && d !== null && (d < 1 || d > daysInMonth(y, m))) return { ok: false, error: NO_SUCH_DATE };

  // The period the entry names: a day, a month or a year.
  const start = iso(y, m ?? 1, d ?? 1);
  const end = iso(y, m ?? 12, d ?? daysInMonth(y, m ?? 12));
  if (!bounds) return { ok: true, iso: edge === "from" ? start : end };
  if (end < bounds.from || start > bounds.to) return { ok: false, error: rangeError(bounds) };
  // ISO dates compare as strings.
  return { ok: true, iso: edge === "from" ? (start < bounds.from ? bounds.from : start) : end > bounds.to ? bounds.to : end };
}
