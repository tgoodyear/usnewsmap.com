import type { Map as MapLibreMap, MapLayerMouseEvent } from "maplibre-gl";
import type { FeatureCollection, Geometry } from "geojson";
import { stateColor, type StateTotal } from "../engine/states";

const SOURCE = "state-boundaries";
const FILL = "state-boundaries-fill";

export class StatesMapLayer {
  private destroyed = false;
  private hover = (event: MapLayerMouseEvent) => {
    this.onHover(event.features?.[0]?.properties?.STUSPS as string | undefined, event.point.x, event.point.y);
  };
  private leave = () => this.onHover(undefined, 0, 0);
  private click = (event: MapLayerMouseEvent) => {
    const code = event.features?.[0]?.properties?.STUSPS as string | undefined;
    if (code) this.onClick(code, event.point.x, event.point.y);
  };

  constructor(
    private map: MapLibreMap,
    data: FeatureCollection<Geometry>,
    private onHover: (code: string | undefined, x: number, y: number) => void,
    private onClick: (code: string, x: number, y: number) => void,
  ) {
    map.addSource(SOURCE, { type: "geojson", data, promoteId: "STUSPS" });
    const label = map.getStyle().layers.find((layer) => layer.type === "symbol")?.id;
    map.addLayer(
      {
        id: FILL,
        type: "fill",
        source: SOURCE,
        paint: {
          "fill-color": ["coalesce", ["feature-state", "color"], "rgba(142,142,142,0.569)"],
          "fill-outline-color": "rgba(55,55,55,0.863)",
        },
      },
      label,
    );
    map.on("mousemove", FILL, this.hover);
    map.on("mouseleave", FILL, this.leave);
    map.on("click", FILL, this.click);
  }

  update(rows: StateTotal[], breaks: number[]) {
    for (const row of rows) {
      if (!row.code) continue;
      const [r, g, b, a] = stateColor(row, breaks);
      this.map.setFeatureState({ source: SOURCE, id: row.code }, { color: `rgba(${r},${g},${b},${a / 255})` });
    }
  }

  hasDrawn() {
    return this.isReady() && this.map.queryRenderedFeatures(undefined, { layers: [FILL] }).length > 0;
  }

  isReady() {
    return this.map.isSourceLoaded(SOURCE);
  }

  destroy() {
    if (this.destroyed) return;
    this.destroyed = true;
    this.map.off("mousemove", FILL, this.hover);
    this.map.off("mouseleave", FILL, this.leave);
    this.map.off("click", FILL, this.click);
    if (this.map.getLayer(FILL)) this.map.removeLayer(FILL);
    if (this.map.getSource(SOURCE)) this.map.removeSource(SOURCE);
  }
}
