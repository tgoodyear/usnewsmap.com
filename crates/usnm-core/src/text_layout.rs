//! How the main index lays out a page's texts for searching (05 §5.5.7,
//! #283, #925): a field for each text, or one field for all of them.
//!
//! [`TextLayout::Separate`] is the layout of every version before #283:
//! LoC's text in `text` and its common-word pairs in `text_cg`, and, with
//! American Stories' text (05 §5.5.4), `text_as` and `text_as_cg`. A query
//! looks each word up in each text's field.
//!
//! [`TextLayout::Single`] searches one field, [`FIELD`], and one pairs
//! field, [`PAIRS_FIELD`]: LoC's words, then [`GAP_POSITIONS`] positions that no query
//! can match, then American Stories' words. `text` and `text_as` are kept,
//! stored but not indexed, for snippets and `matched_in`. A query then looks
//! each word up once. The release writes both fields' tokens
//! ([`index_fields`]), one per word as the version's analyzer has it, and
//! the fields' tokenizer is `whitespace`, as for the pairs (05 §5.5.3).
//!
//! [`TextLayout::SingleWithLoc`] is [`TextLayout::Single`] with a third text
//! (#925): on a page where our own OCR replaced LoC's text (12 §12.5 Step 9),
//! `text` is ours, and LoC's original text goes after American Stories' in
//! [`FIELD`] and [`PAIRS_FIELD`], after another [`GAP_POSITIONS`], and in
//! [`LOC_FIELD`], stored but not indexed, for snippets and `matched_in`. A
//! page is still one document, so it counts once. The texts are laid out by
//! [`index_texts`].
//!
//! The layout is part of the index: `current.json` names it
//! (`text_layout`, absent for [`TextLayout::Separate`]), every main index
//! of a version has the same one, and only a full rebuild changes it. The
//! API searches each version as its layout says.

use crate::common_grams;
use crate::text::Analyzer;

/// The text fields of a version's main indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextLayout {
    /// A searched field per text: `text` and `text_cg`, `text_as` and
    /// `text_as_cg`.
    #[default]
    Separate,
    /// One searched field for both texts, [`FIELD`] and [`PAIRS_FIELD`].
    Single,
    /// [`TextLayout::Single`], and on a page where our OCR replaced LoC's
    /// text, LoC's original text too: a third text in the searched field,
    /// and stored in [`LOC_FIELD`] (#925).
    SingleWithLoc,
}

impl TextLayout {
    /// The layout a full rebuild with `--single-text-field` builds: the
    /// latest one with a single searched field.
    pub const LATEST: Self = Self::SingleWithLoc;

    /// The number `current.json` records as `text_layout`.
    pub fn version(self) -> u32 {
        match self {
            Self::Separate => 1,
            Self::Single => 2,
            Self::SingleWithLoc => 3,
        }
    }

    /// Whether the texts are searched in one field, [`FIELD`], and not a
    /// field each: [`Self::Single`] and the layouts after it.
    pub fn searches_one_field(self) -> bool {
        match self {
            Self::Separate => false,
            Self::Single | Self::SingleWithLoc => true,
        }
    }

    /// Whether a page our OCR replaced LoC's text on keeps LoC's text, in
    /// [`LOC_FIELD`] and in [`FIELD`] (#925).
    pub fn keeps_loc_original(self) -> bool {
        self == Self::SingleWithLoc
    }

    /// The layout `current.json`'s `text_layout` names: [`Self::Separate`]
    /// when it has none, `None` for a number this code doesn't know (a
    /// newer one).
    pub fn from_version(version: Option<u32>) -> Option<Self> {
        match version {
            None | Some(1) => Some(Self::Separate),
            Some(2) => Some(Self::Single),
            Some(3) => Some(Self::SingleWithLoc),
            Some(_) => None,
        }
    }
}

/// The one searched field of [`TextLayout::Single`] and the layouts after it.
pub const FIELD: &str = "text_all";

/// Its common-word pairs (05 §5.5.3).
pub const PAIRS_FIELD: &str = "text_all_cg";

/// LoC's original text on a page our OCR replaced it on (#925), stored but
/// not indexed, in [`TextLayout::SingleWithLoc`]. Absent on every other page.
pub const LOC_FIELD: &str = "text_loc";

/// Positions between two texts' words in [`FIELD`] and [`PAIRS_FIELD`],
/// each a [`common_grams::GAP`], so no phrase or NEAR search matches across
/// two texts. Quickwit 0.9.1 matched no phrase of
/// up to 12 words with slop 20 across 32, in order or reversed (it counts a
/// swap against the slop, #243); a two-word phrase needs slop 32.
pub const GAP_POSITIONS: usize = 32;

const _: () = assert!(GAP_POSITIONS > crate::query::MAX_SLOP as usize + 1);

/// [`FIELD`] and [`PAIRS_FIELD`] for a page with LoC's `text` (empty when
/// it has none) and American Stories' `text_as`, folded by `analyzer`, as
/// [`TextLayout::Single`] holds them: [`index_texts`] of the two.
pub fn index_fields(text: &str, text_as: Option<&str>, analyzer: Analyzer) -> (String, String) {
    index_texts(std::iter::once(text).chain(text_as), analyzer)
}

/// [`FIELD`] and [`PAIRS_FIELD`] for a page's `texts` in order, folded by
/// `analyzer`: each text's words ([`common_grams::index_words`]) and pairs
/// ([`common_grams::index_text`], made per text, so the last word of one is
/// never paired with the first of the next), with [`GAP_POSITIONS`] between
/// one text and the next. A text with no words takes no gap.
///
/// [`TextLayout::SingleWithLoc`] passes the page's text, American Stories'
/// and LoC's original, in that order.
pub fn index_texts<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    analyzer: Analyzer,
) -> (String, String) {
    let mut words = Vec::new();
    let mut pairs = Vec::new();
    for t in texts {
        let (w, p) = common_grams::index_both(t, analyzer);
        if !w.is_empty() {
            words.push(w);
            pairs.push(p);
        }
    }
    let gap = format!(" {} ", vec![common_grams::GAP; GAP_POSITIONS].join(" "));
    (words.join(&gap), pairs.join(&gap))
}

/// Whether `text` has a word the index keeps once `analyzer` folds it: a
/// text of nothing but punctuation, or of words too long to index, has none
/// and is not worth a text of its own (#925).
pub fn has_words(text: &str, analyzer: Analyzer) -> bool {
    common_grams::index_words(text, analyzer)
        .split(' ')
        .any(|w| !w.is_empty() && w != common_grams::GAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_round_trip_and_unknown_ones_are_refused() {
        for l in [
            TextLayout::Separate,
            TextLayout::Single,
            TextLayout::SingleWithLoc,
        ] {
            assert_eq!(TextLayout::from_version(Some(l.version())), Some(l));
        }
        assert_eq!(TextLayout::from_version(None), Some(TextLayout::Separate));
        assert_eq!(TextLayout::from_version(Some(4)), None);
        assert_eq!(TextLayout::from_version(Some(0)), None);
        assert_eq!(TextLayout::default(), TextLayout::Separate);
        // The numbers are part of the published indexes: never renumbered.
        assert_eq!(
            [
                TextLayout::Separate,
                TextLayout::Single,
                TextLayout::SingleWithLoc
            ]
            .map(TextLayout::version),
            [1, 2, 3]
        );
        assert_eq!(TextLayout::LATEST, TextLayout::SingleWithLoc);
    }

    #[test]
    fn layouts_say_how_their_texts_are_searched_and_kept() {
        assert!(!TextLayout::Separate.searches_one_field());
        assert!(TextLayout::Single.searches_one_field());
        assert!(TextLayout::SingleWithLoc.searches_one_field());
        // Only the latest keeps LoC's text on a page ours replaced it on.
        assert!(!TextLayout::Separate.keeps_loc_original());
        assert!(!TextLayout::Single.keeps_loc_original());
        assert!(TextLayout::SingleWithLoc.keeps_loc_original());
    }

    #[test]
    fn both_texts_with_a_gap_between_them() {
        let a = Analyzer::LATEST;
        let (words, pairs) = index_fields("A cross of Gold, the", Some("BRYAN: of the people"), a);
        let gap = vec!["_"; GAP_POSITIONS].join(" ");
        assert_eq!(
            words,
            format!("a cross of gold the {gap} bryan of the people")
        );
        // The last word of LoC's text pairs with nothing from the other.
        assert_eq!(
            pairs,
            format!("a_cross cross of_gold gold the_ {gap} bryan of_the the_people people")
        );
        // Same positions in both fields.
        assert_eq!(words.split(' ').count(), pairs.split(' ').count());
    }

    #[test]
    fn one_text_has_no_gap() {
        let a = Analyzer::LATEST;
        assert_eq!(
            index_fields("Cross of gold", None, a),
            ("cross of gold".into(), "cross of_gold gold".into())
        );
        // A page only American Stories has text for (LoC's empty or short).
        assert_eq!(
            index_fields("", Some("Cross of gold"), a),
            ("cross of gold".into(), "cross of_gold gold".into())
        );
        assert_eq!(
            index_fields(" -- ", Some("gold"), a),
            ("gold".into(), "gold".into())
        );
        assert_eq!(index_fields("", None, a), (String::new(), String::new()));
    }

    #[test]
    fn dropped_words_keep_their_positions() {
        let long = "x".repeat(41);
        let (words, pairs) = index_fields(&format!("gold {long} of"), None, Analyzer::LATEST);
        assert_eq!(words, "gold _ of");
        assert_eq!(pairs, "gold _ of_");
    }

    #[test]
    fn three_texts_with_a_gap_between_each() {
        let a = Analyzer::LATEST;
        let (words, pairs) = index_texts(
            ["Cross of gold", "BRYAN: of the people", "A cross of iron"],
            a,
        );
        let gap = vec!["_"; GAP_POSITIONS].join(" ");
        assert_eq!(
            words,
            format!("cross of gold {gap} bryan of the people {gap} a cross of iron")
        );
        // Pairs are made per text: no word pairs with the next text's first.
        assert_eq!(
            pairs,
            format!(
                "cross of_gold gold {gap} bryan of_the the_people people \
                 {gap} a_cross cross of_iron iron"
            )
        );
        // Same positions in both fields, and `gap` positions between texts.
        assert_eq!(words.split(' ').count(), pairs.split(' ').count());
        assert_eq!(words.split(' ').count(), 3 + 4 + 4 + 2 * GAP_POSITIONS);
        // The first two are what `index_fields` makes of two texts.
        let (two, _) = index_fields("Cross of gold", Some("BRYAN: of the people"), a);
        assert!(words.starts_with(&two));
    }

    #[test]
    fn two_texts_without_the_middle_one() {
        // A page with no American Stories' text: ours, then LoC's original,
        // with one gap, not two.
        let a = Analyzer::LATEST;
        let gap = vec!["_"; GAP_POSITIONS].join(" ");
        let (words, pairs) = index_texts(["Cross of gold", "", "A cross of iron"], a);
        assert_eq!(words, format!("cross of gold {gap} a cross of iron"));
        assert_eq!(
            pairs,
            format!("cross of_gold gold {gap} a_cross cross of_iron iron")
        );
        assert_eq!(words.split(' ').count(), 3 + GAP_POSITIONS + 4);
    }

    #[test]
    fn a_text_with_no_words_is_skipped_with_no_gap() {
        let a = Analyzer::LATEST;
        let gap = vec!["_"; GAP_POSITIONS].join(" ");
        // Nothing but punctuation first and last, and between.
        let (words, _) = index_texts([" -- ", "gold", "...", "silver", " "], a);
        assert_eq!(words, format!("gold {gap} silver"));
        let (words, pairs) = index_texts(["", "", ""], a);
        assert_eq!((words, pairs), (String::new(), String::new()));
        // Two texts through `index_fields` are the same two through `index_texts`.
        for (text, text_as) in [("a b", Some("c d")), ("", Some("c d")), ("a b", None)] {
            assert_eq!(
                index_fields(text, text_as, a),
                index_texts(std::iter::once(text).chain(text_as), a)
            );
        }
    }

    #[test]
    fn a_text_has_words_when_the_index_keeps_one() {
        let a = Analyzer::LATEST;
        assert!(has_words("Die Frauen", a));
        assert!(has_words(" -- gold", a));
        assert!(!has_words("", a));
        assert!(!has_words(" \n -- ", a));
        // A run too long to index keeps its position and is no word.
        assert!(!has_words(&"x".repeat(41), a));
        assert!(has_words(&format!("{} gold", "x".repeat(41)), a));
    }
}
