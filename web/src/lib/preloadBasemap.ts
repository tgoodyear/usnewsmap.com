import { parseView } from "../state/url";
import { DEFAULT_ZOOM, STYLE_URL, US_CENTER } from "./basemap";

const MAX_LATITUDE = 85.0511287798066;
const REQUEST_LIMIT = 40;

type Tile = { z: number; x: number; y: number };
type Source = {
  type: string;
  url?: string;
  tiles?: string[];
  tileSize?: number;
  minzoom?: number;
  maxzoom?: number;
  scheme?: string;
};
type Style = {
  sources: Record<string, Source>;
  sprite?: string | { id: string; url: string }[];
  glyphs?: string;
  layers: {
    type: string;
    source?: string;
    minzoom?: number;
    maxzoom?: number;
    layout?: { visibility?: string; "text-font"?: unknown };
  }[];
};

/** Fractional XYZ coordinates; longitude is deliberately unwrapped. */
export function lonLatToTile([lon, lat]: [number, number], z: number): [number, number] {
  const latitude = (Math.max(-MAX_LATITUDE, Math.min(MAX_LATITUDE, lat)) * Math.PI) / 180;
  return [((lon + 180) / 360) * 2 ** z, ((1 - Math.asinh(Math.tan(latitude)) / Math.PI) / 2) * 2 ** z];
}

/** North-up, unpitched initial view, with MapLibre's Mercator world-edge constraint. */
export function viewportTiles(
  center: [number, number],
  zoom: number,
  width: number,
  height: number,
  source: Source = { type: "vector" },
  limit = REQUEST_LIMIT,
): Tile[] {
  // coveringZoomLevel: vector sources floor at 512px; raster sources round
  // after accounting for their tileSize (often 256px).
  const worldSize = 512 * 2 ** zoom;
  const effectiveZoom = Math.max(zoom, Math.log2(height / 512));
  const level = effectiveZoom + Math.log2(512 / (source.type === "vector" ? 512 : (source.tileSize ?? 512)));
  const requested = Math.max(0, source.type === "vector" ? Math.floor(level) : Math.round(level));
  if (requested < (source.minzoom ?? 0)) return [];
  const z = Math.min(requested, source.maxzoom ?? 22);
  const n = 2 ** z;
  const [cx, cy] = lonLatToTile(center, z);
  const halfWidth = ((width / (512 * 2 ** effectiveZoom)) * n) / 2;
  const halfHeight = ((height / Math.max(worldSize, height)) * n) / 2;
  const constrainedY = Math.max(halfHeight, Math.min(n - halfHeight, cy));
  const tiles: Tile[] = [];
  const seen = new Set<string>();
  for (
    let y = Math.max(0, Math.floor(constrainedY - halfHeight));
    y < Math.min(n, Math.ceil(constrainedY + halfHeight));
    y++
  ) {
    for (let x = Math.floor(cx - halfWidth); x < Math.ceil(cx + halfWidth); x++) {
      const wrappedX = ((x % n) + n) % n;
      const key = `${wrappedX}/${y}`;
      if (seen.has(key)) continue;
      seen.add(key);
      tiles.push({ z, x: wrappedX, y });
      if (tiles.length >= limit) return tiles;
    }
  }
  return tiles;
}

/** Best-effort HTTP-cache warming, independent of the lazy WebGL stack. */
export async function preloadBasemap({
  styleUrl = STYLE_URL,
  search = window.location.search,
  width = window.innerWidth,
  height = window.innerHeight,
  pixelRatio = window.devicePixelRatio,
  fetcher = fetch,
}: {
  styleUrl?: string;
  search?: string;
  width?: number;
  height?: number;
  pixelRatio?: number;
  fetcher?: typeof fetch;
} = {}): Promise<void> {
  if (styleUrl === "none" || width <= 0 || height <= 0) return;
  let requests = 0;
  const seen = new Set<string>();
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 10_000);
  const request = async (url: string, kind: "json" | "image" | "binary") => {
    if (requests >= REQUEST_LIMIT || seen.has(url)) return;
    seen.add(url);
    requests++;
    try {
      // MapLibre's ajax.ts uses Request defaults (GET, cors, same-origin),
      // application/json for JSON and image/webp,*/* for ImageRequest.
      const response = await fetcher(url, {
        method: "GET",
        mode: "cors",
        credentials: "same-origin",
        ...(kind === "binary" ? {} : { headers: { Accept: kind === "json" ? "application/json" : "image/webp,*/*" } }),
        signal: controller.signal,
      });
      if (!response.ok) return;
      return kind === "json" ? ((await response.json()) as unknown) : await response.arrayBuffer();
    } catch {
      // Offline, blocked or changed styles must never delay the application.
      return;
    }
  };
  try {
    styleUrl = new URL(styleUrl, window.location.href).href;
    const style = (await request(styleUrl, "json")) as Style | undefined;
    if (!style) return;
    const view = parseView(search);
    const zoom = view.z ?? DEFAULT_ZOOM;
    const center = view.c ?? US_CENTER;
    const visible = style.layers.filter(
      (l) => l.layout?.visibility !== "none" && zoom >= (l.minzoom ?? 0) && zoom < (l.maxzoom ?? 24),
    );
    const jobs: Promise<unknown>[] = [];
    const suffix = pixelRatio > 1 ? "@2x" : "";
    const sprites = typeof style.sprite === "string" ? [{ url: style.sprite }] : (style.sprite ?? []);
    for (const sprite of sprites) {
      for (const extension of ["json", "png"] as const) {
        const url = new URL(sprite.url, styleUrl);
        url.pathname += `${suffix}.${extension}`;
        jobs.push(request(url.href, extension === "json" ? "json" : "image"));
      }
    }
    for (const layer of visible) {
      const fonts = layer.layout?.["text-font"];
      if (
        style.glyphs &&
        layer.type === "symbol" &&
        Array.isArray(fonts) &&
        fonts.every((f) => typeof f === "string")
      ) {
        jobs.push(
          request(
            new URL(style.glyphs.replace("{fontstack}", fonts.join(",")).replace("{range}", "0-255"), styleUrl).href,
            "binary",
          ),
        );
      }
    }
    for (const [id, original] of Object.entries(style.sources)) {
      if (!["vector", "raster"].includes(original.type) || !visible.some((l) => l.source === id)) continue;
      jobs.push(
        (async () => {
          const base = original.url ? new URL(original.url, styleUrl).href : styleUrl;
          const metadata = original.url ? ((await request(base, "json")) as Partial<Source> | undefined) : {};
          if (!metadata) return;
          const source = { ...original, ...metadata };
          if (!source.tiles?.length) return;
          await Promise.all(
            viewportTiles(center, zoom, width, height, source).map(({ z, x, y }) => {
              const template = source.tiles![(x + y) % source.tiles!.length]!;
              const row = source.scheme === "tms" ? 2 ** z - y - 1 : y;
              const url = new URL(
                template.replace("{z}", String(z)).replace("{x}", String(x)).replace("{y}", String(row)),
                base,
              ).href;
              return request(url, source.type === "raster" ? "image" : "binary");
            }),
          );
        })(),
      );
    }
    await Promise.all(jobs);
  } catch {
    // Malformed/unsupported third-party metadata is not an application error.
  } finally {
    controller.abort();
    clearTimeout(timeout);
  }
}
