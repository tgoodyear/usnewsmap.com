import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

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
const layers: { id: string; data: unknown }[][] = [];
vi.mock("@deck.gl/mapbox", () => ({
  MapboxOverlay: class {
    setProps(props: { layers: { id: string; data: unknown }[] }) {
      layers.push(props.layers);
    }
  },
}));
vi.mock("@deck.gl/layers", () => ({
  ScatterplotLayer: class {},
  GeoJsonLayer: class {
    constructor(props: object) {
      Object.assign(this, props);
    }
  },
}));
vi.mock("@deck.gl/aggregation-layers", () => ({ HeatmapLayer: class {} }));

const { default: MapView } = await import("./MapView");
const wrapper = ({ children }: { children: ReactNode }) => (
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    {children}
  </QueryClientProvider>
);

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
    layers.length = 0;
    vi.unstubAllGlobals();
  });

  it("leaves a gesture in progress alone when a re-render repeats the URL viewport", () => {
    // Playback re-renders every frame with a freshly parsed center.
    const { rerender } = render(<MapView {...props} zoom={4} center={[-90, 40]} />, { wrapper });
    const m = maps[0]!;
    m.zoom = 5.5; // mid-pinch: no moveend yet, so the URL still says 4
    rerender(<MapView {...props} zoom={4} center={[-90, 40]} />);
    expect(m.jumpTo).not.toHaveBeenCalled();
    expect(m.zoom).toBe(5.5);
  });

  it("moves to a viewport the URL changes to", () => {
    const { rerender } = render(<MapView {...props} zoom={4} center={[-90, 40]} />, { wrapper });
    const m = maps[0]!;
    rerender(<MapView {...props} zoom={6} center={[-87.6, 41.9]} />);
    expect(m.jumpTo).toHaveBeenCalledWith({ center: [-87.6, 41.9], zoom: 6 });
  });

  it("loads geometry only for States and draws only a GeoJsonLayer, reusing its geometry", async () => {
    const geometry = { type: "FeatureCollection", features: [] };
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => geometry });
    vi.stubGlobal("fetch", fetcher);
    const { rerender } = render(<MapView {...props} zoom={4} center={null} />, { wrapper });
    expect(fetcher).not.toHaveBeenCalled();
    rerender(<MapView {...props} layer="states" zoom={4} center={null} />);
    await waitFor(() => expect(layers.at(-1)?.map((l) => l.id)).toEqual(["states"]));
    const data = layers.at(-1)![0]!.data;
    rerender(<MapView {...props} layer="states" zoom={4} center={null} states={[]} />);
    expect(layers.at(-1)![0]!.data).toBe(data);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});
