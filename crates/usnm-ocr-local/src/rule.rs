//! Rule v1 (12 §12.5 Step 9; provisional until the pilot, §12.6 question
//! 3): when our OCR of a page replaces LoC's text in the index.
//!
//! Ours replaces LoC's when
//!
//! 1. both are decided in the same language, or LoC's is `und` (or has no
//!    words) and ours is decided in one of the title's languages;
//! 2. our damage rate is at least [`MARGIN`] lower than LoC's (any rate
//!    does when LoC's has none);
//! 3. our head count is at least LoC's ([`HEAD_TEST`]), so a short output
//!    that looks clean can't win on its rate alone;
//! 4. our reading is complete next to LoC's ([`Completeness`]): at least
//!    [`MIN_LEN_RATIO`] of LoC's characters and [`MIN_COVER`] of LoC's words
//!    (as a bag) are in ours. The damage rate is a rate over the words that
//!    remain, and the head count is only a floor on what is left: a layout
//!    pass that misses columns of a page (seq-7 of the 8 Oct 1876 issue:
//!    25k characters where LoC has 70k) can pass both while the index loses
//!    most of the page. Fraktur pages, whose LoC text is mostly noise, pass
//!    the head test with a lot of the page missing; this test keeps them.
//!
//! The release scores both texts again with `usnm_core::damage`; it never
//! trusts stored scores. `ocr-sync --report` applies the rule to the stored
//! ones, without the title-language test, as an estimate. A version records
//! the rule it applied ([`record`]), so a change to these constants is a new
//! [`VERSION`].
//!
//! A line job's page (#1621: LoC's page with its Cherokee lines read again one at a time, by a
//! `lines_only` profile) is decided by [`line_merge`] instead, rule `line-merge-v1`. Rule v1's
//! whole-page language and damage tests don't apply to it: the page is LoC's apart from its
//! Cherokee lines, which the damage metric barely sees on a mostly English page.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::pages::Scores;

/// The rule's version, in each version's `ocr_local.json`.
pub const VERSION: u32 = 1;
/// How much lower our damage rate must be.
pub const MARGIN: f64 = 0.05;
/// Whether our head count must be at least LoC's.
pub const HEAD_TEST: bool = true;
/// The shortest our text may be next to LoC's, in characters, as a ratio (ours over LoC's).
pub const MIN_LEN_RATIO: f64 = 0.85;
/// The least share of LoC's words (lower case, with multiplicity) our text must have.
pub const MIN_COVER: f64 = 0.30;
/// LoC texts with fewer words than this are not weighed for completeness: a bag of a few
/// words says little, and a page that short has little to lose.
pub const MIN_LOC_WORDS: usize = 20;
/// Environment variables that override the three thresholds for a release.
pub const ENV_MIN_LEN_RATIO: &str = "USNM_OCR_LOCAL_MIN_LEN_RATIO";
pub const ENV_MIN_COVER: &str = "USNM_OCR_LOCAL_MIN_COVER";
pub const ENV_MIN_LOC_WORDS: &str = "USNM_OCR_LOCAL_MIN_LOC_WORDS";

/// Rates are sums of counts divided by counts: a rate exactly [`MARGIN`]
/// lower can come out a rounding error above it.
const EPSILON: f64 = 1e-9;

/// Why LoC's text stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Keep {
    /// Our text has no language: too few words, or no clear winner.
    OursUndecided,
    /// Our text has a language but no damage rate.
    OursUnscored,
    /// The two texts are decided in different languages.
    LanguageDiffers,
    /// LoC's is `und` and ours is decided in a language the title doesn't list.
    TitleLanguage,
    /// Our damage rate isn't [`MARGIN`] lower.
    Margin,
    /// Our text has fewer head words.
    Head,
    /// Our text has under [`Completeness::min_len_ratio`] of LoC's characters.
    Short,
    /// Our text has under [`Completeness::min_cover`] of LoC's words.
    Cover,
    /// `line-merge-v1`: a line the job didn't read isn't LoC's text, byte for byte.
    LineChanged,
    /// `line-merge-v1`: under half the page's Cherokee lines took the model's reading.
    ModelShare,
}

impl Keep {
    pub fn as_str(self) -> &'static str {
        match self {
            Keep::OursUndecided => "ours_undecided",
            Keep::OursUnscored => "ours_unscored",
            Keep::LanguageDiffers => "language_differs",
            Keep::TitleLanguage => "title_language",
            Keep::Margin => "margin",
            Keep::Head => "head",
            Keep::Short => "short",
            Keep::Cover => "cover",
            Keep::LineChanged => "line_changed",
            Keep::ModelShare => "model_share",
        }
    }
}

/// Whether `ours` replaces `loc`. `title_languages` is the title's catalog
/// languages, for an `und` LoC text (none listed: English, as the metric
/// scores against the title's languages plus English); `None` skips that
/// test (`ocr-sync --report`, which has no catalog).
pub fn verdict(ours: &Scores, loc: &Scores, title_languages: Option<&[&str]>) -> Result<(), Keep> {
    let und = usnm_core::damage::UNKNOWN;
    let Some(our_lang) = ours.lang.as_deref().filter(|l| *l != und) else {
        return Err(Keep::OursUndecided);
    };
    let Some(our_rate) = ours.damage_rate else {
        return Err(Keep::OursUnscored);
    };
    let loc_decided = loc.lang.as_deref().filter(|l| *l != und);
    match loc_decided {
        Some(l) if l != our_lang => return Err(Keep::LanguageDiffers),
        Some(_) => {}
        None => {
            if let Some(langs) = title_languages {
                let listed = if langs.is_empty() {
                    our_lang == "eng"
                } else {
                    langs.contains(&our_lang)
                };
                if !listed {
                    return Err(Keep::TitleLanguage);
                }
            }
        }
    }
    if let (Some(_), Some(loc_rate)) = (loc_decided, loc.damage_rate) {
        if our_rate > loc_rate - MARGIN + EPSILON {
            return Err(Keep::Margin);
        }
    }
    if HEAD_TEST && ours.head.unwrap_or(0) < loc.head.unwrap_or(0) {
        return Err(Keep::Head);
    }
    Ok(())
}

/// The completeness gate: our text replaces LoC's only if it keeps enough of the page.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Completeness {
    /// Our characters over LoC's, at least.
    pub min_len_ratio: f64,
    /// The share of LoC's words that ours also has, at least.
    pub min_cover: f64,
    /// LoC texts with fewer words than this pass without being weighed.
    pub min_loc_words: usize,
}

impl Default for Completeness {
    fn default() -> Self {
        Self {
            min_len_ratio: MIN_LEN_RATIO,
            min_cover: MIN_COVER,
            min_loc_words: MIN_LOC_WORDS,
        }
    }
}

/// What the gate measured on one page.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct Measure {
    pub ours_chars: usize,
    pub loc_chars: usize,
    /// `ours_chars / loc_chars`.
    pub len_ratio: f64,
    /// The share of LoC's words (counted with multiplicity) that ours also has.
    pub cover: f64,
}

/// The words of a text for the bag: lower case runs of letters and digits, and their count.
fn bag(text: &str) -> (HashMap<String, u32>, usize) {
    let lower = text.to_lowercase();
    let mut bag: HashMap<String, u32> = HashMap::new();
    let mut total = 0;
    for w in lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        *bag.entry(w.to_owned()).or_default() += 1;
        total += 1;
    }
    (bag, total)
}

impl Completeness {
    /// The thresholds as the environment sets them ([`ENV_MIN_LEN_RATIO`], [`ENV_MIN_COVER`],
    /// [`ENV_MIN_LOC_WORDS`]), else the defaults. A value that doesn't parse or is out of
    /// range is an error: a release must not run on a threshold the operator mistyped.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_vars(|k| std::env::var(k).ok())
    }

    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut c = Self::default();
        if let Some(v) = get(ENV_MIN_LEN_RATIO) {
            c.min_len_ratio = v
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{ENV_MIN_LEN_RATIO} is not a number"))?;
        }
        if let Some(v) = get(ENV_MIN_COVER) {
            c.min_cover = v
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{ENV_MIN_COVER} is not a number"))?;
        }
        if let Some(v) = get(ENV_MIN_LOC_WORDS) {
            c.min_loc_words = v
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{ENV_MIN_LOC_WORDS} is not a whole number"))?;
        }
        anyhow::ensure!(
            (0.0..=1.5).contains(&c.min_len_ratio) && (0.0..=1.0).contains(&c.min_cover),
            "completeness thresholds out of range: ratio {} (0..=1.5), cover {} (0..=1)",
            c.min_len_ratio,
            c.min_cover
        );
        Ok(c)
    }

    /// The gate on a page whose other tests passed: what was measured (none when LoC has no
    /// text to lose, or too few words), and `Err` with why LoC's text stays.
    pub fn check(&self, ours: &str, loc: Option<&str>) -> (Option<Measure>, Result<(), Keep>) {
        let Some(loc) = loc.filter(|t| !t.trim().is_empty()) else {
            return (None, Ok(()));
        };
        let (loc_bag, loc_words) = bag(loc);
        if loc_words == 0 || loc_words < self.min_loc_words {
            return (None, Ok(()));
        }
        let (ours_bag, _) = bag(ours);
        let shared: u64 = loc_bag
            .iter()
            .map(|(w, n)| u64::from((*n).min(ours_bag.get(w).copied().unwrap_or(0))))
            .sum();
        let (ours_chars, loc_chars) = (ours.chars().count(), loc.chars().count());
        let m = Measure {
            ours_chars,
            loc_chars,
            len_ratio: ours_chars as f64 / loc_chars as f64,
            cover: shared as f64 / loc_words as f64,
        };
        let v = if m.len_ratio < self.min_len_ratio - EPSILON {
            Err(Keep::Short)
        } else if m.cover < self.min_cover - EPSILON {
            Err(Keep::Cover)
        } else {
            Ok(())
        };
        (Some(m), v)
    }
}

/// Both texts scored again against the title's languages, and the verdict.
pub fn judge(
    ours_text: &str,
    loc_text: Option<&str>,
    title_languages: &[&str],
) -> (Scores, Scores, Result<(), Keep>) {
    let ours = Scores::of(ours_text, title_languages);
    let loc = Scores::of(loc_text.unwrap_or_default(), title_languages);
    let v = verdict(&ours, &loc, Some(title_languages));
    (ours, loc, v)
}

/// The rule that decides a row read with `ocr_engine`: [`line_merge::VERSION`] for a line
/// job's page, else rule v1 (`v1`).
pub fn rule_for(ocr_engine: &str) -> &'static str {
    if line_merge::is_line_job(ocr_engine) {
        line_merge::VERSION
    } else {
        "v1"
    }
}

/// [`judge`] for a row read with `ocr_engine`, and the rule that decided. A line job's page
/// passed [`line_merge::verdict`] against LoC's ALTO when it was imported (its lines are
/// checked where LoC's ALTO is, `usnm-boinc` import), so it is admitted here; both texts are
/// still scored for the tally. Every other row goes through rule v1 as before, and a page
/// that passes it goes through the completeness gate `gate`; the [`Measure`] is given when
/// the gate weighed the page, whichever way it went.
pub fn judge_row(
    ocr_engine: &str,
    ours_text: &str,
    loc_text: Option<&str>,
    title_languages: &[&str],
    gate: &Completeness,
) -> (
    Scores,
    Scores,
    Result<(), Keep>,
    &'static str,
    Option<Measure>,
) {
    let (ours, loc, v1) = judge(ours_text, loc_text, title_languages);
    match rule_for(ocr_engine) {
        line_merge::VERSION => (ours, loc, Ok(()), line_merge::VERSION, None),
        rule if v1.is_err() => (ours, loc, v1, rule, None),
        rule => {
            let (measure, v) = gate.check(ours_text, loc_text);
            (ours, loc, v, rule, measure)
        }
    }
}

/// Rule `line-merge-v1` (#1621), for a line job's page: admitted when every line the job
/// didn't read is LoC's text byte for byte, and the model's reading was taken on at least
/// [`MIN_MODEL_SHARE`](line_merge::MIN_MODEL_SHARE) of the lines it read.
pub mod line_merge {
    use super::Keep;

    pub const VERSION: &str = "line-merge-v1";
    pub const MIN_MODEL_SHARE: f64 = 0.5;

    /// One line of a line job's page, in reading order: LoC's text from LoC's ALTO, ours as
    /// the page is published, and the model's reading when the job read the line (a Cherokee
    /// line).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Line<'a> {
        pub loc: &'a str,
        pub ours: &'a str,
        pub read: Option<&'a str>,
    }

    /// The Cherokee lines and those of them that took the model's reading.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
    pub struct Counts {
        pub cherokee: usize,
        pub model: usize,
    }

    /// Whether the page is admitted, with its counts either way. A Cherokee line took the
    /// model's reading when ours is that reading and it isn't empty, even when it is LoC's text
    /// too: a model that agrees with LoC is still the model's reading.
    pub fn verdict(lines: &[Line<'_>]) -> (Counts, Result<(), Keep>) {
        let mut counts = Counts::default();
        let mut changed = false;
        for line in lines {
            match line.read {
                None => changed |= line.ours != line.loc,
                Some(read) => {
                    counts.cherokee += 1;
                    let read = read.trim();
                    counts.model += usize::from(!read.is_empty() && line.ours == read);
                }
            }
        }
        let verdict = if changed {
            Err(Keep::LineChanged)
        } else if counts.cherokee == 0
            || (counts.model as f64) < MIN_MODEL_SHARE * counts.cherokee as f64
        {
            Err(Keep::ModelShare)
        } else {
            Ok(())
        };
        (counts, verdict)
    }

    /// Whether a row read with `ocr_engine` is a line job's page: its tag's profile is one the
    /// engine catalog marks `lines_only`, in the tag's engine family.
    pub fn is_line_job(ocr_engine: &str) -> bool {
        let Ok(usnm_core::ocr_engine::EngineTag::Tag(tag)) =
            usnm_core::ocr_engine::EngineTag::parse(ocr_engine)
        else {
            return false;
        };
        usnm_ocr_route::ENGINES.engine.iter().any(|e| {
            e.family == tag.family()
                && e.profile
                    .iter()
                    .any(|p| p.lines_only && p.id == tag.profile())
        })
    }
}

/// The rule and the metric, as a version's `ocr_local.json` records them, with the completeness
/// thresholds the build ran on.
pub fn record(completeness: &Completeness) -> Value {
    json!({
        "rule": {
            "version": VERSION,
            "margin": MARGIN,
            "head_test": HEAD_TEST,
            "same_language": true,
            "completeness": completeness,
        },
        "line_merge": {
            "version": line_merge::VERSION,
            "min_model_share": line_merge::MIN_MODEL_SHARE,
            "other_lines": "LoC's ALTO text, byte for byte",
        },
        "metric": {
            "name": "damage",
            "version": usnm_core::damage::VERSION,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden texts for the rule: an English title's page as LoC read it
    /// (garbled), as our OCR read it, and a short clean output that read
    /// only part of the page.
    const LOC_GARBLED: &str = "Tbe cify councll met on Tuesdav and votcd for tbe ncw scbool. \
        Tlie mayor said tbe railroad wouid bring more trade to tlie town. Cotton prices \
        rosc in tbe market and tbe farmers were glad. The weatber was fair aud tbe roads \
        were dry. A new cburch will be built on Main street ncxt spring by the people.";
    const OURS_CLEAN: &str = "The city council met on Tuesday and voted for the new school. \
        The mayor said the railroad would bring more trade to the town. Cotton prices \
        rose in the market and the farmers were glad. The weather was fair and the roads \
        were dry. A new church will be built on Main street next spring by the people.";
    const OURS_SHORT: &str = "The city council met on Tuesday and voted for the new school. \
        The mayor said the railroad would bring more trade to the town of the county.";

    #[test]
    fn replaces_garbled_text_with_a_cleaner_reading() {
        let (ours, loc, v) = judge(OURS_CLEAN, Some(LOC_GARBLED), &["eng"]);
        assert_eq!(ours.lang.as_deref(), Some("eng"));
        assert_eq!(loc.lang.as_deref(), Some("eng"));
        assert!(
            ours.damage_rate.unwrap() + MARGIN <= loc.damage_rate.unwrap(),
            "{ours:?} {loc:?}"
        );
        assert_eq!(v, Ok(()));
    }

    #[test]
    fn keeps_locs_text_when_ours_is_no_better() {
        let (_, _, v) = judge(LOC_GARBLED, Some(OURS_CLEAN), &["eng"]);
        assert_eq!(v, Err(Keep::Margin));
        let (_, _, v) = judge(OURS_CLEAN, Some(OURS_CLEAN), &["eng"]);
        assert_eq!(v, Err(Keep::Margin), "the same text doesn't win");
    }

    #[test]
    fn a_short_clean_output_loses_on_the_head_test() {
        // LoC's reading of a page twice as long as what ours kept of it.
        let loc_page = format!("{LOC_GARBLED}\n{LOC_GARBLED}");
        let (ours, loc, v) = judge(OURS_SHORT, Some(&loc_page), &["eng"]);
        assert!(ours.damage_rate.unwrap() + MARGIN <= loc.damage_rate.unwrap());
        assert!(ours.head < loc.head, "{ours:?} {loc:?}");
        assert_eq!(v, Err(Keep::Head));
    }

    #[test]
    fn missing_locs_text_takes_any_decided_reading_in_the_titles_language() {
        for loc in [None, Some(""), Some("   ")] {
            let (_, loc_scores, v) = judge(OURS_CLEAN, loc, &["eng"]);
            assert_eq!(loc_scores, Scores::default());
            assert_eq!(v, Ok(()));
        }
        // Not when ours is in a language the title doesn't list.
        let (_, _, v) = judge(OURS_CLEAN, None, &["ger"]);
        assert_eq!(v, Err(Keep::TitleLanguage));
        // A title listing none is scored against English.
        let (_, _, v) = judge(OURS_CLEAN, None, &[]);
        assert_eq!(v, Ok(()));
        // An empty reading of ours never wins.
        let (_, _, v) = judge("", None, &["eng"]);
        assert_eq!(v, Err(Keep::OursUndecided));
    }

    fn s(lang: &str, rate: f64, head: u32) -> Scores {
        Scores {
            lang: Some(lang.into()),
            decision: Some("detected".into()),
            damage_rate: Some(rate),
            function_share: Some(0.4),
            head: Some(head),
        }
    }

    #[test]
    fn ties_go_to_ours() {
        // Exactly the margin lower, as floating point computes it.
        assert_eq!(
            verdict(&s("eng", 0.10, 40), &s("eng", 0.15, 40), None),
            Ok(())
        );
        assert_eq!(
            verdict(&s("eng", 0.2, 40), &s("eng", 0.25, 40), None),
            Ok(())
        );
        assert_eq!(
            verdict(&s("eng", 0.1000001, 40), &s("eng", 0.15, 40), None),
            Err(Keep::Margin)
        );
        // The same head count passes.
        assert_eq!(
            verdict(&s("eng", 0.0, 40), &s("eng", 0.3, 40), None),
            Ok(())
        );
        assert_eq!(
            verdict(&s("eng", 0.0, 39), &s("eng", 0.3, 40), None),
            Err(Keep::Head)
        );
    }

    #[test]
    fn languages_must_agree() {
        assert_eq!(
            verdict(
                &s("eng", 0.0, 40),
                &s("ger", 0.4, 40),
                Some(&["eng", "ger"])
            ),
            Err(Keep::LanguageDiffers)
        );
        let und = Scores {
            lang: Some("und".into()),
            decision: Some("und".into()),
            ..Scores::default()
        };
        assert_eq!(verdict(&s("ger", 0.1, 50), &und, Some(&["ger"])), Ok(()));
        assert_eq!(
            verdict(&s("eng", 0.1, 50), &und, Some(&["ger"])),
            Err(Keep::TitleLanguage)
        );
        assert_eq!(verdict(&s("eng", 0.1, 50), &und, None), Ok(()));
        assert_eq!(verdict(&und, &und, None), Err(Keep::OursUndecided));
    }

    #[test]
    fn the_record_names_the_rule_and_the_metric() {
        let r = record(&Completeness::default());
        assert_eq!(r["rule"]["version"], VERSION);
        assert_eq!(r["rule"]["margin"], MARGIN);
        assert_eq!(r["metric"]["version"], usnm_core::damage::VERSION);
    }

    /// A page of four columns as LoC read them (noise, as on a Fraktur page), and the same page
    /// as our layout pass saw it: the first column only (seq-7 of the 8 Oct 1876 issue: 25k
    /// characters where LoC has 70k). Columns differ by a number so the bags aren't one
    /// repeated line.
    const NOISE_PAGE: &str = "T1e c0unc1l m3t 0n Tu3sd4y 4nd v0t3d f0r th3 n3w sch00l; th3 m4y0r \
        s41d th3 r41lr04d w0u1d br1ng m0r3 tr4d3 t0 th3 t0wn 4nd c0tt0n pr1c3s r0s3 1n th3 m4rk3t.";

    fn columns(text: &str, n: usize) -> String {
        (1..=n)
            .map(|i| format!("{text} Column {i}."))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn a_page_missing_its_columns_keeps_locs_text_though_it_passes_rule_v1() {
        let langs = ["eng"];
        let gate = Completeness::default();
        let loc = columns(NOISE_PAGE, 4);
        let ours = columns(OURS_CLEAN, 1);
        // Rule v1 alone prefers ours: clean, and as many head words as LoC's noisy page.
        let (_, _, v1) = judge(&ours, Some(&loc), &langs);
        assert_eq!(
            v1,
            Ok(()),
            "the seq-7 case passes the damage and head tests"
        );
        let (_, _, v, rule, m) = judge_row(ENGLISH, &ours, Some(&loc), &langs, &gate);
        assert_eq!((v, rule), (Err(Keep::Short), "v1"));
        let m = m.unwrap();
        assert!(m.len_ratio < 0.5, "{m:?}");
        assert!(m.cover < 0.3, "{m:?}");
        // The whole page is replaced as before.
        let ours_all = columns(OURS_CLEAN, 4);
        let loc_same = columns(LOC_GARBLED, 1);
        let (_, _, v, _, m) = judge_row(ENGLISH, &ours_all, Some(&loc_same), &langs, &gate);
        assert_eq!(v, Ok(()));
        assert!(m.unwrap().len_ratio >= 0.85);
    }

    #[test]
    fn long_enough_text_of_other_words_fails_the_cover_test() {
        let gate = Completeness::default();
        let loc = columns(OURS_CLEAN, 4);
        // As long as LoC's, but a different page's words.
        let other = columns(
            "Wheat sold at a dollar the bushel in Chicago while the ships lay idle in the harbor \
             and the senators argued the tariff bill far into the night at Washington.",
            9,
        );
        let (m, v) = gate.check(&other, Some(&loc));
        assert!(m.unwrap().len_ratio >= 0.85);
        assert_eq!(v, Err(Keep::Cover));
    }

    #[test]
    fn the_gate_passes_what_it_cannot_weigh() {
        let gate = Completeness::default();
        assert_eq!(gate.check("anything", None), (None, Ok(())));
        assert_eq!(gate.check("anything", Some("  \n ")), (None, Ok(())));
        // LoC's text has too few words to weigh.
        let few = "a handful of words only";
        assert_eq!(gate.check("x", Some(few)), (None, Ok(())));
    }

    #[test]
    fn the_thresholds_are_exact_at_the_boundary_and_configurable() {
        let loc = (0..100)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let ours = (0..90)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let strict = Completeness {
            min_len_ratio: 0.95,
            min_cover: 0.5,
            min_loc_words: 20,
        };
        let loose = Completeness {
            min_len_ratio: 0.5,
            min_cover: 0.5,
            min_loc_words: 20,
        };
        assert_eq!(strict.check(&ours, Some(&loc)).1, Err(Keep::Short));
        let (m, v) = loose.check(&ours, Some(&loc));
        assert_eq!(v, Ok(()));
        assert!((m.unwrap().cover - 0.9).abs() < 1e-9);
        // A ratio exactly at the floor passes.
        let at = Completeness {
            min_len_ratio: 1.0,
            min_cover: 1.0,
            min_loc_words: 20,
        };
        assert_eq!(at.check(&loc, Some(&loc)).1, Ok(()));
        // Word multiplicity counts: one copy of a repeated word doesn't cover all of them.
        let rep = "the ".repeat(40);
        let c = Completeness {
            min_len_ratio: 0.0,
            min_cover: 0.9,
            min_loc_words: 20,
        };
        assert_eq!(c.check("the", Some(&rep)).1, Err(Keep::Cover));
    }

    #[test]
    fn thresholds_come_from_the_environment_and_a_typo_is_an_error() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            Completeness::from_vars(vars(&[])).unwrap(),
            Completeness::default()
        );
        let c = Completeness::from_vars(vars(&[
            (ENV_MIN_LEN_RATIO, "0.7"),
            (ENV_MIN_COVER, " 0.4 "),
            (ENV_MIN_LOC_WORDS, "5"),
        ]))
        .unwrap();
        assert_eq!(
            (c.min_len_ratio, c.min_cover, c.min_loc_words),
            (0.7, 0.4, 5)
        );
        assert!(Completeness::from_vars(vars(&[(ENV_MIN_COVER, "most")])).is_err());
        assert!(Completeness::from_vars(vars(&[(ENV_MIN_COVER, "1.5")])).is_err());
        assert!(Completeness::from_vars(vars(&[(ENV_MIN_LEN_RATIO, "-1")])).is_err());
    }

    #[test]
    fn the_record_names_the_completeness_gate() {
        let r = record(&Completeness::default());
        assert_eq!(r["rule"]["completeness"]["min_len_ratio"], MIN_LEN_RATIO);
        assert_eq!(r["rule"]["completeness"]["min_cover"], MIN_COVER);
        let own = Completeness {
            min_len_ratio: 0.5,
            ..Completeness::default()
        };
        assert_eq!(record(&own)["rule"]["completeness"]["min_len_ratio"], 0.5);
    }

    const PHOENIX: &str = "tesseract/5.5.0 profile=chr_phoenix image=sha256:3f9a0c1d2e4b \
        models=sha256:843614a53083 route=boinc@r1 contract=0";
    const ENGLISH: &str = "tesseract/5.5.0 profile=eng image=sha256:3f9a0c1d2e4b \
        models=sha256:75ba1066e1a4 route=boinc@r1 contract=0";

    fn line<'a>(loc: &'a str, ours: &'a str, read: Option<&'a str>) -> line_merge::Line<'a> {
        line_merge::Line { loc, ours, read }
    }

    #[test]
    fn line_merge_admits_a_page_whose_other_lines_are_locs() {
        let lines = [
            line("CHEROKEE PHOENIX.", "CHEROKEE PHOENIX.", None),
            line("ᎦᎪᎯ ᎠᎴ", "ᎦᏙᎯ ᎠᎴ", Some("ᎦᏙᎯ ᎠᎴ")),
            line("ᎦᏙᎯ", "ᎦᏙᎯ", Some("ᎦᎪᎯ")),
            line("The end.", "The end.", None),
        ];
        let (counts, v) = line_merge::verdict(&lines);
        assert_eq!(v, Ok(()));
        assert_eq!(
            counts,
            line_merge::Counts {
                cherokee: 2,
                model: 1
            }
        );
    }

    #[test]
    fn line_merge_counts_model_readings_that_agree_with_loc() {
        // Nine lines the model read as LoC did, and one it corrected: all ten are the model's.
        let mut lines = vec![line("ᎠᎴ", "ᎠᎴ", Some("ᎠᎴ")); 9];
        lines.push(line("ᎦᎪᎯ", "ᎦᏙᎯ", Some("ᎦᏙᎯ")));
        let (counts, v) = line_merge::verdict(&lines);
        assert_eq!(
            counts,
            line_merge::Counts {
                cherokee: 10,
                model: 10
            }
        );
        assert_eq!(v, Ok(()));
        // An empty reading, or a fallback to LoC's text, is not the model's.
        let lines = [
            line("ᎠᎴ", "ᎠᎴ", Some(" ")),
            line("ᎦᏙᎯ", "ᎦᏙᎯ", Some("ᎦᎪᎯ")),
            line("ᎦᎪᎯ", "ᎦᏙᎯ", Some("ᎦᏙᎯ")),
        ];
        let (counts, v) = line_merge::verdict(&lines);
        assert_eq!((counts.model, v), (1, Err(Keep::ModelShare)));
    }

    #[test]
    fn line_merge_rejects_a_changed_other_line() {
        let lines = [
            line("The end.", "The end", None),
            line("ᎦᎪᎯ", "ᎦᏙᎯ", Some("ᎦᏙᎯ")),
        ];
        assert_eq!(line_merge::verdict(&lines).1, Err(Keep::LineChanged));
        // Byte for byte: a different space is a change too.
        let lines = [
            line("The end.", "The  end.", None),
            line("ᎦᎪᎯ", "ᎦᏙᎯ", Some("ᎦᏙᎯ")),
        ];
        assert_eq!(line_merge::verdict(&lines).1, Err(Keep::LineChanged));
    }

    #[test]
    fn line_merge_rejects_under_half_model_lines() {
        let lines = [
            line("ᎦᎪᎯ", "ᎦᏙᎯ", Some("ᎦᏙᎯ")),
            line("ᎦᏙᎯ", "ᎦᏙᎯ", Some("ᎦᎪᎯ")),
            line("ᎠᎴ", "ᎠᎴ", Some("")),
        ];
        let (counts, v) = line_merge::verdict(&lines);
        assert_eq!((counts.cherokee, counts.model), (3, 1));
        assert_eq!(v, Err(Keep::ModelShare));
        assert_eq!(line_merge::verdict(&lines[..2]).1, Ok(()));
        // A page with no Cherokee line is no line job's page.
        assert_eq!(
            line_merge::verdict(&[line("a", "a", None)]).1,
            Err(Keep::ModelShare)
        );
    }

    #[test]
    fn page_jobs_still_go_through_rule_v1() {
        assert!(line_merge::is_line_job(PHOENIX));
        assert!(!line_merge::is_line_job(ENGLISH));
        assert!(!line_merge::is_line_job("tesseract-5.5/chr_phoenix"));
        assert_eq!(rule_for(PHOENIX), line_merge::VERSION);
        assert_eq!(rule_for(ENGLISH), "v1");
        let langs = ["eng"];
        for (ours, loc) in [
            (LOC_GARBLED, Some(LOC_GARBLED)),
            (OURS_CLEAN, Some(LOC_GARBLED)),
            (OURS_CLEAN, None),
        ] {
            let (a, b, v1) = judge(ours, loc, &langs);
            let (c, d, v, rule, _) =
                judge_row(ENGLISH, ours, loc, &langs, &Completeness::default());
            assert_eq!((a, b, v1, "v1"), (c, d, v, rule));
        }
        // A line job's page skips v1's tests: LoC's garbled text "wins" v1 here.
        let (_, _, v1) = judge(LOC_GARBLED, Some(LOC_GARBLED), &langs);
        assert!(v1.is_err());
        let (_, _, v, rule, m) = judge_row(
            PHOENIX,
            LOC_GARBLED,
            Some(LOC_GARBLED),
            &langs,
            &Completeness::default(),
        );
        assert_eq!((v, rule, m), (Ok(()), line_merge::VERSION, None));
    }
}
