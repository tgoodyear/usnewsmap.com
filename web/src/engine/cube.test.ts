import { describe, expect, it } from "vitest";
import { alignCube, prefixSums, relative, windowQuantile, windowValues } from "./cube";

// A deterministic random sparse cube and a brute-force oracle.
function randomCube(places: number, buckets: number, seed: number) {
  let s = seed;
  const rnd = () => ((s = (s * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  const cube = { p: [] as number[], b: [] as number[], h: [] as number[] };
  const dense = Array.from({ length: places }, () => new Array<number>(buckets).fill(0));
  for (let p = 0; p < places; p++)
    for (let b = 0; b < buckets; b++)
      if (rnd() < 0.3) {
        const h = 1 + Math.floor(rnd() * 20);
        cube.p.push(p);
        cube.b.push(b);
        cube.h.push(h);
        dense[p]![b] = h;
      }
  return { cube, dense };
}

describe("prefix sums", () => {
  it("match brute force for every bucket and window", () => {
    const P = 7;
    const B = 25;
    const { cube, dense } = randomCube(P, B, 42);
    const ps = prefixSums(cube, P, B);
    for (let t = 0; t < B; t++) {
      for (const w of [null, 1, 3, 10, 100]) {
        const got = windowValues(ps, t, w);
        for (let p = 0; p < P; p++) {
          const lo = w === null ? 0 : Math.max(0, t - w + 1);
          const want = dense[p]!.slice(lo, t + 1).reduce((a, x) => a + x, 0);
          expect(got[p]).toBe(want);
        }
      }
    }
  });

  it("clamp t and ignore out-of-range cells", () => {
    const ps = prefixSums({ p: [0, 5, 0], b: [0, 0, 99], h: [2, 7, 9] }, 1, 3);
    expect(Array.from(windowValues(ps, 99, null))).toEqual([2]);
    expect(Array.from(windowValues(ps, -1, null))).toEqual([2]);
  });
});

describe("coverage alignment and relative frequency", () => {
  it("re-indexes onto the aggregate's places", () => {
    const aligned = alignCube(
      { p: [0, 1, 2, 2], b: [0, 0, 1, 2], h: [10, 20, 30, 40] },
      ["P3", "P1", "P2"],
      ["P1", "P2"],
    );
    expect(aligned).toEqual({ p: [0, 1, 1], b: [0, 1, 2], h: [20, 30, 40] });
  });

  it("divides safely", () => {
    const r = relative(new Float64Array([1, 2, 0]), new Float64Array([4, 0, 0]));
    expect(Array.from(r)).toEqual([0.25, 0, 0]);
  });
});

describe("quantiles (#127)", () => {
  // Place 0: 1 page in bucket 0, 2 in bucket 2, 1 in bucket 5. Place 1: none.
  const ps = prefixSums({ p: [0, 0, 0], b: [0, 2, 5], h: [1, 2, 1] }, 2, 6);

  it("finds the bucket where each share of a place's pages has appeared", () => {
    expect(Array.from(windowQuantile(ps, 5, null, 0.5))).toEqual([2, Number.NaN]);
    expect(windowQuantile(ps, 5, null, 0.25)[0]).toBe(0);
    expect(windowQuantile(ps, 5, null, 0.75)[0]).toBe(2);
    expect(windowQuantile(ps, 5, null, 1)[0]).toBe(5);
  });

  it("follows the window, as the page counts do", () => {
    // Up to bucket 1, only the first page.
    expect(windowQuantile(ps, 1, null, 0.5)[0]).toBe(0);
    // The trailing 3 buckets ending at 5: buckets 3..5 hold the last page.
    expect(windowQuantile(ps, 5, 3, 0.5)[0]).toBe(5);
    // A window with nothing in it.
    expect(windowQuantile(ps, 4, 2, 0.5)[0]).toBeNaN();
  });

  it("matches a brute-force median on random cubes", () => {
    let seed = 7;
    const rand = () => ((seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
    for (let trial = 0; trial < 50; trial++) {
      const buckets = 1 + Math.floor(rand() * 20);
      const counts = Array.from({ length: buckets }, () => (rand() < 0.5 ? 0 : Math.floor(rand() * 5)));
      const cube = { p: [] as number[], b: [] as number[], h: [] as number[] };
      counts.forEach((h, b) => h > 0 && (cube.p.push(0), cube.b.push(b), cube.h.push(h)));
      const got = windowQuantile(prefixSums(cube, 1, buckets), buckets - 1, null, 0.5)[0]!;
      const total = counts.reduce((a, c) => a + c, 0);
      let want = Number.NaN;
      for (let b = 0, run = 0; b < buckets && total > 0; b++) {
        run += counts[b]!;
        if (run >= total / 2) {
          want = b;
          break;
        }
      }
      expect(got).toEqual(want);
    }
  });
});
