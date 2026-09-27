import { useState } from "react";
import { formatRel } from "../lib/scale";
import { dateFromDay, formatDate } from "../lib/time";
import type { MapPoint } from "./mapTypes";

interface Row extends MapPoint {
  firstDay: number;
}

type Key = "name" | "state" | "value" | "rel" | "firstDay";

interface Props {
  rows: Row[];
  onSelect: (id: string) => void;
  selected: string;
}

const COLUMNS: { key: Key; label: string; numeric?: boolean }[] = [
  { key: "name", label: "Place" },
  { key: "state", label: "State" },
  { key: "value", label: "Pages", numeric: true },
  { key: "rel", label: "Share of pages published", numeric: true },
  { key: "firstDay", label: "First appearance" },
];

/** Everything on the map as a sortable table (F-27, WCAG). */
export function PlaceTable({ rows, onSelect, selected }: Props) {
  const [sort, setSort] = useState<{ key: Key; desc: boolean }>({ key: "value", desc: true });
  const sorted = [...rows].sort((a, b) => {
    const x = a[sort.key];
    const y = b[sort.key];
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
        <caption className="visually-hidden">Places with matching pages up to the current date</caption>
        <thead>
          <tr>
            {COLUMNS.map((c) => (
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
              <td>{r.state}</td>
              <td className="num">{r.value.toLocaleString()}</td>
              <td className="num">{formatRel(r.rel)}</td>
              <td>{formatDate(dateFromDay(r.firstDay))}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
