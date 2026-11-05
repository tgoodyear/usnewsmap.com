//! The `crop-consensus-v2` floor against human transcriptions (#1434).
//!
//! The rule's floor was set on agreement between replicas. This measures it
//! where the truth is known: crops with a human transcription, each read by
//! several readers (builds, backends, models). For each pair of readers it
//! counts the crops under the floor (character similarity and, for
//! comparison, the strict rule's word-bag Dice) and the rule's crop states,
//! and splits them by what the transcription says about the two readings:
//!
//! - `both_good`: both readings within `--good-cer` of the transcription.
//!   These should agree.
//! - `one_bad`: one within `--good-cer`, the other at `--bad-cer` or worse.
//!   These should not.
//! - `both_bad`: both at `--bad-cer` or worse. Agreement here vouches for
//!   wrong text.
//!
//! It also gives the error of the readings the rule would accept, and a
//! shuffled control: each crop's first reading against the second reader's
//! reading of another crop in the same language. Groups are `all`,
//! `lang:<code>`, `source:<name>` and, with `--legibility`, `legibility:<band>`
//! from one reader's error on the crop (`clean` under 5%, `fair` under 20%,
//! `poor` from 20%). `--floor` measures another floor for the rule on the
//! same crops; the strict rule keeps its Dice floor, the one in force.
use super::consensus::{self, CropState, Params, Side};
use anyhow::Context;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    io::Write as _,
    path::{Path, PathBuf},
};
use usnm_ocr_quality::{align, distance, text};

#[derive(Deserialize)]
struct Reference {
    id: String,
    /// The page the crop is from; pairs are judged page by page.
    #[serde(default)]
    page: Option<String>,
    source: String,
    lang: String,
    #[serde(rename = "ref")]
    text: String,
}

#[derive(Deserialize)]
struct Reading {
    id: String,
    #[serde(default)]
    text: Option<String>,
    /// A loop guard cut the reading (Study 5's `stop` files).
    #[serde(default)]
    looped: bool,
}

fn lines<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<Vec<T>> {
    let body = crate::read(path, 256 << 20)?;
    std::str::from_utf8(&body)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| {
            serde_json::from_str(l).with_context(|| format!("{}:{}", path.display(), i + 1))
        })
        .collect()
}

/// Each crop's reading, and whether it looped: marked so, or ending in a
/// repeated run as the rule finds it.
fn readings(path: &Path) -> anyhow::Result<HashMap<String, (String, bool)>> {
    Ok(lines::<Reading>(path)?
        .into_iter()
        .map(|r| {
            let text = r.text.unwrap_or_default();
            let looped = r.looped || usnm_ocr_contract::qwen::tail_repetition(&text).is_some();
            (r.id, (text, looped))
        })
        .collect())
}

/// A reading's character error against the transcription, both normalised
/// as the quality tables are and then, as the rule compares readings, with
/// Hebrew points left out. The normalising is NFC. A pointed presentation
/// form such as U+FB2E has a canonical decomposition, letter and point, and
/// Unicode excludes it from composition, so NFC splits it and the point goes
/// too. A Yiddish reading that differs from its transcription only in points
/// is good, as the rule would treat it.
pub fn cer(hyp: &str, reference: &str) -> f64 {
    let (h, r) = (
        consensus::without_hebrew_points(&text::normalize(hyp, false)),
        consensus::without_hebrew_points(&text::normalize(reference, true)),
    );
    align::rate(
        distance::chars(&h, &r),
        r.chars().count(),
        h.chars().count(),
    )
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Tally {
    pub crops: usize,
    pub similarity_below: usize,
    pub dice_below: usize,
    pub agree: usize,
    pub disagree: usize,
    pub looped: usize,
    pub no_text: usize,
    pub both_good: usize,
    pub both_good_agree: usize,
    pub both_good_dice_pass: usize,
    pub one_bad: usize,
    pub one_bad_agree: usize,
    pub one_bad_dice_pass: usize,
    pub both_bad: usize,
    pub both_bad_agree: usize,
    pub both_bad_dice_pass: usize,
    /// Of the `agree` crops, those whose worse reading is at `--bad-cer` or
    /// worse against the transcription, and those over 25%.
    pub agree_worse_bad: usize,
    pub agree_worse_over_25: usize,
    /// The mean error of the worse reading of the `agree` crops.
    pub agree_worse_cer_mean: Option<f64>,
    #[serde(skip)]
    agree_worse_sum: f64,
    /// The same for the crops the strict rule's Dice passes.
    pub dice_pass: usize,
    pub dice_pass_worse_bad: usize,
    pub dice_pass_worse_over_25: usize,
    /// Crops only the rule passes (character similarity at the floor, Dice
    /// under it), and how far their worse reading is from the transcription.
    pub rule_only: usize,
    pub rule_only_worse_bad: usize,
    pub rule_only_worse_over_25: usize,
    pub rule_only_worse_cer_mean: Option<f64>,
    #[serde(skip)]
    rule_only_worse_sum: f64,
    /// Crops only the Dice passes.
    pub dice_only: usize,
    /// Each page both readers read in full, judged as one pair by both rules
    /// (a page with a crop missing on either side is not counted): the pages that
    /// pass (counted in their first crop's language and source groups), and
    /// the crops each rule would join, with their worse reading's error. The
    /// strict rule joins every crop of a page it passes; the rule joins the
    /// `agree` crops of a page it passes.
    pub pages: usize,
    pub strict_pages: usize,
    pub rule_pages: usize,
    pub strict_joined: usize,
    pub strict_joined_worse_bad: usize,
    pub strict_joined_worse_over_25: usize,
    pub strict_joined_worse_cer_mean: Option<f64>,
    #[serde(skip)]
    strict_joined_worse_sum: f64,
    pub rule_joined: usize,
    pub rule_joined_worse_bad: usize,
    pub rule_joined_worse_over_25: usize,
    pub rule_joined_worse_cer_mean: Option<f64>,
    #[serde(skip)]
    rule_joined_worse_sum: f64,
    pub shuffled: usize,
    pub shuffled_similarity_below: usize,
    pub shuffled_dice_below: usize,
    pub shuffled_agree: usize,
}

pub struct Options<'a> {
    pub refs: &'a Path,
    pub readers: &'a [(String, PathBuf)],
    pub pairs: &'a [(String, String)],
    pub legibility: Option<&'a Path>,
    pub good_cer: f64,
    pub bad_cer: f64,
    /// The rule's floor, to measure another one; the rule's own is
    /// [`Params::IN_RULE`]'s.
    pub floor: f64,
    pub seed: u64,
    pub out: &'a Path,
    /// Where to write one line per crop and pair, when given.
    pub crops: Option<&'a Path>,
}

fn band(cer: f64) -> &'static str {
    if cer < 0.05 {
        "clean"
    } else if cer < 0.20 {
        "fair"
    } else {
        "poor"
    }
}

/// The strict rule's Dice floor, as the validator wrapper runs it.
const STRICT_DICE: f64 = usnm_ocr_contract::qwen::PROBE_MIN_DICE;

/// The strict rule on one crop: both read or neither, and Dice at the floor.
fn strict_crop(a: &str, b: &str, floor: f64) -> bool {
    match (a.trim().is_empty(), b.trim().is_empty()) {
        (true, true) => true,
        (false, false) => usnm_ocr_contract::boinc::dice(a, b) >= floor,
        _ => false,
    }
}

fn side(r: &(String, bool)) -> Side<'_> {
    Side {
        text: &r.0,
        looped: r.1,
    }
}

/// A seeded permutation of `items` with no item left in place, when there
/// are at least two.
fn derangement(n: usize, seed: &mut u64) -> Vec<usize> {
    let mut next = || {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as usize
    };
    let mut p: Vec<usize> = (0..n).collect();
    if n < 2 {
        return p;
    }
    // Sattolo's algorithm: one cycle through every item.
    for i in (1..n).rev() {
        let j = next() % i;
        p.swap(i, j);
    }
    p
}

pub fn run(opts: &Options<'_>) -> anyhow::Result<BTreeMap<(String, String), Tally>> {
    let refs: Vec<Reference> = lines(opts.refs)?;
    let mut by_name: HashMap<&str, HashMap<String, (String, bool)>> = HashMap::new();
    for (name, path) in opts.readers {
        by_name.insert(name, readings(path)?);
    }
    let legible: Option<HashMap<String, (String, bool)>> =
        opts.legibility.map(readings).transpose()?;
    let p = Params {
        floor: opts.floor,
        ..Params::IN_RULE
    };
    let mut out: BTreeMap<(String, String), Tally> = BTreeMap::new();
    let mut seed = opts.seed;
    let mut crop_lines = Vec::new();
    for (a, b) in opts.pairs {
        let (ra, rb) = (
            by_name
                .get(a.as_str())
                .with_context(|| format!("no reader {a}"))?,
            by_name
                .get(b.as_str())
                .with_context(|| format!("no reader {b}"))?,
        );
        let pair = format!("{a}:{b}");
        // Crops both readers read, with their groups.
        type Crop<'a> = (
            &'a Reference,
            &'a (String, bool),
            &'a (String, bool),
            Vec<String>,
        );
        let crops: Vec<Crop<'_>> = refs
            .iter()
            .filter_map(|r| {
                let (ta, tb) = (ra.get(&r.id)?, rb.get(&r.id)?);
                let mut groups = vec![
                    "all".to_owned(),
                    format!("lang:{}", r.lang),
                    format!("source:{}", r.source),
                ];
                if let Some(l) = &legible {
                    groups.push(format!(
                        "legibility:{}",
                        l.get(&r.id).map_or("unread", |t| band(cer(&t.0, &r.text)))
                    ));
                }
                Some((r, ta, tb, groups))
            })
            .collect();
        // Each page's crops as one pair, under both rules.
        let page_of = |r: &Reference| r.page.clone().unwrap_or_else(|| r.id.clone());
        let mut pages: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, (r, ..)) in crops.iter().enumerate() {
            pages.entry(page_of(r)).or_default().push(i);
        }
        let mut verdicts: HashMap<String, (bool, bool)> = HashMap::new();
        // Only pages both readers read in full are judged as pairs: a missing
        // crop would change either verdict.
        let total: HashMap<String, usize> = refs.iter().fold(HashMap::new(), |mut m, r| {
            *m.entry(page_of(r)).or_default() += 1;
            m
        });
        for (page, idx) in &pages {
            if idx.len() != total[page] {
                continue;
            }
            let strict = idx
                .iter()
                .all(|&i| strict_crop(&crops[i].1 .0, &crops[i].2 .0, STRICT_DICE));
            let (sa, sb): (BTreeMap<&str, Side<'_>>, BTreeMap<&str, Side<'_>>) = (
                idx.iter()
                    .map(|&i| (crops[i].0.id.as_str(), side(crops[i].1)))
                    .collect(),
                idx.iter()
                    .map(|&i| (crops[i].0.id.as_str(), side(crops[i].2)))
                    .collect(),
            );
            let rule = consensus::judge_crops(&sa, &sb, p).1.is_ok();
            verdicts.insert(page.clone(), (strict, rule));
            for g in crops[idx[0]]
                .3
                .iter()
                .filter(|g| !g.starts_with("legibility:"))
            {
                let t = out.entry((pair.clone(), g.clone())).or_default();
                t.pages += 1;
                t.strict_pages += usize::from(strict);
                t.rule_pages += usize::from(rule);
            }
        }
        for (r, ta, tb, groups) in &crops {
            let (ca, cb) = (cer(&ta.0, &r.text), cer(&tb.0, &r.text));
            let (page_strict, page_rule) =
                verdicts.get(&page_of(r)).copied().unwrap_or((false, false));
            let (good, bad) = (opts.good_cer, opts.bad_cer);
            let (state, _) = consensus::crop_state(side(ta), side(tb), p);
            let sim = consensus::similarity(&ta.0, &tb.0);
            let dice = usnm_ocr_contract::boinc::dice(&ta.0, &tb.0);
            let agree = state == CropState::Agree;
            let dice_pass = dice >= STRICT_DICE;
            let worse = ca.max(cb);
            if opts.crops.is_some() {
                crop_lines.push(serde_json::json!({
                    "pair": pair, "id": r.id, "lang": r.lang, "source": r.source,
                    "cer_a": usnm_ocr_quality::report::round6(ca),
                    "cer_b": usnm_ocr_quality::report::round6(cb),
                    "similarity": usnm_ocr_quality::report::round6(sim),
                    "dice": usnm_ocr_quality::report::round6(dice),
                    "state": state.as_str(),
                }));
            }
            for g in groups {
                let t = out.entry((pair.clone(), g.clone())).or_default();
                t.crops += 1;
                t.similarity_below += usize::from(sim < p.floor);
                t.dice_below += usize::from(!dice_pass);
                match state {
                    CropState::Agree => t.agree += 1,
                    CropState::Disagree => t.disagree += 1,
                    CropState::Looped => t.looped += 1,
                    CropState::NoText => t.no_text += 1,
                }
                if ca <= good && cb <= good {
                    t.both_good += 1;
                    t.both_good_agree += usize::from(agree);
                    t.both_good_dice_pass += usize::from(dice_pass);
                } else if (ca <= good && cb >= bad) || (cb <= good && ca >= bad) {
                    t.one_bad += 1;
                    t.one_bad_agree += usize::from(agree);
                    t.one_bad_dice_pass += usize::from(dice_pass);
                } else if ca >= bad && cb >= bad {
                    t.both_bad += 1;
                    t.both_bad_agree += usize::from(agree);
                    t.both_bad_dice_pass += usize::from(dice_pass);
                }
                if agree {
                    t.agree_worse_bad += usize::from(worse >= bad);
                    t.agree_worse_over_25 += usize::from(worse > 0.25);
                    t.agree_worse_sum += worse;
                }
                if dice_pass {
                    t.dice_pass += 1;
                    t.dice_pass_worse_bad += usize::from(worse >= bad);
                    t.dice_pass_worse_over_25 += usize::from(worse > 0.25);
                }
                if agree && !dice_pass {
                    t.rule_only += 1;
                    t.rule_only_worse_bad += usize::from(worse >= bad);
                    t.rule_only_worse_over_25 += usize::from(worse > 0.25);
                    t.rule_only_worse_sum += worse;
                }
                t.dice_only += usize::from(dice_pass && !agree);
                if page_strict {
                    t.strict_joined += 1;
                    t.strict_joined_worse_bad += usize::from(worse >= bad);
                    t.strict_joined_worse_over_25 += usize::from(worse > 0.25);
                    t.strict_joined_worse_sum += worse;
                }
                if page_rule && agree {
                    t.rule_joined += 1;
                    t.rule_joined_worse_bad += usize::from(worse >= bad);
                    t.rule_joined_worse_over_25 += usize::from(worse > 0.25);
                    t.rule_joined_worse_sum += worse;
                }
            }
        }
        // The control: within each language, the first reading of each crop
        // against the second reader's reading of another crop.
        let mut by_lang: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, (r, ..)) in crops.iter().enumerate() {
            by_lang.entry(r.lang.as_str()).or_default().push(i);
        }
        for idx in by_lang.values() {
            let perm = derangement(idx.len(), &mut seed);
            for (k, &i) in idx.iter().enumerate() {
                let j = idx[perm[k]];
                if i == j {
                    continue;
                }
                let (ta, tb) = (crops[i].1, crops[j].2);
                let (state, _) = consensus::crop_state(side(ta), side(tb), p);
                let sim = consensus::similarity(&ta.0, &tb.0);
                let dice = usnm_ocr_contract::boinc::dice(&ta.0, &tb.0);
                for g in &crops[i].3 {
                    let t = out.entry((pair.clone(), g.clone())).or_default();
                    t.shuffled += 1;
                    t.shuffled_similarity_below += usize::from(sim < p.floor);
                    t.shuffled_dice_below += usize::from(dice < STRICT_DICE);
                    t.shuffled_agree += usize::from(state == CropState::Agree);
                }
            }
        }
    }
    for t in out.values_mut() {
        t.agree_worse_cer_mean = (t.agree > 0)
            .then(|| usnm_ocr_quality::report::round6(t.agree_worse_sum / t.agree as f64));
        let mean =
            |sum: f64, n: usize| (n > 0).then(|| usnm_ocr_quality::report::round6(sum / n as f64));
        t.rule_only_worse_cer_mean = mean(t.rule_only_worse_sum, t.rule_only);
        t.strict_joined_worse_cer_mean = mean(t.strict_joined_worse_sum, t.strict_joined);
        t.rule_joined_worse_cer_mean = mean(t.rule_joined_worse_sum, t.rule_joined);
    }
    if let Some(path) = opts.crops {
        let mut f = std::fs::File::create(path)?;
        for line in &crop_lines {
            writeln!(f, "{line}")?;
        }
    }
    if let Some(parent) = opts.out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(opts.out)?;
    for ((pair, group), t) in &out {
        let mut v = serde_json::to_value(t)?;
        v["pair"] = pair.as_str().into();
        v["group"] = group.as_str().into();
        v["floor"] = p.floor.into();
        v["good_cer"] = opts.good_cer.into();
        v["bad_cer"] = opts.bad_cer.into();
        writeln!(f, "{v}")?;
    }
    Ok(out)
}

/// The tallies as a Markdown table.
pub fn table(out: &BTreeMap<(String, String), Tally>) -> String {
    let mut s = String::from(
        "| Pair | Group | Crops | Sim < floor | Dice < floor | Agree | Looped | Both good: agree | One bad: agree | Both bad: agree | Agree, worse reading CER >= bad | Shuffled: agree |\n|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n",
    );
    for ((pair, group), t) in out {
        s.push_str(&format!(
            "| {pair} | {group} | {} | {} | {} | {} | {} | {} of {} | {} of {} | {} of {} | {} | {} of {} |\n",
            t.crops,
            t.similarity_below,
            t.dice_below,
            t.agree,
            t.looped,
            t.both_good_agree,
            t.both_good,
            t.one_bad_agree,
            t.one_bad,
            t.both_bad_agree,
            t.both_bad,
            t.agree_worse_bad,
            t.shuffled_agree,
            t.shuffled,
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounded_pairs_split_by_the_transcription() {
        let tmp = tempfile::tempdir().unwrap();
        let w = |name: &str, body: &str| {
            let p = tmp.path().join(name);
            std::fs::write(&p, body).unwrap();
            p
        };
        let refs = w(
            "refs.jsonl",
            concat!(
                r#"{"id":"c1","source":"s","lang":"eng","ref":"The ship sailed for England on Tuesday."}"#,
                "\n",
                r#"{"id":"c2","source":"s","lang":"eng","ref":"Eggs were sold at twelve cents the dozen."}"#,
                "\n",
            ),
        );
        let a = w(
            "a.jsonl",
            concat!(
                r#"{"id":"c1","text":"The ship sailed for England on Tuesday."}"#,
                "\n",
                r#"{"id":"c2","text":"Eggs were sold at twelve cents the dozen."}"#,
                "\n",
            ),
        );
        let b = w(
            "b.jsonl",
            concat!(
                r#"{"id":"c1","text":"The ship sailed for Ergland on Tuesday."}"#,
                "\n",
                r#"{"id":"c2","text":"Nothing like the crop at all, entirely other words here."}"#,
                "\n",
            ),
        );
        let readers = vec![("a".to_owned(), a), ("b".to_owned(), b)];
        let pairs = vec![("a".to_owned(), "b".to_owned())];
        let out = tmp.path().join("out.jsonl");
        let got = run(&Options {
            refs: &refs,
            readers: &readers,
            pairs: &pairs,
            legibility: None,
            good_cer: 0.10,
            bad_cer: 0.50,
            floor: Params::IN_RULE.floor,
            seed: 1,
            out: &out,
            crops: None,
        })
        .unwrap();
        let t = &got[&("a:b".to_owned(), "all".to_owned())];
        assert_eq!(t.crops, 2);
        assert_eq!((t.both_good, t.both_good_agree), (1, 1));
        assert_eq!((t.one_bad, t.one_bad_agree), (1, 0));
        assert_eq!(t.similarity_below, 1);
        assert_eq!(t.shuffled, 2);
        assert_eq!(t.shuffled_agree, 0);
        // Each crop is its own page here: the strict rule passes c1's page,
        // and the rule doesn't, as 39 agreeing characters are under its limit.
        assert_eq!((t.pages, t.strict_pages, t.rule_pages), (2, 1, 0));
        assert_eq!((t.strict_joined, t.rule_joined), (1, 0));
        assert!(std::fs::read_to_string(&out).unwrap().lines().count() >= 3);
        assert!(table(&got).contains("| a:b | all | 2 |"));
    }

    #[test]
    fn yiddish_error_leaves_points_out_as_the_rule_does() {
        // "der arbeter": the transcription points its alef, one reading
        // writes it as the presentation form U+FB2E, one leaves it bare.
        let reference =
            "\u{5d3}\u{5e2}\u{5e8} \u{5d0}\u{5b7}\u{5e8}\u{5d1}\u{5e2}\u{5d8}\u{5e2}\u{5e8}";
        let form = "\u{5d3}\u{5e2}\u{5e8} \u{fb2e}\u{5e8}\u{5d1}\u{5e2}\u{5d8}\u{5e2}\u{5e8}";
        let bare = "\u{5d3}\u{5e2}\u{5e8} \u{5d0}\u{5e8}\u{5d1}\u{5e2}\u{5d8}\u{5e2}\u{5e8}";
        assert_eq!(cer(form, reference), 0.0);
        assert_eq!(cer(bare, reference), 0.0);
        assert_eq!(cer(reference, bare), 0.0);
        // A wrong letter still counts.
        let wrong = "\u{5d3}\u{5e2}\u{5e8} \u{5d0}\u{5e8}\u{5d1}\u{5e2}\u{5d8}\u{5e2}\u{5dd}";
        assert!(cer(wrong, reference) > 0.0);
    }

    #[test]
    fn derangements_move_every_item() {
        let mut seed = 7;
        for n in 2..20 {
            let p = derangement(n, &mut seed);
            assert!(p.iter().enumerate().all(|(i, &j)| i != j));
            let mut q = p.clone();
            q.sort_unstable();
            assert_eq!(q, (0..n).collect::<Vec<_>>());
        }
    }
}
