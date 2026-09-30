import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { api, ApiError, VersionChangedError, type SearchParams } from "./api/client";
import type { Problem } from "./api/types";
import { indexSummary } from "./lib/indexSummary";
import { alignCube, prefixSums, relative, windowValues } from "./engine/cube";
import { EXAMPLES } from "./examples";
import { bucketIndex, bucketLabel, bucketStart } from "./lib/time";
import { cssColor } from "./lib/scale";
import { searchParams, useView, type ViewState } from "./state/url";
import { SearchBar, searchKey } from "./components/SearchBar";
import { Timeline } from "./components/Timeline";
import { TimeDock } from "./components/TimeDock";
import { PlacePanel } from "./components/PlacePanel";
import { PlaceTable } from "./components/PlaceTable";
import { About } from "./components/About";
import type { MapPoint } from "./components/mapTypes";
import { hasWebGL2 } from "./lib/webgl";

const MapView = lazy(() => import("./components/MapView"));
const webgl = typeof document !== "undefined" && hasWebGL2();

export function App() {
  const [view, setView] = useView();
  const about = useRef<HTMLDialogElement>(null);
  const openAbout = () => about.current?.showModal();
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

  const agg = useQuery({
    queryKey: ["aggregate", version, params],
    queryFn: ({ signal }) => api.aggregate(params, version, signal),
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

  // Relative frequency needs the coverage cube; until it arrives (or if it
  // fails) it is unknown (NaN), never 0.
  const points: (MapPoint & { firstDay: number })[] = useMemo(() => {
    if (!data || !hitSums) return [];
    const hits = windowValues(hitSums, t, view.win);
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
        },
      ];
    });
  }, [data, hitSums, pageSums, features, t, view.win]);

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
  const maxRel = Math.max(1e-9, ...points.map((p) => (Number.isFinite(p.rel) ? p.rel : 0)));
  const relReady = pageSums !== null;
  // Colour by raw counts until relative values are known.
  const norm = view.norm === "rel" && !relReady ? "raw" : view.norm;

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
  const search = (patch: Partial<ViewState>) => setView({ ...patch, t: "", place: "" }, true);

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
  const coverageFailed = view.norm === "rel" && coverage.error && !(coverage.error instanceof VersionChangedError);

  return (
    <div className={view.q ? "app" : "app app--empty"}>
      <header className="topbar">
        <a className="brand" href="/" aria-label="US News Map home">
          <span aria-hidden="true">◉</span> US News Map
        </a>
        <SearchBar key={searchKey(view)} view={view} meta={meta.data} onSearch={search} />
        <button type="button" className="link-button topbar__about" aria-haspopup="dialog" onClick={openAbout}>
          About
        </button>
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
          <ul className="examples">
            {EXAMPLES.map((ex) => (
              <li key={ex.id}>
                <button type="button" className="example" onClick={() => search({ ...ex.view })}>
                  <strong>{ex.title}</strong>
                  <span>{ex.blurb}</span>
                </button>
              </li>
            ))}
          </ul>
        </main>
      ) : (
        <main className={agg.isFetching ? "results results--loading" : "results"} aria-busy={agg.isFetching}>
          {updating && (
            <p className="notice" role="status">
              A newer index was just published. Updating to it…
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
                <label>
                  <span className="visually-hidden">Map layer</span>
                  <select value={view.layer} onChange={(e) => setView({ layer: e.target.value as ViewState["layer"] })}>
                    <option value="points">Points</option>
                    <option value="heat">Heat</option>
                  </select>
                </label>
                <label>
                  <span className="visually-hidden">Measure</span>
                  <select value={view.norm} onChange={(e) => setView({ norm: e.target.value as ViewState["norm"] })}>
                    <option value="raw">Pages</option>
                    <option value="rel">Share of pages published</option>
                  </select>
                </label>
                <ShareButton />
              </div>

              {view.norm === "rel" && !relReady && (
                <p className={coverageFailed ? "notice notice--error" : "notice"} role={coverageFailed ? "alert" : "status"}>
                  {coverageFailed
                    ? "Publication counts could not be loaded, so shares of pages published aren't available. Showing page counts."
                    : "Loading publication counts…"}
                </p>
              )}
              {data.total.hits === 0 ? (
                <div className="notice" role="status">
                  <strong>No pages match.</strong> Try “All words” instead of an exact phrase, widen the
                  dates, or remove state filters.
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
                      <PlaceTable rows={visible} onSelect={select} selected={view.place} />
                    </>
                  ) : (
                    <Suspense fallback={<div className="map map--loading">Loading map…</div>}>
                      <MapView
                        points={points}
                        layer={view.layer}
                        norm={norm}
                        maxValue={maxValue}
                        maxRel={maxRel}
                        selected={view.place}
                        onSelect={select}
                        zoom={view.z}
                        center={view.c}
                        onViewport={onViewport}
                      />
                      <Legend norm={norm} />
                    </Suspense>
                  )}
                  {view.place && (
                    <PlacePanel
                      params={params}
                      version={version}
                      placeId={view.place}
                      placeName={selected?.name ?? features.get(view.place)?.properties.name ?? view.place}
                      windowHits={selected?.value ?? 0}
                      synthetic={data.synthetic}
                      onClose={() => setView({ place: "" })}
                    />
                  )}
                </div>
              )}

              {count > 0 && data.total.hits > 0 && (
                <Announcer
                  message={`Showing ${visible.length.toLocaleString()} places, ${visible
                    .reduce((a, p) => a + p.value, 0)
                    .toLocaleString()} pages, up to ${bucketLabel(
                    data.bucket.unit,
                    bucketStart(data.bucket.unit, data.bucket.from, t),
                  )}.`}
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
                    baseline={data.series.baseline}
                    norm={view.norm}
                    t={t}
                    window={view.win}
                    onSeek={seek}
                  />
                </footer>
              )}
            </>
          )}
          {!data && agg.isFetching && <p className="notice" role="status">Searching…</p>}
        </main>
      )}
      <footer className="credits">
        Newspaper pages from{" "}
        <a href="https://chroniclingamerica.loc.gov/" target="_blank" rel="noopener noreferrer">
          Chronicling America
        </a>{" "}
        (
        <a href="https://www.loc.gov/" target="_blank" rel="noopener noreferrer">
          Library of Congress
        </a>{" "}
        and{" "}
        <a href="https://www.neh.gov/" target="_blank" rel="noopener noreferrer">
          National Endowment for the Humanities
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

function Legend({ norm }: { norm: ViewState["norm"] }) {
  return (
    <div className="legend" aria-hidden="true">
      <div className="legend__title">{norm === "rel" ? "Share of pages published" : "Pages containing the match"}</div>
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
