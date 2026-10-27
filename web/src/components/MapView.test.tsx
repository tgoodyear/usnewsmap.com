import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

// MapLibre and deck.gl need WebGL; stand in for the parts MapView calls.
const maps: FakeMap[] = [];
const afterRender: (() => void)[] = [];
const config = vi.hoisted(() => ({ style: "https://tiles.openfreemap.org/styles/positron", styleLoaded: true }));
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
  off(event: string, handler: () => void) {
    this.handlers[event] = this.handlers[event]?.filter((entry) => entry !== handler) ?? [];
  }
  emit(event: string) {
    this.handlers[event]?.forEach((handler) => handler());
  }
  areTilesLoaded = vi.fn(() => true);
  styleLoaded = config.styleLoaded;
  isStyleLoaded = () => this.styleLoaded;
  getStyle = () => (this.styleLoaded ? { version: 8, layers: [] } : undefined);
  setStyle = vi.fn(() => {
    this.styleLoaded = true;
    this.emit("style.load");
  });
  addControl() {}
  remove() {}
}
vi.mock("maplibre-gl", () => ({ Map: FakeMap, NavigationControl: class {}, setWorkerUrl: () => {} }));
vi.mock("maplibre-gl/dist/maplibre-gl.css", () => ({}));
vi.mock("maplibre-gl/dist/maplibre-gl-worker.mjs?worker&url", () => ({ default: "" }));
const layers: { id: string; data: unknown }[][] = [];
const nativeLayers: { data: unknown; update: ReturnType<typeof vi.fn>; destroy: ReturnType<typeof vi.fn> }[] = [];
vi.mock("./StatesMapLayer", () => ({
  StatesMapLayer: class {
    update = vi.fn();
    destroy = vi.fn();
    hasDrawn = () => false;
    isReady = () => true;
    constructor(
      _map: unknown,
      public data: unknown,
    ) {
      nativeLayers.push(this);
    }
  },
}));
vi.mock("@deck.gl/mapbox", () => ({
  MapboxOverlay: class {
    constructor(props: { onAfterRender: () => void }) {
      afterRender.push(props.onAfterRender);
    }
    setProps(props: { layers: { id: string; data: unknown }[] }) {
      layers.push(props.layers);
    }
  },
}));
vi.mock("@deck.gl/layers", () => ({
  ScatterplotLayer: class {
    constructor(props: object) {
      Object.assign(this, props);
    }
  },
}));
vi.mock("@deck.gl/aggregation-layers", () => ({ HeatmapLayer: class {} }));

const { default: MapView } = await import("./MapView");
const wrapper = ({ children }: { children: ReactNode }) => (
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: Infinity } } })}>
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
    afterRender.length = 0;
    layers.length = 0;
    nativeLayers.length = 0;
    Reflect.deleteProperty(window, "__USNM_SHARE_CAPTURE__");
    vi.unstubAllGlobals();
    vi.useRealTimers();
    config.style = "https://tiles.openfreemap.org/styles/positron";
    config.styleLoaded = true;
  });

  it("holds the overlay until load and an idle with loaded tiles, only once", () => {
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
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

  it("exposes screenshot readiness only for loaded tiles and a rendered overlay", () => {
    const { getByTestId, rerender } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    const element = getByTestId("map");
    expect(element.dataset.overlayReady).toBe("false");
    const m = maps[0]!;
    m.emit("load");
    m.emit("idle");
    afterRender[0]!();
    expect(element.dataset.basemapReady).toBe("true");
    expect(element.dataset.overlayReady).toBe("true");
    m.emit("dataloading");
    expect(element.dataset.basemapReady).toBe("false");
    rerender(<MapView {...props} points={[]} zoom={null} center={null} />);
    expect(element.dataset.overlayReady).toBe("false");
  });

  it("reveals the overlay after two seconds if the basemap is blocked", () => {
    vi.useFakeTimers();
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    vi.advanceTimersByTime(1999);
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(false);
    vi.advanceTimersByTime(1);
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(true);
  });

  it("keeps a slow initial basemap request alive during capture", () => {
    vi.useFakeTimers();
    config.styleLoaded = false;
    Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true, configurable: true });
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    const m = maps[0]!;
    act(() => vi.advanceTimersByTime(2500));
    expect(m.setStyle).not.toHaveBeenCalled();
    expect(getByTestId("map").dataset.basemapError).toBe("false");
    m.styleLoaded = true;
    act(() => {
      m.emit("style.load");
      m.emit("load");
      m.emit("idle");
    });
    expect(getByTestId("map").dataset.basemapReady).toBe("true");
  });

  it("never marks a degraded offline background ready for a screenshot", () => {
    config.styleLoaded = false;
    Object.defineProperty(window, "__USNM_SHARE_CAPTURE__", { value: true, configurable: true });
    const { getByTestId } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    const m = maps[0]!;
    act(() => {
      m.emit("error");
      m.emit("load");
      m.emit("idle");
    });
    expect(m.setStyle).toHaveBeenCalledTimes(1);
    expect(getByTestId("map").dataset.basemapError).toBe("true");
    expect(getByTestId("map").dataset.basemapReady).toBe("false");
  });

  it("shows the offline style immediately and cleans up its timer on unmount", () => {
    vi.useFakeTimers();
    config.style = "none";
    const { getByTestId, unmount } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    expect(getByTestId("map").classList.contains("map--basemap-ready")).toBe(true);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });

  it("cancels the fallback when unmounted while waiting", () => {
    vi.useFakeTimers();
    const { unmount } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    expect(vi.getTimerCount()).toBe(1);
    unmount();
    expect(vi.getTimerCount()).toBe(0);
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

  it("loads geometry only for States and reuses the native source when window colours change", async () => {
    const geometry = { type: "FeatureCollection", features: [] };
    const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => geometry });
    vi.stubGlobal("fetch", fetcher);
    const { rerender } = render(<MapView {...props} zoom={4} center={null} />, { wrapper });
    expect(fetcher).not.toHaveBeenCalled();
    rerender(<MapView {...props} layer="states" zoom={4} center={null} />);
    await waitFor(() => expect(fetcher).toHaveBeenCalledTimes(1));
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(layers.at(-1)).toEqual([]);
    const states = [{ code: "NY", name: "New York", hits: 0, pages: 100, rate: 0 }];
    rerender(<MapView {...props} layer="states" zoom={4} center={null} states={states} />);
    await waitFor(() => expect(nativeLayers).toHaveLength(1));
    expect(layers.at(-1)).toEqual([]);
    expect(nativeLayers[0]!.data).toBe(geometry);
    const later = [{ ...states[0]!, hits: 10, rate: 100 }];
    const breaks = [20, 40, 60, 80, 100];
    rerender(<MapView {...props} layer="states" zoom={4} center={null} states={later} stateBreaks={breaks} />);
    expect(nativeLayers).toHaveLength(1);
    expect(nativeLayers[0]!.update).toHaveBeenLastCalledWith(later, breaks);
    rerender(<MapView {...props} layer="states" zoom={4} center={null} states={[]} />);
    expect(layers.at(-1)).toEqual([]);
    expect(nativeLayers[0]!.destroy).toHaveBeenCalled();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it("does not initialize the deck overlay on direct States entry, but creates it when switching to Points", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ type: "FeatureCollection", features: [] }),
      }),
    );
    const states = [{ code: "NY", name: "New York", hits: 1, pages: 100, rate: 10 }];
    const { rerender } = render(<MapView {...props} layer="states" zoom={4} center={null} states={states} />, {
      wrapper,
    });
    await waitFor(() => expect(nativeLayers).toHaveLength(1));
    expect(layers).toHaveLength(0);
    rerender(<MapView {...props} layer="points" zoom={4} center={null} states={states} />);
    expect(nativeLayers[0]!.destroy).toHaveBeenCalled();
    expect(layers.at(-1)?.map((layer) => layer.id)).toEqual(["points"]);
  });

  it("recovers an initial style failure so ready States geometry can render", async () => {
    config.styleLoaded = false;
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({
        ok: true,
        json: async () => ({ type: "FeatureCollection", features: [] }),
      }),
    );
    const states = [{ code: "NY", name: "New York", hits: 1, pages: 100, rate: 10 }];
    render(<MapView {...props} layer="states" zoom={4} center={null} states={states} />, { wrapper });
    expect(nativeLayers).toHaveLength(0);
    act(() => maps[0]!.emit("error"));
    expect(maps[0]!.setStyle).toHaveBeenCalledWith(
      expect.objectContaining({ sources: {}, layers: [expect.objectContaining({ type: "background" })] }),
      { diff: false },
    );
    await waitFor(() => expect(nativeLayers).toHaveLength(1));
    act(() => maps[0]!.emit("error"));
    expect(maps[0]!.setStyle).toHaveBeenCalledTimes(1);
  });

  it("bounds a stalled initial style request but does not replace a loaded style on tile errors", () => {
    vi.useFakeTimers();
    config.styleLoaded = false;
    const { unmount } = render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    act(() => vi.advanceTimersByTime(2000));
    expect(maps[0]!.setStyle).toHaveBeenCalledTimes(1);
    unmount();
    render(<MapView {...props} zoom={null} center={null} />, { wrapper });
    const loaded = maps.at(-1)!;
    loaded.styleLoaded = true;
    act(() => loaded.emit("error"));
    act(() => vi.advanceTimersByTime(2000));
    expect(loaded.setStyle).not.toHaveBeenCalled();
  });
});
