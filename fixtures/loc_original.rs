// The fixture pages as a version that keeps LoC's original text has them
// (05 §5.5.7, #925): on some pages our own OCR replaced LoC's text. The
// fixture's text is a handful of words on every page, so the pages our OCR
// replaced LoC's text on are picked by their day and page number, and our
// reading is LoC's with its second half first, as a column read out of order
// would have it: the same words, but a phrase LoC's text has can fail in
// ours. The page's `text` is then ours and `text_loc` LoC's original.
// Included with `#[path]` by `crates/usnm-ingest/examples/single_field_fixture.rs`
// (which loads them into Quickwit for scripts/dev/quickwit-fixtures.sh) and by
// the parity test (`crates/usnm-search/tests/quickwit_parity.rs`), so both
// backends see the same pages.

use serde_json::Value;

/// Whether our OCR replaced LoC's text on this fixture page: about one page
/// in four of those with text enough to reorder.
pub fn replaced(doc: &Value) -> bool {
    let n = doc["day"].as_u64().expect("day") * 7 + doc["seq"].as_u64().expect("seq");
    n % 4 == 1
        && doc["text"]
            .as_str()
            .is_some_and(|t| t.split_whitespace().count() >= 8)
}

/// `doc` (a fixture index document) with our reading in `text` and LoC's
/// original in `text_loc`, where our OCR replaced it; the other pages as
/// they are.
pub fn add(doc: &mut Value) {
    if !replaced(doc) {
        return;
    }
    let original = doc["text"].as_str().expect("text").to_owned();
    let words: Vec<&str> = original.split_whitespace().collect();
    let (head, tail) = words.split_at(words.len() / 2);
    doc["text"] = tail
        .iter()
        .chain(head)
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .into();
    doc["text_loc"] = original.into();
}
