//! A curated page as a main-index document (05 §5.5): how a release writes
//! them ([`DocFormat`]) and the documents themselves ([`page_doc`],
//! [`main_doc`]).

use chrono::Datelike;
use serde_json::{json, Value};
use usnm_core::text::Analyzer;
use usnm_core::text_layout::TextLayout;
use usnm_core::time::{day_number, ym_number};

use crate::catalog::{Place, Title};
use crate::curated::CuratedRow;

/// How a release writes its main-index documents. Every main index of a
/// version is written the same way: a delta as the indexes it adds to were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocFormat {
    /// The page's decade partition too (05 §5.5.5).
    pub decade: bool,
    /// The analyzer that folds the common-word pairs (05 §5.5.3, #168), and
    /// the words of `text_all`.
    pub analyzer: Analyzer,
    /// Which fields the texts are searched in (05 §5.5.7).
    pub text: TextLayout,
}

/// One engine document for a curated page (05 §5.5), with American
/// Stories' text when it has some (05 §5.5.4). A page LoC has no usable
/// text for is a document only with American Stories' text; its `text` is
/// then empty.
///
/// On a page printed before 1835, in a layout that keeps LoC's text as a
/// third text ([`TextLayout::keeps_loc_original`]), `text` has the
/// syllabary words OCR misread in the first Cherokee type read again
/// ([`usnm_core::cherokee_type::fix_text`], #1620), and LoC's text is kept
/// in `text_loc` and searched after it. Any other page is LoC's text.
pub fn page_doc(
    row: &CuratedRow,
    title: &Title,
    place: &Place,
    text_as: Option<&str>,
    format: DocFormat,
) -> Value {
    let loc = row.text.as_deref().unwrap_or_default();
    // A page set in the first Cherokee type (before 1835) has its
    // syllabary words that OCR misread in it read again (#1620), when the
    // layout keeps LoC's text as a third text, so a search for LoC's
    // reading still finds the page.
    let fixed = (format.text.keeps_loc_original()
        && usnm_core::cherokee_type::printed_with_1828_type(row.key.date))
    .then(|| usnm_core::cherokee_type::fix_text(loc))
    .flatten();
    main_doc(
        &row.key,
        &row.batch,
        fixed.as_deref().unwrap_or(loc),
        title,
        place,
        SideTexts {
            american_stories: text_as,
            loc_original: fixed.is_some().then_some(loc),
        },
        format,
    )
}

/// The texts of a page besides its main one (`text`), as its document keeps
/// them.
#[derive(Debug, Clone, Copy, Default)]
pub struct SideTexts<'a> {
    /// American Stories' text of the page (05 §5.5.4): `text_as`.
    pub american_stories: Option<&'a str>,
    /// LoC's original text of a page whose `text` is our own OCR in its
    /// place (12 §12.5 Step 9): `text_loc`, #925. Only a layout that keeps it
    /// ([`TextLayout::keeps_loc_original`]) writes it, and only when it has
    /// a word the index keeps; layouts 1 and 2 write their documents as
    /// they always did, whatever this holds.
    pub loc_original: Option<&'a str>,
}

/// A main-index document for page `k` of `batch` with `text` as its text:
/// [`page_doc`]'s, our own OCR's text in place of LoC's
/// ([`SideTexts::loc_original`]), and the Latin-script text of our Japanese
/// OCR on a page LoC ships without text (`ocr_ja::latin_doc`).
pub fn main_doc(
    k: &usnm_core::ids::PageKey,
    batch: &str,
    text: &str,
    title: &Title,
    place: &Place,
    side: SideTexts<'_>,
    format: DocFormat,
) -> Value {
    let text_as = side.american_stories;
    let mut doc = json!({
        "doc_id": k.doc_id(),
        "day": day_number(k.date),
        "ym": ym_number(k.date),
        "year": k.date.year(),
        "place_id": place.id,
        "place_shard": place.ordinal % 8,
        "lccn": k.lccn,
        "state": title.state_in(place),
        "language": title.languages,
        "front_page": k.seq == 1,
        "edition": k.edition,
        "seq": k.seq,
        // Hits order within a day: title, then edition, then page.
        "sort_key": (u64::from(title.ordinal) << 32) | (u64::from(k.edition) << 16) | u64::from(k.seq),
        "date": k.date.to_string(),
        "batch": batch,
        "text": text,
    });
    match format.text {
        TextLayout::Separate => {
            doc["text_cg"] = usnm_core::common_grams::index_text(text, format.analyzer).into();
            if let Some(t) = text_as {
                doc["text_as_cg"] = usnm_core::common_grams::index_text(t, format.analyzer).into();
            }
        }
        // One searched field for both texts; `text` and `text_as` are
        // stored for snippets (05 §5.5.7).
        TextLayout::Single => {
            let (words, pairs) =
                usnm_core::text_layout::index_fields(text, text_as, format.analyzer);
            doc[usnm_core::text_layout::FIELD] = words.into();
            doc[usnm_core::text_layout::PAIRS_FIELD] = pairs.into();
        }
        // The same, and LoC's original text of a page our OCR replaced it
        // on as a third text after another gap, stored in `text_loc` (#925).
        TextLayout::SingleWithLoc => {
            let original = side
                .loc_original
                .filter(|t| usnm_core::text_layout::has_words(t, format.analyzer));
            let (words, pairs) = usnm_core::text_layout::index_texts(
                std::iter::once(text).chain(text_as).chain(original),
                format.analyzer,
            );
            doc[usnm_core::text_layout::FIELD] = words.into();
            doc[usnm_core::text_layout::PAIRS_FIELD] = pairs.into();
            if let Some(t) = original {
                doc[usnm_core::text_layout::LOC_FIELD] = t.into();
            }
        }
    }
    if let Some(t) = text_as {
        doc["text_as"] = t.into();
    }
    if format.decade {
        doc["decade"] = usnm_core::decade::of_date(k.date).into();
    }
    doc
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, NaiveDate};
    use usnm_core::ids::PageKey;

    use super::*;

    fn title() -> Title {
        Title {
            lccn: "sn83020866".into(),
            name: "Cherokee Phoenix".into(),
            ordinal: 1,
            place_id: "P1".into(),
            state: "GA".into(),
            languages: vec!["eng".into(), "chr".into()],
            place_periods: vec![],
            extra: BTreeMap::new(),
        }
    }

    fn place() -> Place {
        Place {
            id: "P1".into(),
            ordinal: 1,
            name: "New Echota".into(),
            state: "GA".into(),
            lat: 34.5,
            lon: -84.9,
            precision: "city".into(),
            coordinates_from: None,
            variants: vec![],
        }
    }

    fn row(date: &str, raw: &str) -> CuratedRow {
        let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        CuratedRow::from_ocr(
            PageKey::new("sn83020866", date, 1, 1).unwrap(),
            raw,
            "batch_a",
            1,
            "loc",
            DateTime::UNIX_EPOCH,
        )
    }

    fn format(text: TextLayout) -> DocFormat {
        DocFormat {
            decade: false,
            analyzer: Analyzer::V2,
            text,
        }
    }

    const LOC: &str = "ᏩᏍᎩ ᎠᎴ ᎦᎪᎯ and the earth, words enough to be a page of text here.";

    /// A page set in the first Cherokee type (#1620) has ᏩᏍᎩ and ᎦᎪᎯ read
    /// as ᎾᏍᎩ and ᎦᏙᎯ, and keeps LoC's text as the third text.
    #[test]
    fn a_page_in_the_first_cherokee_type_is_read_again() {
        let doc = page_doc(
            &row("1828-03-06", LOC),
            &title(),
            &place(),
            None,
            format(TextLayout::SingleWithLoc),
        );
        let text = doc["text"].as_str().unwrap();
        assert!(text.starts_with("ᎾᏍᎩ ᎠᎴ ᎦᏙᎯ and the earth"), "{text}");
        assert_eq!(
            doc[usnm_core::text_layout::LOC_FIELD].as_str(),
            Some(row("1828-03-06", LOC).text.unwrap().as_str())
        );
        let words = doc[usnm_core::text_layout::FIELD].as_str().unwrap();
        for w in ["ꮎꮝꭹ", "ꭶꮩꭿ", "ꮹꮝꭹ", "ꭶꭺꭿ"] {
            assert!(words.split(' ').any(|t| t == w), "{w} in {words}");
        }
    }

    /// A later page, or a layout without a third text, keeps LoC's text.
    #[test]
    fn later_pages_and_older_layouts_keep_locs_text() {
        let later = row("1844-09-26", LOC);
        let doc = page_doc(
            &later,
            &title(),
            &place(),
            None,
            format(TextLayout::SingleWithLoc),
        );
        assert_eq!(doc["text"].as_str(), later.text.as_deref());
        assert!(doc.get(usnm_core::text_layout::LOC_FIELD).is_none());
        let early = row("1828-03-06", LOC);
        for layout in [TextLayout::Separate, TextLayout::Single] {
            let doc = page_doc(&early, &title(), &place(), None, format(layout));
            assert_eq!(doc["text"].as_str(), early.text.as_deref(), "{layout:?}");
            assert!(doc.get(usnm_core::text_layout::LOC_FIELD).is_none());
        }
        // A page with nothing to read again is written as it was.
        let english = row(
            "1828-03-06",
            "The council met at New Echota on Monday last.",
        );
        let doc = page_doc(
            &english,
            &title(),
            &place(),
            None,
            format(TextLayout::SingleWithLoc),
        );
        assert_eq!(doc["text"].as_str(), english.text.as_deref());
        assert!(doc.get(usnm_core::text_layout::LOC_FIELD).is_none());
    }
}
