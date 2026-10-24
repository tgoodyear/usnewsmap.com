//! Scoring: the rules, the masks, the tallies and the outputs a run is scored from.

use super::manifest::{Manifest, Sample};
use super::spec::{Damage, IllegibleSpan};
use super::RULES;
use anyhow::{bail, ensure, Context};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Python's `str.isspace`: Unicode White_Space plus the four ASCII information separators.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Applies a score rule to a text.
pub fn normalize(rule: &str, text: &str) -> anyhow::Result<String> {
    match rule {
        "legacy-ws-v1" => Ok(text
            .split(py_space)
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")),
        "raw-v1" | "raw-masked-v1" => Ok(text.to_string()),
        _ => bail!(
            "unknown score rule {rule:?}; known: {}, raw-masked-v1",
            RULES.join(", ")
        ),
    }
}

#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Tally {
    /// Character edits over the samples with an output.
    pub edits: u64,
    /// Samples with no output, or an output that isn't text.
    pub missing: usize,
    /// Reference characters over the samples with an output.
    pub reference_chars: u64,
    /// Reference characters of the missing samples (scored as empty in `cer_missing_as_empty`).
    pub missing_chars: u64,
    pub samples: usize,
    pub scored: usize,
}

impl Tally {
    pub(super) fn cer(&self) -> Option<f64> {
        (self.reference_chars > 0).then(|| self.edits as f64 / self.reference_chars as f64)
    }
    pub(super) fn cer_missing_as_empty(&self) -> Option<f64> {
        let chars = self.reference_chars + self.missing_chars;
        (chars > 0).then(|| (self.edits + self.missing_chars) as f64 / chars as f64)
    }
    fn json(&self) -> Value {
        json!({
            "cer": self.cer(),
            "cer_missing_as_empty": self.cer_missing_as_empty(),
            "edits": self.edits,
            "missing": self.missing,
            "missing_chars": self.missing_chars,
            "reference_chars": self.reference_chars,
            "samples": self.samples,
            "scored": self.scored,
        })
    }
}

pub(super) fn validate_annotations(sample: &Sample, reference: &str) -> anyhow::Result<()> {
    if let Some(t) = &sample.transcription {
        ensure!(
            t.policy == "diplomatic-v1",
            "{}: unknown transcription policy",
            sample.id
        );
        ensure!(
            !t.evidence.trim().is_empty() && !t.reviewed_by.trim().is_empty(),
            "{}: missing transcription review",
            sample.id
        );
        let mut end = 0;
        let len = reference.chars().count();
        for span in &t.illegible_spans {
            ensure!(
                span.start >= end
                    && span.start < span.end
                    && span.end <= len
                    && !span.reason.trim().is_empty(),
                "{}: invalid illegible span",
                sample.id
            );
            end = span.end;
        }
    }
    if sample.damage["status"] != "unannotated" {
        let d: Damage = serde_json::from_value(sample.damage.clone())?;
        ensure!(
            d.status == "reviewed" && !d.evidence.trim().is_empty(),
            "{}: missing damage review",
            sample.id
        );
        ensure!(
            ["none", "mild", "moderate", "severe"].contains(&d.severity.as_str()),
            "{}: unknown severity",
            sample.id
        );
        let allowed = [
            "bleed-through",
            "faded-ink",
            "blur",
            "stain",
            "tear",
            "occlusion",
            "warping",
            "clipping",
        ];
        let unique: BTreeSet<_> = d.labels.iter().collect();
        ensure!(
            unique.len() == d.labels.len()
                && d.labels.iter().all(|x| allowed.contains(&x.as_str())),
            "{}: invalid damage labels",
            sample.id
        );
        ensure!(
            (d.severity == "none") == d.labels.is_empty(),
            "{}: damage severity/labels disagree",
            sample.id
        );
    } else {
        let object = sample
            .damage
            .as_object()
            .context("damage must be an object")?;
        ensure!(
            object
                .keys()
                .all(|key| matches!(key.as_str(), "status" | "observations")),
            "{}: unannotated damage may only contain status and observations",
            sample.id
        );
        if let Some(observations) = object.get("observations") {
            serde_json::from_value::<Vec<String>>(observations.clone()).with_context(|| {
                format!("{}: damage observations must be a string array", sample.id)
            })?;
        }
    }
    Ok(())
}

/// A frozen reference mask is a wildcard matching zero or more hypothesis code points.
/// Only visible reference characters enter the denominator. No hypothesis-side masks exist.
fn masked_distance(reference: &[char], hyp: &[char], spans: &[IllegibleSpan]) -> usize {
    let mut row: Vec<usize> = (0..=hyp.len()).collect();
    let mut i = 0;
    let mut span = 0;
    while i < reference.len() {
        if span < spans.len() && i == spans[span].start {
            for j in 1..row.len() {
                row[j] = row[j].min(row[j - 1]);
            }
            i = spans[span].end;
            span += 1;
        } else {
            let mut next = vec![row[0] + 1; hyp.len() + 1];
            for j in 1..=hyp.len() {
                next[j] = (row[j - 1] + usize::from(reference[i] != hyp[j - 1]))
                    .min(row[j] + 1)
                    .min(next[j - 1] + 1);
            }
            row = next;
            i += 1;
        }
    }
    row[hyp.len()]
}

/// Scores outputs `{id: text}` on a subset. Missing outputs are counted, never silently left out:
/// `cer` is over the samples with an output and `cer_missing_as_empty` counts the rest as empty.
/// Outputs for ids outside the subset are an error.
pub fn score(
    dir: &Path,
    m: &Manifest,
    subset: &str,
    outputs: &BTreeMap<String, String>,
    rule: &str,
) -> anyhow::Result<(Tally, Value)> {
    let s = m
        .subsets
        .get(subset)
        .with_context(|| format!("no subset {subset:?} in release {}", m.release))?;
    let members: BTreeSet<&str> = s.members.iter().map(String::as_str).collect();
    if let Some(stray) = outputs.keys().find(|k| !members.contains(k.as_str())) {
        bail!("output for {stray:?}, which is not in {subset}");
    }
    let by_id: BTreeMap<&str, &Sample> = m.samples.iter().map(|x| (x.id.as_str(), x)).collect();
    let mut total = Tally::default();
    let mut by_source: BTreeMap<String, Tally> = BTreeMap::new();
    let mut rows = Vec::new();
    for id in &s.members {
        let x = by_id[id.as_str()];
        let reference = std::fs::read_to_string(dir.join(&x.reference.path))?;
        validate_annotations(x, &reference)?;
        let truth: Vec<char> = normalize(rule, &reference)?.chars().collect();
        let spans = x
            .transcription
            .as_ref()
            .map(|t| t.illegible_spans.as_slice())
            .unwrap_or(&[]);
        let masked = rule == "raw-masked-v1";
        let excluded = if masked {
            spans.iter().map(|s| s.end - s.start).sum()
        } else {
            0
        };
        let reference_chars = truth.len() - excluded;
        let t = by_source.entry(x.source.clone()).or_default();
        for t in [&mut total, t] {
            t.samples += 1;
        }
        match outputs.get(id) {
            Some(text) => {
                let hyp: Vec<char> = normalize(rule, text)?.chars().collect();
                let e = if masked {
                    masked_distance(&truth, &hyp, spans)
                } else {
                    crate::distance::levenshtein(&truth, &hyp)
                } as u64;
                for t in [&mut total, by_source.get_mut(&x.source).unwrap()] {
                    t.scored += 1;
                    t.edits += e;
                    t.reference_chars += reference_chars as u64;
                }
                rows.push(json!({"edits": e, "id": id, "reference_chars": reference_chars}));
            }
            None => {
                for t in [&mut total, by_source.get_mut(&x.source).unwrap()] {
                    t.missing += 1;
                    t.missing_chars += reference_chars as u64;
                }
                rows.push(json!({"id": id, "missing": true, "reference_chars": reference_chars}));
            }
        }
    }
    if rule == "raw-masked-v1" {
        for (row, id) in rows.iter_mut().zip(&s.members) {
            let x = by_id[id.as_str()];
            let excluded: usize = x
                .transcription
                .as_ref()
                .map(|t| t.illegible_spans.iter().map(|s| s.end - s.start).sum())
                .unwrap_or(0);
            row["masked_reference_chars"] = json!(excluded);
        }
    }
    let mut report = json!({
        "by_source": by_source.iter().map(|(k, v)| (k.clone(), v.json())).collect::<serde_json::Map<_, _>>(),
        "release": m.release,
        "rows": rows,
        "rule": rule,
        "subset": subset,
        "subset_digest": s.digest,
        "total": total.json(),
    });
    if rule == "raw-masked-v1" {
        report["mask_policy"] = json!("reference-wildcard-v1");
    }
    Ok((total, report))
}

/// Reads outputs as JSONL rows `{"id", "text"}`; a duplicate id is an error.
pub fn read_outputs(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).with_context(|| format!("outputs line {}", n + 1))?;
        let id = v["id"]
            .as_str()
            .with_context(|| format!("outputs line {}: no string id", n + 1))?;
        let text = v["text"]
            .as_str()
            .with_context(|| format!("outputs line {}: no string text", n + 1))?;
        ensure!(
            out.insert(id.to_string(), text.to_string()).is_none(),
            "outputs line {}: {id} given twice",
            n + 1
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::tests::fixture;
    use crate::bench::{build, load, Transcription};

    #[test]
    fn bench_per_script_code_point_rules() {
        // Synthetic scorer fixtures, never benchmark ground truth.
        for (reference, hyp, edits, chars) in [
            ("ſtraße", "straße", 1, 6),
            ("日本語", "日本", 1, 3),
            ("かなカナ漢字", "かなカナ漢字", 0, 6),
            ("Русский", "русский", 1, 7),
            ("עברית", "עברית", 0, 5),
            ("ייִדיש", "ייִדיש", 0, 6),
            ("العربية", "العربية", 0, 7),
            ("é", "e\u{301}", 2, 1),
            ("日本 English עברית", "日本 English עברית", 0, 16),
        ] {
            for rule in RULES {
                let r: Vec<_> = normalize(rule, reference).unwrap().chars().collect();
                let h: Vec<_> = normalize(rule, hyp).unwrap().chars().collect();
                assert_eq!(r.len(), chars, "{reference}");
                assert_eq!(crate::distance::levenshtein(&r, &h), edits, "{reference}");
            }
        }
        assert_eq!(
            normalize("legacy-ws-v1", "日\u{3000}本\n語").unwrap(),
            "日 本 語"
        );
        assert_eq!(
            normalize("raw-v1", "日\u{3000}本\n語").unwrap(),
            "日\u{3000}本\n語"
        );
    }

    #[test]
    fn bench_masks_preserve_visible_errors_and_missing_denominators() {
        let f = fixture(None);
        let built = build(&f.opts).unwrap();
        let mut m = load(&built.dir).unwrap();
        let id = m.subsets["legacy/en-3"].members[0].clone();
        let sample = m.samples.iter_mut().find(|s| s.id == id).unwrap();
        std::fs::write(built.dir.join(&sample.reference.path), "日?本").unwrap();
        sample.transcription = Some(Transcription {
            policy: "diplomatic-v1".into(),
            evidence: "synthetic test".into(),
            reviewed_by: "fixture".into(),
            illegible_spans: vec![IllegibleSpan {
                start: 1,
                end: 2,
                reason: "occlusion".into(),
            }],
        });
        m.subsets.get_mut("legacy/en-3").unwrap().members = vec![id.clone()];
        for (text, expected) in [
            ("日本", 0),
            ("日不明本", 0),
            ("日不明木", 1),
            ("", 2),
            ("余日本外", 2),
        ] {
            let outputs = BTreeMap::from([(id.clone(), text.into())]);
            let (t, report) =
                score(&built.dir, &m, "legacy/en-3", &outputs, "raw-masked-v1").unwrap();
            assert_eq!((t.edits, t.reference_chars), (expected, 2), "{text}");
            assert_eq!(report["rows"][0]["masked_reference_chars"], 1);
        }
        let (missing, _) = score(
            &built.dir,
            &m,
            "legacy/en-3",
            &BTreeMap::new(),
            "raw-masked-v1",
        )
        .unwrap();
        assert_eq!((missing.missing, missing.missing_chars), (1, 2));
        let (raw, _) = score(
            &built.dir,
            &m,
            "legacy/en-3",
            &BTreeMap::from([(id, "日本".into())]),
            "raw-v1",
        )
        .unwrap();
        assert_eq!((raw.edits, raw.reference_chars), (1, 3));
        m.samples
            .iter_mut()
            .find(|s| s.id == m.subsets["legacy/en-3"].members[0])
            .unwrap()
            .transcription
            .as_mut()
            .unwrap()
            .illegible_spans = vec![IllegibleSpan {
            start: 0,
            end: 3,
            reason: "entire sample unreadable (synthetic)".into(),
        }];
        let (all_masked, report) = score(
            &built.dir,
            &m,
            "legacy/en-3",
            &BTreeMap::new(),
            "raw-masked-v1",
        )
        .unwrap();
        assert_eq!(all_masked.missing_chars, 0);
        assert!(report["total"]["cer_missing_as_empty"].is_null());
    }

    #[test]
    fn bench_annotations_reject_invalid_spans_and_damage() {
        let f = fixture(None);
        let built = build(&f.opts).unwrap();
        let m = load(&built.dir).unwrap();
        let mut sample = m.samples[0].clone();
        for spans in [
            vec![(0, 0)],
            vec![(2, 1)],
            vec![(0, 4)],
            vec![(1, 2), (0, 1)],
            vec![(0, 2), (1, 3)],
        ] {
            sample.transcription = Some(Transcription {
                policy: "diplomatic-v1".into(),
                evidence: "fixture".into(),
                reviewed_by: "test".into(),
                illegible_spans: spans
                    .into_iter()
                    .map(|(start, end)| IllegibleSpan {
                        start,
                        end,
                        reason: "test".into(),
                    })
                    .collect(),
            });
            assert!(validate_annotations(&sample, "日?本").is_err());
        }
        sample.transcription = None;
        sample.damage = json!({"status":"unannotated", "observations":["existing pilot narrative; not a severity label"]});
        assert!(validate_annotations(&sample, "abc").is_ok());
        for damage in [
            json!({"status":"unannotated", "labels":[], "severity":"none", "evidence":""}),
            json!({"status":"unannotated", "observations":"not an array"}),
            json!({"status":"unannotated", "observations":[1]}),
            json!({"status":"reviewed", "labels":[], "severity":"severe", "evidence":"test"}),
            json!({"status":"reviewed", "labels":["invented"], "severity":"mild", "evidence":"test"}),
            json!({"status":"reviewed", "labels":["blur"], "severity":"mild", "evidence":""}),
        ] {
            sample.damage = damage;
            assert!(validate_annotations(&sample, "abc").is_err());
        }
        sample.damage =
            json!({"status":"reviewed", "labels":["blur"], "severity":"mild", "evidence":"test"});
        assert!(validate_annotations(&sample, "abc").is_ok());
        assert_eq!(
            masked_distance(
                &['?', '?'],
                &['x', 'y'],
                &[IllegibleSpan {
                    start: 0,
                    end: 2,
                    reason: "test".into()
                }]
            ),
            0
        );
        assert_eq!(
            masked_distance(
                &['a', '?', 'b', '?'],
                &['a', 'x', 'b', 'y'],
                &[
                    IllegibleSpan {
                        start: 1,
                        end: 2,
                        reason: "test".into()
                    },
                    IllegibleSpan {
                        start: 3,
                        end: 4,
                        reason: "test".into()
                    },
                ]
            ),
            0
        );
    }

    #[test]
    fn legacy_rule_matches_python_split() {
        assert_eq!(
            normalize("legacy-ws-v1", " a\u{1f}b\u{3000}c\n\n").unwrap(),
            "a b c"
        );
        assert_eq!(
            normalize("legacy-ws-v1", "a\u{200b}b").unwrap(),
            "a\u{200b}b"
        );
        assert_eq!(normalize("raw-v1", " a ").unwrap(), " a ");
        assert!(normalize("nfkc", "a").is_err());
    }
}
