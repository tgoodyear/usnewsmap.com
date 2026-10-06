import { useId, useState, type ReactNode } from "react";

const LIST_LENGTH = 5;
/** The Median date lists leave out places with fewer matching pages than this: one page has a date, not a median. */
export const MIN_MEDIAN_PAGES = 5;
const STORAGE_KEY = "usnm.lists";

/** A place on the map in the current window, as the lists need it. */
export interface ListRow {
  id: string;
  name: string;
  state: string;
  /** Matching pages up to the current date. */
  value: number;
  /** The median date's position, 0..1 across the search; NaN when none. */
  when?: number;
  whenLabel?: string;
}

export interface StateRow {
  state: string;
  pages: number;
  places: number;
}

/** The places with the most matching pages, most first; ties by name. */
export function mostPages<R extends ListRow>(rows: R[]): R[] {
  return rows
    .filter((r) => r.value > 0)
    .sort((a, b) => b.value - a.value || a.name.localeCompare(b.name))
    .slice(0, LIST_LENGTH);
}

/** The states with the most matching pages: each place counts toward its own state. */
export function statePages(rows: ListRow[]): StateRow[] {
  const by = new Map<string, StateRow>();
  for (const r of rows) {
    if (r.value <= 0 || !r.state) continue;
    const s = by.get(r.state) ?? { state: r.state, pages: 0, places: 0 };
    s.pages += r.value;
    s.places += 1;
    by.set(r.state, s);
  }
  return [...by.values()].sort((a, b) => b.pages - a.pages || a.state.localeCompare(b.state)).slice(0, LIST_LENGTH);
}

/**
 * The places whose matching pages fall earliest and latest, by median date,
 * among those with at least MIN_MEDIAN_PAGES pages. Ties go to the place
 * with more pages.
 */
export function medianExtremes<R extends ListRow>(rows: R[]): { earliest: R[]; latest: R[]; eligible: number } {
  const usable = rows.filter((r) => r.value >= MIN_MEDIAN_PAGES && r.when !== undefined && !Number.isNaN(r.when));
  const by = (dir: 1 | -1) => (a: R, b: R) => dir * (a.when! - b.when!) || b.value - a.value || a.name.localeCompare(b.name);
  // With fewer than two lists' worth, split them so no place is in both.
  const n = Math.min(LIST_LENGTH, Math.ceil(usable.length / 2));
  return {
    earliest: [...usable].sort(by(1)).slice(0, n),
    latest: [...usable].sort(by(-1)).slice(0, Math.min(LIST_LENGTH, usable.length - n)),
    eligible: usable.length,
  };
}

function readOpen(): boolean {
  try {
    return window.localStorage.getItem(STORAGE_KEY) !== "hidden";
  } catch {
    return true;
  }
}

/** Whether the place lists are shown; remembered across visits on this browser. */
export function useListsOpen(): [boolean, (open: boolean) => void] {
  const [open, setOpen] = useState(readOpen);
  return [
    open,
    (next: boolean) => {
      setOpen(next);
      try {
        if (next) window.localStorage.removeItem(STORAGE_KEY);
        else window.localStorage.setItem(STORAGE_KEY, "hidden");
      } catch {
        // Private browsing without storage: it still toggles for this visit.
      }
    },
  ];
}

/**
 * The side panel when no place is selected, in every measure. It can be
 * hidden to give the map the room; the choice is kept for the next visit.
 */
export function ListsPanel({
  title,
  label,
  className,
  children,
}: {
  title: string;
  /** The panel's accessible name. */
  label: string;
  className?: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useListsOpen();
  const body = useId();
  if (!open) {
    return (
      <aside className="panel panel--collapsed" aria-label={label}>
        <button
          type="button"
          className="button button--quiet panel__toggle"
          aria-expanded={false}
          onClick={() => setOpen(true)}
        >
          Show {title.toLowerCase()}
        </button>
      </aside>
    );
  }
  return (
    <aside className={`panel lists ${className ?? ""}`.trim()} aria-label={label}>
      <header className="panel__head">
        <h2>{title}</h2>
        <button
          type="button"
          className="button button--quiet panel__toggle"
          aria-expanded={true}
          aria-controls={body}
          onClick={() => setOpen(false)}
        >
          Hide
        </button>
      </header>
      <div id={body}>{children}</div>
    </aside>
  );
}

function List<R>({
  heading,
  items,
  empty,
  render,
}: {
  heading: string;
  items: R[];
  empty: string;
  render: (r: R) => ReactNode;
}) {
  return (
    <section className="skew-list">
      <h3>{heading}</h3>
      {items.length === 0 ? <p className="skew-list__empty">{empty}</p> : <ol>{items.map(render)}</ol>}
    </section>
  );
}

const pages = (n: number) => `${n.toLocaleString("en-US")} ${n === 1 ? "page" : "pages"}`;

function share(part: number, total: number): string {
  if (total <= 0) return "";
  const p = (100 * part) / total;
  return p < 0.1 ? "under 0.1%" : `${p.toFixed(1)}%`;
}

function PlaceButton({ row, onSelect }: { row: ListRow; onSelect: (id: string) => void }) {
  return (
    <button type="button" className="link-button" onClick={() => onSelect(row.id)}>
      {row.name}, {row.state}
    </button>
  );
}

/** Pages: where the matching pages are, by place and by state. */
export function PagesLists({ rows, onSelect }: { rows: ListRow[]; onSelect: (id: string) => void }) {
  const total = rows.reduce((a, r) => a + Math.max(r.value, 0), 0);
  return (
    <ListsPanel title="Most pages" label="Places and states with the most pages">
      <List
        heading="Places"
        items={mostPages(rows)}
        empty="No matching pages up to the current date."
        render={(r) => (
          <li key={r.id}>
            <PlaceButton row={r} onSelect={onSelect} />{" "}
            <span className="skew-list__value">
              {pages(r.value)} ({share(r.value, total)})
            </span>
          </li>
        )}
      />
      <List
        heading="States"
        items={statePages(rows)}
        empty="No matching pages up to the current date."
        render={(s) => (
          <li key={s.state}>
            {s.state}{" "}
            <span className="skew-list__value">
              {pages(s.pages)} ({share(s.pages, total)}) in {s.places.toLocaleString("en-US")}{" "}
              {s.places === 1 ? "place" : "places"}
            </span>
          </li>
        )}
      />
      <p className="skew-list__note">Matching pages up to the current date; shares are of all of them.</p>
    </ListsPanel>
  );
}

/** Median date: the places whose matching pages fall earliest and latest. */
export function WhenLists({ rows, onSelect }: { rows: ListRow[]; onSelect: (id: string) => void }) {
  const { earliest, latest, eligible } = medianExtremes(rows);
  const empty =
    eligible === 1
      ? `Only one place has ${MIN_MEDIAN_PAGES} or more matching pages up to the current date.`
      : `No place has ${MIN_MEDIAN_PAGES} or more matching pages up to the current date.`;
  const item = (r: ListRow) => (
    <li key={r.id}>
      <PlaceButton row={r} onSelect={onSelect} />{" "}
      <span className="skew-list__value">
        {r.whenLabel} · {pages(r.value)}
      </span>
    </li>
  );
  return (
    <ListsPanel title="Earliest and latest" label="Places with the earliest and latest median dates">
      <List heading="Earliest median date" items={earliest} empty={empty} render={item} />
      <List heading="Latest median date" items={latest} empty={empty} render={item} />
      <p className="skew-list__note">
        A place&apos;s median date is when half of its matching pages up to the current date had been printed. Only
        places with at least {MIN_MEDIAN_PAGES} matching pages are listed.
      </p>
    </ListsPanel>
  );
}
