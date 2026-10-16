import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render } from "@testing-library/react";

// MapLibre and deck.gl need WebGL; stand in for the parts MapView calls.
const maps: FakeMap[] = [];
const config = vi.hoisted(() => ({ style: "https://tiles.openfreemap.org/styles/positron" }));
vi.mock("../lib/basemap", () => ({
  get STYLE_URL() {
    return config.style;
  },
  US_CENTER: [-96, 38.5],
  DEFAULT_ZOOM: 3.3,
}));
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
  handlers: Record<string, (() => void)[]> = {};
  on(event: string, handler: () => void) {
    (this.handlers[event] ??= []).push(handler);
  }
  emit(event: string) {
    this.handlers[event]?.forEach((handler) => handler());
  }
  areTilesLoaded = vi.fn(() => true);
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
    vi.useRealTimers();
    config.style = "https://tiles.openfreemap.org/styles/positron";
  });

  it("holds the overlay until load and an idle with loaded tiles, only once", () => {
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />);
    const element = getByTestId("map");
    const m = maps[0]!;
    expect(element.classList.contains("map--basemap-ready")).toBe(false);
    m.emit("idle");
    expect(element.classList.contains("map--basemap-ready")).toBe(false);
    m.emit("load");
    m.areTilesLoaded.mockReturnValue(false);
    m.emit("idle");
    expect(element.classList.contains("map--basemap-ready")).toBe(false);
    m.areTilesLoaded.mockReturnValue(true);
    m.emit("idle");
    expect(element.classList.contains("map--basemap-ready")).toBe(true);
    m.areTilesLoaded.mockReturnValue(false);
    m.emit("idle");
    expect(element.classList.contains("map--basemap-ready")).toBe(true);
  });

  it("reveals the overlay after two seconds if the basemap is blocked", () => {
    vi.useFakeTimers();
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />);
    vi.advanceTimersByTime(1999);
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(false);
    vi.advanceTimersByTime(1);
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(true);
  });

  it("shows the offline style immediately and cleans up its timer on unmount", () => {
    vi.useFakeTimers();
    config.style = "none";
    const { getByTestId, unmount } = render(<MapView {...props} zoom={null} center={null} />);
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(true);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("cancels the fallback when unmounted while waiting", () => {
    vi.useFakeTimers();
    const { unmount } = render(<MapView {...props} zoom={null} center={null} />);
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
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
