import { formatTimes } from "../lib/skewScale";
import { formatExpected, formatRange, type SkewInfo } from "../lib/skewText";
import { dateFromDay } from "../lib/time";

export interface SkewRow {
  id: string;
  name: string;
  state: string;
  skew: SkewInfo;
  /** The place's first and last matching day in the whole search; negative when unknown. */
  firstDay?: number;
  lastDay?: number;
}

const LIST_LENGTH = 5;

/**
 * The places that differ most clearly (doc 11, 11.6): above 1 sorted by the
 * interval's lower bound, below 1 by its upper bound. Only places whose
 * interval excludes 1. Papers in other languages are named, not left out.
 */
export function clearest(rows: SkewRow[]): { above: SkewRow[]; below: SkewRow[] } {
  const usable = rows.filter((r) => r.skew.dir !== 0);
  return {
    above: usable
      .filter((r) => r.skew.dir === 1)
      .sort((a, b) => b.skew.lower - a.skew.lower)
      .slice(0, LIST_LENGTH),
    below: usable
      .filter((r) => r.skew.dir === -1)
      .sort((a, b) => a.skew.upper - b.skew.upper)
      .slice(0, LIST_LENGTH),
  };
}

interface ListsProps {
  rows: SkewRow[];
  onSelect: (id: string) => void;
}

/** The side panel when no place is selected. */
export function SkewLists({ rows, onSelect }: ListsProps) {
  const { above, below } = clearest(rows);
  const list = (items: SkewRow[], heading: string, empty: string) => (
    <section className="skew-list">
      <h3>{heading}</h3>
      {items.length === 0 ? (
        <p className="skew-list__empty">{empty}</p>
      ) : (
        <ol>
          {items.map((r) => (
            <li key={r.id}>
              <button type="button" className="link-button" onClick={() => onSelect(r.id)}>
                {r.name}, {r.state}
              </button>{" "}
              <span className="skew-list__value">
                {formatTimes(r.skew.estimate)} ({formatRange(r.skew)})
              </span>
              {r.skew.languages && <span className="skew-list__lang">{r.skew.languages}</span>}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
  return (
    <aside className="panel skew-lists" aria-label="Places that differ most clearly">
      <h2>Clearest differences</h2>
      {list(above, "Most clearly above 1×", "No place is clearly above 1× in this window.")}
      {list(below, "Most clearly below 1×", "No place is clearly below 1× in this window.")}
      <p className="skew-list__note">Ranked by the end of each place's 90% range nearest 1×.</p>
    </aside>
  );
}

interface StateTableProps {
  rows: { state: string; skew: SkewInfo }[];
}

/** The same score per state, each compared with the other states. */
export function StateTable({ rows }: StateTableProps) {
  const sorted = [...rows].filter((r) => r.skew.pages > 0).sort((a, b) => b.skew.estimate - a.skew.estimate);
  return (
    <div className="table-wrap">
      <table className="places">
        <caption>By state, each compared with the other states</caption>
        <thead>
          <tr>
            <th scope="col">State</th>
            <th scope="col" className="num">Pages published</th>
            <th scope="col" className="num">Matched</th>
            <th scope="col" className="num">Expected</th>
            <th scope="col" className="num">Relative rate</th>
            <th scope="col" className="num">90% range</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((r) => (
            <tr key={r.state}>
              <th scope="row">{r.state}</th>
              <td className="num">{r.skew.pages.toLocaleString()}</td>
              <td className="num">{r.skew.observed.toLocaleString()}</td>
              <td className="num">{formatExpected(r.skew.expected)}</td>
              <td className="num">{formatTimes(r.skew.estimate)}</td>
              <td className="num">
                {formatRange(r.skew)}
                {r.skew.dir === 0 ? " (not enough pages to tell)" : ""}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** Per-place CSV for the window (doc 11, 11.6). */
export function skewCsv(rows: SkewRow[]): string {
  const q = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const n = (x: number) => (Number.isFinite(x) ? String(Number(x.toPrecision(6))) : "");
  const day = (d: number | undefined) => (d !== undefined && d >= 0 ? dateFromDay(d) : "");
  const lines = ["place_id,name,state,pages,hits,expected,estimate,lower,upper,languages,first_seen,last_seen"];
  for (const r of rows) {
    const s = r.skew;
    lines.push(
      [
        q(r.id),
        q(r.name),
        q(r.state),
        s.pages,
        s.observed,
        n(s.expected),
        n(s.estimate),
        n(s.lower),
        n(s.upper),
        q(s.languages ?? ""),
        day(r.firstDay),
        day(r.lastDay),
      ].join(","),
    );
  }
  return `${lines.join("\n")}\n`;
}

export function DownloadCsv({ rows, filename }: { rows: SkewRow[]; filename: string }) {
  return (
    <button
      type="button"
      className="button"
      onClick={() => {
        const url = URL.createObjectURL(new Blob([skewCsv(rows)], { type: "text/csv" }));
        const a = document.createElement("a");
        a.href = url;
        a.download = filename;
        a.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
      }}
    >
      Download CSV
    </button>
  );
}
