//! Replays validator rules over the retained results (#1420), offline.
//!
//! Reads what `check --keep` retained and writes one JSON report: how each
//! rule judges every pair of replicas of a job, how it judges pairs that
//! must fail, and whether a third replica confirms what a pair vouched for.
//! It reads no database and changes nothing. Agreement is not accuracy.
use super::consensus::{self, CropState, Metric, Params, Side};
use super::validate::{self, Loaded};
use super::{DIAG_FILE, READINGS_FILE, RESULT_FILE};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};
use usnm_ocr_contract::{boinc::dice, qwen, qwen::Outcome};

/// A small seeded generator: the controls are the same on every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33) as usize % n
    }
}

/// `language` from the `== unit` section of a result's diagnostics.
fn language(diag: &[u8]) -> String {
    String::from_utf8_lossy(diag)
        .lines()
        .find(|l| l.starts_with("job ") && l.contains(" language "))
        .and_then(|l| l.split(" language ").nth(1))
        .and_then(|l| l.split(':').next())
        .unwrap_or("unknown")
        .to_owned()
}

fn group(language: &str) -> &'static str {
    if language == "yid" {
        "yid"
    } else {
        "other"
    }
}

struct Replica {
    language: String,
    plan_class: String,
    loaded: Loaded,
}

type Crops<'a> = BTreeMap<&'a str, Side<'a>>;

/// The strict rule on texts alone: every crop at the Dice floor, both read or neither.
fn strict(a: &Crops<'_>, b: &Crops<'_>) -> bool {
    let blank = Side {
        text: "",
        looped: false,
    };
    a.keys().chain(b.keys()).all(|id| {
        let (x, y) = (*a.get(id).unwrap_or(&blank), *b.get(id).unwrap_or(&blank));
        x.text.trim().is_empty() == y.text.trim().is_empty()
            && dice(x.text, y.text) >= qwen::PROBE_MIN_DICE
    })
}

/// Rule variants measured side by side. Only [`Params::IN_RULE`] can go live.
fn candidates() -> Vec<(String, Params)> {
    let mut out = Vec::new();
    for (metric, name) in [(Metric::Similarity, "similarity"), (Metric::Dice, "dice")] {
        for share in [0.0, 0.5, 2.0 / 3.0] {
            for floor in [0.4, 0.5, 0.6] {
                out.push((
                    format!("{name}>={floor},share>{share:.2}"),
                    Params {
                        metric,
                        floor,
                        min_share: share,
                        min_agreed_chars: Params::IN_RULE.min_agreed_chars,
                    },
                ));
            }
        }
    }
    out
}

fn passes(a: &Crops<'_>, b: &Crops<'_>, p: Params) -> bool {
    consensus::judge_crops(a, b, p).1.is_ok()
}

fn loaded(seen: &Path, model: &str) -> anyhow::Result<(Vec<Replica>, usize)> {
    let mut out = Vec::new();
    let mut skipped = 0;
    let mut dirs: Vec<_> = std::fs::read_dir(seen)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for dir in dirs {
        let files = [READINGS_FILE, RESULT_FILE, DIAG_FILE].map(|f| dir.join(f));
        let Ok(l) = validate::load(&files) else {
            skipped += 1;
            continue;
        };
        if l.result.outcome == Outcome::Ok && l.result.model.as_deref() == Some(model) {
            out.push(Replica {
                language: language(&l.diag),
                plan_class: l.result.host.plan_class.clone().unwrap_or_default(),
                loaded: l,
            });
        }
    }
    Ok((out, skipped))
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        (n as f64 / of as f64 * 10_000.0).round() / 100.0
    }
}

/// The false-negative and false-positive rates of a crop metric at each floor.
fn roc(truth: &[(f64, f64)], shuffled: &[(f64, f64)]) -> serde_json::Value {
    let rate = |rows: &[(f64, f64)], f: &dyn Fn(&(f64, f64)) -> bool| {
        pct(rows.iter().filter(|r| f(r)).count(), rows.len())
    };
    let floors: BTreeMap<String, serde_json::Value> = [0.3, 0.4, 0.5, 0.6, 0.7, 0.8]
        .iter()
        .map(|&t| {
            (
                format!("{t:.1}"),
                serde_json::json!({
                    "dice_false_reject_pct": rate(truth, &|r| r.0 < t),
                    "dice_false_accept_pct": rate(shuffled, &|r| r.0 >= t),
                    "similarity_false_reject_pct": rate(truth, &|r| r.1 < t),
                    "similarity_false_accept_pct": rate(shuffled, &|r| r.1 >= t),
                }),
            )
        })
        .collect();
    serde_json::json!({"true_crops": truth.len(), "shuffled_crops": shuffled.len(), "floors": floors})
}

pub fn run(seen: &Path, model: &str, seed: u64, controls: usize, out: &Path) -> anyhow::Result<()> {
    let (replicas, skipped) = loaded(seen, model)?;
    let crops: Vec<Crops<'_>> = replicas
        .iter()
        .map(|r| consensus::sides(&r.loaded))
        .collect();
    let mut jobs: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, r) in replicas.iter().enumerate() {
        jobs.entry(&r.loaded.result.job_id).or_default().push(i);
    }
    for v in jobs.values_mut() {
        // Earliest first: the two a unit needs, then the extras.
        v.sort_by(|a, b| {
            replicas[*a]
                .loaded
                .result
                .completed_at
                .cmp(&replicas[*b].loaded.result.completed_at)
        });
    }
    let pairs: Vec<(usize, usize)> = jobs
        .values()
        .flat_map(|v| {
            v.iter()
                .enumerate()
                .flat_map(move |(i, a)| v[i + 1..].iter().map(move |b| (*a, *b)))
        })
        .collect();
    let lang = |i: usize| group(&replicas[i].language);
    let multi: Vec<&Vec<usize>> = jobs.values().filter(|v| v.len() >= 2).collect();

    // Every rule over every pair and job.
    let mut rules = serde_json::Map::new();
    let tally = |pass: &dyn Fn(usize, usize) -> bool| {
        let mut by: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for &(a, b) in &pairs {
            let e = by.entry(lang(a)).or_default();
            e.0 += 1;
            e.1 += usize::from(pass(a, b));
        }
        let any = multi
            .iter()
            .filter(|v| {
                v.iter()
                    .enumerate()
                    .any(|(i, a)| v[i + 1..].iter().any(|b| pass(*a, *b)))
            })
            .count();
        let first_two: Vec<_> = multi.iter().filter(|v| pass(v[0], v[1])).collect();
        serde_json::json!({
            "pairs": by.iter().map(|(k, v)| (k.to_string(), serde_json::json!({"pairs": v.0, "agree": v.1}))).collect::<BTreeMap<_, _>>(),
            "units_with_two_replicas": multi.len(),
            "units_resolved_by_any_pair": any,
            "units_resolved_by_first_two": first_two.len(),
            "extra_replicas_not_needed": first_two.iter().map(|v| v.len() - 2).sum::<usize>(),
        })
    };
    rules.insert(
        "strict-v1".into(),
        tally(&|a, b| strict(&crops[a], &crops[b])),
    );
    for (name, params) in candidates() {
        rules.insert(name, tally(&|a, b| passes(&crops[a], &crops[b], params)));
    }

    // The rule in force, per crop and per vendor pair.
    let p = Params::IN_RULE;
    let mut states: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    let mut vendors: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    let mut by_language: BTreeMap<(String, String), [usize; 6]> = BTreeMap::new();
    let mut same_pages: BTreeMap<String, [usize; 6]> = BTreeMap::new();
    let mut loop_pairs = [0usize; 4];
    for &(a, b) in &pairs {
        let (vs, v) = consensus::judge_crops(&crops[a], &crops[b], p);
        let by = states.entry(lang(a)).or_default();
        let mut names = [
            replicas[a].plan_class.as_str(),
            replicas[b].plan_class.as_str(),
        ];
        names.sort_unstable();
        let key = names.join("+");
        let job = &jobs[replicas[a].loaded.result.job_id.as_str()];
        // Pairs on pages an AMD host also read: the same pages for every vendor.
        let amd_page = job.len() >= 3
            && job
                .iter()
                .any(|i| replicas[*i].plan_class == "amd_vulkan_8b");
        let mut rows: Vec<&mut [usize; 6]> = vec![
            vendors.entry(key.clone()).or_default(),
            by_language
                .entry((replicas[a].language.clone(), key.clone()))
                .or_default(),
        ];
        if amd_page {
            rows.push(same_pages.entry(key).or_default());
        }
        let was_strict = strict(&crops[a], &crops[b]);
        let row_counts = [
            1,
            usize::from(was_strict),
            usize::from(v.is_ok()),
            vs.iter().filter(|c| c.state == CropState::Looped).count(),
            vs.iter().filter(|c| c.state == CropState::Disagree).count(),
            vs.iter().filter(|c| c.state == CropState::Agree).count(),
        ];
        for row in rows {
            for (total, n) in row.iter_mut().zip(row_counts) {
                *total += n;
            }
        }
        let mut any_loop = false;
        let mut failed_on_text = false;
        for c in &vs {
            *by.entry(format!("{:?}", c.state)).or_default() += 1;
            any_loop |= c.state == CropState::Looped;
            failed_on_text |= c.state == CropState::Disagree;
        }
        loop_pairs[0] += usize::from(!was_strict);
        loop_pairs[1] += usize::from(!was_strict && any_loop);
        loop_pairs[2] += usize::from(!was_strict && failed_on_text);
        loop_pairs[3] += usize::from(was_strict && any_loop);
    }

    // Would guarding both sides have turned a strict failure into a pass?
    let cut_texts: Vec<HashMap<&str, String>> = crops
        .iter()
        .map(|c| {
            c.iter()
                .map(|(id, s)| {
                    (
                        *id,
                        qwen::loop_cut(s.text).unwrap_or_else(|| s.text.to_owned()),
                    )
                })
                .collect()
        })
        .collect();
    let guard_alone = pairs
        .iter()
        .filter(|&&(a, b)| {
            let side =
                |m: &'_ HashMap<&str, String>, k: &str| m.get(k).cloned().unwrap_or_default();
            !strict(&crops[a], &crops[b])
                && crops[a].keys().all(|id| {
                    let (x, y) = (side(&cut_texts[a], id), side(&cut_texts[b], id));
                    x.trim().is_empty() == y.trim().is_empty()
                        && dice(&x, &y) >= qwen::PROBE_MIN_DICE
                })
        })
        .count();

    // Pairs that must fail.
    let mut rng = Rng(seed | 1);
    let mut cross = (0usize, 0usize, 0usize);
    let n = replicas.len();
    let mut tries = 0;
    while n > 1 && cross.0 < controls && tries < controls * 200 {
        tries += 1;
        let (a, b) = (rng.below(n), rng.below(n));
        let (ra, rb) = (&replicas[a], &replicas[b]);
        if ra.loaded.result.job_id == rb.loaded.result.job_id
            || ra.language != rb.language
            || crops[a].len() != crops[b].len()
            || !crops[a].keys().eq(crops[b].keys())
        {
            continue;
        }
        cross.0 += 1;
        cross.1 += usize::from(strict(&crops[a], &crops[b]));
        cross.2 += usize::from(passes(&crops[a], &crops[b], p));
    }
    let forged = |f: &dyn for<'x> Fn(&Crops<'x>, &'static str) -> Crops<'x>| {
        let run: &'static str = Box::leak("der ".repeat(80).into_boxed_str());
        let (mut s, mut c) = (0, 0);
        for &(a, b) in &pairs {
            let fake = f(&crops[b], run);
            s += usize::from(strict(&crops[a], &fake));
            c += usize::from(passes(&crops[a], &fake, p));
        }
        serde_json::json!({"strict_pass": s, "rule_pass": c})
    };
    let looped = |run: &'static str| Side {
        text: run,
        looped: true,
    };
    let rotated = forged(&|c, _| {
        let ids: Vec<&str> = c.keys().copied().collect();
        ids.iter()
            .enumerate()
            .map(|(i, id)| (*id, c[ids[(i + 1) % ids.len()]]))
            .collect()
    });
    let all_loop = forged(&|c, run| c.keys().map(|id| (*id, looped(run))).collect());
    let half_loop = forged(&|c, run| {
        c.iter()
            .enumerate()
            .map(|(i, (id, s))| (*id, if i % 2 == 0 { looped(run) } else { *s }))
            .collect()
    });
    let all_unread = forged(&|c, _| {
        c.keys()
            .map(|id| {
                (
                    *id,
                    Side {
                        text: "[?]",
                        looped: false,
                    },
                )
            })
            .collect()
    });
    // One crop both sides read replaced by unrelated text, on pairs that have one.
    let mut one_wrong = (0usize, 0usize, 0usize);
    for &(a, b) in &pairs {
        let Some(id) = crops[a].iter().find_map(|(id, x)| {
            let y = crops[b].get(id)?;
            (!x.looped && !y.looped && x.text.len() > 40 && y.text.len() > 40).then_some(*id)
        }) else {
            continue;
        };
        let mut fake = crops[b].clone();
        fake.insert(
            id,
            Side {
                text: "Eggs were sold at twelve cents the dozen in town on market day.",
                looped: false,
            },
        );
        one_wrong.0 += 1;
        one_wrong.1 += usize::from(strict(&crops[a], &fake));
        one_wrong.2 += usize::from(passes(&crops[a], &fake, p));
    }

    // Does a third replica confirm what a pair vouched for?
    let mut third = (0usize, 0usize, 0usize, 0usize);
    for v in multi.iter().filter(|v| v.len() >= 3) {
        for i in 0..v.len() {
            for j in i + 1..v.len() {
                let (a, b) = (v[i], v[j]);
                let (vs, verdict) = consensus::judge_crops(&crops[a], &crops[b], p);
                if verdict.is_err() {
                    continue;
                }
                let was_strict = strict(&crops[a], &crops[b]);
                for c in vs.iter().filter(|c| c.state == CropState::Agree) {
                    for &k in v.iter().filter(|k| **k != a && **k != b) {
                        let Some(s) = crops[k].get(c.id.as_str()) else {
                            continue;
                        };
                        if s.looped {
                            continue;
                        }
                        let (x, y) = (crops[a][c.id.as_str()], crops[b][c.id.as_str()]);
                        let hit = consensus::similarity(x.text, s.text) >= p.floor
                            || consensus::similarity(y.text, s.text) >= p.floor;
                        let row = if was_strict {
                            (&mut third.2, &mut third.3)
                        } else {
                            (&mut third.0, &mut third.1)
                        };
                        *row.0 += usize::from(hit);
                        *row.1 += usize::from(!hit);
                    }
                }
            }
        }
    }

    // Crop-level separation of both metrics.
    let mut roc_rows: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    for g in ["yid", "other"] {
        let compared = |x: Side<'_>, y: Side<'_>| {
            !x.looped
                && !y.looped
                && x.text.chars().any(char::is_alphanumeric)
                && y.text.chars().any(char::is_alphanumeric)
        };
        let mut truth = Vec::new();
        for &(a, b) in pairs.iter().filter(|(a, _)| lang(*a) == g) {
            for (id, x) in &crops[a] {
                if let Some(y) = crops[b].get(id).filter(|y| compared(*x, **y)) {
                    truth.push((dice(x.text, y.text), consensus::similarity(x.text, y.text)));
                }
            }
        }
        let pool: Vec<usize> = (0..n).filter(|i| lang(*i) == g).collect();
        let mut shuffled = Vec::new();
        let mut tries = 0;
        while !pool.is_empty() && shuffled.len() < controls && tries < controls * 200 {
            tries += 1;
            let (a, b) = (pool[rng.below(pool.len())], pool[rng.below(pool.len())]);
            if replicas[a].loaded.result.job_id == replicas[b].loaded.result.job_id
                || crops[a].is_empty()
                || crops[b].is_empty()
            {
                continue;
            }
            let pick =
                |c: &Crops<'_>, k: usize| c.values().nth(k).map(|s| (s.text.to_owned(), s.looped));
            let (ka, kb) = (rng.below(crops[a].len()), rng.below(crops[b].len()));
            let (Some(x), Some(y)) = (pick(&crops[a], ka), pick(&crops[b], kb)) else {
                continue;
            };
            let (sx, sy) = (
                Side {
                    text: &x.0,
                    looped: x.1,
                },
                Side {
                    text: &y.0,
                    looped: y.1,
                },
            );
            if compared(sx, sy) {
                shuffled.push((dice(&x.0, &y.0), consensus::similarity(&x.0, &y.0)));
            }
        }
        roc_rows.insert(g, roc(&truth, &shuffled));
    }

    let counts = |v: &[usize; 6]| {
        serde_json::json!({
            "pairs": v[0], "strict_agree": v[1], "rule_agree": v[2],
            "looped_crops": v[3], "disagree_crops": v[4], "agree_crops": v[5],
        })
    };
    let report = serde_json::json!({
        "accuracy_established": false,
        "replicas": n,
        "skipped_unreadable": skipped,
        "jobs": jobs.len(),
        "pairs": pairs.len(),
        "strict_failures": {
            "pairs": loop_pairs[0],
            "with_a_looped_crop": loop_pairs[1],
            "with_a_disagreeing_crop_under_the_rule": loop_pairs[2],
            "fixed_by_cutting_loops_alone": guard_alone,
        },
        "strict_passes_with_a_looped_crop": loop_pairs[3],
        "rules": rules,
        "in_rule": {
            "crop_states": states,
            "plan_class_pairs": vendors.iter().map(|(k, v)| (k.clone(), counts(v))).collect::<BTreeMap<_, _>>(),
            "plan_class_pairs_by_language": by_language.iter().map(|((l, k), v)| (format!("{l} {k}"), counts(v))).collect::<BTreeMap<_, _>>(),
            "plan_class_pairs_on_pages_amd_also_read": same_pages.iter().map(|(k, v)| (k.clone(), counts(v))).collect::<BTreeMap<_, _>>(),
        },
        "controls_that_must_fail": {
            "cross_page_same_language": {"pairs": cross.0, "strict_pass": cross.1, "rule_pass": cross.2},
            "crops_rotated_within_page": rotated,
            "every_crop_looped": all_loop,
            "half_the_crops_looped": half_loop,
            "every_crop_unread": all_unread,
            "one_crop_replaced": {"pairs": one_wrong.0, "strict_pass": one_wrong.1, "rule_pass": one_wrong.2},
        },
        "third_replica_check": {
            "rescued_pairs": {"confirmed": third.0, "not_confirmed": third.1},
            "strict_pairs": {"confirmed": third.2, "not_confirmed": third.3},
        },
        "crop_metric_separation": roc_rows,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qwen::validate::tests::write;
    use std::path::PathBuf;

    #[test]
    fn replays_retained_results_and_runs_the_controls() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let keep = tmp.path().join("seen");
        let text = "The ship sailed for England on Tuesday morning.";
        for (tag, c2) in [
            ("a", text),
            ("b", "Eggs were sold at twelve cents the dozen."),
        ] {
            let files: Vec<PathBuf> = write(tmp.path(), tag, &[("c01", text), ("c02", c2)]);
            validate::check(&files, Some(&keep)).unwrap();
        }
        let out = tmp.path().join("report.json");
        run(&keep, "qwen3-vl-8b-q4km", 7, 10, &out).unwrap();
        let r: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(r["replicas"], 2);
        assert_eq!(r["pairs"], 1);
        assert_eq!(r["strict_failures"]["pairs"], 1);
        assert_eq!(r["accuracy_established"], false);
        assert_eq!(
            r["controls_that_must_fail"]["every_crop_looped"]["rule_pass"],
            0
        );
    }
}
