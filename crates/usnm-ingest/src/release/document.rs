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
pub fn page_doc(
    row: &CuratedRow,
    title: &Title,
    place: &Place,
    text_as: Option<&str>,
    format: DocFormat,
) -> Value {
    main_doc(
        &row.key,
        &row.batch,
        row.text.as_deref().unwrap_or_default(),
        title,
        place,
        text_as,
        format,
    )
}

/// A main-index document for page `k` of `batch` with `text` as its text:
/// [`page_doc`]'s, and the Latin-script text of our Japanese OCR on a page
/// LoC ships without text (`ocr_ja::latin_doc`).
pub fn main_doc(
    k: &usnm_core::ids::PageKey,
    batch: &str,
    text: &str,
    title: &Title,
    place: &Place,
    text_as: Option<&str>,
    format: DocFormat,
) -> Value {
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
    }
    if let Some(t) = text_as {
        doc["text_as"] = t.into();
    }
    if format.decade {
        doc["decade"] = usnm_core::decade::of_date(k.date).into();
    }
    doc
}
