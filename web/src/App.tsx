import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, ApiError, VersionChangedError, type SearchParams } from "./api/client";
import type { Problem } from "./api/types";
import { indexSummary } from "./lib/indexSummary";
import { alignCube, prefixSums, relative, windowQuantile, windowValues } from "./engine/cube";
import { EXAMPLE_ORDER, EXAMPLES_SHOWN, examplesAt } from "./examples";
import { bucketIndex, bucketLabel, bucketStart } from "./lib/time";
import { cssColor, cssTimeColor } from "./lib/scale";
import { searchParams, serializeView, useView, type ViewState } from "./state/url";
import { SearchBar, searchKey } from "./components/SearchBar";
import { Timeline } from "./components/Timeline";
import { TimeDock } from "./components/TimeDock";
import { PlacePanel } from "./components/PlacePanel";
import { PlaceTable } from "./components/PlaceTable";
import { NewspaperTable, paperRows } from "./components/NewspaperTable";
import { languageMix } from "./lib/languages";
import { Mentions } from "./components/Mentions";
import { About } from "./components/About";
import type { MapPoint } from "./components/mapTypes";
import { MeasureToggle } from "./components/MeasureToggle";
import { SkewLegend } from "./components/SkewLegend";
import { useMediaQuery } from "./lib/useMediaQuery";
import { DownloadCsv, SkewLists, StateTable, clearest, type SkewRow } from "./components/SkewPanels";
import { hasWebGL2 } from "./lib/webgl";
import { prepareSkew, type Prepared, type Unavailable } from "./engine/skewInput";
import { maxWindowExpected, scoreFrame } from "./engine/skewModel";
import { useSkewModel } from "./engine/useSkew";
import { skewInfo } from "./lib/skewText";
import { Brand } from "./components/Brand";

const MapView = lazy(() => import("./components/MapView"));
const webgl = typeof document !== "undefined" && hasWebGL2();

export function App() {
  const [view, setView] = useView();
  // Which set of examples the home page shows; kept across searches so Back
  // returns to the same cards.
  const [examplePage, setExamplePage] = useState(0);
  const about = useRef<HTMLDialogElement>(null);
  // showModal() focuses the first link, which then shows a focus ring before anyone has tabbed.
  // Start on the heading instead: screen readers announce the dialog by it, and Tab goes on to
  // the first link.
  const openAbout = () => {
    about.current?.showModal();
    about.current?.querySelector<HTMLElement>("#about-title")?.focus();
  };
  const meta = useQuery({ queryKey: ["meta"], queryFn: ({ signal }) => api.meta(signal) });
  const version = meta.data?.index_version ?? "";
  const places = useQuery({
    queryKey: ["places", version],
    queryFn: ({ signal }) => api.places(version, signal),
    enabled: !!version,
    staleTime: Infinity,
  });

  // Parsed afresh on every URL change; a stable key keeps queries cached.
  const paramsKey = JSON.stringify(searchParams(view));
  const params = useMemo(() => JSON.parse(paramsKey) as SearchParams, [paramsKey]);

  // The search the API said it is still computing (a `202`), by query key,
  // and how many searches are ahead of it while it waits for a slot.
  const aggKey = JSON.stringify([version, params]);
  const [computing, setComputing] = useState<{ key: string; ahead: number | null } | null>(null);
  const agg = useQuery({
    queryKey: ["aggregate", version, params],
    queryFn: ({ signal }) => {
      // A new attempt starts as a plain search until the API says otherwise.
      setComputing((c) => (c?.key === aggKey ? null : c));
      return api.aggregate(params, version, signal, (ahead) => setComputing({ key: aggKey, ahead }));
    },
    enabled: !!version && !!view.q,
    // Keep showing the previous search while the next loads, but never a
    // result from another index version (it would be drawn against this
    // version's places).
    placeholderData: (prev) => (prev?.index_version === version ? prev : undefined),
  });
  const baselineRef = agg.data?.cube.baseline_ref ?? null;
  const coverage = useQuery({
    queryKey: ["coverage", baselineRef],
    queryFn: ({ signal }) => api.coverage(baselineRef!, version, signal),
    enabled: !!baselineRef,
    staleTime: Infinity,
  });

  const data = agg.data;
  const stillComputing = agg.isFetching && computing?.key === aggKey;
  const ahead = stillComputing ? computing.ahead : null;
  const count = data?.bucket.count ?? 0;
  // Scrubbing and playback update the view at once but write the URL only
  // when movement pauses: browsers throttle the History API (Firefox allows
  // about 200 calls per 10 s), and a permalink only needs where it stopped.
  // `base` is the URL's `t` when scrubbing began; once the URL changes for
  // any reason (the deferred write, back/forward, a new search) it wins.
  // It also belongs to one search: a new search never inherits it.
  const key = searchKey(view);
  const [scrub, setScrub] = useState<{ key: string; base: string; t: string } | null>(null);
  const tIso = scrub && scrub.key === key && scrub.base === view.t ? scrub.t : view.t;
  const t = data ? (tIso ? bucketIndex(data.bucket.unit, data.bucket.from, count, tIso) : count - 1) : 0;
  const urlWrite = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(urlWrite.current), [key]);

  // Prefix sums are built once per response; each frame is O(places).
  const hitSums = useMemo(
    () => (data ? prefixSums(data.cube, data.places.id.length, count) : null),
    [data, count],
  );
  const pageSums = useMemo(() => {
    if (!data || !coverage.data || coverage.data.count !== count) return null;
    const aligned = alignCube(coverage.data.pages, coverage.data.places, data.places.id);
    return prefixSums(aligned, data.places.id.length, count);
  }, [data, coverage.data, count]);

  const features = useMemo(
    () => new Map((places.data?.features ?? []).map((f) => [f.id, f])),
    [places.data],
  );

  // Relative rate (doc 11): fitted once per search, off the main thread;
  // each frame is then scored from prefix sums.
  const wantSkew = view.norm === "skew";
  // Once the view has been shown, keep its fit when switching to Pages and
  // back; until then don't fit at all.
  const [skewUsed, setSkewUsed] = useState(wantSkew);
  if (wantSkew && !skewUsed) setSkewUsed(true);
  const prepared: Prepared | Unavailable | null = useMemo(() => {
    if (!skewUsed || !data || !places.data) return null;
    // No baselines (a language filter on a version published before pages were
    // counted per language): there is no coverage to wait for (07 §7.9).
    if (data.cube.baseline_ref === null || data.series.baseline === null) return "filters";
    return coverage.data ? prepareSkew(data, coverage.data, features) : null;
  }, [skewUsed, data, coverage.data, places.data, features]);
  const { state: skew, last: lastSkew } = useSkewModel(typeof prepared === "object" ? prepared : null);
  // While a refit of the same search (and index version) runs, keep drawing
  // the previous fit rather than flipping to page counts and back. Another
  // search's scores must never be shown as this one's.
  const skewModel =
    skew.status === "ready"
      ? skew
      : skew.status === "computing" &&
          typeof prepared === "object" &&
          prepared !== null &&
          lastSkew?.prepared.search === prepared.search
        ? lastSkew
        : null;
  const frame = useMemo(
    () => (skewModel ? scoreFrame(skewModel.model, t, view.win) : null),
    [skewModel, t, view.win],
  );
  const maxExpected = useMemo(
    () => (skewModel ? maxWindowExpected(skewModel.model.places, view.win) : 1),
    [skewModel, view.win],
  );
  // Biggest circles first, so smaller ones are drawn on top of them. Circle size follows the
  // current frame's expected matches, so the order does too: in early cumulative frames and
  // trailing windows it differs from the whole search's.
  const drawOrder = useMemo(() => {
    if (!skewModel || !frame) return [];
    const units = skewModel.model.places.units;
    return Array.from({ length: units }, (_, u) => u).sort(
      (a, b) => (frame.places[b]?.expected ?? 0) - (frame.places[a]?.expected ?? 0),
    );
  }, [skewModel, frame]);
  const daysById = useMemo(
    () =>
      new Map(
        (data?.places.id ?? []).map((id, i) => [
          id,
          { firstDay: data!.places.first_day[i]!, lastDay: data!.places.last_day?.[i] ?? -1 },
        ]),
      ),
    [data],
  );
  const skewRows: (SkewRow & MapPoint & { firstDay: number; lastDay: number })[] = useMemo(() => {
    if (!skewModel || !frame) return [];
    const { placeIds, languages, languageCounts } = skewModel.prepared;
    return drawOrder.flatMap((i) => {
      const id = placeIds[i]!;
      const f = features.get(id);
      if (!f) return [];
      const info = skewInfo(frame.places[i]!, frame.placePages[i]!, languages[i] ?? null, languageCounts[i] ?? null);

      return [
        {
          id,
          name: f.properties.name,
          state: f.properties.state,
          precision: f.properties.precision,
          position: f.geometry.coordinates,
          value: info.observed,
          rel: info.pages > 0 ? info.observed / info.pages : Number.NaN,
          skew: info,
          firstDay: daysById.get(id)?.firstDay ?? -1,
          lastDay: daysById.get(id)?.lastDay ?? -1,
        },
      ];
    });
  }, [skewModel, frame, features, drawOrder, daysById]);
  // Lists, table, export and the announcement: places with pages in the frame.
  const skewListed = useMemo(() => skewRows.filter((r) => r.skew.pages > 0), [skewRows]);
  const stateRows = useMemo(
    () =>
      skewModel && frame
        ? skewModel.prepared.stateCodes.map((state, i) => ({
            state,
            skew: skewInfo(frame.states[i]!, frame.statePages[i]!, null),
          }))
        : [],
    [skewModel, frame],
  );

  // Relative frequency needs the coverage cube; until it arrives (or if it
  // fails) it is unknown (NaN), never 0.
  const points: (MapPoint & { firstDay: number; lastDay: number; middle: string })[] = useMemo(() => {
    if (!data || !hitSums) return [];
    const hits = windowValues(hitSums, t, view.win);
    // When each place's matches fell (#127): the median and middle half of
    // its pages in the window, by bucket.
    const median = windowQuantile(hitSums, t, view.win, 0.5);
    const q1 = windowQuantile(hitSums, t, view.win, 0.25);
    const q3 = windowQuantile(hitSums, t, view.win, 0.75);
    const label = (b: number) =>
      Number.isNaN(b) ? "" : bucketLabel(data.bucket.unit, bucketStart(data.bucket.unit, data.bucket.from, b));
    const span = Math.max(count - 1, 1);
    const rel = pageSums
      ? relative(hits, windowValues(pageSums, t, view.win))
      : new Float64Array(hits.length).fill(Number.NaN);
    return data.places.id.flatMap((id, i) => {
      const f = features.get(id);
      if (!f) return [];
      return [
        {
          id,
          name: f.properties.name,
          state: f.properties.state,
          precision: f.properties.precision,
          position: f.geometry.coordinates,
          value: hits[i]!,
          rel: rel[i]!,
          firstDay: data.places.first_day[i]!,
          lastDay: data.places.last_day?.[i] ?? -1,
          when: median[i]! / span,
          whenLabel: label(median[i]!),
          middle: q1[i] === q3[i] ? label(q1[i]!) : `${label(q1[i]!)} – ${label(q3[i]!)}`,
        },
      ];
    });
  }, [data, hitSums, pageSums, features, t, view.win, count]);

  // Scale circles to the largest value the playback will reach, so they
  // grow and shrink on one scale (cumulative: the totals; trailing: the
  // biggest window anywhere in the range). O(places × buckets), once.
  const maxValue = useMemo(() => {
    if (!hitSums) return 1;
    let max = 1;
    const buf = new Float64Array(hitSums.places);
    const ts = view.win === null ? [count - 1] : Array.from({ length: count }, (_, i) => i);
    for (const i of ts) for (const v of windowValues(hitSums, i, view.win, buf)) max = Math.max(max, v);
    return max;
  }, [hitSums, count, view.win]);
  // Colour by page counts until the relative rate is ready.
  const norm = view.norm === "skew" && !skewModel ? "raw" : view.norm;

  const urlT = view.t;
  const seek = useCallback(
    (i: number) => {
      if (!data) return;
      const iso = bucketStart(data.bucket.unit, data.bucket.from, i);
      setScrub((s) => ({ key, base: s && s.key === key && s.base === urlT ? s.base : urlT, t: iso }));
      clearTimeout(urlWrite.current);
      urlWrite.current = setTimeout(() => {
        setView({ t: iso });
        setScrub(null);
      }, 300);
    },
    [data, setView, urlT, key],
  );
  const select = useCallback((id: string) => setView({ place: id }), [setView]);
  const onViewport = useCallback(
    (z: number, c: [number, number]) => setView({ z, c }),
    [setView],
  );
  const search = (patch: Partial<ViewState>) => setView({ ...patch, t: "", place: "", sort: "oldest" }, true);
  const placeName = (id: string) => {
    const f = features.get(id);
    return f ? `${f.properties.name}, ${f.properties.state}` : id;
  };

  const papers = useMemo(
    () =>
      paperRows(data?.papers, (id) => {
        const f = features.get(id);
        return f ? `${f.properties.name}, ${f.properties.state}` : id;
      }),
    [data?.papers, features],
  );
  // Under a newspaper filter the view shows Pages, not the relative rate (supportedNorm).
  const onlyPaper = (lccn: string) => setView({ lccn: [lccn], t: "", place: "", sort: "oldest" }, true);
  const newspapers =
    data && data.total.hits > 0 && data.papers ? (
      <NewspaperTable
        rows={papers}
        total={data.total.papers ?? papers.length}
        onOnly={onlyPaper}
        filename={`usnewsmap-newspapers-${version}.csv`}
      />
    ) : null;
  const days = data?.total.days;
  const mix = data
    ? [
        days !== undefined && data.total.hits > 0
          ? `Matching pages on ${days.toLocaleString("en-US")} ${days === 1 ? "day" : "days"}.`
          : null,
        languageMix(data.languages, data.total.hits, data.total.papers),
      ]
        .filter(Boolean)
        .join(" ") || null
    : null;
  const filteredPaper = view.lccn.length > 0 ? (papers.find((p) => view.lccn.includes(p.lccn))?.title ?? view.lccn.join(", ")) : null;

  const visible = points.filter((p) => p.value > 0);
  const selected = points.find((p) => p.id === view.place);
  const updating = [agg.error, places.error, coverage.error].some((e) => e instanceof VersionChangedError);
  const problem: Problem | null = updating || !agg.error
    ? null
    : agg.error instanceof ApiError
      ? agg.error.problem
      : {
          type: "about:blank",
          title: "The search failed",
          status: 0,
          detail: "The search service could not be reached or sent an unreadable response.",
          hint: "Check your connection and try again.",
        };
  const placesFailed = places.error && !(places.error instanceof VersionChangedError);
  const coverageFailed =
    view.norm === "skew" && coverage.error && !(coverage.error instanceof VersionChangedError);
  const skewNotice = wantSkew && !skewModel ? skewStatus(prepared, skew.status, !!coverageFailed) : null;
  const shown = norm === "skew" ? skewRows : points;
  const clear = norm === "skew" ? clearest(skewListed) : null;

  // Phones show the legend and the place lists after the playback controls, in the DOM as well as
  // on screen, so the controls sit right under the map. They move, not the controls: the legend and
  // lists keep no state of their own, so a rotation across the breakpoint loses nothing.
  const narrow = useMediaQuery("(max-width: 640px)");
  const legend = !data ? null : norm === "skew" && skewModel ? (
    <SkewLegend
      places={frame ? frame.placePages.filter((p) => p > 0).length : 0}
      states={frame ? frame.statePages.filter((p) => p > 0).length : 0}
      unit={data.bucket.unit}
    />
  ) : norm === "when" ? (
    <WhenLegend
      from={bucketLabel(data.bucket.unit, bucketStart(data.bucket.unit, data.bucket.from, 0))}
      to={bucketLabel(data.bucket.unit, bucketStart(data.bucket.unit, data.bucket.from, Math.max(count - 1, 0)))}
    />
  ) : (
    <Legend />
  );
  const sidePanel = !data ? null : (
    <>
      {norm === "skew" && !view.place && <SkewLists rows={skewListed} onSelect={select} />}
      {view.place && (
        <PlacePanel
          params={params}
          version={version}
          placeId={view.place}
          placeName={selected?.name ?? features.get(view.place)?.properties.name ?? view.place}
          sort={view.sort}
          onSort={(sort) => setView({ sort })}
          windowHits={selected?.value ?? 0}
          note={norm === "skew" ? skewRows.find((r) => r.id === view.place)?.skew : undefined}
          synthetic={data.synthetic}
          onClose={() => setView({ place: "", sort: "oldest" })}
        />
      )}
    </>
  );

  return (
    <div className={view.q ? "app" : "app app--empty"}>
      <header className="topbar">
        <Brand />
        <SearchBar key={searchKey(view)} view={view} meta={meta.data} onSearch={search} />
      </header>
      <About ref={about} />

      {meta.data?.synthetic && (
        <p className="notice notice--demo" role="note">
          Demo data: this server is running on a small synthetic corpus, not real newspapers.
        </p>
      )}
      {meta.error && (
        <p className="notice notice--error" role="alert">
          The search service is unavailable. Please try again shortly.
        </p>
      )}

      {!view.q ? (
        <main className="empty">
          <h1>Where and when did America's newspapers print it?</h1>
          <p>
            Type a word or phrase to map every matching page from Chronicling America, then play it
            through time.
          </p>
          <ul className="examples" id="examples" aria-live="polite">
            {examplesAt(EXAMPLE_ORDER, examplePage).map((ex) => (
              <li key={ex.id}>
                <button type="button" className="example" onClick={() => search({ ...ex.view })}>
                  <strong>{ex.title}</strong>
                  <span>{ex.blurb}</span>
                </button>
              </li>
            ))}
          </ul>
          {EXAMPLE_ORDER.length > EXAMPLES_SHOWN && (
            <p className="examples__more">
              <button
                type="button"
                className="link-button"
                aria-controls="examples"
                onClick={() => setExamplePage((p) => p + 1)}
              >
                Show other examples
              </button>
            </p>
          )}
        </main>
      ) : (
        <main className={agg.isFetching ? "results results--loading" : "results"} aria-busy={agg.isFetching}>
          {updating && (
            <p className="notice" role="status">
              A newer index was just published. Updating to it…
            </p>
          )}
          {stillComputing && (
            <p className="notice" role="status">
              {ahead === null
                ? "Large search, still working… This can take up to two minutes."
                : ahead === 0
                  ? "Other searches are running. Yours is next in line…"
                  : `Other searches are running. Yours is in line behind ${ahead} more…`}
            </p>
          )}
          {placesFailed && (
            <p className="notice notice--error" role="alert">
              Place locations could not be loaded, so results can't be mapped. Please reload the page.
            </p>
          )}
          {problem && (
            <div className="notice notice--error" role="alert">
              <strong>{problem.title}.</strong> {problem.detail}
              {problem.hint && <div>{problem.hint}</div>}
            </div>
          )}
          {data && !places.data && !placesFailed && (
            <p className="notice" role="status">
              Loading places…
            </p>
          )}
          {data && places.data && (
            <>
              <div className="toolbar">
                <p className="summary">
                  <strong>{visible.length.toLocaleString()}</strong> places ·{" "}
                  <strong>{visible.reduce((a, p) => a + p.value, 0).toLocaleString()}</strong> pages
                  {data.coarsened && " · buckets coarsened to fit"}
                </p>
                <div className="segmented" role="group" aria-label="View">
                  {(["map", "table"] as const).map((tab) => (
                    <button
                      key={tab}
                      type="button"
                      aria-pressed={view.tab === tab}
                      onClick={() => setView({ tab })}
                    >
                      {tab === "map" ? "Map" : "Table"}
                    </button>
                  ))}
                </div>
                <MeasureToggle
                  norm={view.norm}
                  onChange={(n) => setView({ norm: n })}
                  unavailable={
                    view.lccn.length > 0
                      ? { skew: "Not for one newspaper: pages published aren't counted per newspaper." }
                      : undefined
                  }
                />
                {/* The relative-rate view draws points only (doc 11, 11.6). */}
                {view.norm === "raw" && (
                  <label>
                    <span className="visually-hidden">Map layer</span>
                    <select value={view.layer} onChange={(e) => setView({ layer: e.target.value as ViewState["layer"] })}>
                      <option value="points">Points</option>
                      <option value="heat">Heat</option>
                    </select>
                  </label>
                )}
                {norm === "skew" && data && (
                  <DownloadCsv
                    rows={skewListed}
                    filename={`usnewsmap-relative-rate-${version}-${bucketStart(data.bucket.unit, data.bucket.from, t)}.csv`}
                  />
                )}
                <ShareButton />
              </div>

              {mix && <p className="mix">{mix}</p>}
              {filteredPaper && (
                <p className="notice" role="status">
                  Only pages from <cite>{filteredPaper}</cite>.{" "}
                  <button
                    type="button"
                    className="link-button"
                    onClick={() => setView({ lccn: [], t: "", place: "", sort: "oldest" }, true)}
                  >
                    Show every newspaper
                  </button>
                </p>
              )}
              {/* Not while the previous search stands in: its pages would get this search's links. */}
              {data.total.hits > 0 && data.total.first && !agg.isPlaceholderData && (
                <Mentions
                  first={data.total.first}
                  last={data.total.last ?? null}
                  placeName={placeName}
                  hrefFor={(hit, sort) => `${window.location.pathname}${serializeView({ ...view, place: hit.place_id, sort })}`}
                  onOpen={(hit, sort) => setView({ place: hit.place_id, sort })}
                />
              )}
              {skewNotice && (
                <p className={skewNotice.error ? "notice notice--error" : "notice"} role={skewNotice.error ? "alert" : "status"}>
                  {skewNotice.text}
                </p>
              )}
              {data.total.hits === 0 ? (
                <div className="notice" role="status">
                  <strong>No pages match.</strong> Try “All words” instead of an exact phrase, widen the
                  dates, or remove {view.lang.length > 0 ? "language or state filters" : "state filters"}.
                </div>
              ) : (
                <div className="stage">
                  {view.tab === "table" || !webgl ? (
                    <>
                      {!webgl && view.tab === "map" && (
                        <p className="notice" role="note">
                          The map needs WebGL2, which this browser doesn't provide. Showing the table instead.
                        </p>
                      )}
                      {norm === "skew" ? (
                        <div className="tables">
                          <PlaceTable key="skew" skew rows={skewListed} onSelect={select} selected={view.place} />
                          <StateTable rows={stateRows} />
                          {newspapers}
                        </div>
                      ) : (
                        <div className="tables">
                          <PlaceTable
                            // A new sort when the share column comes or goes, so it never sorts by a hidden column.
                            key={data.cube.baseline_ref !== null ? "raw" : "raw-no-share"}
                            rows={visible}
                            onSelect={select}
                            selected={view.place}
                            share={data.cube.baseline_ref !== null}
                          />
                          {newspapers}
                        </div>
                      )}
                    </>
                  ) : (
                    <Suspense fallback={<div className="map map--loading">Loading map…</div>}>
                      <MapView
                        points={shown}
                        layer={view.norm === "raw" ? view.layer : "points"}
                        norm={norm}
                        maxValue={norm === "skew" ? maxExpected : maxValue}
                        selected={view.place}
                        onSelect={select}
                        zoom={view.z}
                        center={view.c}
                        onViewport={onViewport}
                      />
                      {!narrow && legend}
                    </Suspense>
                  )}
                  {!narrow && sidePanel}
                </div>
              )}

              {count > 0 && data.total.hits > 0 && (
                <Announcer
                  message={
                    clear
                      ? `Relative rate for ${skewListed.length.toLocaleString()} places ${
                          view.win === null ? "up to" : "in the window ending"
                        } ${bucketLabel(data.bucket.unit, bucketStart(data.bucket.unit, data.bucket.from, t))}: ${skewListed.filter((r) => r.skew.dir === 1).length} clearly above 1×, ${
                          skewListed.filter((r) => r.skew.dir === -1).length
                        } clearly below.`
                      : `Showing ${visible.length.toLocaleString()} places, ${visible
                          .reduce((a, p) => a + p.value, 0)
                          .toLocaleString()} pages, up to ${bucketLabel(
                          data.bucket.unit,
                          bucketStart(data.bucket.unit, data.bucket.from, t),
                        )}.`
                  }
                />
              )}
              {count > 0 && data.total.hits > 0 && (
                <footer className="timebar">
                  <TimeDock
                    unit={data.bucket.unit}
                    from={data.bucket.from}
                    count={count}
                    t={t}
                    window={view.win}
                    onSeek={seek}
                    onWindow={(win) => setView({ win })}
                  />
                  <Timeline
                    unit={data.bucket.unit}
                    from={data.bucket.from}
                    hits={data.series.hits}
                    t={t}
                    window={view.win}
                    onSeek={seek}
                  />
                </footer>
              )}
              {narrow && data && (
                <div className="after-timebar">
                  {view.tab === "map" && webgl && legend}
                  {sidePanel}
                </div>
              )}
            </>
          )}
          {!data && agg.isFetching && !stillComputing && <p className="notice" role="status">Searching…</p>}
        </main>
      )}
      <footer className="credits">
        Newspaper pages from{" "}
        <a href="https://chroniclingamerica.loc.gov/" target="_blank" rel="noopener noreferrer">
          Chronicling America
        </a>{" "}
        (
        <a href="https://www.loc.gov/ndnp/" target="_blank" rel="noopener noreferrer">
          NEH and Library of Congress
        </a>
        ).
        {meta.data && ` ${indexSummary(meta.data)}`}{" "}
        <button type="button" className="link-button" aria-haspopup="dialog" onClick={openAbout}>
          About
        </button>{" "}
        · <a href="/status">Pipeline status</a> · <a href="/privacy">Privacy</a>
      </footer>
    </div>
  );
}

/**
 * The one live region for results (07 §7.6): the date and counts together,
 * announced at most every 1.5 s, so playback doesn't flood a screen reader.
 */
function Announcer({ message }: { message: string }) {
  const [spoken, setSpoken] = useState("");
  const last = useRef(0);
  useEffect(() => {
    const wait = Math.max(0, last.current + 1500 - Date.now());
    const timer = setTimeout(() => {
      last.current = Date.now();
      setSpoken(message);
    }, wait);
    return () => clearTimeout(timer);
  }, [message]);
  return (
    <p className="visually-hidden" role="status" aria-live="polite">
      {spoken}
    </p>
  );
}

function Legend() {
  return (
    <div className="legend" aria-hidden="true">
      <div className="legend__title">Pages containing the match</div>
      <div className="legend__ramp">
        {[0, 0.25, 0.5, 0.75, 1].map((x) => (
          <span key={x} style={{ background: cssColor(x) }} />
        ))}
      </div>
      <div className="legend__ends">
        <span>fewer</span>
        <span>more</span>
      </div>
      <div className="legend__note">Circle area ∝ pages · hollow ring = county/state location</div>
    </div>
  );
}

/** The median-date view's legend (#127): the search's first and last buckets. */
function WhenLegend({ from, to }: { from: string; to: string }) {
  return (
    <div className="legend" aria-hidden="true">
      <div className="legend__title">Median date of the matching pages</div>
      <div className="legend__ramp">
        {[0, 0.25, 0.5, 0.75, 1].map((x) => (
          <span key={x} style={{ background: cssTimeColor(x) }} />
        ))}
      </div>
      <div className="legend__ends">
        <span>{from}</span>
        <span>{to}</span>
      </div>
      <div className="legend__note">
        A place&apos;s colour is the period by whose end half its pages had appeared · circle area ∝ pages
      </div>
    </div>
  );
}

function ShareButton() {
  return (
    <button
      type="button"
      className="button"
      onClick={(e) => {
        const btn = e.currentTarget;
        void navigator.clipboard?.writeText(window.location.href).then(
          () => {
            btn.textContent = "Link copied";
            setTimeout(() => (btn.textContent = "Share"), 2000);
          },
          () => undefined,
        );
      }}
    >
      Share
    </button>
  );
}

/** Why the relative rate isn't showing yet, or can't be shown. */
function skewStatus(
  prepared: Prepared | Unavailable | null,
  status: "idle" | "computing" | "ready" | "error",
  coverageFailed: boolean,
): { text: string; error: boolean } {
  if (coverageFailed)
    return {
      text: "Publication counts could not be loaded, so the relative rate isn't available. Showing page counts.",
      error: true,
    };
  if (prepared === "filters")
    return {
      text: "The relative rate isn't available with a language filter on this version of the index. Showing page counts.",
      error: false,
    };
  if (prepared === "few-places")
    return {
      text: "The relative rate needs at least 5 places with pages between these dates. Showing page counts.",
      error: false,
    };
  if (status === "error")
    return { text: "The relative rate could not be worked out. Showing page counts.", error: true };
  if (prepared === "mismatch")
    return {
      text: "Publication counts don't match this search's time buckets, so the relative rate isn't available. Showing page counts.",
      error: true,
    };
  if (prepared === null) return { text: "Loading publication counts…", error: false };
  return { text: "Comparing places…", error: false };
}

