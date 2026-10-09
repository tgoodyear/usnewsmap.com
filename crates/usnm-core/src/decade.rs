//! Decade partitions (05 §5.5.5, #123): which split a page goes to in a base
//! built with `--partition-decade`, and which splits a date-limited search
//! needs.
//!
//! Each page carries its decade in the `decade` field. A partitioned index
//! keeps every decade in its own splits (Quickwit's `partition_key`), and
//! records it as a split tag (`tag_fields`), so a search with a
//! `decade:IN [...]` clause skips the splits of the other decades before it
//! opens them. The `day` filter stays the exact one: the decade clause only
//! prunes.
//!
//! Timestamps can't do this: Quickwit 0.9.1 matches no page on a range
//! before 1972 (#158), so searches never send `start_timestamp`,
//! `end_timestamp` or `date:` ranges.
//!
//! The decades before [`FIRST`] are one partition: together they hold fewer
//! pages (about 190,000) than one split of the 1820s, and a split of each
//! would add nine splits to every full-range search.
//!
//! The API adds the clause only when the published version was built with
//! this [`VERSION`] (`current.json`'s `decades`), like the common-word
//! pairs: a version without the field would refuse the clause, and one with
//! another bucketing would skip splits it needs.

use std::ops::RangeInclusive;

use chrono::{Datelike, NaiveDate};

/// Bumped whenever [`of_year`] changes. The API then stops pruning on
/// versions built before it, and a full rebuild lays them out again.
pub const VERSION: u32 = 1;

/// The first decade with its own partition: pages from before 1820 are
/// counted in it.
pub const FIRST: u16 = 1820;

/// The partition of a page printed in `year`.
pub fn of_year(year: i32) -> u16 {
    let decade = year.div_euclid(10) * 10;
    u16::try_from(decade).map_or(FIRST, |d| d.max(FIRST))
}

/// The partition of a page printed on `date`.
pub fn of_date(date: NaiveDate) -> u16 {
    of_year(date.year())
}

/// Every partition a page printed from `from` to `to` (inclusive) can be in,
/// oldest first; empty when `to` is before `from`.
pub fn between(from: NaiveDate, to: NaiveDate) -> Vec<u16> {
    if to < from {
        return Vec::new();
    }
    (of_date(from)..=of_date(to)).step_by(10).collect()
}

/// The `decade:IN [...]` clause that limits a search from `from` to `to` to
/// the splits it needs, in a version whose pages span the partitions
/// `version`. `None` when the search covers all of them: the clause would
/// skip nothing and only cost each split a posting list.
pub fn clause(from: NaiveDate, to: NaiveDate, version: &RangeInclusive<u16>) -> Option<String> {
    let wanted = between(from, to);
    let covers_all = wanted.first().is_some_and(|f| f <= version.start())
        && wanted.last().is_some_and(|l| l >= version.end());
    if covers_all {
        return None;
    }
    // Nothing of the version is in range: no split can match, and an
    // empty set isn't valid query syntax, so name one outside it.
    let wanted = if wanted.is_empty() {
        vec![FIRST.saturating_sub(10)]
    } else {
        wanted
    };
    let list: Vec<String> = wanted.iter().map(u16::to_string).collect();
    Some(format!("decade:IN [{}]", list.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn decades_from_1820_and_one_for_everything_before() {
        assert_eq!(of_year(1896), 1890);
        assert_eq!(of_year(1890), 1890);
        assert_eq!(of_year(1899), 1890);
        assert_eq!(of_year(1963), 1960);
        assert_eq!(of_year(1820), 1820);
        assert_eq!(of_year(1819), 1820);
        assert_eq!(of_year(1736), 1820);
        assert_eq!(of_year(-5), 1820);
        assert_eq!(of_date(d("1910-01-01")), 1910);
        assert_eq!(of_date(d("1909-12-31")), 1900);
    }

    #[test]
    fn a_range_spans_its_decades() {
        assert_eq!(between(d("1896-01-01"), d("1896-12-31")), [1890]);
        assert_eq!(between(d("1895-06-01"), d("1905-06-01")), [1890, 1900]);
        assert_eq!(between(d("1700-01-01"), d("1835-01-01")), [1820, 1830]);
        assert_eq!(between(d("1897-01-01"), d("1896-01-01")), [] as [u16; 0]);
    }

    #[test]
    fn the_clause_names_the_decades_unless_it_would_skip_nothing() {
        let version = 1820..=1960;
        assert_eq!(
            clause(d("1896-01-01"), d("1896-12-31"), &version).as_deref(),
            Some("decade:IN [1890]")
        );
        assert_eq!(
            clause(d("1895-01-01"), d("1905-12-31"), &version).as_deref(),
            Some("decade:IN [1890 1900]")
        );
        // The whole version, or more: no clause.
        assert_eq!(clause(d("1736-01-01"), d("1963-12-31"), &version), None);
        assert_eq!(clause(d("1700-01-01"), d("2000-12-31"), &version), None);
        // A version of a few decades: a search over them all skips nothing.
        assert_eq!(
            clause(d("1895-01-01"), d("1897-12-31"), &(1890..=1890)),
            None
        );
        assert_eq!(
            clause(d("1860-01-01"), d("1899-12-31"), &(1830..=1890)).as_deref(),
            Some("decade:IN [1860 1870 1880 1890]")
        );
        // No day in range: a decade no page has.
        assert_eq!(
            clause(d("1897-01-01"), d("1896-01-01"), &version).as_deref(),
            Some("decade:IN [1810]")
        );
    }
}
