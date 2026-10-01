import { useState } from "react";
import { formatRel } from "../lib/scale";
import { formatTimes } from "../lib/skewScale";
import { formatExpected, formatRange } from "../lib/skewText";
import { dateFromDay, formatDate } from "../lib/time";
import type { MapPoint } from "./mapTypes";

interface Row extends MapPoint {
  firstDay: number;
}

interface Column {
  key: string;
  label: string;
  numeric?: boolean;
  /** Sort value; NaN sorts last. */
  get: (r: Row) => number | string;
  show: (r: Row) => string;
}

interface Props {
  rows: Row[];
  onSelect: (id: string) => void;
  selected: string;
  /** Columns for the relative-rate view. */
  skew?: boolean;
}

const NAME: Column = { key: "name", label: "Place", get: (r) => r.name, show: (r) => r.name };
const STATE: Column = { key: "state", label: "State", get: (r) => r.state, show: (r) => r.state };

const RAW: Column[] = [
  NAME,
  STATE,
  { key: "value", label: "Pages", numeric: true, get: (r) => r.value, show: (r) => r.value.toLocaleString() },
  { key: "rel", label: "Share of pages published", numeric: true, get: (r) => r.rel, show: (r) => formatRel(r.rel) },
  {
    key: "firstDay",
    label: "First appearance",
    get: (r) => r.firstDay,
    show: (r) => (r.firstDay >= 0 ? formatDate(dateFromDay(r.firstDay)) : ""),
  },
];

const nan = Number.NaN;
const SKEW: Column[] = [
  NAME,
  STATE,
  {
    key: "published",
    label: "Pages published",
    numeric: true,
    get: (r) => r.skew?.pages ?? nan,
    show: (r) => (r.skew ? r.skew.pages.toLocaleString() : ""),
  },
  {
    key: "observed",
    label: "Matched",
    numeric: true,
    get: (r) => r.skew?.observed ?? nan,
    show: (r) => (r.skew ? r.skew.observed.toLocaleString() : ""),
  },
  {
    key: "expected",
    label: "Expected",
    numeric: true,
    get: (r) => r.skew?.expected ?? nan,
    show: (r) => (r.skew ? formatExpected(r.skew.expected) : ""),
  },
  {
    key: "estimate",
    label: "Relative rate",
    numeric: true,
    get: (r) => r.skew?.estimate ?? nan,
    show: (r) => (r.skew ? formatTimes(r.skew.estimate) : ""),
  },
  {
    key: "range",
    label: "90% range",
    numeric: true,
    get: (r) => r.skew?.lower ?? nan,
    show: (r) =>
      r.skew
        ? `${formatRange(r.skew)}${r.skew.dir === 0 ? " (can't tell)" : ""}`
        : "",
  },
  {
    key: "languages",
    label: "Languages",
    get: (r) => r.skew?.languages ?? "",
    show: (r) => r.skew?.languages ?? "",
  },
];

/** Everything on the map as a sortable table (F-27, WCAG). */
export function PlaceTable({ rows, onSelect, selected, skew = false }: Props) {
  const columns = skew ? SKEW : RAW;
  const [sort, setSort] = useState<{ key: string; desc: boolean }>({ key: skew ? "estimate" : "value", desc: true });
  const col = columns.find((c) => c.key === sort.key) ?? columns[2]!;
  const sorted = [...rows].sort((a, b) => {
    const x = col.get(a);
    const y = col.get(b);
    // Unknown values (NaN) sort last in both directions.
    if (typeof x === "number" && typeof y === "number" && (Number.isNaN(x) || Number.isNaN(y))) {
      return Number.isNaN(x) ? (Number.isNaN(y) ? 0 : 1) : -1;
    }
    const c = typeof x === "number" && typeof y === "number" ? x - y : String(x).localeCompare(String(y));
    return sort.desc ? -c : c;
  });
  return (
    <div className="table-wrap">
      <table className="places">
        <caption className="visually-hidden">
          {skew
            ? "Relative rate of each place with pages up to the current date"
            : "Places with matching pages up to the current date"}
        </caption>
        <thead>
          <tr>
            {columns.map((c) => (
              <th
                key={c.key}
                scope="col"
                aria-sort={sort.key === c.key ? (sort.desc ? "descending" : "ascending") : "none"}
                className={c.numeric ? "num" : undefined}
              >
                <button
                  type="button"
                  className="th-button"
                  onClick={() =>
                    setSort((s) => ({ key: c.key, desc: s.key === c.key ? !s.desc : c.numeric === true }))
                  }
                >
                  {c.label}
                </button>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {sorted.map((r) => (
            <tr key={r.id} aria-selected={r.id === selected}>
              <th scope="row">
                <button type="button" className="link-button" onClick={() => onSelect(r.id)}>
                  {r.name}
                </button>
              </th>
              {columns.slice(1).map((c) => (
                <td key={c.key} className={c.numeric ? "num" : undefined}>
                  {c.show(r)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
