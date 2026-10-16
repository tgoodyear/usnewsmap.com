// @vitest-environment node
import { expect, it } from "vitest";
import type { AggregateResponse, CoverageResponse, PlaceFeature, SparseCube } from "../api/types";
import { prepareStates, stateTotals, stateBreaks, stateColor, STATE_CODES } from "./states";

it.skipIf(process.env.USNM_STATES_PERF !== "1")(
  "scores a synthetic 5,000-place search within the 2 ms p95 frame budget",
  () => {
    const places = 5000;
    const buckets = 120;
    const ids = Array.from({ length: places }, (_, i) => `synthetic-${i}`);
    const features = new Map(
      ids.map((id, i) => [
        id,
        {
          properties: { state: STATE_CODES[i % (STATE_CODES.length - 1)] },
        } as PlaceFeature,
      ]),
    );
    const hits: SparseCube = { p: [], b: [], h: [] };
    const pages: SparseCube = { p: [], b: [], h: [] };
    for (let p = 0; p < places; p++)
      for (let b = 0; b < buckets; b++) {
        hits.p.push(p);
        hits.b.push(b);
        hits.h.push((p + b) % 5);
        pages.p.push(p);
        pages.b.push(b);
        pages.h.push(100 + (p % 20));
      }
    const data = {
      index_version: "synthetic-performance",
      bucket: { unit: "month", from: "1890-01-01", to: "1899-12-31", count: buckets },
      places: { id: ids },
      cube: hits,
    } as AggregateResponse;
    const coverage: CoverageResponse = {
      index_version: data.index_version,
      bucket: "month",
      from: data.bucket.from,
      to: data.bucket.to,
      count: buckets,
      places: ids,
      pages,
    };
    const start = performance.now();
    const sums = prepareStates(data, coverage, features)!;
    const prepareMs = performance.now() - start;
    const frame = (i: number) => {
      const rows = stateTotals(sums, i % buckets, i % 2 ? 12 : null);
      const breaks = stateBreaks(rows);
      for (const row of rows) stateColor(row, breaks);
      return rows.reduce((n, row) => n + row.hits, 0);
    };
    for (let i = 0; i < 200; i++) frame(i);
    const timings: number[] = [];
    let checksum = 0;
    for (let i = 0; i < 2000; i++) {
      const start = performance.now();
      checksum += frame(i);
      timings.push(performance.now() - start);
    }
    timings.sort((a, b) => a - b);
    const p95 = timings[Math.floor(timings.length * 0.95)]!;
    console.log(
      JSON.stringify({
        places,
        buckets,
        cellsPerCube: hits.h.length,
        frames: timings.length,
        prepareMs,
        medianMs: timings[1000],
        p95Ms: p95,
        maxMs: timings.at(-1),
        node: process.version,
        arch: process.arch,
        checksum,
      }),
    );
    expect(checksum).toBeGreaterThan(0);
    expect(p95).toBeLessThanOrEqual(2);
  },
);
