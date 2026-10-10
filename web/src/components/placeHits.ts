import { useEffect, useRef } from "react";
import { infiniteQueryOptions, useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { api, type SearchParams } from "../api/client";
import type { HitSort } from "../api/types";
import { medianLists, mostPages, type ListRow } from "./PlaceLists";
import { clearest, type SkewRow } from "./SkewPanels";

/** The search as `/v1/hits` asks it: a page list doesn't depend on the time bucket, which isn't sent. */
function hitsSearch(params: SearchParams): SearchParams {
  return { ...params, bucket: undefined };
}

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
    // Without the bucket, as the request is: another bucket of the same search reuses the list.
    queryKey: ["hits", version, hitsSearch(params), placeId, sort],
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
  if (lists.norm === "raw")
    return mostPages(lists.rows)
      .slice(0, n)
      .map((r) => r.id);
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

/** One search's prefetching: a queue that at most {@link MAX_IN_FLIGHT} requests work through. */
interface Prefetcher {
  /** Queue the page lists of `ids` not queued before. */
  add: (ids: string[], sort: HitSort) => void;
  /** Start no more, and cancel those in flight unless a place panel is now showing one. */
  stop: () => void;
}

function prefetcher(client: QueryClient, params: SearchParams, version: string): Prefetcher {
  let stopped = false;
  let workers = 0;
  const queue: { id: string; sort: HitSort }[] = [];
  const queued = new Set<string>();
  const inFlight = new Set<QueryKey>();
  const worker = async () => {
    workers++;
    try {
      while (!stopped && queue.length > 0) {
        const { id, sort } = queue.shift()!;
        // Once a panel shows this place, its request waits on a busy API as the panel's own would.
        const query = placeHitsQuery(params, version, id, sort, () => !observed(client, query.queryKey));
        inFlight.add(query.queryKey);
        try {
          // Already cached and fresh: no request. Failures are dropped, never retried; a panel opened later asks again.
          await client.prefetchInfiniteQuery({ ...query, retry: false });
        } finally {
          inFlight.delete(query.queryKey);
        }
      }
    } finally {
      workers--;
    }
  };
  return {
    add: (ids, sort) => {
      for (const id of ids) {
        const key = JSON.stringify([id, sort]);
        if (queued.has(key)) continue;
        queued.add(key);
        queue.push({ id, sort });
      }
      while (!stopped && workers < MAX_IN_FLIGHT && queue.length > 0) void worker();
    },
    stop: () => {
      stopped = true;
      for (const queryKey of inFlight) {
        if (!observed(client, queryKey)) void client.cancelQueries({ queryKey, exact: true });
      }
    },
  };
}

/**
 * Once a search's lists are final, load in the background the page lists of
 * the places at each end of them (#265), so opening one is instant and the
 * API's caches hold them for later visitors. `lists` is null until then:
 * while the search is computing, a place panel is open, or the lists still
 * depend on something loading. Once per search and measure, from the lists
 * as they first stand: not per playback step, and not again when the
 * visitor switches back to a measure. A new search cancels what is left.
 * Skipped when the visitor's browser asks to save data.
 */
export function usePrefetchPlaceHits(
  params: SearchParams,
  version: string,
  sort: HitSort,
  lists: ListsShown | null,
): void {
  const client = useQueryClient();
  // Another bucket is the same search here: its page lists are the same requests.
  const search = JSON.stringify([version, hitsSearch(params)]);
  const run = useRef<{ search: string; started: Set<string>; prefetcher: Prefetcher } | null>(null);
  // A new search, or leaving the page, stops the last one's prefetching.
  useEffect(
    () => () => {
      run.current?.prefetcher.stop();
      run.current = null;
    },
    [search],
  );
  useEffect(() => {
    if (!lists || saveData()) return;
    if (run.current?.search !== search) {
      run.current?.prefetcher.stop();
      run.current = { search, started: new Set(), prefetcher: prefetcher(client, params, version) };
    }
    const measure = JSON.stringify([sort, lists.norm]);
    if (run.current.started.has(measure)) return;
    run.current.started.add(measure);
    run.current.prefetcher.add(placesToPrefetch(lists), sort);
  }, [client, search, lists, params, version, sort]);
}
