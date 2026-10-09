//! The Quickwit searcher's cache metrics, read from its Prometheus endpoint
//! (`GET /metrics`), so the API can report them (#125, 08 §8.4).
//!
//! Quickwit 0.9.1 (`quickwit-storage/src/metrics.rs`) keeps one set of
//! metrics per cache, told apart by the `component_name` label:
//!
//! | metric                                  | type    | what                     |
//! |-----------------------------------------|---------|--------------------------|
//! | `quickwit_cache_in_cache_num_bytes`     | gauge   | bytes in the cache now   |
//! | `quickwit_cache_in_cache_count`         | gauge   | items in the cache now   |
//! | `quickwit_cache_cache_hits_total`       | counter | hits since start         |
//! | `quickwit_cache_cache_misses_total`     | counter | misses since start       |
//! | `quickwit_cache_cache_evict_total`      | counter | evictions since start    |
//!
//! The searcher's caches are `splitfooter` (`split_footer_cache_capacity`,
//! one item per split footer), `fastfields` (`fast_field_cache_capacity`),
//! `partial_request` (`partial_request_cache_capacity`) and `predicate`
//! (`predicate_cache_capacity`). Each metric also has a series without the
//! label (always 0), which is ignored. A metric shows up only once Quickwit
//! has touched it, so a cache can be missing from an early scrape.

use std::collections::BTreeMap;

use serde::Serialize;

/// A searcher cache the API reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cache {
    /// Split footers (each split's hotcache): `split_footer_cache_capacity`.
    SplitFooter,
    /// Fast-field column data: `fast_field_cache_capacity`.
    FastField,
    /// Per-split partial results: `partial_request_cache_capacity`.
    PartialRequest,
    /// Per-split filter results: `predicate_cache_capacity`.
    Predicate,
}

impl Cache {
    pub const ALL: [Cache; 4] = [
        Cache::SplitFooter,
        Cache::FastField,
        Cache::PartialRequest,
        Cache::Predicate,
    ];

    /// Quickwit's `component_name` for the cache.
    pub fn component(self) -> &'static str {
        match self {
            Cache::SplitFooter => "splitfooter",
            Cache::FastField => "fastfields",
            Cache::PartialRequest => "partial_request",
            Cache::Predicate => "predicate",
        }
    }

    /// The name the API reports it under (the `cache` attribute).
    pub fn label(self) -> &'static str {
        match self {
            Cache::SplitFooter => "split_footer",
            Cache::FastField => "fast_field",
            Cache::PartialRequest => "partial_request",
            Cache::Predicate => "predicate",
        }
    }

    fn from_component(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.component() == name)
    }
}

/// One cache's numbers. The counters run from the searcher's start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CacheStats {
    pub bytes: u64,
    pub items: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

/// The caches a scrape found, by cache.
pub type CacheReport = BTreeMap<Cache, CacheStats>;

/// Pick the searcher caches' numbers out of Prometheus text. Lines it
/// doesn't know, or can't read, are skipped.
pub fn parse(text: &str) -> CacheReport {
    let mut report = CacheReport::new();
    for (name, labels, value) in samples(text) {
        let field: fn(&mut CacheStats) -> &mut u64 = match name {
            "quickwit_cache_in_cache_num_bytes" => |s| &mut s.bytes,
            "quickwit_cache_in_cache_count" => |s| &mut s.items,
            "quickwit_cache_cache_hits_total" => |s| &mut s.hits,
            "quickwit_cache_cache_misses_total" => |s| &mut s.misses,
            "quickwit_cache_cache_evict_total" => |s| &mut s.evictions,
            _ => continue,
        };
        let Some(cache) = label(labels, "component_name").and_then(|c| Cache::from_component(&c))
        else {
            continue;
        };
        let Some(value) = parse_value(value) else {
            continue;
        };
        *field(report.entry(cache).or_default()) = value;
    }
    report
}

/// The samples of Prometheus text, as `(name, labels, value)`
/// ([`split_sample`]); comments, blank lines and lines it can't split are
/// left out.
pub(crate) fn samples(text: &str) -> impl Iterator<Item = (&str, &str, &str)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(split_sample)
}

/// `name{labels} value [timestamp]` into its name, label text (without the
/// braces, empty when there are none) and value.
fn split_sample(line: &str) -> Option<(&str, &str, &str)> {
    let (name, labels, rest) = match line.find('{') {
        Some(open) => {
            let close = closing_brace(line, open)?;
            (&line[..open], &line[open + 1..close], &line[close + 1..])
        }
        None => {
            let (name, rest) = line.split_once(char::is_whitespace)?;
            (name, "", rest)
        }
    };
    let value = rest.split_whitespace().next()?;
    Some((name.trim(), labels, value))
}

/// The `}` that closes the label set opened at `open`, skipping quoted values.
fn closing_brace(line: &str, open: usize) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in line[open + 1..].char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            '}' if !quoted => return Some(open + 1 + i),
            _ => {}
        }
    }
    None
}

/// The value of label `key` in `a="1",b="2"`, unescaped.
pub(crate) fn label(labels: &str, key: &str) -> Option<String> {
    let mut rest = labels;
    loop {
        rest = rest.trim_start_matches([',', ' ']);
        if rest.is_empty() {
            return None;
        }
        let (name, after) = rest.split_once('=')?;
        let after = after.trim_start().strip_prefix('"')?;
        let mut value = String::new();
        let mut chars = after.char_indices();
        let end = loop {
            match chars.next()? {
                (i, '"') => break i,
                (_, '\\') => match chars.next()?.1 {
                    'n' => value.push('\n'),
                    c => value.push(c),
                },
                (_, c) => value.push(c),
            }
        };
        if name.trim() == key {
            return Some(value);
        }
        rest = &after[end + 1..];
    }
}

/// A sample value as a whole number: Quickwit writes counters as integers
/// and gauges as floats. Negative, NaN and infinite values are unreadable.
pub(crate) fn parse_value(value: &str) -> Option<u64> {
    let v: f64 = value.parse().ok()?;
    (v.is_finite() && v >= 0.0).then(|| v.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/metrics` from Quickwit 0.9.1 serving the fixture indexes
    /// (`scripts/quickwit-fixtures.sh`) after two searches over both
    /// indexes, trimmed to the cache, thread pool and runtime metrics and
    /// two others.
    const QUICKWIT_091: &str = include_str!("../tests/data/quickwit-0.9.1-metrics.txt");

    #[test]
    fn reads_quickwit_091() {
        let r = parse(QUICKWIT_091);
        assert_eq!(r.len(), 4, "{r:?}");
        // Two splits, so two footers: 8,761 + 8,459 bytes, the
        // `footer_offsets` of the two splits.
        assert_eq!(
            r[&Cache::SplitFooter],
            CacheStats {
                bytes: 17_220,
                items: 2,
                hits: 4,
                misses: 2,
                evictions: 0,
            }
        );
        assert_eq!(
            r[&Cache::FastField],
            CacheStats {
                bytes: 663,
                items: 4,
                hits: 0,
                misses: 4,
                evictions: 0,
            }
        );
        assert_eq!(r[&Cache::PartialRequest].misses, 8);
        assert_eq!(r[&Cache::Predicate].misses, 4);
    }

    #[test]
    fn skips_what_it_does_not_know() {
        let text = "\
# TYPE quickwit_cache_in_cache_num_bytes gauge
quickwit_cache_in_cache_num_bytes 0
quickwit_cache_in_cache_num_bytes{component_name=\"shortlived\"} 99
quickwit_cache_in_cache_num_bytes{component_name=\"splitfooter\"} 2.5e8
quickwit_cache_in_cache_count{policy=\"lru\",component_name=\"splitfooter\"} 800 1700000000000
quickwit_cache_cache_hits_total{component_name=\"splitfooter\"} NaN
quickwit_cache_cache_misses_total{component_name=\"splitfooter\"} -1
quickwit_cache_cache_evict_total{component_name=\"fastfields\"} 3
quickwit_cache_cache_evict_total{component_name=\"fastfields\"
quickwit_cache_virtual_cache_hits_total{component_name=\"splitfooter\",capacity=\"1\",policy=\"lru\"} 7
garbage
";
        let r = parse(text);
        assert_eq!(
            r[&Cache::SplitFooter],
            CacheStats {
                bytes: 250_000_000,
                items: 800,
                ..CacheStats::default()
            }
        );
        assert_eq!(r[&Cache::FastField].evictions, 3);
        assert!(!r.contains_key(&Cache::Predicate));
        assert!(parse("").is_empty());
    }

    #[test]
    fn labels_with_quotes_and_braces() {
        assert_eq!(
            label(r#"a="x\"}",component_name="predicate""#, "component_name").as_deref(),
            Some("predicate")
        );
        assert_eq!(label(r#"a="1""#, "b"), None);
        assert_eq!(label(r#"a="un\\terminated"#, "a"), None);
        let r =
            parse(r#"quickwit_cache_in_cache_count{note="a } b",component_name="predicate"} 5"#);
        assert_eq!(r[&Cache::Predicate].items, 5);
    }
}
