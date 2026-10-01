//! Shared test vectors for the scoring in `usnm_core::skew` and its browser
//! port (`web/src/engine/skew.ts`, checked by `web/src/engine/skew.test.ts`).
//!
//! `fixtures/skew-vectors.json` holds deterministic inputs, the scores this
//! crate gives them, and the tolerances both suites allow. This test
//! recomputes every expected value and compares; with
//! `UPDATE_SKEW_VECTORS=1` it rewrites the file instead.

use std::path::PathBuf;

use serde_json::{json, Value};
use usnm_core::skew::{
    gamma_p, gamma_quantile, ln_gamma, normal_quantile, place_counts, score_groups, score_search,
    Cell, Counts, Prior, Score,
};
use usnm_core::time::{BucketSpec, BucketUnit};

const LEVEL: f64 = 0.9;

/// Allowed differences, `|actual - expected| <= abs + rel * |expected|`.
/// The browser's `Math.exp` and `Math.log` may differ from the platform's
/// libm in the last bit, and the prior's search stops at a tolerance, so the
/// fitted prior and everything after it get a little room.
fn tolerances() -> Value {
    json!({
        "counts": { "rel": 1e-12, "abs": 1e-12 },
        "phi": { "rel": 1e-10, "abs": 0.0 },
        "prior": { "rel": 1e-5, "abs": 1e-9 },
        "score": { "rel": 1e-6, "abs": 1e-12 },
        "above": { "rel": 0.0, "abs": 1e-6 },
        "special": { "rel": 1e-12, "abs": 1e-300 }
    })
}

/// splitmix64, so the inputs are the same on every machine.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        let (u, v) = (self.uniform(), self.uniform());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }

    fn poisson(&mut self, mut lambda: f64) -> u64 {
        let mut total = 0;
        while lambda > 0.0 {
            let l = lambda.min(500.0);
            lambda -= l;
            let u = self.uniform();
            let (mut k, mut p) = (0u64, (-l).exp());
            let mut cdf = p;
            while cdf < u && k < 100_000 {
                k += 1;
                p *= l / k as f64;
                cdf += p;
            }
            total += k;
        }
        total
    }

    /// Gamma(shape, 1), shape >= 1 (Marsaglia and Tsang).
    fn gamma(&mut self, shape: f64) -> f64 {
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();
        loop {
            let x = self.normal();
            let v = (1.0 + c * x).powi(3);
            if v <= 0.0 {
                continue;
            }
            if self.uniform().ln() < 0.5 * x * x + d - d * v + d * v.ln() {
                return d * v;
            }
        }
    }
}

fn day(s: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// One search's inputs.
struct Case {
    name: &'static str,
    unit: BucketUnit,
    from: &'static str,
    to: &'static str,
    cells: Vec<Cell>,
    places: usize,
    in_fit: Vec<bool>,
    state_of: Vec<usize>,
    states: usize,
    /// Playback frames: (t, trailing window or None for cumulative).
    windows: Vec<(usize, Option<usize>)>,
}

impl Case {
    fn spec(&self) -> BucketSpec {
        BucketSpec::new(self.unit, day(self.from), day(self.to))
    }

    fn national(&self) -> (Vec<u64>, Vec<u64>) {
        let n = self.spec().len();
        let (mut h, mut p) = (vec![0u64; n], vec![0u64; n]);
        for c in &self.cells {
            h[c.bucket] += c.hits;
            p[c.bucket] += c.pages;
        }
        (h, p)
    }
}

/// Places with heavy-tailed volumes, lifts from `lift`, interest drifting
/// over the window, items reprinted in runs of `run` pages, and some empty
/// buckets.
#[allow(clippy::too_many_arguments)]
fn simulate(
    rng: &mut Rng,
    places: usize,
    buckets: usize,
    rate: f64,
    log_pages: (f64, f64),
    run: u64,
    gaps: f64,
    lift: &mut dyn FnMut(&mut Rng, usize) -> f64,
) -> Vec<Cell> {
    let mut cells = Vec::new();
    for p in 0..places {
        let per_bucket = (log_pages.0 + log_pages.1 * rng.normal()).exp().max(1.0);
        let t = lift(rng, p);
        let phase = rng.uniform() * std::f64::consts::TAU;
        for b in 0..buckets {
            if rng.uniform() < gaps {
                continue;
            }
            let pages = (per_bucket * (0.5 + rng.uniform())).round().max(1.0) as u64;
            let season =
                1.0 + 0.3 * (std::f64::consts::TAU * b as f64 / buckets as f64 + phase).sin();
            let e = pages as f64 * rate * t * season;
            let hits = (run * rng.poisson(e / run as f64)).min(pages);
            cells.push(Cell {
                place: p,
                bucket: b,
                pages,
                hits,
            });
        }
    }
    cells
}

fn cases() -> Vec<Case> {
    let mut rng = Rng(2026);
    let mut out = Vec::new();

    // The epidemic-year example from the unit tests, with a bucket where one
    // place is the only publisher.
    out.push(Case {
        name: "epidemic_year_and_lone_publisher",
        unit: BucketUnit::Year,
        from: "1880-01-01",
        to: "1882-12-31",
        cells: vec![
            Cell {
                place: 0,
                bucket: 0,
                pages: 100,
                hits: 10,
            },
            Cell {
                place: 1,
                bucket: 1,
                pages: 90_000,
                hits: 90,
            },
            Cell {
                place: 2,
                bucket: 0,
                pages: 10_000,
                hits: 1_000,
            },
            Cell {
                place: 2,
                bucket: 1,
                pages: 1_000,
                hits: 1,
            },
            Cell {
                place: 2,
                bucket: 2,
                pages: 50,
                hits: 50,
            },
        ],
        places: 3,
        in_fit: vec![true; 3],
        state_of: vec![0, 0, 1],
        states: 2,
        windows: vec![(2, None), (0, None), (2, Some(1))],
    });

    // Hits in only one place: the continuity correction.
    out.push(Case {
        name: "hits_only_here",
        unit: BucketUnit::Day,
        from: "1896-07-01",
        to: "1896-07-02",
        cells: vec![
            Cell {
                place: 0,
                bucket: 0,
                pages: 100,
                hits: 5,
            },
            Cell {
                place: 1,
                bucket: 0,
                pages: 10_000,
                hits: 0,
            },
            Cell {
                place: 1,
                bucket: 1,
                pages: 9_000,
                hits: 0,
            },
            Cell {
                place: 2,
                bucket: 1,
                pages: 40,
                hits: 0,
            },
        ],
        places: 3,
        in_fit: vec![true; 3],
        state_of: vec![0, 1, 1],
        states: 2,
        windows: vec![(1, None), (1, Some(1))],
    });

    // Six years by month: places that differ, reprinting, drifting interest,
    // gaps, and four places printed in another language (lift near 0, left
    // out of the fit). Eight states.
    let cells = simulate(
        &mut rng,
        80,
        72,
        0.003,
        (5.0, 1.6),
        3,
        0.15,
        &mut |rng, p| {
            if p % 20 == 7 {
                0.05
            } else {
                rng.gamma(2.0) / 2.0
            }
        },
    );
    out.push(Case {
        name: "monthly_differences_reprints_languages",
        unit: BucketUnit::Month,
        from: "1880-01-01",
        to: "1885-12-31",
        cells,
        places: 80,
        in_fit: (0..80).map(|p| p % 20 != 7).collect(),
        state_of: (0..80).map(|p| p % 8).collect(),
        states: 8,
        windows: vec![
            (71, None),
            (40, None),
            (71, Some(12)),
            (30, Some(6)),
            (0, None),
        ],
    });

    // Half a year by week (dispersion measured by calendar month, weeks that
    // cross a month go to the earlier one).
    let cells = simulate(&mut rng, 30, 31, 0.01, (4.0, 1.0), 2, 0.1, &mut |rng, _| {
        rng.gamma(4.0) / 4.0
    });
    out.push(Case {
        name: "weekly_half_year",
        unit: BucketUnit::Week,
        from: "1896-06-01",
        to: "1896-12-31",
        cells,
        places: 30,
        in_fit: vec![true; 30],
        state_of: (0..30).map(|p| p % 5).collect(),
        states: 5,
        windows: vec![(30, None), (20, Some(4))],
    });

    // Two months by day with few hits: dispersion falls back.
    let cells = simulate(&mut rng, 12, 60, 0.002, (2.5, 0.8), 1, 0.3, &mut |_, _| 1.0);
    out.push(Case {
        name: "daily_few_hits_fallback",
        unit: BucketUnit::Day,
        from: "1896-07-01",
        to: "1896-08-29",
        cells,
        places: 12,
        in_fit: vec![true; 12],
        state_of: (0..12).map(|p| p % 3).collect(),
        states: 3,
        windows: vec![(59, None), (59, Some(7))],
    });

    // No difference between places: alpha at its bound, so the posterior
    // shapes are large (the Wilson and Hilferty path).
    let cells = simulate(
        &mut rng,
        200,
        20,
        0.004,
        (6.0, 1.2),
        1,
        0.05,
        &mut |_, _| 1.0,
    );
    out.push(Case {
        name: "no_difference_by_year",
        unit: BucketUnit::Year,
        from: "1850-01-01",
        to: "1869-12-31",
        cells,
        places: 200,
        in_fit: vec![true; 200],
        state_of: (0..200).map(|p| p % 42).collect(),
        states: 42,
        windows: vec![(19, None), (12, Some(5))],
    });

    // The three-year boundary of the dispersion resolution (the end date is
    // inclusive; a month added to 29 February ends on the 28th).
    for (name, from, to) in [
        ("exactly_three_years_by_month", "1880-01-01", "1882-12-31"),
        (
            "a_day_short_of_three_years_by_month",
            "1880-01-02",
            "1882-12-31",
        ),
        ("three_years_from_a_leap_day", "1884-02-29", "1887-02-27"),
        ("a_day_short_from_a_leap_day", "1884-02-29", "1887-02-26"),
    ] {
        let spec = BucketSpec::new(BucketUnit::Month, day(from), day(to));
        let cells = simulate(
            &mut rng,
            25,
            spec.len(),
            0.004,
            (6.0, 1.0),
            2,
            0.0,
            &mut |rng, _| rng.gamma(3.0) / 3.0,
        );
        let last = spec.len() - 1;
        out.push(Case {
            name,
            unit: BucketUnit::Month,
            from,
            to,
            cells,
            places: 25,
            in_fit: vec![true; 25],
            state_of: (0..25).map(|p| p % 4).collect(),
            states: 4,
            windows: vec![(last, None)],
        });
    }
    out
}

fn score_json(s: &Score) -> Value {
    json!({
        "observed": s.observed,
        "expected": s.expected,
        "lift": s.lift,
        "estimate": s.estimate,
        "lower": s.lower,
        "upper": s.upper,
        "above": s.above,
    })
}

fn prior_json(p: &Prior) -> Value {
    json!({ "alpha": p.alpha, "mean": p.mean, "mean_var": p.mean_var, "shape": p.shape() })
}

fn cells_json(cells: &[Cell]) -> Value {
    let mut sorted = cells.to_vec();
    sorted.sort_by_key(|x| (x.place, x.bucket));
    json!({
        "p": sorted.iter().map(|x| x.place).collect::<Vec<_>>(),
        "b": sorted.iter().map(|x| x.bucket).collect::<Vec<_>>(),
        "pages": sorted.iter().map(|x| x.pages).collect::<Vec<_>>(),
        "hits": sorted.iter().map(|x| x.hits).collect::<Vec<_>>(),
    })
}

fn input_json(c: &Case) -> Value {
    let (hits, pages) = c.national();
    json!({
        "unit": c.unit.as_str(),
        "from": c.from,
        "to": c.to,
        "level": LEVEL,
        "national_hits": hits,
        "national_pages": pages,
        "cells": cells_json(&c.cells),
        "places": c.places,
        "in_fit": c.in_fit,
        "state_of": c.state_of,
        "states": c.states,
        "windows": c.windows.iter().map(|&(t, w)| json!({ "t": t, "win": w })).collect::<Vec<_>>(),
    })
}

fn u64s(v: &Value) -> Vec<u64> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_u64().unwrap())
        .collect()
}

fn usizes(v: &Value) -> Vec<usize> {
    u64s(v).into_iter().map(|x| x as usize).collect()
}

/// What the reference computes for one case's inputs.
fn expected_json(input: &Value) -> Value {
    let unit = BucketUnit::parse(input["unit"].as_str().unwrap()).unwrap();
    let spec = BucketSpec::new(
        unit,
        day(input["from"].as_str().unwrap()),
        day(input["to"].as_str().unwrap()),
    );
    let level = input["level"].as_f64().unwrap();
    let hits = u64s(&input["national_hits"]);
    let pages = u64s(&input["national_pages"]);
    let c = &input["cells"];
    let (cp, cb, cn, ch) = (
        usizes(&c["p"]),
        usizes(&c["b"]),
        u64s(&c["pages"]),
        u64s(&c["hits"]),
    );
    let cells: Vec<Cell> = (0..cp.len())
        .map(|i| Cell {
            place: cp[i],
            bucket: cb[i],
            pages: cn[i],
            hits: ch[i],
        })
        .collect();
    let places = input["places"].as_u64().unwrap() as usize;
    let in_fit: Vec<bool> = input["in_fit"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_bool().unwrap())
        .collect();
    let state_of = usizes(&input["state_of"]);
    let states = input["states"].as_u64().unwrap() as usize;
    let scored = score_search(&spec, &hits, &pages, &cells, places, &in_fit, level);
    let (state_prior, state_scores) =
        score_groups(&hits, &pages, &cells, &state_of, states, scored.phi, level);
    // A playback frame: the buckets in the window, scored with the full
    // window's phi and prior (doc 11, 11.6).
    let windows: Vec<Value> = input["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            let t = w["t"].as_u64().unwrap() as usize;
            let lo = w["win"]
                .as_u64()
                .map_or(0, |n| (t + 1).saturating_sub(n as usize));
            let cells: Vec<Cell> = cells
                .iter()
                .filter(|x| x.bucket >= lo && x.bucket <= t)
                .copied()
                .collect();
            let counts: Vec<Counts> = place_counts(&hits, &pages, &cells, places);
            let scores: Vec<Value> = counts
                .iter()
                .map(|&k| score_json(&scored.prior.score(k, scored.phi, level)))
                .collect();
            json!({ "places": scores })
        })
        .collect();
    json!({
        "phi": scored.phi,
        "phi_places": scored.dispersion.places,
        "phi_measured": scored.dispersion.phi.is_some(),
        "prior": prior_json(&scored.prior),
        "places": scored.scores.iter().map(score_json).collect::<Vec<_>>(),
        "states": {
            "prior": prior_json(&state_prior),
            "scores": state_scores.iter().map(score_json).collect::<Vec<_>>(),
        },
        "windows": windows,
    })
}

/// Searches recorded from the public API for doc 11 (`/tmp/termskew` in
/// 11.5.1), reduced to the scoring inputs. With `SKEW_RECORDED_DIR` set the
/// generator reads them from there; otherwise it keeps the stored ones.
const RECORDED: [&str; 2] = ["cross_of_gold", "klondike"];

fn recorded_input(dir: &std::path::Path, name: &str) -> Value {
    let load = |f: String| -> Value {
        serde_json::from_slice(&std::fs::read(dir.join(&f)).unwrap_or_else(|e| panic!("{f}: {e}")))
            .unwrap()
    };
    let (agg, cov, places) = (
        load(format!("agg_{name}.json")),
        load(format!("cov_{name}.json")),
        load("places.json".into()),
    );
    let state_by_id: std::collections::HashMap<&str, &str> = places["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["id"].as_str().unwrap(),
                f["properties"]["state"].as_str().unwrap(),
            )
        })
        .collect();
    let ids: Vec<&str> = cov["places"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let index: std::collections::HashMap<&str, usize> =
        ids.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let agg_ids: Vec<&str> = agg["places"]["id"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    let mut cube: std::collections::HashMap<(usize, usize), u64> = Default::default();
    let (qp, qb, qh) = (
        usizes(&agg["cube"]["p"]),
        usizes(&agg["cube"]["b"]),
        u64s(&agg["cube"]["h"]),
    );
    for i in 0..qh.len() {
        *cube.entry((index[agg_ids[qp[i]]], qb[i])).or_default() += qh[i];
    }
    let (cp, cb, ch) = (
        usizes(&cov["pages"]["p"]),
        usizes(&cov["pages"]["b"]),
        u64s(&cov["pages"]["h"]),
    );
    let cells: Vec<Cell> = (0..ch.len())
        .map(|i| Cell {
            place: cp[i],
            bucket: cb[i],
            pages: ch[i],
            hits: cube.remove(&(cp[i], cb[i])).unwrap_or(0),
        })
        .collect();
    assert!(cube.is_empty());
    let mut state_names: Vec<&str> = ids.iter().map(|id| state_by_id[id]).collect();
    state_names.sort_unstable();
    state_names.dedup();
    let state_of: Vec<usize> = ids
        .iter()
        .map(|id| state_names.binary_search(&state_by_id[id]).unwrap())
        .collect();
    let count = agg["bucket"]["count"].as_u64().unwrap() as usize;
    json!({
        "unit": agg["bucket"]["unit"],
        "from": agg["bucket"]["from"],
        "to": agg["bucket"]["to"],
        "level": LEVEL,
        "national_hits": agg["series"]["hits"],
        "national_pages": agg["series"]["baseline"],
        "cells": cells_json(&cells),
        "places": ids.len(),
        "in_fit": vec![true; ids.len()],
        "state_of": state_of,
        "states": state_names.len(),
        "windows": [
            { "t": count - 1, "win": null },
            { "t": count / 2, "win": null },
            { "t": count - 1, "win": 4 },
        ],
    })
}

fn special_json() -> Value {
    let xs = [0.001, 0.1, 0.5, 1.0, 2.5, 7.0, 33.3, 100.0, 1_234.5, 1e5];
    let pairs = [
        (0.5, 1.920_729_410_5),
        (1.0, 2.995_732_273_5),
        (5.0, 9.153_519_025),
        (50.0, 62.171_056_7),
        (0.01, 0.003),
        (3.0, 0.5),
        (40.0, 55.0),
        (999.0, 1_010.0),
        (9_999.0, 9_900.0),
        (2e4, 2.01e4),
    ];
    let quantiles = [
        (0.05, 0.01),
        (0.5, 0.01),
        (0.95, 0.05),
        (0.05, 0.5),
        (0.5, 1.0),
        (0.95, 3.0),
        (0.05, 40.0),
        (0.5, 999.0),
        (0.95, 9_999.0),
        (0.05, 1.5e4),
        (0.5, 2e4),
        (0.95, 1e6),
    ];
    let ps = [1e-6, 0.001, 0.02, 0.05, 0.3, 0.5, 0.7, 0.95, 0.98, 0.999];
    json!({
        "ln_gamma": xs.iter().map(|&x| json!([x, ln_gamma(x)])).collect::<Vec<_>>(),
        "gamma_p": pairs.iter().map(|&(a, x)| json!([a, x, gamma_p(a, x)])).collect::<Vec<_>>(),
        "gamma_quantile": quantiles.iter().map(|&(p, a)| json!([p, a, gamma_quantile(p, a)])).collect::<Vec<_>>(),
        "normal_quantile": ps.iter().map(|&p| json!([p, normal_quantile(p)])).collect::<Vec<_>>(),
    })
}

/// Every case's name and inputs: generated, then recorded.
fn inputs(stored: Option<&Value>) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = cases()
        .iter()
        .map(|c| (c.name.to_owned(), input_json(c)))
        .collect();
    let dir = std::env::var_os("SKEW_RECORDED_DIR").map(PathBuf::from);
    for name in RECORDED {
        let input = match (&dir, stored) {
            (Some(dir), _) => recorded_input(dir, name),
            (None, Some(stored)) => stored["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == name)
                .unwrap_or_else(|| panic!("{name} isn't stored; set SKEW_RECORDED_DIR"))["input"]
                .clone(),
            (None, None) => panic!("set SKEW_RECORDED_DIR to record {name}"),
        };
        out.push((name.to_owned(), input));
    }
    out
}

fn vectors(stored: Option<&Value>) -> Value {
    let cases: Vec<Value> = inputs(stored)
        .into_iter()
        .map(|(name, input)| json!({ "name": name, "expected": expected_json(&input), "input": input }))
        .collect();
    json!({
        "about": "Inputs and expected scores for usnm_core::skew and web/src/engine/skew.ts. Generated by crates/usnm-core/tests/skew_vectors.rs; regenerate with UPDATE_SKEW_VECTORS=1 cargo test -p usnm-core --test skew_vectors.",
        "tolerances": tolerances(),
        "special": special_json(),
        "cases": cases,
    })
}

/// One case per line, so a diff shows which case changed.
fn render(v: &Value) -> String {
    let mut s = String::from("{\n");
    let obj = v.as_object().expect("object");
    let keys = ["about", "tolerances", "special"];
    for k in keys {
        s += &format!("  {}: {},\n", json!(k), obj[k]);
    }
    s += "  \"cases\": [\n";
    let cases = obj["cases"].as_array().expect("cases");
    for (i, c) in cases.iter().enumerate() {
        s += &format!("    {}{}\n", c, if i + 1 < cases.len() { "," } else { "" });
    }
    s += "  ]\n}\n";
    s
}

fn path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/skew-vectors.json")
}

fn close(actual: f64, expected: f64, tol: &Value) -> bool {
    let (rel, abs) = (tol["rel"].as_f64().unwrap(), tol["abs"].as_f64().unwrap());
    actual == expected || (actual - expected).abs() <= abs + rel * expected.abs()
}

/// Compare `actual` with `expected` (both JSON), numbers within `tol`.
fn check(path: &str, actual: &Value, expected: &Value, tol: &Value, failures: &mut Vec<String>) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(e)) => {
            let (a, e) = (a.as_f64().unwrap(), e.as_f64().unwrap());
            if !close(a, e, tol) {
                failures.push(format!("{path}: {a} vs {e}"));
            }
        }
        (Value::Array(a), Value::Array(e)) if a.len() == e.len() => {
            for (i, (x, y)) in a.iter().zip(e).enumerate() {
                check(&format!("{path}[{i}]"), x, y, tol, failures);
            }
        }
        (a, e) if a == e => {}
        (a, e) => failures.push(format!("{path}: {a} vs {e}")),
    }
}

fn check_score(path: &str, a: &Value, e: &Value, tols: &Value, failures: &mut Vec<String>) {
    for k in ["observed", "expected", "lift"] {
        check(
            &format!("{path}.{k}"),
            &a[k],
            &e[k],
            &tols["counts"],
            failures,
        );
    }
    for k in ["estimate", "lower", "upper"] {
        check(
            &format!("{path}.{k}"),
            &a[k],
            &e[k],
            &tols["score"],
            failures,
        );
    }
    check(
        &format!("{path}.above"),
        &a["above"],
        &e["above"],
        &tols["above"],
        failures,
    );
}

fn stored() -> Option<Value> {
    let text = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&text).ok()
}

#[test]
fn shared_vectors_match_the_reference() {
    if std::env::var_os("UPDATE_SKEW_VECTORS").is_some() {
        let old = stored();
        std::fs::write(path(), render(&vectors(old.as_ref()))).expect("write vectors");
        return;
    }
    let text = std::fs::read_to_string(path()).expect("fixtures/skew-vectors.json");
    let stored: Value = serde_json::from_str(&text).expect("valid JSON");
    let fresh = vectors(Some(&stored));
    let tols = &stored["tolerances"];
    assert_eq!(tols, &fresh["tolerances"], "tolerances changed: regenerate");
    let mut failures = Vec::new();
    for k in ["ln_gamma", "gamma_p", "gamma_quantile", "normal_quantile"] {
        check(
            k,
            &fresh["special"][k],
            &stored["special"][k],
            &tols["special"],
            &mut failures,
        );
    }
    let (a, e) = (
        fresh["cases"].as_array().unwrap(),
        stored["cases"].as_array().unwrap(),
    );
    assert_eq!(a.len(), e.len(), "number of cases");
    for (a, e) in a.iter().zip(e) {
        let name = e["name"].as_str().unwrap();
        assert_eq!(a["name"], e["name"]);
        assert_eq!(a["input"], e["input"], "{name}: inputs changed: regenerate");
        compare_case(name, &a["expected"], &e["expected"], tols, &mut failures);
    }
    assert!(
        failures.is_empty(),
        "{} differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

fn compare_case(name: &str, ax: &Value, ex: &Value, tols: &Value, failures: &mut Vec<String>) {
    check(
        &format!("{name}.phi"),
        &ax["phi"],
        &ex["phi"],
        &tols["phi"],
        failures,
    );
    for k in ["phi_places", "phi_measured"] {
        check(
            &format!("{name}.{k}"),
            &ax[k],
            &ex[k],
            &tols["counts"],
            failures,
        );
    }
    for (pa, pe, label) in [
        (&ax["prior"], &ex["prior"], "prior"),
        (
            &ax["states"]["prior"],
            &ex["states"]["prior"],
            "states.prior",
        ),
    ] {
        // alpha is compared as 1 / alpha, the prior's squared coefficient of
        // variation: near its bound the likelihood is flat in alpha, and
        // only its inverse matters to the scores.
        let inv = |v: &Value| json!(1.0 / v["alpha"].as_f64().unwrap());
        check(
            &format!("{name}.{label}.1/alpha"),
            &inv(pa),
            &inv(pe),
            &tols["prior"],
            failures,
        );
        for k in ["mean", "mean_var", "shape"] {
            check(
                &format!("{name}.{label}.{k}"),
                &pa[k],
                &pe[k],
                &tols["prior"],
                failures,
            );
        }
    }
    let lists = [
        ("places", &ax["places"], &ex["places"]),
        ("states", &ax["states"]["scores"], &ex["states"]["scores"]),
    ];
    for (label, la, le) in lists {
        let (la, le) = (la.as_array().unwrap(), le.as_array().unwrap());
        assert_eq!(la.len(), le.len(), "{name}.{label}");
        for (i, (x, y)) in la.iter().zip(le).enumerate() {
            check_score(&format!("{name}.{label}[{i}]"), x, y, tols, failures);
        }
    }
    let (wa, we) = (
        ax["windows"].as_array().unwrap(),
        ex["windows"].as_array().unwrap(),
    );
    assert_eq!(wa.len(), we.len(), "{name}.windows");
    for (j, (w, v)) in wa.iter().zip(we).enumerate() {
        let (la, le) = (
            w["places"].as_array().unwrap(),
            v["places"].as_array().unwrap(),
        );
        assert_eq!(la.len(), le.len(), "{name}.windows[{j}]");
        for (i, (x, y)) in la.iter().zip(le).enumerate() {
            check_score(&format!("{name}.windows[{j}][{i}]"), x, y, tols, failures);
        }
    }
}

#[test]
fn vectors_cover_what_the_view_needs() {
    // The cases exercise the paths the port must get right.
    let stored = stored().expect("fixtures/skew-vectors.json");
    let all: Vec<Value> = stored["cases"].as_array().unwrap().clone();
    let phi_measured = |v: &Value| v["expected"]["phi_measured"].as_bool().unwrap();
    assert!(all.iter().any(phi_measured), "a measured phi");
    assert!(all.iter().any(|v| !phi_measured(v)), "the fallback phi");
    assert!(
        all.iter()
            .any(|v| v["expected"]["prior"]["alpha"].as_f64().unwrap() > 9e5),
        "alpha at its bound"
    );
    assert!(
        all.iter()
            .flat_map(|v| v["expected"]["places"].as_array().unwrap().clone())
            .any(|s| s["lift"].is_null()),
        "a place with nothing expected"
    );
    let dirs: Vec<i64> = all
        .iter()
        .flat_map(|v| v["expected"]["places"].as_array().unwrap().clone())
        .map(|s| {
            let (lo, hi) = (s["lower"].as_f64().unwrap(), s["upper"].as_f64().unwrap());
            if lo > 1.0 {
                1
            } else if hi < 1.0 {
                -1
            } else {
                0
            }
        })
        .collect();
    for d in [-1, 0, 1] {
        assert!(dirs.contains(&d), "a place in direction {d}");
    }
    // Both sides of the three-year boundary, and the recorded searches.
    let names: Vec<&str> = all.iter().map(|v| v["name"].as_str().unwrap()).collect();
    for n in [
        "exactly_three_years_by_month",
        "a_day_short_of_three_years_by_month",
        "cross_of_gold",
        "klondike",
    ] {
        assert!(names.contains(&n), "{n}");
    }
}
