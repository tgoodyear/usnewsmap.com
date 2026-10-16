import { describe, expect, it, vi } from "vitest";
import { lonLatToTile, preloadBasemap, viewportTiles } from "./preloadBasemap";

const styleUrl = "https://tiles.example/style";
const style = {
  sources: {
    vector: { type: "vector", url: "https://tiles.example/planet" },
    raster: { type: "raster", tileSize: 256, tiles: ["https://tiles.example/r/{z}/{x}/{y}.png"] },
    unused: { type: "vector", url: "https://tiles.example/unused" },
  },
  sprite: "https://tiles.example/sprite?key=test",
  glyphs: "https://tiles.example/fonts/{fontstack}/{range}.pbf",
  layers: [
    { type: "fill", source: "vector" },
    { type: "raster", source: "raster", maxzoom: 5 },
    { type: "symbol", layout: { "text-font": ["Noto Sans Regular"] } },
    { type: "symbol", layout: { "text-font": ["Noto Sans Regular"] } },
    { type: "symbol", minzoom: 6, layout: { "text-font": ["Hidden"] } },
    { type: "symbol", layout: { visibility: "none", "text-font": ["Hidden"] } },
  ],
};

function mockFetch() {
  return vi.fn<typeof fetch>(async (input) => {
    const url = String(input);
    return new Response(
      JSON.stringify(
        url === styleUrl
          ? style
          : url.endsWith("/planet")
            ? {
                tiles: ["https://tiles.example/planet/runtime-date/{z}/{x}/{y}.pbf"],
                maxzoom: 14,
              }
            : {},
      ),
    );
  });
}

describe("initial tile coverage", () => {
  it("projects the equator and clamps the Mercator poles", () => {
    expect(lonLatToTile([0, 0], 3)).toEqual([4, 4]);
    expect(lonLatToTile([-180, 90], 3)[1]).toBeCloseTo(0);
    expect(lonLatToTile([180, -90], 3)[1]).toBeCloseTo(8);
  });

  it("covers the fractional zoom viewport using 512px vector tiles at z3, not z4", () => {
    expect(viewportTiles([0, 0], 3.3, 512, 512)).toEqual([
      { z: 3, x: 3, y: 3 },
      { z: 3, x: 4, y: 3 },
      { z: 3, x: 3, y: 4 },
      { z: 3, x: 4, y: 4 },
    ]);
    expect(viewportTiles([0, 0], 3.6, 512, 512)[0]?.z).toBe(3);
    expect(viewportTiles([0, 0], 3.6, 512, 512, { type: "raster", tileSize: 256 })[0]?.z).toBe(5);
  });

  it("wraps the antimeridian, deduplicates world copies and clamps rows", () => {
    const tiles = viewportTiles([180, 90], 2, 600, 600);
    expect(new Set(tiles.map((t) => t.x))).toEqual(new Set([3, 0]));
    expect(new Set(tiles.map((t) => t.y))).toEqual(new Set([0, 1]));
    const world = viewportTiles([0, -90], 0, 4000, 4000);
    expect(world).toHaveLength(16);
    expect(new Set(world.map((t) => `${t.z}/${t.x}/${t.y}`)).size).toBe(16);
    expect(world.every((t) => t.z === 2 && t.x >= 0 && t.x < 4 && t.y >= 0 && t.y < 4)).toBe(true);
  });

  it("honors source zoom bounds and limits oversized viewport work", () => {
    expect(viewportTiles([0, 0], 3.3, 512, 512, { type: "vector", minzoom: 4 })).toEqual([]);
    expect(viewportTiles([0, 0], 10, 512, 512, { type: "vector", maxzoom: 5 }).every((t) => t.z === 5)).toBe(true);
    expect(viewportTiles([0, 0], 3.3, 100000, 100000)).toHaveLength(40);
  });
});

describe("basemap HTTP cache warming", () => {
  it("resolves dated TileJSON URLs, visible glyphs, raster tiles and retina sprites with MapLibre request options", async () => {
    const fetcher = mockFetch();
    await preloadBasemap({ styleUrl, search: "?z=3.3&c=0,0", width: 512, height: 512, pixelRatio: 2, fetcher });
    const urls = fetcher.mock.calls.map(([url]) => String(url));
    expect(urls).toContain("https://tiles.example/planet/runtime-date/3/3/3.pbf");
    expect(urls).toContain("https://tiles.example/r/4/7/7.png");
    expect(urls).toContain("https://tiles.example/sprite@2x.json?key=test");
    expect(urls).toContain("https://tiles.example/sprite@2x.png?key=test");
    expect(urls).toContain("https://tiles.example/fonts/Noto%20Sans%20Regular/0-255.pbf");
    expect(urls.some((url) => /Hidden|unused/.test(url))).toBe(false);
    expect(new Set(urls).size).toBe(urls.length);
    for (const [url, options] of fetcher.mock.calls) {
      expect(options).toMatchObject({ method: "GET", mode: "cors", credentials: "same-origin" });
      if (String(url).includes(".pbf")) expect(options?.headers).toBeUndefined();
    }
    expect(fetcher.mock.calls[0]?.[1]?.headers).toEqual({ Accept: "application/json" });
    expect(fetcher.mock.calls.find(([url]) => String(url).includes(".png"))?.[1]?.headers).toEqual({
      Accept: "image/webp,*/*",
    });
  });

  it("uses default view, non-retina sprites and skips raster above its visible zoom", async () => {
    const fetcher = mockFetch();
    await preloadBasemap({ styleUrl, search: "", width: 512, height: 512, pixelRatio: 1, fetcher });
    expect(fetcher.mock.calls.map(([url]) => url)).toContain("https://tiles.example/planet/runtime-date/3/1/2.pbf");
    expect(fetcher.mock.calls.map(([url]) => url)).toContain("https://tiles.example/sprite.json?key=test");
    fetcher.mockClear();
    await preloadBasemap({ styleUrl, search: "?z=6&c=0,0", width: 512, height: 512, fetcher });
    expect(fetcher.mock.calls.some(([url]) => String(url).includes("/r/"))).toBe(false);
  });

  it("bounds all requests, and skips none and zero-sized views", async () => {
    const fetcher = mockFetch();
    await preloadBasemap({ styleUrl, width: 100000, height: 100000, fetcher });
    expect(fetcher.mock.calls.length).toBeLessThanOrEqual(40);
    fetcher.mockClear();
    await preloadBasemap({ styleUrl: "none", fetcher });
    await preloadBasemap({ styleUrl, width: 0, fetcher });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("abandons a stalled provider after ten seconds", async () => {
    vi.useFakeTimers();
    try {
      const fetcher = vi.fn<typeof fetch>(
        (_input, options) =>
          new Promise((_resolve, reject) => {
            options?.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
          }),
      );
      const warming = preloadBasemap({ styleUrl, fetcher });
      await vi.advanceTimersByTimeAsync(10000);
      await expect(warming).resolves.toBeUndefined();
      expect(fetcher.mock.calls[0]?.[1]?.signal?.aborted).toBe(true);
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("silently tolerates network errors, HTTP failures and malformed styles", async () => {
    await expect(
      preloadBasemap({ styleUrl, fetcher: vi.fn().mockRejectedValue(new Error("offline")) }),
    ).resolves.toBeUndefined();
    await expect(
      preloadBasemap({ styleUrl, fetcher: vi.fn().mockResolvedValue(new Response("", { status: 503 })) }),
    ).resolves.toBeUndefined();
    await expect(
      preloadBasemap({ styleUrl, fetcher: vi.fn().mockResolvedValue(new Response("{}")) }),
    ).resolves.toBeUndefined();
  });
});
