import { describe, expect, it } from "vitest";
import { placeCounts } from "./skew";
import { buildSkew, maxWindowExpected, scoreFrame, unitSums, windowCounts } from "./skewModel";

// A random sparse place × bucket table and a brute-force oracle.
function random(places: number, buckets: number, seed: number) {
  let s = seed;
  const rnd = () => ((s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  const cells = { p: [] as number[], b: [] as number[], pages: [] as number[], hits: [] as number[] };
  const nh = new Array<number>(buckets).fill(0);
  const np = new Array<number>(buckets).fill(0);
  for (let p = 0; p < places; p++)
    for (let b = 0; b < buckets; b++)
      if (rnd() < 0.6) {
        const pages = 1 + Math.floor(rnd() * 500);
        const hits = Math.floor(rnd() * pages * 0.05);
        cells.p.push(p);
        cells.b.push(b);
        cells.pages.push(pages);
        cells.hits.push(hits);
        nh[b] += hits;
        np[b] += pages;
      }
  return { cells, nh, np };
}

function only(cells: ReturnType<typeof random>["cells"], lo: number, hi: number) {
  const keep = cells.b.map((b) => b >= lo && b < hi);
  const pick = (a: number[]) => a.filter((_, i) => keep[i]);
  return { p: pick(cells.p), b: pick(cells.b), pages: pick(cells.pages), hits: pick(cells.hits) };
}

describe("relative-rate playback", () => {
  const { cells, nh, np } = random(20, 30, 7);
  const sums = unitSums(nh, np, cells, 20, 30);

  it("windowed counts from prefix sums equal place counts over the window's cells", () => {
    for (const [t, win] of [
      [29, null],
      [10, null],
      [29, 5],
      [3, 12],
      [0, 1],
    ] as const) {
      const lo = win === null ? 0 : Math.max(0, t + 1 - win);
      const direct = placeCounts(nh, np, only(cells, lo, t + 1), 20);
      windowCounts(sums, t, win).forEach((c, i) => {
        expect(c.observed).toBe(direct[i]!.observed);
        expect(c.expected).toBeCloseTo(direct[i]!.expected, 9);
      });
    }
  });

  it("scales circles to the largest window the playback reaches", () => {
    const full = Math.max(...windowCounts(sums, 29, null).map((c) => c.expected));
    expect(maxWindowExpected(sums, null)).toBeCloseTo(full, 9);
    let trailing = 0;
    for (let t = 0; t < 30; t++) trailing = Math.max(trailing, ...windowCounts(sums, t, 6).map((c) => c.expected));
    expect(maxWindowExpected(sums, 6)).toBeCloseTo(trailing, 9);
  });

  it("keeps the full window's prior for every frame and counts pages published", () => {
    const m = buildSkew({
      spec: { unit: "month", from: "1880-01-01", to: "1882-06-30" },
      nationalHits: nh,
      nationalPages: np,
      cells,
      places: 20,
      inFit: [],
      stateOf: Array.from({ length: 20 }, (_, i) => i % 4),
      states: 4,
      level: 0.9,
    });
    const f = scoreFrame(m, 4, 3);
    expect(f.places).toHaveLength(20);
    expect(f.states).toHaveLength(4);
    const pages0 = only(cells, 2, 5).pages.filter((_, i) => only(cells, 2, 5).p[i] === 0).reduce((a, x) => a + x, 0);
    expect(f.placePages[0]).toBe(pages0);
    expect(f.statePages.reduce((a, x) => a + x, 0)).toBe(f.placePages.reduce((a, x) => a + x, 0));
  });
});
