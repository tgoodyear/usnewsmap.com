import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

// MapLibre and deck.gl need WebGL; stand in for the parts MapView calls.
const maps: FakeMap[] = [];
class FakeMap {
  zoom: number;
  center: { lng: number; lat: number };
  jumpTo = vi.fn((o: { center: [number, number]; zoom: number }) => {
    this.zoom = o.zoom;
    this.center = { lng: o.center[0], lat: o.center[1] };
  });
  touchZoomRotate = { disableRotation: () => {} };
  constructor(o: { center: [number, number]; zoom: number }) {
    this.zoom = o.zoom;
    this.center = { lng: o.center[0], lat: o.center[1] };
    maps.push(this);
  }
  getZoom = () => this.zoom;
  getCenter = () => this.center;
  on() {}
  addControl() {}
  remove() {}
}
vi.mock("maplibre-gl", () => ({ Map: FakeMap, NavigationControl: class {}, setWorkerUrl: () => {} }));
vi.mock("maplibre-gl/dist/maplibre-gl.css", () => ({}));
vi.mock("maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url", () => ({ default: "" }));
vi.mock("@deck.gl/mapbox", () => ({
  MapboxOverlay: class {
    setProps() {}
  },
}));
vi.mock("@deck.gl/layers", () => ({ ScatterplotLayer: class {} }));
vi.mock("@deck.gl/aggregation-layers", () => ({ HeatmapLayer: class {} }));

const { default: MapView } = await import("./MapView");

const props = {
  points: [],
  layer: "points" as const,
  norm: "raw" as const,
  maxValue: 1,
  maxRel: 1,
  selected: "",
  onSelect: () => {},
  onViewport: () => {},
};

describe("MapView", () => {
  afterEach(() => {
    cleanup();
    maps.length = 0;
  });

  it("leaves a gesture in progress alone when a re-render repeats the URL viewport", () => {
    // Playback re-renders every frame with a freshly parsed center.
    const { rerender } = render(<MapView {...props} zoom={4} center={[-90, 40]} />);
    const m = maps[0]!;
    m.zoom = 5.5; // mid-pinch: no moveend yet, so the URL still says 4
    rerender(<MapView {...props} zoom={4} center={[-90, 40]} />);
    expect(m.jumpTo).not.toHaveBeenCalled();
    expect(m.zoom).toBe(5.5);
  });

  it("moves to a viewport the URL changes to", () => {
    const { rerender } = render(<MapView {...props} zoom={4} center={[-90, 40]} />);
    const m = maps[0]!;
    rerender(<MapView {...props} zoom={6} center={[-87.6, 41.9]} />);
    expect(m.jumpTo).toHaveBeenCalledWith({ center: [-87.6, 41.9], zoom: 6 });
  });
});
