import { useEffect, useRef } from "react";
import { infiniteQueryOptions, useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { api, type SearchParams } from "../api/client";
import type { HitSort } from "../api/types";
import { medianLists, mostPages, type ListRow } from "./PlaceLists";
import { clearest, type SkewRow } from "./SkewPanels";

/**
 * A place's page list as the place panel loads it. Prefetching uses the same
 * query key and the same request, so a prefetched list is the panel's
 * cached answer, and the API's caches are keyed on the same URL (#265).
 * `giveUp` is for prefetching only (see `GetOptions.giveUp`).
 */
export function placeHitsQuery(
  params: SearchParams,
  version: string,
  placeId: string,
  sort: HitSort,
  giveUp?: () => boolean,
) {
  return infiniteQueryOptions({
    queryKey: ["hits", version, params, placeId, sort],
    queryFn: ({ pageParam, signal }) => api.hits(params, version, placeId, sort, pageParam, signal, giveUp),
    initialPageParam: null as string | null,
    getNextPageParam: (last) => last.next_cursor,
  });
}

/** How many places at each end of the side panel's lists get their page lists prefetched. */
export const PREFETCH_EACH_END = 3;
/** The most prefetches in flight at once, so they don't crowd visitors' searches. */
const MAX_IN_FLIGHT = 2;

/** The side panel's lists, in the measure shown. */
export type ListsShown =
  | { norm: "skew"; rows: SkewRow[] }
  | { norm: "raw"; rows: ListRow[] }
  | { norm: "when"; rows: ListRow[]; exact: Map<string, number> | null };

/** The first of each list, then the second of each, and so on; each place once. */
function alternate(a: { id: string }[], b: { id: string }[]): string[] {
  const ids: string[] = [];
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    for (const r of [a[i], b[i]]) if (r && !ids.includes(r.id)) ids.push(r.id);
  }
  return ids;
}

/**
 * The places whose page lists are prefetched, in order: the 3 clearest above
 * and below 1× (relative rate), the 3 places with the most pages (Pages; the
 * states list has no place panel), or the 3 earliest and latest median dates.
 */
export function placesToPrefetch(lists: ListsShown): string[] {
  const n = PREFETCH_EACH_END;
  if (lists.norm === "skew") {
    const { above, below } = clearest(lists.rows);
    return alternate(above.slice(0, n), below.slice(0, n));
  }
  if (lists.norm === "raw") return mostPages(lists.rows).slice(0, n).map((r) => r.id);
  const { earliest, latest } = medianLists(lists.rows, lists.exact);
  return alternate(earliest.slice(0, n), latest.slice(0, n));
}

/** The visitor asked browsers to save data (Data Saver, Lite mode). */
function saveData(): boolean {
  const nav = navigator as Navigator & { connection?: { saveData?: boolean } };
  return nav.connection?.saveData === true;
}

function observed(client: QueryClient, queryKey: QueryKey): boolean {
  return (client.getQueryCache().find({ queryKey, exact: true })?.getObserversCount() ?? 0) > 0;
}

/**
 * Prefetch the page lists of `ids`, at most {@link MAX_IN_FLIGHT} at a time,
 * without retries. Returns a function that stops: no more are started, and
 * those in flight are cancelled unless a place panel is now showing one.
 */
function prefetch(
  client: QueryClient,
  params: SearchParams,
  version: string,
  sort: HitSort,
  ids: string[],
): () => void {
  let stopped = false;
  let next = 0;
  const inFlight = new Set<QueryKey>();
  const worker = async () => {
    while (!stopped && next < ids.length) {
      const id = ids[next++]!;
      // Once a panel shows this place, its request waits on a busy API as the panel's own would.
      const query = placeHitsQuery(params, version, id, sort, () => !observed(client, query.queryKey));
      inFlight.add(query.queryKey);
      try {
        // Already cached and fresh: no request. Failures are dropped; a panel opened later asks again.
        await client.prefetchInfiniteQuery({ ...query, retry: false });
      } finally {
        inFlight.delete(query.queryKey);
      }
    }
  };
  for (let i = 0; i < MAX_IN_FLIGHT; i++) void worker();
  return () => {
    stopped = true;
    for (const queryKey of inFlight) {
      if (!observed(client, queryKey)) void client.cancelQueries({ queryKey, exact: true });
    }
  };
}

/**
 * Once a search's lists are final, load in the background the page lists of
 * the places at each end of them (#265), so opening one is instant and the
 * API's caches hold them for later visitors. `lists` is null until then:
 * while the search is computing, a place panel is open, or the lists still
 * depend on something loading. Once per search and measure, not per
 * playback step; a new search cancels what is left. Skipped when the
 * visitor's browser asks to save data.
 */
export function usePrefetchPlaceHits(
  params: SearchParams,
  version: string,
  sort: HitSort,
  lists: ListsShown | null,
): void {
  const client = useQueryClient();
  const search = JSON.stringify([version, params]);
  const run = useRef<{ key: string; stop: () => void } | null>(null);
  // A new search, or leaving the page, stops the last one's prefetching.
  useEffect(
    () => () => {
      run.current?.stop();
      run.current = null;
    },
    [search],
  );
  const key = JSON.stringify([search, sort, lists?.norm]);
  useEffect(() => {
    if (!lists || run.current?.key === key || saveData()) return;
    run.current?.stop();
    run.current = { key, stop: prefetch(client, params, version, sort, placesToPrefetch(lists)) };
  }, [client, key, lists, params, version, sort]);
}
