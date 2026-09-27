import { describe, expect, it } from "vitest";
import { alignCube, prefixSums, relative, windowValues } from "./cube";

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
