import { useInfiniteQuery } from "@tanstack/react-query";
import { api, ApiError, type SearchParams } from "../api/client";
import { snippetSegments } from "../lib/snippet";
import { formatDate } from "../lib/time";
import { skewSentence, type SkewInfo } from "../lib/skewText";

interface Props {
  params: SearchParams;
  version: string;
  placeId: string;
  placeName: string;
  windowHits: number;
  /** The place's relative rate in the current window (relative-rate view). */
  note?: SkewInfo;
  /** Synthetic fixtures: their LCCNs are invented, so LoC has no such pages. */
  synthetic: boolean;
  onClose: () => void;
}

/** Place drill-down (F-03): date-sorted pages with snippets and LoC links. */
export function PlacePanel({ params, version, placeId, placeName, windowHits, note, synthetic, onClose }: Props) {
  const query = useInfiniteQuery({
    queryKey: ["hits", version, params, placeId],
    queryFn: ({ pageParam, signal }) => api.hits(params, version, placeId, pageParam, signal),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor,
  });
  const first = query.data?.pages[0];
  const items = query.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <aside className="panel" aria-label={`Pages from ${placeName}`}>
      <header className="panel__head">
        <h2>{first?.place ? `${first.place.name}, ${first.place.state}` : placeName}</h2>
        <button type="button" className="button button--quiet" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>
      {note && <p className="panel__skew">{skewSentence(placeName, note)}</p>}
      <p className="panel__summary">
        {first ? `${first.total.toLocaleString()} pages in this search` : "Loading…"}
        {first && windowHits !== first.total ? ` · ${windowHits.toLocaleString()} up to the current date` : ""}
      </p>
      {query.error && (
        <p role="alert" className="notice notice--error">
          {query.error instanceof ApiError ? (query.error.problem.hint ?? query.error.message) : "Could not load pages."}
        </p>
      )}
      <ol className="hits">
        {items.map((h) => (
          <li key={h.doc_id} className="hit">
            <div className="hit__meta">
              <time dateTime={h.date}>{formatDate(h.date)}</time> · {h.title ?? h.lccn} · page {h.seq}
              {h.front_page ? " (front page)" : ""}
            </div>
            {h.snippets.map((s, i) => (
              <p key={i} className="hit__snippet">
                {snippetSegments(s).map((seg, j) => (seg.mark ? <mark key={j}>{seg.text}</mark> : seg.text))}
              </p>
            ))}
            {synthetic ? (
              <p className="hit__demo">Demo page: not a real Library of Congress page.</p>
            ) : h.links.viewer && (
              <a href={h.links.viewer} target="_blank" rel="noopener noreferrer">
                View page at the Library of Congress
              </a>
            )}
          </li>
        ))}
      </ol>
      {query.hasNextPage && (
        <button
          type="button"
          className="button"
          disabled={query.isFetchingNextPage}
          onClick={() => void query.fetchNextPage()}
        >
          {query.isFetchingNextPage ? "Loading…" : "Load more"}
        </button>
      )}
    </aside>
  );
}
