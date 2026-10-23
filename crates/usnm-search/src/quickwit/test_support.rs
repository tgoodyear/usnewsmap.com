//! What the tests of the `quickwit` modules share.

use chrono::NaiveDate;
use usnm_core::params::Filters;

use crate::IndexSet;

pub(super) fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

pub(super) fn filters() -> Filters {
    Filters {
        from: d("1896-06-01"),
        to: d("1896-12-31"),
        states: vec!["GA".into(), "SC".into()],
        lccns: vec![],
        langs: vec![],
        front_only: true,
        series: Vec::new(),
    }
}

pub(super) fn none() -> IndexSet {
    IndexSet::new(vec!["i".into()])
}

/// A set that searches American Stories' text or not.
pub(super) fn stories(on: bool) -> IndexSet {
    none().with_american_stories(on)
}
