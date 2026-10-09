// The fixture pages spread over several decades, for the tests of decade
// partitions (05 §5.5.5, #123): the fixture corpus covers 1895 to 1897, one
// decade. Included with `#[path]` by `crates/usnm-ingest/examples/decade_fixture.rs`
// (which loads them into Quickwit for scripts/quickwit-fixtures.sh) and by
// the parity test (`crates/usnm-search/tests/quickwit_parity.rs`), so both
// backends see the same pages.
//
// Each title's pages move back 0, 28 or 56 years by its LCCN: whole
// 28-year cycles keep every date valid (29 February included, and none of
// the years crosses 1900) and on the same weekday. The pages then fall in
// the 1830s, 1840s, 1860s and 1890s.

use chrono::{Datelike, NaiveDate};
use serde_json::Value;

/// Years a title's pages move back.
pub fn years_back(lccn: &str) -> i32 {
    let n: i32 = lccn
        .trim_start_matches(|c: char| !c.is_ascii_digit())
        .parse()
        .unwrap_or(0);
    28 * (n % 3)
}

/// `doc` (a fixture index document) moved back by its title's years: its
/// date, day, month and year numbers and doc id, with its `decade` added.
pub fn shift(doc: &mut Value) {
    let lccn = doc["lccn"].as_str().expect("lccn").to_owned();
    let date =
        NaiveDate::parse_from_str(doc["date"].as_str().expect("date"), "%Y-%m-%d").expect("a date");
    let moved = date
        .with_year(date.year() - years_back(&lccn))
        .expect("a date in a 28-year cycle");
    let id = doc["doc_id"].as_str().expect("doc_id").replace(
        &date.format("%Y-%m-%d").to_string(),
        &moved.format("%Y-%m-%d").to_string(),
    );
    doc["doc_id"] = id.into();
    doc["date"] = moved.format("%Y-%m-%d").to_string().into();
    doc["day"] = usnm_core::time::day_number(moved).into();
    doc["ym"] = usnm_core::time::ym_number(moved).into();
    doc["year"] = moved.year().into();
    doc["decade"] = usnm_core::decade::of_date(moved).into();
}
