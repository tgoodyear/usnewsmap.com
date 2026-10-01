import { useState, type ReactNode } from "react";
import type { ByLanguage, LanguagePages, StatePages } from "../api/types";
import { count, share } from "./format";

/** A column of a sortable table: what it shows and what it sorts by. */
export interface Column<R> {
  key: string;
  label: string;
  numeric?: boolean;
  value: (r: R) => number | string;
  cell?: (r: R) => ReactNode;
}

interface Sort {
  key: string;
  desc: boolean;
}

/**
 * A table whose column headers sort it (the search page's place table does
 * the same). The first column names each row (`<th scope="row">`); numbers
 * are right-aligned. A footer row, if given, stays at the bottom.
 */
export function SortableTable<R>({
  caption,
  columns,
  rows,
  rowKey,
  initial,
  foot,
}: {
  caption: string;
  columns: Column<R>[];
  rows: R[];
  rowKey: (r: R) => string;
  initial: Sort;
  foot?: ReactNode[];
}) {
  const [sort, setSort] = useState<Sort>(initial);
  const col = columns.find((c) => c.key === sort.key) ?? columns[0]!;
  const name = columns[0]!;
  const sorted = [...rows].sort((a, b) => {
    const x = col.value(a);
    const y = col.value(b);
    const c =
      typeof x === "number" && typeof y === "number"
        ? x - y
        : String(x).localeCompare(String(y));
    // Ties keep a stable, alphabetical order whichever way the column sorts.
    return (
      (sort.desc ? -c : c) ||
      String(name.value(a)).localeCompare(String(name.value(b)))
    );
  });
  return (
    // Focusable so keyboard users can scroll a wide table on a narrow screen.
    <div
      className="table-scroll"
      tabIndex={0}
      role="region"
      aria-label={caption}
    >
      <table className="places status-table pages-table">
        <caption>{caption}</caption>
        <thead>
          <tr>
            {columns.map((c) => (
              <th
                key={c.key}
                scope="col"
                className={c.numeric ? "num" : undefined}
                aria-sort={
                  sort.key === c.key
                    ? sort.desc
                      ? "descending"
                      : "ascending"
                    : "none"
                }
              >
                <button
                  type="button"
                  className="th-button"
                  onClick={() =>
                    setSort((s) => ({
                      key: c.key,
                      desc: s.key === c.key ? !s.desc : c.numeric === true,
                    }))
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
            <tr key={rowKey(r)}>
              {columns.map((c, i) => {
                const content = c.cell ? c.cell(r) : c.value(r);
                return i === 0 ? (
                  <th key={c.key} scope="row">
                    {content}
                  </th>
                ) : (
                  <td key={c.key} className={c.numeric ? "num" : undefined}>
                    {content}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
        {foot && (
          <tfoot>
            <tr>
              {foot.map((f, i) =>
                i === 0 ? (
                  <th key={i} scope="row">
                    {f}
                  </th>
                ) : (
                  <td
                    key={i}
                    className={columns[i]?.numeric ? "num" : undefined}
                  >
                    {f}
                  </td>
                ),
              )}
            </tr>
          </tfoot>
        )}
      </table>
    </div>
  );
}

const STATE_COLUMNS: Column<StatePages>[] = [
  { key: "name", label: "State", value: (r) => r.name },
  {
    key: "places",
    label: "Places",
    numeric: true,
    value: (r) => r.places,
    cell: (r) => count(r.places),
  },
  {
    key: "titles",
    label: "Newspapers",
    numeric: true,
    value: (r) => r.titles,
    cell: (r) => count(r.titles),
  },
  {
    key: "pages",
    label: "Pages",
    numeric: true,
    value: (r) => r.pages,
    cell: (r) => count(r.pages),
  },
  {
    key: "percent",
    label: "Share of pages",
    numeric: true,
    value: (r) => r.pages,
    cell: (r) => share(r.percent, r.pages),
  },
];

export function StatesSection({
  rows,
  pages,
}: {
  rows: StatePages[];
  pages: number;
}) {
  const sum = (f: (r: StatePages) => number) =>
    rows.reduce((a, r) => a + f(r), 0);
  return (
    <section aria-labelledby="by-state" className="status-section">
      <h2 id="by-state">Pages by state</h2>
      {rows.length === 0 ? (
        <p className="status-empty">No pages are published yet.</p>
      ) : (
        <>
          <p>
            {rows.length === 1
              ? "1 state or territory has pages in the published version."
              : `${count(rows.length)} states and territories have pages in the published version.`}{" "}
            Each newspaper counts toward the state of the place it was published
            in.
          </p>
          <SortableTable
            caption="Published pages by state"
            columns={STATE_COLUMNS}
            rows={rows}
            rowKey={(r) => r.state}
            initial={{ key: "pages", desc: true }}
            foot={[
              "Total",
              count(sum((r) => r.places)),
              count(sum((r) => r.titles)),
              count(pages),
              "100.0%",
            ]}
          />
        </>
      )}
    </section>
  );
}

function languageColumns(pagesKnown: boolean): Column<LanguagePages>[] {
  const cols: Column<LanguagePages>[] = [
    { key: "name", label: "Language", value: (r) => r.name },
    {
      key: "titles",
      label: "Newspapers",
      numeric: true,
      value: (r) => r.titles,
      cell: (r) => count(r.titles),
    },
  ];
  if (pagesKnown) {
    cols.push(
      {
        key: "pages",
        label: "Pages",
        numeric: true,
        value: (r) => r.pages ?? 0,
        cell: (r) => count(r.pages ?? 0),
      },
      {
        key: "percent",
        label: "Share of pages",
        numeric: true,
        value: (r) => r.pages ?? 0,
        cell: (r) => share(r.percent ?? 0, r.pages ?? 0),
      },
    );
  }
  return cols;
}

export function LanguagesSection({ data }: { data: ByLanguage }) {
  const known = data.pages_known;
  return (
    <section aria-labelledby="by-language" className="status-section">
      <h2 id="by-language">Pages by language</h2>
      {data.rows.length === 0 ? (
        <p className="status-empty">No pages are published yet.</p>
      ) : (
        <>
          <p>
            Languages are the ones LoC lists for each newspaper.{" "}
            {data.multilingual_titles > 0
              ? `${count(data.multilingual_titles)} ${data.multilingual_titles === 1 ? "newspaper lists" : "newspapers list"} more than one language${
                  known && data.multilingual_pages !== null
                    ? ` (${count(data.multilingual_pages)} pages)`
                    : ""
                }. Those count once in each of their languages, so the rows add up to more than the total.`
              : "Every newspaper lists one language or none."}
          </p>
          {!known && (
            <p className="notice" role="note">
              The published version doesn&apos;t record pages per newspaper, so
              this table has newspaper counts only. Page counts appear after the
              next release.
            </p>
          )}
          <SortableTable
            caption={
              known
                ? "Published pages by language"
                : "Published newspapers by language"
            }
            columns={languageColumns(known)}
            rows={data.rows}
            rowKey={(r) => r.code ?? ""}
            initial={{ key: known ? "pages" : "titles", desc: true }}
          />
        </>
      )}
    </section>
  );
}
