import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, renderHook, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";
import type { SearchParams } from "../api/client";
import type { HitSort } from "../api/types";
import type { SkewInfo } from "../lib/skewText";
import type { ListRow } from "./PlaceLists";
import { PlacePanel } from "./PlacePanel";
import { placesToPrefetch, usePrefetchPlaceHits, type ListsShown } from "./placeHits";
import type { SkewRow } from "./SkewPanels";

const info = (lower: number, upper: number): SkewInfo => ({
  estimate: (lower + upper) / 2,
  lower,
  upper,
  observed: 10,
  expected: 5,
  pages: 100,
  dir: lower > 1 ? 1 : upper < 1 ? -1 : 0,
  languages: null,
  languageCounts: null,
});
const skewRow = (id: string, lower: number, upper: number): SkewRow => ({ id, name: id, state: "AL", skew: info(lower, upper) });
const listRow = (id: string, value: number, when = 0.5): ListRow => ({ id, name: id, state: "AL", value, when, whenLabel: "" });

/** Ten places by pages: p1 has the most; p1 is also the earliest median, p10 the latest. */
const rows = Array.from({ length: 10 }, (_, i) => listRow(`p${i + 1}`, 100 - i, i / 10));

describe("which places are prefetched", () => {
  it("relative rate: the 3 clearest above 1× and the 3 clearest below, alternating", () => {
    const skew = [
      skewRow("a1", 3, 5),
      skewRow("a2", 2, 4),
      skewRow("a3", 1.5, 2),
      skewRow("a4", 1.1, 9),
      skewRow("b1", 0.1, 0.2),
      skewRow("b2", 0.2, 0.3),
      skewRow("b3", 0.3, 0.5),
      skewRow("b4", 0.5, 0.9),
      skewRow("unclear", 0.5, 2),
    ];
    expect(placesToPrefetch({ norm: "skew", rows: skew })).toEqual(["a1", "b1", "a2", "b2", "a3", "b3"]);
  });

  it("relative rate: one end may have fewer, or none", () => {
    expect(placesToPrefetch({ norm: "skew", rows: [skewRow("a1", 3, 5), skewRow("x", 0.5, 2)] })).toEqual(["a1"]);
  });

  it("Pages: the 3 places with the most pages (the states list has no place panel)", () => {
    expect(placesToPrefetch({ norm: "raw", rows })).toEqual(["p1", "p2", "p3"]);
  });

  it("Median date: the 3 earliest and the 3 latest", () => {
    expect(placesToPrefetch({ norm: "when", rows, exact: null })).toEqual(["p1", "p10", "p2", "p9", "p3", "p8"]);
  });

  it("Median date: by the exact days when the lists have them", () => {
    // Exact days reverse the order the buckets gave.
    const exact = new Map(rows.map((r, i) => [r.id, 1000 - i]));
    expect(placesToPrefetch({ norm: "when", rows, exact })).toEqual(["p10", "p1", "p9", "p2", "p8", "p3"]);
  });
});

interface Call {
  place: string;
  signal: AbortSignal | undefined;
  answer: (r: Response) => void;
}

/** A fetch whose answers the test gives, one call at a time. */
function heldFetch() {
  const calls: Call[] = [];
  const fn = vi.fn(
    (url: string, init?: RequestInit) =>
      new Promise<Response>((resolve, reject) => {
        const signal = init?.signal ?? undefined;
        calls.push({ place: new URL(url, "http://localhost").searchParams.get("place")!, signal, answer: resolve });
        signal?.addEventListener("abort", () => reject(signal.reason));
      }),
  );
  vi.stubGlobal("fetch", fn);
  return { fn, calls };
}

const json = (body: unknown, status = 200, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });
const hits = (place: string) =>
  json({
    index_version: "v1",
    place: { id: place, name: `Town ${place}`, state: "AL" },
    title: null,
    total: 7,
    items: [],
    next_cursor: null,
  });

function setup() {
  const client = new QueryClient({ defaultOptions: { queries: { staleTime: 5 * 60_000, retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return { client, wrapper };
}

interface HookProps {
  params: SearchParams;
  lists: ListsShown | null;
  sort?: HitSort;
}

function renderPrefetch(initial: HookProps) {
  const { client, wrapper } = setup();
  const hook = renderHook(
    ({ params, lists, sort = "oldest" }: HookProps) => usePrefetchPlaceHits(params, "v1", sort, lists),
    { wrapper, initialProps: initial },
  );
  return { client, wrapper, ...hook };
}

const params: SearchParams = { q: "railroad", mode: "phrase", bucket: "auto" };
const pages: ListsShown = { norm: "raw", rows };

/** Let pending promises and effects run. */
const settle = () => new Promise((r) => setTimeout(r, 0));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  delete (navigator as { connection?: unknown }).connection;
});

describe("usePrefetchPlaceHits", () => {
  it("asks for nothing while the lists aren't final (a search still computing)", async () => {
    const { fn } = heldFetch();
    renderPrefetch({ params, lists: null });
    await settle();
    expect(fn).not.toHaveBeenCalled();
  });

  it("asks for the places' page lists with the place panel's request, two at a time", async () => {
    const { fn, calls } = heldFetch();
    renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls.map((c) => c.place)).toEqual(["p1", "p2"]);
    expect(fn.mock.calls[0]![0]).toBe("/v1/hits?q=railroad&mode=phrase&v=v1&place=p1&limit=20");
    calls[0]!.answer(hits("p1"));
    await waitFor(() => expect(calls).toHaveLength(3));
    expect(calls[2]!.place).toBe("p3");
    calls[1]!.answer(hits("p2"));
    calls[2]!.answer(hits("p3"));
    await settle();
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("prefetches once per search and measure, not on every playback step", async () => {
    const { fn, calls } = heldFetch();
    const view = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    for (const c of calls) c.answer(hits(c.place));
    await waitFor(() => expect(calls).toHaveLength(3));
    calls[2]!.answer(hits("p3"));
    // Playback moves the lists: other places lead now.
    const later = rows.map((r, i) => ({ ...r, value: i }));
    view.rerender({ params, lists: { norm: "raw", rows: later } });
    view.rerender({ params, lists: null });
    view.rerender({ params, lists: { norm: "raw", rows: later } });
    await settle();
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("switching measures queues each measure once, through the same two requests at a time", async () => {
    const { fn, calls } = heldFetch();
    const view = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    // Median date while Pages is still loading: p1 to p3 are queued already, so only its other places are added.
    view.rerender({ params, lists: { norm: "when", rows, exact: null } });
    await settle();
    expect(calls).toHaveLength(2);
    // Back to Pages, with playback having moved its list: nothing new.
    view.rerender({ params, lists: { norm: "raw", rows: rows.map((r, i) => ({ ...r, value: i })) } });
    // p1 finds the API busy; the rest answer. Never more than two waiting at once.
    for (let i = 0; i < calls.length; i++) {
      expect(calls.length - i).toBeLessThanOrEqual(2);
      calls[i]!.answer(i === 0 ? json({ type: "/errors/busy", title: "Busy", status: 503 }, 503) : hits(calls[i]!.place));
      await settle();
    }
    await waitFor(() => expect(fn).toHaveBeenCalledTimes(6));
    expect(calls.map((c) => c.place)).toEqual(["p1", "p2", "p3", "p10", "p9", "p8"]);
    // Switching again asks for nothing, not even p1, which failed.
    view.rerender({ params, lists: { norm: "when", rows, exact: null } });
    view.rerender({ params, lists: pages });
    await settle();
    expect(fn).toHaveBeenCalledTimes(6);
  });

  it("a new search cancels the last one's prefetching and starts its own", async () => {
    const { calls } = heldFetch();
    const view = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    const next = { ...params, q: "cotton" };
    view.rerender({ params: next, lists: null });
    await settle();
    expect(calls.map((c) => c.signal?.aborted)).toEqual([true, true]);
    // Nothing more for the old search, and nothing for the new one until its lists are final.
    expect(calls).toHaveLength(2);
    view.rerender({ params: next, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(4));
  });

  it("stops when the page goes away", async () => {
    const { calls } = heldFetch();
    const view = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    view.unmount();
    await settle();
    expect(calls.every((c) => c.signal?.aborted)).toBe(true);
    expect(calls).toHaveLength(2);
  });

  it("asks for nothing when the browser asks to save data", async () => {
    Object.defineProperty(navigator, "connection", { value: { saveData: true }, configurable: true });
    const { fn } = heldFetch();
    renderPrefetch({ params, lists: pages });
    await settle();
    expect(fn).not.toHaveBeenCalled();
  });

  it("gives up quietly on a busy API or a rate limit, and goes on to the next place", async () => {
    const { fn, calls } = heldFetch();
    renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    calls[0]!.answer(json({ type: "/errors/busy", title: "Busy", status: 503 }, 503, { "retry-after": "1" }));
    calls[1]!.answer(json({ type: "/errors/rate-limited", title: "Slow down", status: 429 }, 429, { "retry-after": "1" }));
    await waitFor(() => expect(calls).toHaveLength(3));
    calls[2]!.answer(hits("p3"));
    // Well past the Retry-After: neither is asked for again.
    await new Promise((r) => setTimeout(r, 1200));
    expect(fn).toHaveBeenCalledTimes(3);
    expect(calls.map((c) => c.place)).toEqual(["p1", "p2", "p3"]);
  });

  it("a place panel opened later shows the prefetched list without asking again", async () => {
    const { fn, calls } = heldFetch();
    const { wrapper } = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    calls[0]!.answer(hits("p1"));
    await waitFor(() => expect(calls).toHaveLength(3));
    render(
      <PlacePanel
        params={params}
        version="v1"
        placeId="p1"
        placeName="Town p1"
        sort="oldest"
        onSort={() => undefined}
        windowHits={7}
        synthetic={false}
        onClose={() => undefined}
      />,
      { wrapper },
    );
    expect(screen.getByText("7 pages in this search")).toBeTruthy();
    await settle();
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it("a place panel opened while its prefetch is in flight shares the request, and a new search doesn't cancel it", async () => {
    const { fn, calls } = heldFetch();
    const { wrapper, rerender } = renderPrefetch({ params, lists: pages });
    await waitFor(() => expect(calls).toHaveLength(2));
    render(
      <PlacePanel
        params={params}
        version="v1"
        placeId="p2"
        placeName="Town p2"
        sort="oldest"
        onSort={() => undefined}
        windowHits={7}
        synthetic={false}
        onClose={() => undefined}
      />,
      { wrapper },
    );
    await settle();
    expect(fn).toHaveBeenCalledTimes(2);
    rerender({ params: { ...params, q: "cotton" }, lists: null });
    await settle();
    expect(calls[0]!.signal?.aborted).toBe(true);
    expect(calls[1]!.signal?.aborted).toBe(false);
    calls[1]!.answer(hits("p2"));
    await screen.findByText("7 pages in this search");
  });
});
