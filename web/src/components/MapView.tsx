// Map (07 §7.4): MapLibre basemap/States with a deck.gl points/heat overlay. Loaded lazily so
// the map stack stays off the critical path.

import { useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { Feature, FeatureCollection, Geometry } from "geojson";
import { Map as MapLibreMap, NavigationControl, setWorkerUrl, type StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
// MapLibre finds its worker relative to its own module at runtime, which a
// bundler can't follow; bundle the worker explicitly and hand it the URL.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import { MapboxOverlay } from "@deck.gl/mapbox";
import { ScatterplotLayer } from "@deck.gl/layers";
import { HeatmapLayer } from "@deck.gl/aggregation-layers";
import type { Layer, Norm } from "../state/url";
import { colorFor, timeColor } from "../lib/scale";
import { ALPHA_CLEAR, ALPHA_UNCLEAR, skewColor } from "../lib/skewScale";
import { skewSentence } from "../lib/skewText";
import type { MapPoint } from "./mapTypes";
import { DEFAULT_ZOOM, STYLE_URL, US_CENTER } from "../lib/basemap";
import { installBasemapProtocol } from "../lib/basemapProtocol";
import { MAX_ZOOM, MIN_ZOOM } from "../lib/mapLimits";
import { stateDescription, type StateTotal } from "../engine/states";
import boundariesUrl from "../assets/states.geojson?url";
import { StatesMapLayer } from "./StatesMapLayer";

type StateFeature = Feature<Geometry, { STUSPS: string; NAME: string }>;

interface Props {
  points: MapPoint[];
  layer: Layer;
  norm: Norm;
  states?: StateTotal[];
  stateBreaks?: number[];
  onStateFilter?: (code: string) => void;
  /** The largest circle's value: pages with a match, or expected matches in the relative-rate view. */
  maxValue: number;
  selected: string;
  onSelect: (id: string) => void;
  zoom: number | null;
  center: [number, number] | null;
  onViewport: (zoom: number, center: [number, number]) => void;
}

setWorkerUrl(workerUrl);

const MAX_RADIUS_PX = 26;
/** Extra slop around a circle for hover and click, in pixels. */
const HIT_SLOP_PX = 3;

/** Circle radius in pixels: area ∝ pages, so radius ∝ √pages; 0 hides a place. */
function radiusOf(value: number, maxValue: number): number {
  return value > 0 ? Math.max(3, MAX_RADIUS_PX * Math.sqrt(value / Math.max(maxValue, 1))) : 0;
}

/** Smallest circle in the relative-rate view, so a place with pages but almost nothing expected is still drawn. */
const MIN_SKEW_RADIUS_PX = 4;

/**
 * Relative-rate view (doc 11, 11.6): area ∝ matches expected, the evidence
 * behind the colour; every place with pages in the window is drawn.
 */
function skewRadius(p: MapPoint, maxExpected: number): number {
  const s = p.skew;
  if (!s || s.pages <= 0) return 0;
  return Math.max(MIN_SKEW_RADIUS_PX, MAX_RADIUS_PX * Math.sqrt(s.expected / Math.max(maxExpected, 1e-9)));
}

/** Pages: darker for more. Median date (#127): early to late on the time palette. */
function pointColor(p: MapPoint, norm: Norm, maxValue: number): [number, number, number, number] {
  return norm === "when" ? timeColor(p.when ?? Number.NaN) : colorFor(p.value / Math.max(maxValue, 1));
}

function radiusFor(p: MapPoint, norm: Norm, maxValue: number): number {
  return norm === "skew" ? skewRadius(p, maxValue) : radiusOf(p.value, maxValue);
}

/** Faded when the 90% range includes 1. */
function skewAlpha(p: MapPoint): number {
  return p.skew && p.skew.dir !== 0 ? ALPHA_CLEAR : ALPHA_UNCLEAR;
}

function skewFill(p: MapPoint): [number, number, number, number] {
  return skewColor(p.skew?.estimate ?? 1, skewAlpha(p));
}

// `none` gives a plain background: offline development and tests.
const PLAIN_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#dfe4ea" } }],
};

export default function MapView(props: Props) {
  const boundaries = useQuery({
    queryKey: ["state-boundaries"],
    queryFn: async ({ signal }) => {
      const response = await fetch(boundariesUrl, { signal });
      if (!response.ok) throw new Error("State boundaries could not be loaded");
      const data = (await response.json()) as FeatureCollection<Geometry, StateFeature["properties"]>;
      performance.mark("usnm-states-parsed");
      return data;
    },
    enabled: props.layer === "states" && props.norm === "raw",
    staleTime: Infinity,
    retry: false,
  });
  const [stateTip, setStateTip] = useState<{ code: string; x: number; y: number; pinned: boolean } | null>(null);
  useEffect(() => {
    if (!stateTip) return;
    const dismiss = (event: KeyboardEvent) => {
      if (event.key === "Escape") setStateTip(null);
    };
    document.addEventListener("keydown", dismiss);
    return () => document.removeEventListener("keydown", dismiss);
  }, [stateTip]);
  const paintMeasured = useRef(false);
  const [styleReady, setStyleReady] = useState(false);
  const nativeStates = useRef<StatesMapLayer | null>(null);
  const container = useRef<HTMLDivElement>(null);
  const map = useRef<MapLibreMap | null>(null);
  const overlay = useRef<MapboxOverlay | null>(null);
  // Set while the map moves to match the URL, so that move isn't written back.
  const syncing = useRef(false);
  const tooltip = useRef<HTMLDivElement>(null);
  // What the hit test sees: the drawn points and their scale.
  const drawn = useRef<{ points: MapPoint[]; maxValue: number; layer: Layer; norm: Norm }>({
    points: [],
    maxValue: 1,
    layer: "points",
    norm: "raw",
  });
  // The map is created once; its handlers read the latest viewport props.
  const latest = useRef(props);
  useEffect(() => {
    latest.current = props;
  });

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    installBasemapProtocol();
    const m = new MapLibreMap({
      container: element,
      ...(STYLE_URL === "none" ? { style: PLAIN_STYLE } : {}),
      center: latest.current.center ?? US_CENTER,
      zoom: latest.current.zoom ?? DEFAULT_ZOOM,
      minZoom: MIN_ZOOM,
      maxZoom: MAX_ZOOM,
      attributionControl: { compact: true },
      dragRotate: false,
      pitchWithRotate: false,
    });
    m.touchZoomRotate.disableRotation();
    m.addControl(new NavigationControl({ showCompass: false }), "top-left");
    const capturing = "__USNM_SHARE_CAPTURE__" in window;
    let fallbackStyleUsed = STYLE_URL === "none";
    element.dataset.basemapError = "false";
    const recoverStyle = () => {
      // getStyle is undefined until the initial document is parsed. A failed
      // tile or font must not replace a usable style or remove native fills.
      if (fallbackStyleUsed || m.getStyle()) return;
      fallbackStyleUsed = true;
      element.dataset.basemapError = "true";
      element.dataset.basemapReady = "false";
      m.setStyle(PLAIN_STYLE, { diff: false });
    };
    m.on("error", () => {
      if (capturing) {
        element.dataset.basemapError = "true";
        element.dataset.basemapReady = "false";
      }
      recoverStyle();
    });
    m.on("style.load", () => setStyleReady(true));
    if (STYLE_URL !== "none") {
      m.setStyle(STYLE_URL, {
        transformStyle: (_previous, next) => ({
          ...next,
          // MapLibre requires absolute sprite URLs, including same-origin assets.
          ...(typeof next.sprite === "string"
            ? { sprite: new URL(next.sprite, new URL(STYLE_URL, window.location.href)).href }
            : {}),
        }),
      });
    }
    if (m.isStyleLoaded()) setStyleReady(true);

    let ready = false;
    let loaded = false;
    const reveal = () => {
      if (ready) return;
      ready = true;
      element.classList.add("map--basemap-ready");
      clearTimeout(fallback);
    };
    const fallback = setTimeout(() => {
      // Captures have a separate bounded deadline; do not cancel a slow style
      // request and permanently cache a screenshot of the offline background.
      if (!capturing) recoverStyle();
      reveal();
    }, 2000);
    m.on("load", () => {
      loaded = true;
    });
    m.on("idle", () => {
      if (loaded && m.areTilesLoaded()) reveal();
      element.dataset.basemapReady = String(
        loaded &&
          m.areTilesLoaded() &&
          element.dataset.basemapError !== "true" &&
          (STYLE_URL === "none" || !fallbackStyleUsed),
      );
    });
    m.on("movestart", () => {
      element.dataset.basemapReady = "false";
    });
    m.on("dataloading", () => {
      element.dataset.basemapReady = "false";
    });
    m.on("render", () => {
      if (latest.current.layer === "states" && latest.current.norm === "raw") {
        element.dataset.overlayReady = String(nativeStates.current?.isReady() ?? false);
      }
    });
    if (STYLE_URL === "none") reveal();

    // Hover and click are hit-tested on the CPU against the projected
    // circles. GPU picking would read pixels back on every mouse move,
    // stalling the pipeline; a few thousand projections cost far less.
    const hit = (x: number, y: number): MapPoint | null => {
      if (!ready) return null;
      const { points, maxValue, layer, norm } = drawn.current;
      if (layer !== "points") return null;
      let best: MapPoint | null = null;
      let bestD = Infinity;
      for (const p of points) {
        const radius = radiusFor(p, norm, maxValue);
        if (radius <= 0) continue;
        const s = m.project(p.position);
        const r = radius + HIT_SLOP_PX;
        const d = (s.x - x) ** 2 + (s.y - y) ** 2;
        // Prefer the nearest centre among circles under the pointer.
        if (d <= r * r && d < bestD) {
          best = p;
          bestD = d;
        }
      }
      return best;
    };
    let frame = 0;
    m.on("mousemove", (e) => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const p = hit(e.point.x, e.point.y);
        m.getCanvas().style.cursor = p ? "pointer" : "";
        const tip = tooltip.current;
        if (!tip) return;
        if (!p) {
          tip.hidden = true;
          return;
        }
        const skew = drawn.current.norm === "skew" ? p.skew : undefined;
        const when = drawn.current.norm === "when" && p.whenLabel ? ` · median ${p.whenLabel}` : "";
        tip.textContent = skew
          ? skewSentence(`${p.name}, ${p.state}`, skew)
          : `${p.name}, ${p.state} · ${p.value.toLocaleString()} pages${when}`;
        tip.classList.toggle("map-tooltip--wrap", skew !== undefined);
        tip.style.transform = `translate(${e.point.x + 12}px, ${e.point.y + 12}px)`;
        tip.hidden = false;
      });
    });
    m.on("mouseout", () => {
      cancelAnimationFrame(frame);
      if (tooltip.current) tooltip.current.hidden = true;
    });
    m.on("click", (e) => {
      const p = hit(e.point.x, e.point.y);
      if (p) latest.current.onSelect(p.id);
    });
    m.on("moveend", () => {
      if (syncing.current) {
        syncing.current = false;
        return;
      }
      const c = m.getCenter();
      latest.current.onViewport(m.getZoom(), [c.lng, c.lat]);
    });
    map.current = m;
    return () => {
      nativeStates.current?.destroy();
      nativeStates.current = null;
      clearTimeout(fallback);
      element.classList.remove("map--basemap-ready");
      cancelAnimationFrame(frame);
      m.remove();
      map.current = null;
      overlay.current = null;
    };
  }, []);

  // Back/forward or a pasted permalink changes the URL viewport while the map
  // stays mounted: move the map to match. Differences below the precision the
  // URL stores are the map's own last move echoing back, so they're ignored.
  // Keyed on the values, not the array: the URL is re-parsed on every render
  // (each playback frame), and a jump mid-gesture would cancel a pinch zoom.
  const zoom = props.zoom ?? DEFAULT_ZOOM;
  const [lng, lat] = props.center ?? US_CENTER;
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    const c = m.getCenter();
    if (Math.abs(m.getZoom() - zoom) < 0.01 && Math.abs(c.lng - lng) < 0.001 && Math.abs(c.lat - lat) < 0.001) {
      return;
    }
    syncing.current = true;
    m.jumpTo({ center: [lng, lat], zoom });
  }, [zoom, lng, lat]);

  const { points, layer, norm, maxValue, selected, states, stateBreaks } = props;
  const hasStates = !!states?.length;
  useEffect(() => {
    const m = map.current;
    if (!m || !styleReady || layer !== "states" || norm !== "raw" || !boundaries.data || !hasStates) return;
    const native = new StatesMapLayer(
      m,
      boundaries.data,
      (code, x, y) => {
        if (window.matchMedia("(pointer: coarse)").matches) return;
        const left = Math.min(x + 12, Math.max(0, (container.current?.clientWidth ?? 320) - 280));
        setStateTip((old) => (old?.pinned ? old : code ? { code, x: left, y, pinned: false } : null));
      },
      (code, x, y) => {
        if (window.matchMedia("(pointer: coarse)").matches) setStateTip({ code, x, y, pinned: true });
        else latest.current.onStateFilter?.(code);
      },
    );
    nativeStates.current = native;
    let frame = 0;
    let measuring = false;
    const draw = () => {
      if (paintMeasured.current || measuring || !native.hasDrawn()) return;
      const asset = performance.getEntriesByType("resource").find((entry) => entry.name.endsWith(boundariesUrl)) as
        PerformanceResourceTiming | undefined;
      if (!asset) return;
      measuring = true;
      performance.measure("usnm-states-first-draw", { start: asset.responseEnd, end: performance.now() });
      const measurePaint = () => {
        const canvas = m.getCanvas();
        const style = getComputedStyle(canvas);
        if (
          Number(style.opacity) === 0 ||
          style.visibility === "hidden" ||
          canvas.getBoundingClientRect().width === 0
        ) {
          frame = requestAnimationFrame(measurePaint);
          return;
        }
        paintMeasured.current = true;
        performance.measure("usnm-states-first-paint", {
          start: asset.responseEnd,
          end: performance.now(),
          detail: "First rendered native state features plus two animation frames; presentation proxy",
        });
      };
      frame = requestAnimationFrame(() => {
        frame = requestAnimationFrame(measurePaint);
      });
    };
    m.on("render", draw);
    return () => {
      cancelAnimationFrame(frame);
      m.off("render", draw);
      nativeStates.current = null;
      native.destroy();
    };
  }, [styleReady, layer, norm, boundaries.data, hasStates]);
  useEffect(() => {
    nativeStates.current?.update(states ?? [], stateBreaks ?? []);
    if (container.current && layer === "states" && norm === "raw") container.current.dataset.overlayReady = "false";
  }, [styleReady, layer, norm, boundaries.data, hasStates, states, stateBreaks]);
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    if (container.current) container.current.dataset.overlayReady = "false";
    drawn.current = { points, maxValue, layer, norm };
    if (tooltip.current) tooltip.current.hidden = true;
    let o = overlay.current;
    if (!o) {
      // Direct States entry needs only MapLibre. Don't initialize a second
      // WebGL device and compile unused deck pipelines until another layer needs it.
      if (layer === "states" && norm === "raw") return;
      // Overlaid, not interleaved: deck.gl 9.4's interleaved mode reads
      // MapLibre internals that changed in MapLibre 6.
      o = new MapboxOverlay({
        id: "search-overlay",
        interleaved: false,
        layers: [],
        onAfterRender: () => {
          if (container.current && !(latest.current.layer === "states" && latest.current.norm === "raw"))
            container.current.dataset.overlayReady = "true";
        },
      });
      m.addControl(o);
      overlay.current = o;
    }
    // Every place in the search is always drawn; places with no pages yet
    // have zero radius. A fixed-size dataset lets deck.gl update attribute
    // values in place on each playback step instead of rebuilding buffers.
    const layers =
      layer === "states" && norm === "raw"
        ? []
        : norm === "skew"
          ? [
              new ScatterplotLayer<MapPoint>({
                id: "skew",
                data: points,
                stroked: true,
                radiusUnits: "pixels",
                lineWidthUnits: "pixels",
                getRadius: (p) => skewRadius(p, maxValue),
                // Hollow rings (county or state precision) carry the colour on the ring.
                getFillColor: (p) => (p.precision === "city" ? skewFill(p) : [0, 0, 0, 0]),
                // A hollow ring keeps its colour when selected (the colour is all it shows); the wider
                // line marks the selection. A filled city circle gets a dark outline instead.
                getLineColor: (p) =>
                  p.precision !== "city"
                    ? skewFill(p)
                    : p.id === selected
                      ? [20, 20, 20, 255]
                      : [40, 40, 40, skewAlpha(p) === ALPHA_CLEAR ? 170 : 70],
                getLineWidth: (p) =>
                  !p.skew || p.skew.pages <= 0
                    ? 0
                    : p.precision === "city"
                      ? p.id === selected
                        ? 3
                        : 1
                      : p.id === selected
                        ? 4.5
                        : 2.5,
                updateTriggers: {
                  getRadius: [maxValue, points],
                  getFillColor: [points],
                  getLineColor: [points, selected],
                  getLineWidth: [selected, points],
                },
              }),
            ]
          : layer === "heat"
            ? [
                new HeatmapLayer<MapPoint>({
                  id: "heat",
                  data: points,
                  getPosition: (p) => p.position,
                  getWeight: (p) => p.value,
                  radiusPixels: 40,
                }),
              ]
            : [
                new ScatterplotLayer<MapPoint>({
                  id: "points",
                  data: points,
                  stroked: true,
                  radiusUnits: "pixels",
                  lineWidthUnits: "pixels",
                  // Area ∝ hits, so radius ∝ √hits (perceptually honest).
                  getRadius: (p) => radiusOf(p.value, maxValue),
                  getFillColor: (p) => (p.precision === "city" ? pointColor(p, norm, maxValue) : [0, 0, 0, 0]),
                  getLineColor: (p) => (p.id === selected ? [20, 20, 20, 255] : pointColor(p, norm, maxValue)),
                  getLineWidth: (p) => (p.value <= 0 ? 0 : p.id === selected ? 3 : p.precision === "city" ? 1 : 2.5),
                  updateTriggers: {
                    getRadius: [maxValue],
                    getFillColor: [maxValue, norm, points],
                    getLineColor: [maxValue, selected, norm, points],
                    getLineWidth: [selected, points],
                  },
                }),
              ];
    // No attribute transitions: deck.gl runs them on the GPU with transform
    // feedback and reads buffers back, which stalls every playback step.
    o.setProps({ layers });
  }, [points, layer, norm, maxValue, selected]);

  const tipRow =
    layer === "states" && norm === "raw" && stateTip ? states?.find((r) => r.code === stateTip.code) : undefined;

  return (
    <div className="map-wrap">
      <div ref={container} className="map map--gated" data-testid="map" />
      <div ref={tooltip} className="map-tooltip" role="tooltip" hidden />
      {layer === "states" && boundaries.isError && (
        <div className="map-state-error" role="alert">
          State boundaries could not be loaded.{" "}
          <button type="button" className="button button--quiet" onClick={() => void boundaries.refetch()}>
            Retry boundaries
          </button>
          . The states table remains available.
        </div>
      )}
      {layer === "states" && boundaries.isPending && (
        <p className="map-state-error" role="status">
          Loading state boundaries…
        </p>
      )}
      {tipRow && stateTip && (
        <div
          className={`map-state-tip${stateTip.pinned ? " map-state-tip--pinned" : ""}`}
          role={stateTip.pinned ? "group" : "tooltip"}
          aria-label={stateTip.pinned ? `${tipRow.name} details` : undefined}
          onKeyDown={(e) => {
            if (e.key === "Escape") setStateTip(null);
          }}
          style={stateTip.pinned ? undefined : { left: stateTip.x, top: Math.max(0, stateTip.y - 90) }}
        >
          <p>{stateDescription(tipRow)}</p>
          {stateTip.pinned && (
            <>
              <button
                type="button"
                className="button button--quiet"
                onClick={() => {
                  props.onStateFilter?.(tipRow.code);
                  setStateTip(null);
                }}
              >
                Add state filter: {tipRow.name}
              </button>
              <button type="button" className="button button--quiet" onClick={() => setStateTip(null)}>
                Dismiss state details
              </button>
            </>
          )}
        </div>
      )}
    </div>
  );
}
