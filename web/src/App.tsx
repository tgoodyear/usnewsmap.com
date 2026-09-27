import { lazy, Suspense, useCallback, useMemo } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { api, ApiError, type SearchParams } from "./api/client";
import { alignCube, prefixSums, relative, windowValues } from "./engine/cube";
import { EXAMPLES } from "./examples";
import { bucketIndex, bucketStart } from "./lib/time";
import { cssColor } from "./lib/scale";
import { useView, type ViewState } from "./state/url";
import { SearchBar, searchKey } from "./components/SearchBar";
import { Timeline } from "./components/Timeline";
import { TimeDock } from "./components/TimeDock";
import { PlacePanel } from "./components/PlacePanel";
import { PlaceTable } from "./components/PlaceTable";
import type { MapPoint } from "./components/mapTypes";
import { hasWebGL2 } from "./lib/webgl";

const MapView = lazy(() => import("./components/MapView"));
const webgl = typeof document !== "undefined" && hasWebGL2();

export function App() {
  const [view, setView] = useView();
  const meta = useQuery({ queryKey: ["meta"], queryFn: ({ signal }) => api.meta(signal) });
  const version = meta.data?.index_version ?? "";
  const places = useQuery({
    queryKey: ["places", version],
    queryFn: ({ signal }) => api.places(version, signal),
    enabled: !!version,
    staleTime: Infinity,
  });

  // Parsed afresh on every URL change; a stable key keeps queries cached.
  const stateList = view.state.join(",");
  const params: SearchParams = useMemo(
    () => ({
      q: view.q,
      mode: view.mode,
      near: view.near,
      from: view.from,
      to: view.to,
      bucket: view.bucket,
      state: stateList ? stateList.split(",") : [],
    }),
    [view.q, view.mode, view.near, view.from, view.to, view.bucket, stateList],
  );

  const agg = useQuery({
    queryKey: ["aggregate", version, params],
    queryFn: ({ signal }) => api.aggregate(params, version, signal),
    enabled: !!version && !!view.q,
    placeholderData: keepPreviousData,
  });
  const baselineRef = agg.data?.cube.baseline_ref ?? null;
  const coverage = useQuery({
    queryKey: ["coverage", baselineRef],
    queryFn: ({ signal }) => api.coverage(baselineRef!, signal),
    enabled: !!baselineRef,
    staleTime: Infinity,
  });

  const data = agg.data;
  const count = data?.bucket.count ?? 0;
  const t = data ? (view.t ? bucketIndex(data.bucket.unit, data.bucket.from, count, view.t) : count - 1) : 0;

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

  const points: (MapPoint & { firstDay: number })[] = useMemo(() => {
    if (!data || !hitSums) return [];
    const hits = windowValues(hitSums, t, view.win);
    const rel = pageSums ? relative(hits, windowValues(pageSums, t, view.win)) : new Float64Array(hits.length);
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
  const maxRel = Math.max(1e-9, ...points.map((p) => p.rel));

  const seek = useCallback(
    (i: number) => {
      if (!data) return;
      setView({ t: bucketStart(data.bucket.unit, data.bucket.from, i) });
    },
    [data, setView],
  );
  const select = useCallback((id: string) => setView({ place: id }), [setView]);
  const onViewport = useCallback(
    (z: number, c: [number, number]) => setView({ z, c }),
    [setView],
  );
  const search = (patch: Partial<ViewState>) => setView({ ...patch, t: "", place: "" }, true);

  const visible = points.filter((p) => p.value > 0);
  const selected = points.find((p) => p.id === view.place);
  const problem = agg.error instanceof ApiError ? agg.error.problem : null;

  return (
    <div className={view.q ? "app" : "app app--empty"}>
      <header className="topbar">
        <a className="brand" href="/" aria-label="US News Map home">
          <span aria-hidden="true">◉</span> US News Map
        </a>
        <SearchBar key={searchKey(view)} view={view} meta={meta.data} onSearch={search} />
      </header>

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
          {problem && (
            <div className="notice notice--error" role="alert">
              <strong>{problem.title}.</strong> {problem.detail}
              {problem.hint && <div>{problem.hint}</div>}
            </div>
          )}
          {data && (
            <>
              <div className="toolbar">
                <p className="summary" aria-live="polite">
                  <strong>{visible.length.toLocaleString()}</strong> places ·{" "}
                  <strong>{visible.reduce((a, p) => a + p.value, 0).toLocaleString()}</strong> pages
                  {data.coarsened && " · buckets coarsened to fit"}
                </p>
                <div className="segmented" role="tablist" aria-label="View">
                  {(["map", "table"] as const).map((tab) => (
                    <button
                      key={tab}
                      type="button"
                      role="tab"
                      aria-selected={view.tab === tab}
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
                        norm={view.norm}
                        maxValue={maxValue}
                        maxRel={maxRel}
                        selected={view.place}
                        onSelect={select}
                        zoom={view.z}
                        center={view.c}
                        onViewport={onViewport}
                      />
                      <Legend norm={view.norm} />
                    </Suspense>
                  )}
                  {view.place && (
                    <PlacePanel
                      params={params}
                      version={version}
                      placeId={view.place}
                      placeName={selected?.name ?? features.get(view.place)?.properties.name ?? view.place}
                      windowHits={selected?.value ?? 0}
                      onClose={() => setView({ place: "" })}
                    />
                  )}
                </div>
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
        (Library of Congress and National Endowment for the Humanities).
        {meta.data && ` Index ${meta.data.index_version}.`}
      </footer>
    </div>
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
