import { addProtocol } from "maplibre-gl";
import { PMTiles } from "pmtiles";
import manifest from "../../basemap.json";

const prefix = `/basemap/${manifest.version}`;
const fonts = new Set(manifest.fonts);
const ranges = new Set(manifest.ranges);
let installed = false;

type Archive = Pick<PMTiles, "getZxy">;

export function basemapLoader(archive: Archive, fetcher: typeof fetch = fetch) {
  let detail: Promise<string> | undefined;
  const detailTemplate = () => {
    detail ??= (async () => {
      const response = await fetcher(manifest.tilejson, { signal: AbortSignal.timeout(10_000) });
      if (!response.ok) throw new Error(`Basemap metadata failed: HTTP ${response.status}`);
      const metadata: unknown = await response.json();
      if (!metadata || typeof metadata !== "object" || !("tiles" in metadata) || !Array.isArray(metadata.tiles)) {
        throw new Error("Invalid basemap metadata");
      }
      const template: unknown = metadata.tiles[0];
      if (
        typeof template !== "string" ||
        !/^https:\/\/tiles\.openfreemap\.org\/planet\/[^/?#]+\/\{z\}\/\{x\}\/\{y\}\.pbf$/.test(template)
      ) {
        throw new Error("Invalid basemap upstream tile URL");
      }
      return template;
    })().catch((error: unknown) => {
      detail = undefined;
      throw error;
    });
    return detail;
  };
  return async ({ url }: { url: string }, controller: AbortController): Promise<{ data: ArrayBuffer }> => {
    const tile = /^basemap:\/\/tiles\/(\d+)\/(\d+)\/(\d+)\.pbf$/.exec(url);
    let target: string;
    if (tile) {
      const [z, x, y] = tile.slice(1).map(Number) as [number, number, number];
      if (z > 14 || x >= 2 ** z || y >= 2 ** z) throw new Error("Invalid basemap tile coordinates");
      if (z <= manifest.maxzoom) {
        const result = await archive.getZxy(z, x, y, controller.signal);
        if (result) return { data: result.data };
      }
      target = (await detailTemplate()).replace("{z}", String(z)).replace("{x}", String(x)).replace("{y}", String(y));
    } else {
      const glyph = /^basemap:\/\/glyphs\/([^/]+)\/(\d+)-(\d+)\.pbf$/.exec(url);
      if (!glyph) throw new Error("Invalid basemap resource URL");
      const font = decodeURIComponent(glyph[1]!);
      const start = Number(glyph[2]);
      if (!fonts.has(font) || start % 256 !== 0 || Number(glyph[3]) !== start + 255 || start > 65280) {
        throw new Error("Invalid basemap glyph range");
      }
      const span = `${start}-${start + 255}`;
      target = ranges.has(start)
        ? `${prefix}/fonts/${encodeURIComponent(font)}/${span}.pbf`
        : manifest.glyphs.replace("{fontstack}", encodeURIComponent(font)).replace("{range}", span);
    }
    const response = await fetcher(target, { signal: controller.signal });
    if (!response.ok) throw new Error(`Basemap resource failed: HTTP ${response.status}`);
    return { data: await response.arrayBuffer() };
  };
}

export function installBasemapProtocol() {
  if (installed) return;
  addProtocol(
    "basemap",
    basemapLoader(new PMTiles(new URL(`${prefix}/overview.pmtiles.bin`, window.location.href).href)),
  );
  installed = true;
}
