// Map (07 §7.4): MapLibre basemap with a deck.gl overlay. Loaded lazily so
// the map stack stays off the critical path.

import { useEffect, useRef } from "react";
import { Map as MapLibreMap, NavigationControl, setWorkerUrl, type StyleSpecification } from "maplibre-gl";
import "maplibre-gl/dist/maplibre-gl.css";
// MapLibre finds its worker relative to its own module at runtime, which a
// bundler can't follow; bundle the worker explicitly and hand it the URL.
import workerUrl from "maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url";
import { MapboxOverlay } from "@deck.gl/mapbox";
import { ScatterplotLayer } from "@deck.gl/layers";
import { HeatmapLayer } from "@deck.gl/aggregation-layers";
import type { Layer, Norm } from "../state/url";
import { colorFor } from "../lib/scale";
import { ALPHA_CLEAR, ALPHA_UNCLEAR, skewColor } from "../lib/skewScale";
import { skewSentence } from "../lib/skewText";
import type { MapPoint } from "./mapTypes";
import { MAX_ZOOM, MIN_ZOOM } from "../lib/mapLimits";


interface Props {
  points: MapPoint[];
  layer: Layer;
  norm: Norm;
  /** The largest circle's value: pages with a match, or expected matches in the relative-rate view. */
  maxValue: number;
  selected: string;
  onSelect: (id: string) => void;
  zoom: number | null;
  center: [number, number] | null;
  onViewport: (zoom: number, center: [number, number]) => void;
}

setWorkerUrl(workerUrl);

const US_CENTER: [number, number] = [-96, 38.5];
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
const STYLE_URL = import.meta.env.VITE_BASEMAP_STYLE ?? "https://tiles.openfreemap.org/styles/positron";
const PLAIN_STYLE: StyleSpecification = {
  version: 8,
  sources: {},
  layers: [{ id: "bg", type: "background", paint: { "background-color": "#dfe4ea" } }],
};

export default function MapView(props: Props) {
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
    if (!container.current) return;
    const m = new MapLibreMap({
      container: container.current,
      style: STYLE_URL === "none" ? PLAIN_STYLE : STYLE_URL,
      center: latest.current.center ?? US_CENTER,
      zoom: latest.current.zoom ?? 3.3,
      minZoom: MIN_ZOOM,
      maxZoom: MAX_ZOOM,
      attributionControl: { compact: true },
      dragRotate: false,
      pitchWithRotate: false,
    });
    m.touchZoomRotate.disableRotation();
    m.addControl(new NavigationControl({ showCompass: false }), "top-left");
    // Overlaid, not interleaved: deck.gl 9.4's interleaved mode reads
    // MapLibre internals that changed in MapLibre 6.
    const o = new MapboxOverlay({ interleaved: false, layers: [] });
    m.addControl(o);

    // Hover and click are hit-tested on the CPU against the projected
    // circles. GPU picking would read pixels back on every mouse move,
    // stalling the pipeline; a few thousand projections cost far less.
    const hit = (x: number, y: number): MapPoint | null => {
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
        tip.textContent = skew
          ? skewSentence(`${p.name}, ${p.state}`, skew)
          : `${p.name}, ${p.state} · ${p.value.toLocaleString()} pages`;
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
    overlay.current = o;
    return () => {
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
  const zoom = props.zoom ?? 3.3;
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

  const { points, layer, norm, maxValue, selected } = props;
  useEffect(() => {
    const o = overlay.current;
    if (!o) return;
    // Every place in the search is always drawn; places with no pages yet
    // have zero radius. A fixed-size dataset lets deck.gl update attribute
    // values in place on each playback step instead of rebuilding buffers.
    drawn.current = { points, maxValue, layer, norm };
    const layers =
      norm === "skew"
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
                    ? p.id === selected ? 3 : 1
                    : p.id === selected ? 4.5 : 2.5,
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
              getFillColor: (p) =>
                p.precision === "city"
                  ? colorFor(p.value / Math.max(maxValue, 1))
                  : [0, 0, 0, 0],
              getLineColor: (p) =>
                p.id === selected
                  ? [20, 20, 20, 255]
                  : colorFor(p.value / Math.max(maxValue, 1)),
              getLineWidth: (p) => (p.value <= 0 ? 0 : p.id === selected ? 3 : p.precision === "city" ? 1 : 2.5),
              updateTriggers: {
                getRadius: [maxValue],
                getFillColor: [maxValue],
                getLineColor: [maxValue, selected],
                getLineWidth: [selected, points],
              },
            }),
          ];
    // No attribute transitions: deck.gl runs them on the GPU with transform
    // feedback and reads buffers back, which stalls every playback step.
    o.setProps({ layers });
  }, [points, layer, norm, maxValue, selected]);

  return (
    <div className="map-wrap">
      <div ref={container} className="map" data-testid="map" />
      <div ref={tooltip} className="map-tooltip" role="tooltip" hidden />
    </div>
  );
}
