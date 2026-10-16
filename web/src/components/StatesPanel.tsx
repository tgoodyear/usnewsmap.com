import { useState } from "react";
import {
  formatStateRate,
  stateBreakLabels,
  NO_MATCHES,
  NO_PAGES,
  STATE_COLORS,
  type StateTotal,
} from "../engine/states";

const css = (color: number[]) => `rgb(${color.slice(0, 3).join(",")})`;
export function StatesLegend({ breaks }: { breaks: number[] }) {
  const labels = stateBreakLabels(breaks);
  return (
    <section className="legend states-legend" aria-label="States legend">
      <strong>Matching pages per 1,000 pages published</strong>
      <ol>
        {breaks.map((b, i) => (
          <li key={i}>
            <span className="states-swatch" style={{ background: css(STATE_COLORS[i]!) }} />
            {b === 0
              ? "No positive rates"
              : `${i === 0 ? "> 0" : `> ${labels[i - 1]}`} – ${labels[i]}${i > 0 && b === breaks[i - 1] ? " (empty class)" : ""}`}
          </li>
        ))}
        <li>
          <span className="states-swatch" style={{ background: css(NO_MATCHES) }} />
          No matches
        </li>
        <li>
          <span className="states-swatch states-swatch--empty" style={{ backgroundColor: css(NO_PAGES) }} />
          No pages in this window
        </li>
      </ol>
      <p>
        Modern state borders. Quantiles follow the window; ties may leave empty classes. Small page counts give noisy
        rates.
      </p>
      <a
        href="https://www.census.gov/geographies/mapping-files/time-series/geo/carto-boundary-file.html"
        target="_blank"
        rel="noopener noreferrer"
      >
        Census 2024 boundaries
      </a>
    </section>
  );
}

export function statesCsv(rows: StateTotal[]): string {
  const cell = (value: string | number | null) => `"${String(value ?? "").replace(/"/g, '""')}"`;
  return (
    [
      ["state", "name", "matching_pages", "pages_published", "matches_per_1000"].join(","),
      ...rows.map((r) => [r.code, r.name, r.hits, r.pages, r.rate].map(cell).join(",")),
    ].join("\r\n") + "\r\n"
  );
}

export function StatesTable({
  rows,
  onFilter,
  baseline,
}: {
  rows: StateTotal[];
  onFilter: (code: string) => void;
  baseline?: string | null;
}) {
  const [sort, setSort] = useState<{ key: "name" | "hits" | "pages" | "rate"; desc: boolean }>({
    key: "name",
    desc: false,
  });
  const sorted = [...rows].sort((a, b) => {
    const av = a[sort.key],
      bv = b[sort.key];
    if (av === null) return bv === null ? a.name.localeCompare(b.name) : 1;
    if (bv === null) return -1;
    const c = typeof av === "string" ? av.localeCompare(String(bv)) : av - Number(bv);
    return (sort.desc ? -c : c) || a.name.localeCompare(b.name);
  });
  const columns = [
    ["name", "State"],
    ["hits", "Matching pages"],
    ["pages", "Pages published"],
    ["rate", "Per 1,000 pages"],
  ] as const;
  return (
    <section className="states-panel table-wrap" aria-label="States table">
      <h2>States in this window</h2>
      <p>
        Modern borders, including DC and territories. Filter buttons add to the state filter; remove states in the
        search filters.
      </p>
      {baseline && (
        <p>{baseline.replaceAll("Each place's", "Each state's").replaceAll("each place's", "each state's")}</p>
      )}
      <table className="places">
        <caption>Matching pages and all pages published in the current window</caption>
        <thead>
          <tr>
            {columns.map(([key, label]) => (
              <th
                scope="col"
                key={key}
                aria-sort={sort.key === key ? (sort.desc ? "descending" : "ascending") : "none"}
              >
                <button
                  type="button"
                  className="th-button"
                  onClick={() => setSort({ key, desc: sort.key === key ? !sort.desc : key !== "name" })}
                >
                  {label}
                </button>
              </th>
            ))}
            <th scope="col">Filter</th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((r) => (
            <tr key={r.code}>
              <th scope="row">{r.name}</th>
              <td className="num">{r.hits.toLocaleString("en-US")}</td>
              <td className="num">{r.pages.toLocaleString("en-US")}</td>
              <td className="num">{formatStateRate(r.rate)}</td>
              <td>
                {r.code && (
                  <button
                    type="button"
                    className="link-button"
                    aria-label={`Add state filter: ${r.name}`}
                    onClick={() => onFilter(r.code)}
                  >
                    Add state filter<span className="visually-hidden">: {r.name}</span>
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <button
        type="button"
        className="button"
        onClick={() => {
          const url = URL.createObjectURL(new Blob([statesCsv(rows)], { type: "text/csv;charset=utf-8" }));
          const a = document.createElement("a");
          a.href = url;
          a.download = "usnewsmap-states-window.csv";
          a.click();
          setTimeout(() => URL.revokeObjectURL(url), 1000);
        }}
      >
        Download states CSV
      </button>
    </section>
  );
}
