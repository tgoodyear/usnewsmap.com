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
import type { MapPoint } from "./mapTypes";


interface Props {
  points: MapPoint[];
  layer: Layer;
  norm: Norm;
  maxValue: number;
  maxRel: number;
  selected: string;
  onSelect: (id: string) => void;
  zoom: number | null;
  center: [number, number] | null;
  onViewport: (zoom: number, center: [number, number]) => void;
}

setWorkerUrl(workerUrl);

const US_CENTER: [number, number] = [-96, 38.5];
const MAX_RADIUS_PX = 26;

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
      minZoom: 2,
      maxZoom: 12,
      attributionControl: { compact: true },
      dragRotate: false,
      pitchWithRotate: false,
    });
    m.touchZoomRotate.disableRotation();
    m.addControl(new NavigationControl({ showCompass: false }), "top-left");
    const o = new MapboxOverlay({ interleaved: false, layers: [] });
    m.addControl(o);
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
      m.remove();
      map.current = null;
      overlay.current = null;
    };
  }, []);

  // Back/forward or a pasted permalink changes the URL viewport while the map
  // stays mounted: move the map to match. Differences below the precision the
  // URL stores are the map's own last move echoing back, so they're ignored.
  const { zoom, center } = props;
  useEffect(() => {
    const m = map.current;
    if (!m) return;
    const c = m.getCenter();
    const target = center ?? US_CENTER;
    const z = zoom ?? 3.3;
    if (Math.abs(m.getZoom() - z) < 0.01 && Math.abs(c.lng - target[0]) < 0.001 && Math.abs(c.lat - target[1]) < 0.001) {
      return;
    }
    syncing.current = true;
    m.jumpTo({ center: target, zoom: z });
  }, [zoom, center]);

  const { points, layer, norm, maxValue, maxRel, selected, onSelect } = props;
  useEffect(() => {
    const o = overlay.current;
    if (!o) return;
    const visible = points.filter((p) => p.value > 0);
    const layers =
      layer === "heat"
        ? [
            new HeatmapLayer<MapPoint>({
              id: "heat",
              data: visible,
              getPosition: (p) => p.position,
              getWeight: (p) => (norm === "rel" ? p.rel : p.value),
              radiusPixels: 40,
              updateTriggers: { getWeight: [norm] },
            }),
          ]
        : [
            new ScatterplotLayer<MapPoint>({
              id: "points",
              data: visible,
              pickable: true,
              stroked: true,
              radiusUnits: "pixels",
              lineWidthUnits: "pixels",
              // Area ∝ hits, so radius ∝ √hits (perceptually honest).
              getRadius: (p) => Math.max(3, MAX_RADIUS_PX * Math.sqrt(p.value / Math.max(maxValue, 1))),
              getFillColor: (p) =>
                p.precision === "city"
                  ? colorFor(norm === "rel" ? p.rel / Math.max(maxRel, 1e-9) : p.value / Math.max(maxValue, 1))
                  : [0, 0, 0, 0],
              getLineColor: (p) =>
                p.id === selected
                  ? [20, 20, 20, 255]
                  : colorFor(norm === "rel" ? p.rel / Math.max(maxRel, 1e-9) : p.value / Math.max(maxValue, 1)),
              getLineWidth: (p) => (p.id === selected ? 3 : p.precision === "city" ? 1 : 2.5),
              onClick: (info) => {
                if (info.object) onSelect(info.object.id);
              },
              updateTriggers: {
                getRadius: [maxValue],
                getFillColor: [norm, maxValue, maxRel],
                getLineColor: [norm, maxValue, maxRel, selected],
                getLineWidth: [selected],
              },
              transitions: window.matchMedia("(prefers-reduced-motion: reduce)").matches
                ? {}
                : { getRadius: 120 },
            }),
          ];
    o.setProps({
      layers,
      getTooltip: ({ object }) =>
        object && "name" in object
          ? {
              text: `${(object as MapPoint).name}, ${(object as MapPoint).state}\n${(object as MapPoint).value.toLocaleString()} pages`,
            }
          : null,
    });
  }, [points, layer, norm, maxValue, maxRel, selected, onSelect]);

  return <div ref={container} className="map" data-testid="map" />;
}
