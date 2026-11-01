//! The crop-by-crop validator rule, `crop-consensus-v2` (#1420).
//!
//! `strict-v1` asks every crop of a pair for a word-bag Dice of at least the
//! floor, so one crop decides twelve, and two loops that repeat the same
//! words pass. This rule asks what each crop can show:
//!
//! - `looped`: a side's reading ends in a repeated run, or its result marks
//!   the crop looped. A loop is a model failure, not a reading, so the crop
//!   counts for neither side.
//! - `no_text`: neither side has a letter or digit (`[?]` on an
//!   illustration). Nothing was read, so nothing can agree.
//! - `agree`: the character similarity of the two readings, Hebrew points
//!   left out, is at least [`Params::IN_RULE`]'s floor.
//! - `disagree`: anything else, and it fails the pair.
//!
//! A pair agrees when no crop disagrees and `agree` crops are more than half
//! of the crops with any text and hold at least 100 characters, or when no
//! crop has text at all, as the strict rule has always agreed. Results that did not both finish keep the strict
//! verdict. A pair's agreement vouches for its `agree` crops only: the crop
//! states are logged and kept, and accuracy is not established by either.
use super::validate::Loaded;
use anyhow::Context;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::qwen::{tail_repetition, Outcome};
use usnm_ocr_quality::distance;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CropState {
    Agree,
    Disagree,
    Looped,
    NoText,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Metric {
    /// `1 - edits / longer length` over characters, whitespace collapsed and
    /// Hebrew points left out.
    Similarity,
    /// The strict rule's word-bag Dice.
    Dice,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub metric: Metric,
    pub floor: f64,
    /// `agree` crops must be more than this share of the crops with text.
    pub min_share: f64,
    /// `agree` crops must hold at least this many characters. Two issues of a
    /// paper share a masthead, so a few agreeing words don't show the same page.
    pub min_agreed_chars: usize,
}

impl Params {
    /// Measured in the November 2026 replay of the retained results
    /// (docs/notes/2026-11-01-qwen-cross-vendor-validation.md): against Dice at
    /// the same floor, a fifth as many true Yiddish crop pairs fall short, at
    /// the same false-accept rate, 0.1% of shuffled pairs.
    pub const IN_RULE: Params = Params {
        metric: Metric::Similarity,
        floor: 0.5,
        min_share: 0.5,
        min_agreed_chars: 100,
    };
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CropVerdict {
    pub id: String,
    pub state: CropState,
    /// The metric's value, where the crop was compared.
    pub score: Option<f64>,
}

/// One side's reading of one crop.
#[derive(Debug, Clone, Copy)]
pub struct Side<'a> {
    pub text: &'a str,
    pub looped: bool,
}

/// `1 - edits / longer length`, over characters with whitespace collapsed
/// and Hebrew points left out, for every reader alike: 1.0 for two equal
/// texts, including two empty ones. The readings themselves stay as read.
pub fn similarity(a: &str, b: &str) -> f64 {
    let squeeze = |s: &str| {
        without_hebrew_points(s)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (a, b) = (squeeze(a), squeeze(b));
    let longer = a.chars().count().max(b.chars().count());
    if longer == 0 {
        return 1.0;
    }
    1.0 - distance::chars(&a, &b) as f64 / longer as f64
}

/// `text` without Hebrew cantillation marks and points, U+0591 to U+05C7
/// except the punctuation in that range. Models add vowel points print
/// doesn't have, so two readings of the same letters can differ only there.
pub fn without_hebrew_points(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{0591}'
                    ..='\u{05BD}'
                        | '\u{05BF}'
                        | '\u{05C1}'
                        | '\u{05C2}'
                        | '\u{05C4}'
                        | '\u{05C5}'
                        | '\u{05C7}'
            )
        })
        .collect()
}

fn has_text(text: &str) -> bool {
    text.chars().any(char::is_alphanumeric)
}

pub fn crop_state(a: Side<'_>, b: Side<'_>, params: Params) -> (CropState, Option<f64>) {
    if a.looped || b.looped {
        return (CropState::Looped, None);
    }
    if !has_text(a.text) && !has_text(b.text) {
        return (CropState::NoText, None);
    }
    let score = match params.metric {
        Metric::Similarity => similarity(a.text, b.text),
        Metric::Dice => usnm_ocr_contract::boinc::dice(a.text, b.text),
    };
    let state = if score >= params.floor {
        CropState::Agree
    } else {
        CropState::Disagree
    };
    (state, Some(score))
}

/// Applies the rule to two sides' crops.
pub fn judge_crops(
    a: &BTreeMap<&str, Side<'_>>,
    b: &BTreeMap<&str, Side<'_>>,
    params: Params,
) -> (Vec<CropVerdict>, Result<(), String>) {
    const BLANK: Side<'static> = Side {
        text: "",
        looped: false,
    };
    let mut ids: Vec<&str> = a.keys().chain(b.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    let verdicts: Vec<CropVerdict> = ids
        .iter()
        .map(|id| {
            let (state, score) = crop_state(
                a.get(id).copied().unwrap_or(BLANK),
                b.get(id).copied().unwrap_or(BLANK),
                params,
            );
            CropVerdict {
                id: (*id).to_owned(),
                state,
                score,
            }
        })
        .collect();
    let count = |s| verdicts.iter().filter(|v| v.state == s).count();
    let (agree, looped) = (count(CropState::Agree), count(CropState::Looped));
    let with_text = agree + looped;
    let chars = |side: &BTreeMap<&str, Side<'_>>, id: &str| {
        side.get(id).map_or(0, |s| s.text.chars().count())
    };
    let agreed_chars: usize = verdicts
        .iter()
        .filter(|v| v.state == CropState::Agree)
        .map(|v| chars(a, &v.id).min(chars(b, &v.id)))
        .sum();
    let verdict = if verdicts.is_empty() {
        Err("no crop in common".to_owned())
    } else if let Some(v) = verdicts.iter().find(|v| v.state == CropState::Disagree) {
        Err(format!(
            "{}: reading agreement {:.4}",
            v.id,
            v.score.unwrap_or(0.0)
        ))
    } else if with_text > 0 && (agree as f64) <= params.min_share * with_text as f64 {
        Err(format!("{agree} of {with_text} crops with text agree"))
    } else if with_text > 0 && agreed_chars < params.min_agreed_chars {
        Err(format!("{agreed_chars} characters agree"))
    } else {
        Ok(())
    };
    (verdicts, verdict)
}

/// Whether the result marks its crop `i` looped: bit `i % 8` of byte `i / 8`
/// of the hex `repetition_loop_crop_bits`. Absent in older results.
fn loop_bit(l: &Loaded, i: usize) -> bool {
    l.readings
        .diag
        .get("repetition_loop_crop_bits")
        .and_then(|hex| hex.get(i / 8 * 2..i / 8 * 2 + 2))
        .and_then(|byte| u8::from_str_radix(byte, 16).ok())
        .is_some_and(|byte| byte >> (i % 8) & 1 == 1)
}

/// One result's crops. A finished result's readings are in job order, so
/// the position is the crop's bit.
pub fn sides(l: &Loaded) -> BTreeMap<&str, Side<'_>> {
    l.readings
        .readings
        .iter()
        .enumerate()
        .map(|(i, r)| {
            (
                r.id.as_str(),
                Side {
                    text: &r.text,
                    looped: loop_bit(l, i) || tail_repetition(&r.text).is_some(),
                },
            )
        })
        .collect()
}

/// Judges a pair of results. A pair that did not both finish, or that is for
/// different jobs or engines, keeps `strict`, the strict rule's verdict.
pub fn judge(
    a: &Loaded,
    b: &Loaded,
    params: Params,
    strict: Result<(), String>,
) -> (Vec<CropVerdict>, Result<(), String>) {
    let finished = a.result.outcome == Outcome::Ok && b.result.outcome == Outcome::Ok;
    if !finished || a.result.job_id != b.result.job_id || a.result.engine != b.result.engine {
        return (Vec::new(), strict);
    }
    judge_crops(&sides(a), &sides(b), params)
}

/// Writes a pair's crop states once under `<dir>/<job_id>/<pair>.json`.
pub fn record(
    dir: &Path,
    a: &[PathBuf],
    b: &[PathBuf],
    la: &Loaded,
    lb: &Loaded,
    states: &[CropVerdict],
    agree: bool,
) -> anyhow::Result<()> {
    let mut sides = [
        (crate::quality::validate::result_name(&a[1])?, la),
        (crate::quality::validate::result_name(&b[1])?, lb),
    ];
    sides.sort_by(|x, y| x.0.cmp(&y.0));
    let names = [sides[0].0.clone(), sides[1].0.clone()];
    let record = serde_json::json!({
        "v": 1,
        "rule": "crop-consensus-v2",
        "job_id": la.result.job_id,
        "results": names,
        "readings_sha256": [sides[0].1.result.readings_sha256, sides[1].1.result.readings_sha256],
        "agree": agree,
        "crops": states,
        "accuracy_established": false,
    });
    let bytes = serde_json::to_vec_pretty(&record)?;
    usnm_ocr_contract::quality::check_doc_id(&la.result.job_id)?;
    let pair = usnm_ocr_contract::boinc::sha256(names.join("\n").as_bytes());
    let parent = dir.join(&la.result.job_id);
    std::fs::create_dir_all(&parent)?;
    let path = parent.join(format!("{}.json", &pair[..16]));
    let mut f = tempfile::NamedTempFile::new_in(&parent)?;
    f.write_all(&bytes)?;
    f.as_file().sync_all()?;
    match f.persist_noclobber(&path) {
        Ok(_) => Ok(()),
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.error).context("crop states"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qwen::validate::{compare_with, tests::write, Compare, Rule};
    use std::path::PathBuf;

    const P: Params = Params::IN_RULE;

    fn map<'a>(crops: &[(&'a str, &'a str, bool)]) -> BTreeMap<&'a str, Side<'a>> {
        crops
            .iter()
            .map(|(id, text, looped)| {
                (
                    *id,
                    Side {
                        text,
                        looped: *looped,
                    },
                )
            })
            .collect()
    }

    fn verdict(a: &[(&str, &str, bool)], b: &[(&str, &str, bool)]) -> Result<(), String> {
        judge_crops(&map(a), &map(b), P).1
    }

    const LINE_A: &str = "The ship sailed for England on Tuesday morning.";
    const LINE_B: &str = "The ship sailed for Ergland on Tuesday morninq.";
    const OTHER: &str = "Eggs were sold at twelve cents the dozen in town.";

    #[test]
    fn near_identical_readings_agree_and_unrelated_ones_do_not() {
        assert!(similarity(LINE_A, LINE_B) > 0.9);
        assert!(similarity(LINE_A, OTHER) < 0.5);
        assert_eq!(similarity("a  b\nc", "a b c"), 1.0);
        assert_eq!(similarity("", ""), 1.0);
        assert_eq!(similarity("abc", ""), 0.0);
    }

    #[test]
    fn scattered_character_errors_agree_where_word_dice_does_not() {
        // WU 21128 c02's shape: most characters shared, few words whole.
        let a = "alpha betta gamma delta epsilon zeta";
        let b = "alpho betty gammo delty epsilon zeta";
        assert!(usnm_ocr_contract::boinc::dice(a, b) < 0.5);
        assert_eq!(
            crop_state(
                Side {
                    text: a,
                    looped: false
                },
                Side {
                    text: b,
                    looped: false
                },
                P
            )
            .0,
            CropState::Agree
        );
    }

    #[test]
    fn a_looped_crop_counts_for_neither_side() {
        let (long_a, long_b) = (LINE_A.repeat(2), LINE_B.repeat(2));
        let a = [
            ("c01", long_a.as_str(), false),
            ("c02", long_a.as_str(), false),
            ("c03", "anything", true),
        ];
        let b = [
            ("c01", long_b.as_str(), false),
            ("c02", long_b.as_str(), false),
            ("c03", OTHER, false),
        ];
        assert_eq!(verdict(&a, &b), Ok(()));
        let (crops, _) = judge_crops(&map(&a), &map(&b), P);
        assert_eq!(crops[2].state, CropState::Looped);
    }

    #[test]
    fn known_bad_pairs_fail() {
        let good = [
            ("c01", LINE_A, false),
            ("c02", OTHER, false),
            ("c03", LINE_A, false),
            ("c04", OTHER, false),
        ];
        // Two loops of the same words: Dice 1.0, and nothing read (WU 21131 c07).
        let run = "tobacco ".repeat(120);
        let looped = [("c01", run.as_str(), true), ("c02", run.as_str(), true)];
        assert!(verdict(&looped, &looped).is_err());
        // Loops on half the crops with text: no majority.
        let half = [
            ("c01", LINE_A, false),
            ("c02", OTHER, false),
            ("c03", "x", true),
            ("c04", "x", true),
        ];
        assert!(verdict(&good, &half).is_err());
        // Nothing read against something read.
        let unread = [
            ("c01", "[?]", false),
            ("c02", "[?]", false),
            ("c03", "[?]", false),
            ("c04", "[?]", false),
        ];
        assert!(verdict(&good, &unread).is_err());
        // One unrelated crop, and crops read out of place.
        let mut one_wrong = good;
        one_wrong[2].1 = OTHER;
        assert!(verdict(&good, &one_wrong).is_err());
        let rotated = [
            ("c01", OTHER, false),
            ("c02", LINE_A, false),
            ("c03", OTHER, false),
            ("c04", LINE_A, false),
        ];
        assert!(verdict(&good, &rotated).is_err());
        // A crop only one side read.
        assert!(verdict(&good, &good[..3]).is_err());
    }

    #[test]
    fn folding_points_leaves_letters_and_punctuation() {
        let pointed = "\u{5e7}\u{5b0}\u{5e8}\u{5b7}\u{5e4}\u{5bc} \u{5be} \u{5e1}";
        assert_eq!(
            without_hebrew_points(pointed),
            "\u{5e7}\u{5e8}\u{5e4} \u{5be} \u{5e1}"
        );
        assert_eq!(without_hebrew_points("plain, text"), "plain, text");
        let (a, b) = (
            "\u{5e7}\u{5b0}\u{5e8}\u{5b7}\u{5e4}\u{5bc}\u{5b0}",
            "\u{5e7}\u{5e8}\u{5e4}",
        );
        assert_eq!(similarity(a, b), 1.0);
    }

    #[test]
    fn a_shared_masthead_alone_is_not_the_same_page() {
        let masthead = [
            ("c01", "JEWISH MORNING JOURNAL", false),
            ("c02", "[?]", false),
        ];
        let (crops, verdict) = judge_crops(&map(&masthead), &map(&masthead), P);
        assert_eq!(crops[0].state, CropState::Agree);
        assert_eq!(verdict.unwrap_err(), "22 characters agree");
    }

    #[test]
    fn no_crops_at_all_is_not_agreement() {
        assert_eq!(verdict(&[], &[]).unwrap_err(), "no crop in common");
    }

    #[test]
    fn a_page_with_no_text_keeps_the_strict_rules_answer() {
        let blank = [("c01", "[?]", false), ("c02", "", false)];
        assert_eq!(verdict(&blank, &blank), Ok(()));
    }

    #[test]
    fn the_loop_bits_of_a_guarded_result_count_as_looped() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write(tmp.path(), "a", &[("c01", LINE_A), ("c02", "cut head")]);
        let mut l = crate::qwen::validate::load(&a).unwrap();
        assert!(!sides(&l)["c02"].looped);
        l.readings
            .diag
            .insert("repetition_loop_crop_bits".into(), "02".into());
        let s = sides(&l);
        assert!(s["c02"].looped && !s["c01"].looped);
    }

    #[test]
    fn compare_keeps_the_strict_rule_unless_asked_and_records_crop_states() {
        let tmp = tempfile::tempdir().unwrap();
        let (x, y) = (
            "alpha betta gamma delta epsilon zeta",
            "alpho betty gammo delty epsilon zeta",
        );
        let (long_a, long_b) = (
            format!("{LINE_A} {OTHER} {LINE_A}"),
            format!("{LINE_B} {OTHER} {LINE_B}"),
        );
        let a = write(tmp.path(), "a", &[("c01", &long_a), ("c02", x)]);
        let b = write(tmp.path(), "b", &[("c01", &long_b), ("c02", y)]);
        let (log, states) = (tmp.path().join("pairs.jsonl"), tmp.path().join("states"));
        let opts = |rule| Compare {
            min_dice: 0.5,
            rule,
            log: Some(&log),
            pages: None,
            crop_states: Some(&states),
        };
        assert!(compare_with(&a, &b, opts(Rule::StrictV1)).is_err());
        assert!(!states.exists());
        compare_with(&a, &b, opts(Rule::CropConsensusV2)).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let line: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(line["agree"], true);
        assert_eq!(line["rule"], "crop-consensus-v2");
        assert_eq!(line["crop_states"][1]["state"], "agree");
        assert!(line["strict_reason"].as_str().unwrap().contains("c02"));
        let job = std::fs::read_dir(&states).unwrap().next().unwrap().unwrap();
        let kept = std::fs::read_dir(job.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        // Compared either way round, the record pairs each name with its own digest.
        std::fs::remove_file(kept.path()).unwrap();
        compare_with(&b, &a, opts(Rule::CropConsensusV2)).unwrap();
        let r: serde_json::Value =
            serde_json::from_slice(&std::fs::read(kept.path()).unwrap()).unwrap();
        let load = |f: &[PathBuf]| crate::qwen::validate::load(f).unwrap().result;
        let names = (
            crate::quality::validate::result_name(&a[1]).unwrap(),
            crate::quality::validate::result_name(&b[1]).unwrap(),
        );
        assert!(names.0 < names.1);
        assert_eq!(r["results"][0], names.0);
        assert_eq!(r["readings_sha256"][0], load(&a).readings_sha256);
        assert_eq!(r["readings_sha256"][1], load(&b).readings_sha256);
    }
}
