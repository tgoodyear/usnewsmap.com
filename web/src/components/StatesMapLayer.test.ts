import { expect, it, vi } from "vitest";
import type { Map as MapLibreMap } from "maplibre-gl";
import { StatesMapLayer } from "./StatesMapLayer";
import { NO_MATCHES, NO_PAGES, type StateTotal } from "../engine/states";

it("adds lazy geometry below labels, changes only feature state, and removes its own resources once", () => {
  const map = {
    addSource: vi.fn(),
    addLayer: vi.fn(),
    getStyle: () => ({ layers: [{ id: "labels", type: "symbol" }] }),
    on: vi.fn<(event: string, layer: string, handler: (event: unknown) => void) => void>(),
    off: vi.fn(),
    setFeatureState: vi.fn(),
    isSourceLoaded: vi.fn(() => true),
    queryRenderedFeatures: vi.fn(() => [{ properties: { STUSPS: "NY" } }]),
    getLayer: vi.fn(() => ({})),
    getSource: vi.fn(() => ({})),
    removeLayer: vi.fn(),
    removeSource: vi.fn(),
  };
  const geometry = { type: "FeatureCollection" as const, features: [] };
  const hover = vi.fn();
  const click = vi.fn();
  const native = new StatesMapLayer(map as unknown as MapLibreMap, geometry, hover, click);
  expect(map.addSource).toHaveBeenCalledWith("state-boundaries", {
    type: "geojson",
    data: geometry,
    promoteId: "STUSPS",
  });
  expect(map.addLayer).toHaveBeenCalledWith(expect.objectContaining({ type: "fill" }), "labels");
  const rows: StateTotal[] = [
    { code: "NY", name: "New York", hits: 0, pages: 100, rate: 0 },
    { code: "DC", name: "District of Columbia", hits: 0, pages: 0, rate: null },
    { code: "", name: "Unassigned", hits: 1, pages: 0, rate: null },
  ];
  native.update(rows, [1, 2, 3, 4, 5]);
  for (const [code, color] of [
    ["NY", NO_MATCHES],
    ["DC", NO_PAGES],
  ] as const) {
    const [r, g, b, a] = color;
    expect(map.setFeatureState).toHaveBeenCalledWith(
      { source: "state-boundaries", id: code },
      { color: `rgba(${r},${g},${b},${a / 255})` },
    );
  }
  native.update(rows, []);
  expect(map.addSource).toHaveBeenCalledTimes(1);
  expect(map.addLayer).toHaveBeenCalledTimes(1);
  expect(native.hasDrawn()).toBe(true);
  map.queryRenderedFeatures.mockReturnValue([]);
  expect(native.hasDrawn()).toBe(false);
  expect(native.isReady()).toBe(true);
  map.isSourceLoaded.mockReturnValue(false);
  expect(native.hasDrawn()).toBe(false);
  expect(native.isReady()).toBe(false);
  const event = { features: [{ properties: { STUSPS: "NY" } }], point: { x: 10, y: 20 } };
  map.on.mock.calls.find((call) => call[0] === "mousemove")![2](event);
  expect(hover).toHaveBeenCalledWith("NY", 10, 20);
  map.on.mock.calls.find((call) => call[0] === "click")![2](event);
  expect(click).toHaveBeenCalledWith("NY", 10, 20);
  map.on.mock.calls.find((call) => call[0] === "mouseleave")![2]({});
  expect(hover).toHaveBeenLastCalledWith(undefined, 0, 0);
  native.destroy();
  native.destroy();
  expect(map.off).toHaveBeenCalledTimes(3);
  for (const call of map.on.mock.calls) expect(map.off).toHaveBeenCalledWith(...call);
  expect(map.removeLayer).toHaveBeenCalledTimes(1);
  expect(map.removeSource).toHaveBeenCalledTimes(1);
});
