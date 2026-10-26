//! Text agreement v1 without curated-store, index or Parquet dependencies.

use usnm_core::damage;
use usnm_ocr_contract::quality::{round6, round6_opt, MAX_TEXT};
use usnm_ocr_quality::agreement::{self, Agreement};

pub const SKIP_TOO_LONG: &str = "too_long";

/// One reader's own scores.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReaderStats {
    pub chars: u32,
    pub words: u32,
    pub lang: Option<String>,
    pub decision: Option<&'static str>,
    pub damage_rate: Option<f64>,
    pub function_share: Option<f64>,
    pub garbage_share: Option<f64>,
    pub known_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextScores {
    pub loc: ReaderStats,
    pub as_: ReaderStats,
    pub known_lang: Option<String>,
    pub agreement: Option<Agreement>,
    pub skipped: Option<&'static str>,
}

/// LoC's detected language, then American Stories', then the first title language.
pub fn known_lang(
    loc: &damage::PageScore,
    as_: &damage::PageScore,
    titles: &[&str],
) -> Option<String> {
    [loc.lang.as_str(), as_.lang.as_str()]
        .into_iter()
        .find(|l| *l != damage::UNKNOWN)
        .or_else(|| titles.first().copied())
        .filter(|l| damage::model(l).is_some())
        .map(str::to_owned)
}

fn reader(text: &str, s: &damage::PageScore, lang: Option<&str>) -> ReaderStats {
    let known_rate = lang
        .and_then(usnm_core::lang::marc_to_iso3)
        .and_then(|iso3| damage::score_forced(text, iso3))
        .and_then(|f| {
            (f.damage.words > 0).then(|| round6(f.known_words as f64 / f.damage.words as f64))
        });
    ReaderStats {
        chars: 0,
        words: 0,
        lang: (!text.is_empty()).then(|| s.lang.clone()),
        decision: (!text.is_empty()).then(|| s.decision.as_str()),
        damage_rate: round6_opt(s.damage_rate),
        function_share: round6_opt(s.function_share),
        garbage_share: round6_opt(s.garbage_share),
        known_rate,
    }
}

/// Inputs have the same normalization as the local pipeline. A short/empty-status
/// LoC row must be passed as an empty string by the source adapter.
pub fn score_texts(loc_text: &str, as_text: &str, languages: &[&str]) -> TextScores {
    let ls = damage::score(loc_text, languages);
    let ass = damage::score(as_text, languages);
    let lang = known_lang(&ls, &ass, languages);
    let mut loc = reader(loc_text, &ls, lang.as_deref());
    let mut as_ = reader(as_text, &ass, lang.as_deref());
    let too_long = loc_text.len() > MAX_TEXT || as_text.len() > MAX_TEXT;
    let agreement = (!too_long).then(|| agreement::compare(loc_text, as_text));
    let clamp = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    if let Some(g) = &agreement {
        loc.chars = clamp(g.pair.chars_a);
        loc.words = clamp(g.pair.words_a);
        as_.chars = clamp(g.pair.chars_b);
        as_.words = clamp(g.pair.words_b);
    } else {
        for (r, text) in [(&mut loc, loc_text), (&mut as_, as_text)] {
            let n = usnm_ocr_quality::text::normalize(text, false);
            r.chars = clamp(n.chars().count());
            r.words = clamp(usnm_ocr_quality::text::py_split(&n).len());
        }
    }
    TextScores {
        loc,
        as_,
        known_lang: lang,
        agreement,
        skipped: too_long.then_some(SKIP_TOO_LONG),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_and_reader_statistics_keep_the_local_metric() {
        let a = "The mayor of Albany spoke with the council about the river.";
        let b = "The mayor of Albany spoke with the counci1 about the rivor.";
        let s = score_texts(a, b, &["eng"]);
        assert_eq!(s.agreement, Some(agreement::compare(a, b)));
        assert_eq!(
            s.loc.damage_rate,
            round6_opt(damage::score(a, &["eng"]).damage_rate)
        );
        assert_eq!(s.known_lang.as_deref(), Some("eng"));
        assert_eq!(s, score_texts(a, b, &["eng"]));
    }

    #[test]
    fn missing_loc_and_oversize_text_are_not_fabricated_scores() {
        let s = score_texts("", "The mayor spoke with the council.", &["eng"]);
        assert_eq!(s.loc.chars, 0);
        assert_eq!(s.loc.lang, None);
        assert!(s.agreement.is_some());
        let long = "word ".repeat(MAX_TEXT / 5 + 1);
        for s in [
            score_texts(&long, "", &["eng"]),
            score_texts("", &long, &["eng"]),
        ] {
            assert_eq!(s.skipped, Some(SKIP_TOO_LONG));
            assert!(s.agreement.is_none());
        }
    }
}
