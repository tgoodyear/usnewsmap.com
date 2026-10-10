import { useEffect, useId, useRef, useState } from "react";
import { formatTimes } from "../lib/skewScale";
import { formatExpected, formatRange, type SkewInfo } from "../lib/skewText";
import { dateFromDay } from "../lib/time";
import { ListsPanel } from "./PlaceLists";

export interface SkewRow {
  id: string;
  name: string;
  state: string;
  skew: SkewInfo;
  /** The place's first and last matching day in the whole search; negative when unknown. */
  firstDay?: number;
  lastDay?: number;
}

/** How many places each list shows at first, and how many more each "Show more" adds. */
const LIST_LENGTH = 5;
/**
 * The most places a list shows, so an expanded list doesn't push the rest of
 * the page off a phone's screen. The table has every place.
 */
const MOST_SHOWN = 25;

/**
 * The places that differ most clearly (doc 11, 11.6): above 1 sorted by the
 * interval's lower bound, below 1 by its upper bound. Only places whose
 * interval excludes 1, all of them: the lists choose how many to show.
 * Papers in other languages are named, not left out.
 */
export function clearest(rows: SkewRow[]): { above: SkewRow[]; below: SkewRow[] } {
  const usable = rows.filter((r) => r.skew.dir !== 0);
  return {
    above: usable.filter((r) => r.skew.dir === 1).sort((a, b) => b.skew.lower - a.skew.lower),
    below: usable.filter((r) => r.skew.dir === -1).sort((a, b) => a.skew.upper - b.skew.upper),
  };
}

/** How many places each Clearest differences list shows. */
export interface ListLengths {
  above: number;
  below: number;
}

/**
 * How many places each Clearest differences list shows, kept above the lists
 * so they keep it when they move (phones put them after the playback
 * controls, so a rotation remounts them). Both go back to the first few when
 * `resetKey` changes: a new search, window or measure.
 */
export function useListLengths(resetKey: string): [ListLengths, (list: keyof ListLengths, n: number) => void] {
  const [state, setState] = useState({ key: resetKey, above: LIST_LENGTH, below: LIST_LENGTH });
  let current = state;
  if (state.key !== resetKey) {
    current = { key: resetKey, above: LIST_LENGTH, below: LIST_LENGTH };
    setState(current);
  }
  return [current, (list, n) => setState((s) => ({ ...s, [list]: n }))];
}

interface ListsProps {
  rows: SkewRow[];
  onSelect: (id: string) => void;
  /** How many places each list shows, from `useListLengths`. */
  shown: ListLengths;
  onShown: (list: keyof ListLengths, n: number) => void;
}

/** The side panel when no place is selected. */
export function SkewLists({ rows, onSelect, shown, onShown }: ListsProps) {
  const { above, below } = clearest(rows);
  return (
    <ListsPanel title="Clearest differences" label="Places that differ most clearly" className="skew-lists">
      <ClearList
        heading="Most clearly above 1×"
        empty="No place is clearly above 1× in this window."
        items={above}
        limit={shown.above}
        onLimit={(n) => onShown("above", n)}
        onSelect={onSelect}
      />
      <ClearList
        heading="Most clearly below 1×"
        empty="No place is clearly below 1× in this window."
        items={below}
        limit={shown.below}
        onLimit={(n) => onShown("below", n)}
        onSelect={onSelect}
      />
      <p className="skew-list__note">Ranked by the end of each place's 90% range nearest 1×.</p>
    </ListsPanel>
  );
}

/**
 * One of the two lists: its first `limit` places, how many there are in all,
 * and buttons to show the next ones or go back to the first few.
 */
function ClearList({
  heading,
  empty,
  items,
  limit,
  onLimit,
  onSelect,
}: {
  heading: string;
  empty: string;
  items: SkewRow[];
  limit: number;
  onLimit: (n: number) => void;
  onSelect: (id: string) => void;
}) {
  const id = useId();
  const listId = `${id}-list`;
  const headingId = `${id}-heading`;
  const countId = `${id}-count`;
  const visible = items.slice(0, Math.min(limit, MOST_SHOWN));
  const more = items.length > visible.length && visible.length < MOST_SHOWN;
  // Showing more than the first few: what aria-expanded says on both buttons.
  const expanded = visible.length > LIST_LENGTH;
  const list = useRef<HTMLOListElement>(null);
  const moreButton = useRef<HTMLButtonElement>(null);
  // Where focus goes once the list has changed: after "Show more", the first
  // place it added, so keyboard and screen reader users land on the new
  // rows; after "Show fewer", which goes away, the "Show more" button.
  const focusNext = useRef<number | "more" | null>(null);

  useEffect(() => {
    const next = focusNext.current;
    if (next === null) return;
    focusNext.current = null;
    if (next === "more") moreButton.current?.focus();
    else list.current?.children[next]?.querySelector("button")?.focus();
  }, [limit]);

  let count = "";
  if (items.length > LIST_LENGTH) {
    count =
      visible.length === items.length
        ? `Showing all ${items.length.toLocaleString("en-US")}.`
        : `Showing ${visible.length} of ${items.length.toLocaleString("en-US")}.`;
    if (visible.length === MOST_SHOWN && items.length > MOST_SHOWN) count += " The table lists every place.";
  }

  return (
    <section className="skew-list">
      <h3 id={headingId}>{heading}</h3>
      {items.length === 0 ? (
        <p className="skew-list__empty">{empty}</p>
      ) : (
        <ol id={listId} ref={list}>
          {visible.map((r) => (
            <li key={r.id}>
              <button type="button" className="link-button" onClick={() => onSelect(r.id)}>
                {r.name}, {r.state}
              </button>{" "}
              <span className="skew-list__value">
                {formatTimes(r.skew.estimate)} ({formatRange(r.skew)})
              </span>
              {r.skew.languages && <LanguageNote label={r.skew.languages} counts={r.skew.languageCounts} />}
            </li>
          ))}
        </ol>
      )}
      {count && (
        <p className="skew-list__count" id={countId}>
          {count}
        </p>
      )}
      {(more || expanded) && (
        <p className="skew-list__more">
          {more && (
            <button
              type="button"
              className="link-button"
              ref={moreButton}
              aria-expanded={expanded}
              aria-controls={listId}
              aria-describedby={`${headingId} ${countId}`}
              onClick={() => {
                focusNext.current = visible.length;
                onLimit(Math.min(visible.length + LIST_LENGTH, MOST_SHOWN));
              }}
            >
              Show more
            </button>
          )}
          {expanded && (
            <button
              type="button"
              className="link-button"
              aria-expanded={true}
              aria-controls={listId}
              aria-describedby={`${headingId} ${countId}`}
              onClick={() => {
                focusNext.current = "more";
                onLimit(LIST_LENGTH);
              }}
            >
              Show fewer
            </button>
          )}
        </p>
      )}
    </section>
  );
}

/** What a "Papers in …" label means, for the info button beside it. */
export const LANGUAGE_EXPLAINER =
  "These are the languages this place's papers are catalogued in, across the whole collection. " +
  "They don't say which language the matched pages are in. " +
  "An English search term rarely matches pages printed in another language, so a place with such papers can read low. " +
  "To compare only pages from English-language papers, choose English under newspaper languages.";

/**
 * A place's language label with a small info button that shows how many of
 * its papers are in each language and what the label means. A tap (not
 * hover), so it works on phones.
 */
export function LanguageNote({ label, counts }: { label: string; counts?: string | null }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <span className="skew-list__lang">
      <span>{label}</span>
      <button
        type="button"
        className="info-button"
        aria-label="What this means"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen((o) => !o)}
        onKeyDown={(e) => {
          if (e.key === "Escape") setOpen(false);
        }}
      >
        i
      </button>
      <span id={id} role="note" className="info-tip" hidden={!open}>
        {counts && <span className="info-tip__counts">{counts}</span>}
        {LANGUAGE_EXPLAINER}
      </span>
    </span>
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
