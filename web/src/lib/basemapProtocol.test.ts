import { describe, expect, it, vi } from "vitest";
import { basemapLoader } from "./basemapProtocol";

vi.mock("maplibre-gl", () => ({ addProtocol: vi.fn() }));
const bytes = new Uint8Array([1, 2, 3]).buffer;

describe("basemap transport", () => {
  it("uses the local archive for overview tiles without contacting upstream", async () => {
    const getZxy = vi.fn().mockResolvedValue({ data: bytes });
    const fetcher = vi.fn();
    const controller = new AbortController();
    expect(await basemapLoader({ getZxy }, fetcher)({ url: "basemap://tiles/3/1/3.pbf" }, controller)).toEqual({
      data: bytes,
    });
    expect(getZxy).toHaveBeenCalledWith(3, 1, 3, controller.signal);
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("routes detailed and uncovered overview tiles to the matching upstream schema", async () => {
    const getZxy = vi.fn().mockResolvedValue(undefined);
    const fetcher = vi
      .fn()
      .mockImplementation((url: string) =>
        Promise.resolve(
          url.endsWith("/planet")
            ? Response.json({ tiles: ["https://tiles.openfreemap.org/planet/current_build/{z}/{x}/{y}.pbf"] })
            : new Response(bytes),
        ),
      );
    const load = basemapLoader({ getZxy }, fetcher);
    for (const tile of ["5/0/0", "6/16/24"]) {
      await load({ url: `basemap://tiles/${tile}.pbf` }, new AbortController());
      expect(fetcher.mock.lastCall?.[0]).toBe(`https://tiles.openfreemap.org/planet/current_build/${tile}.pbf`);
    }
    expect(getZxy).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls.filter(([url]) => url.endsWith("/planet"))).toHaveLength(1);
  });

  it("uses local overview glyphs and upstream only for additional ranges", async () => {
    const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response(bytes)));
    const load = basemapLoader({ getZxy: vi.fn() }, fetcher);
    await load({ url: "basemap://glyphs/Noto%20Sans%20Regular/0-255.pbf" }, new AbortController());
    expect(fetcher.mock.lastCall?.[0]).toMatch(/^\/basemap\/[^/]+\/fonts\/Noto%20Sans%20Regular\/0-255.pbf$/);
    await load({ url: "basemap://glyphs/Noto%20Sans%20Regular/1792-2047.pbf" }, new AbortController());
    expect(fetcher.mock.lastCall?.[0]).toBe("https://tiles.openfreemap.org/fonts/Noto%20Sans%20Regular/1792-2047.pbf");
  });

  it("rejects archive/HTTP failures rather than silently accepting a blank map", async () => {
    const fetcher = vi.fn().mockResolvedValue(new Response("", { status: 503 }));
    await expect(
      basemapLoader({ getZxy: vi.fn().mockRejectedValue(new Error("archive failed")) }, fetcher)(
        { url: "basemap://tiles/3/1/3.pbf" },
        new AbortController(),
      ),
    ).rejects.toThrow("archive failed");
    expect(fetcher).not.toHaveBeenCalled();
    await expect(
      basemapLoader({ getZxy: vi.fn() }, fetcher)({ url: "basemap://tiles/6/16/24.pbf" }, new AbortController()),
    ).rejects.toThrow("HTTP 503");
  });
});
