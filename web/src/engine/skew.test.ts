/// <reference types="node" />
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  direction,
  gammaP,
  gammaQuantile,
  lnGamma,
  normalQuantile,
  priorShape,
  scoreGroups,
  scoreSearch,
  type Prior,
  type Score,
} from "./skew";
import { buildSkew, scoreFrame } from "./skewModel";

// Shared test vectors (fixtures/skew-vectors.json), generated and checked by
// crates/usnm-core/tests/skew_vectors.rs: the browser port must give the
// scores the Rust reference gives, within the file's tolerances.

interface Tol {
  rel: number;
  abs: number;
}
interface StoredScore {
  observed: number;
  expected: number;
  lift: number | null;
  estimate: number;
  lower: number;
  upper: number;
  above: number;
}
interface StoredPrior {
  alpha: number;
  mean: number;
  mean_var: number;
  shape: number;
}
interface Case {
  name: string;
  input: {
    unit: "year" | "month" | "week" | "day";
    from: string;
    to: string;
    level: number;
    national_hits: number[];
    national_pages: number[];
    cells: { p: number[]; b: number[]; pages: number[]; hits: number[] };
    places: number;
    in_fit: boolean[];
    state_of: number[];
    states: number;
    windows: { t: number; win: number | null }[];
  };
  expected: {
    phi: number;
    phi_places: number;
    phi_measured: boolean;
    prior: StoredPrior;
    places: StoredScore[];
    states: { phi: number; prior: StoredPrior; scores: StoredScore[] };
    windows: { places: StoredScore[]; states: StoredScore[] }[];
  };
}
interface Vectors {
  tolerances: Record<"counts" | "phi" | "prior" | "score" | "above" | "special", Tol>;
  special: {
    ln_gamma: [number, number][];
    gamma_p: [number, number, number][];
    gamma_quantile: [number, number, number][];
    normal_quantile: [number, number][];
  };
  cases: Case[];
}

const vectors = JSON.parse(
  // Tests run from web/.
  readFileSync(resolve(process.cwd(), "../fixtures/skew-vectors.json"), "utf8"),
) as Vectors;
const tol = vectors.tolerances;

function near(actual: number | null, expected: number | null, t: Tol, what: string) {
  if (actual === expected) return;
  if (actual === null || expected === null) throw new Error(`${what}: ${actual} vs ${expected}`);
  const ok = Math.abs(actual - expected) <= t.abs + t.rel * Math.abs(expected);
  if (!ok) throw new Error(`${what}: ${actual} vs ${expected} (diff ${Math.abs(actual - expected)})`);
}

function checkScore(a: Score, e: StoredScore, what: string) {
  near(a.observed, e.observed, tol.counts, `${what}.observed`);
  near(a.expected, e.expected, tol.counts, `${what}.expected`);
  near(a.lift, e.lift, tol.counts, `${what}.lift`);
  near(a.estimate, e.estimate, tol.score, `${what}.estimate`);
  near(a.lower, e.lower, tol.score, `${what}.lower`);
  near(a.upper, e.upper, tol.score, `${what}.upper`);
  near(a.above, e.above, tol.above, `${what}.above`);
}

function checkPrior(a: Prior, e: StoredPrior, what: string) {
  // As 1 / alpha: near its bound the likelihood is flat in alpha (see skew_vectors.rs).
  near(1 / a.alpha, 1 / e.alpha, tol.prior, `${what}.1/alpha`);
  near(a.mean, e.mean, tol.prior, `${what}.mean`);
  near(a.meanVar, e.mean_var, tol.prior, `${what}.mean_var`);
  near(priorShape(a), e.shape, tol.prior, `${what}.shape`);
}

describe("shared vectors: special functions", () => {
  it("ln_gamma, gamma_p, gamma_quantile and normal_quantile match the reference", () => {
    for (const [x, v] of vectors.special.ln_gamma) near(lnGamma(x), v, tol.special, `lnGamma(${x})`);
    for (const [a, x, v] of vectors.special.gamma_p) near(gammaP(a, x), v, tol.special, `gammaP(${a}, ${x})`);
    for (const [p, a, v] of vectors.special.gamma_quantile)
      near(gammaQuantile(p, a), v, tol.special, `gammaQuantile(${p}, ${a})`);
    for (const [p, v] of vectors.special.normal_quantile) near(normalQuantile(p), v, tol.special, `normalQuantile(${p})`);
  });
});

describe("shared vectors: searches", () => {
  it("covers the cases the Rust generator writes", () => {
    expect(vectors.cases.length).toBeGreaterThanOrEqual(12);
  });

  for (const c of vectors.cases) {
    const i = c.input;
    const spec = { unit: i.unit, from: i.from, to: i.to };

    it(`${c.name}: scoreSearch and scoreGroups match score_search and score_groups`, () => {
      const s = scoreSearch(spec, i.national_hits, i.national_pages, i.cells, i.places, i.in_fit, i.level);
      near(s.phi, c.expected.phi, tol.phi, "phi");
      expect(s.dispersion.places).toBe(c.expected.phi_places);
      expect(s.dispersion.phi !== null).toBe(c.expected.phi_measured);
      checkPrior(s.prior, c.expected.prior, "prior");
      expect(s.scores).toHaveLength(c.expected.places.length);
      s.scores.forEach((x, k) => checkScore(x, c.expected.places[k]!, `places[${k}]`));
      const g = scoreGroups(spec, i.national_hits, i.national_pages, i.cells, i.state_of, i.states, s.phi, i.level);
      near(g.phi, c.expected.states.phi, tol.phi, "states.phi");
      checkPrior(g.prior, c.expected.states.prior, "states.prior");
      g.scores.forEach((x, k) => checkScore(x, c.expected.states.scores[k]!, `states[${k}]`));
    });

    it(`${c.name}: playback frames from prefix sums match`, () => {
      const model = buildSkew({
        spec,
        nationalHits: i.national_hits,
        nationalPages: i.national_pages,
        cells: i.cells,
        places: i.places,
        inFit: i.in_fit,
        stateOf: i.state_of,
        states: i.states,
        level: i.level,
      });
      checkPrior(model.prior, c.expected.prior, "prior");
      near(model.statePhi, c.expected.states.phi, tol.phi, "states.phi");
      checkPrior(model.statePrior, c.expected.states.prior, "states.prior");
      // The full window is the search's own scores.
      const full = scoreFrame(model, i.national_hits.length - 1, null);
      full.places.forEach((x, k) => checkScore(x, c.expected.places[k]!, `full.places[${k}]`));
      full.states.forEach((x, k) => checkScore(x, c.expected.states.scores[k]!, `full.states[${k}]`));
      i.windows.forEach((w, j) => {
        const f = scoreFrame(model, w.t, w.win);
        f.places.forEach((x, k) => checkScore(x, c.expected.windows[j]!.places[k]!, `windows[${j}][${k}]`));
        f.states.forEach((x, k) => checkScore(x, c.expected.windows[j]!.states[k]!, `windows[${j}].states[${k}]`));
      });
    });
  }

  it("the cases exercise every direction", () => {
    const dirs = new Set(
      vectors.cases.flatMap((c) =>
        c.expected.places.map((s) => direction({ ...s, lift: s.lift }) as number),
      ),
    );
    expect([...dirs].sort()).toEqual([-1, 0, 1]);
  });
});
