//! Day numbers and time buckets.
//!
//! All time aggregation uses integer fields so it is exact and engine
//! independent before 1970 (05 §5.5):
//! - `day`: days since 1700-01-01
//! - `ym`: `year * 12 + (month - 1)`
//! - `year`

use chrono::{Datelike, Days, Months, NaiveDate};
use serde::{Deserialize, Serialize};

/// Day 0 of the `day` field.
pub fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1700, 1, 1).expect("valid epoch")
}

/// Days since 1700-01-01. Dates before the epoch clamp to 0.
pub fn day_number(date: NaiveDate) -> u32 {
    u32::try_from((date - epoch()).num_days()).unwrap_or(0)
}

pub fn date_from_day(day: u32) -> NaiveDate {
    epoch() + Days::new(u64::from(day))
}

pub fn ym_number(date: NaiveDate) -> u32 {
    u32::try_from(date.year()).unwrap_or(0) * 12 + date.month0()
}

pub fn date_from_ym(ym: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt((ym / 12) as i32, ym % 12 + 1, 1).expect("valid ym")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BucketUnit {
    Year,
    Month,
    Week,
    Day,
}

impl BucketUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
            Self::Week => "week",
            Self::Day => "day",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "year" => Some(Self::Year),
            "month" => Some(Self::Month),
            "week" => Some(Self::Week),
            "day" => Some(Self::Day),
            _ => None,
        }
    }

    /// Default bucket for a date span (05 §5.7):
    /// > 30 years → year; 3–30 years → month; 4 months – 3 years → week; ≤ 4 months → day.
    pub fn auto(from: NaiveDate, to: NaiveDate) -> Self {
        if to
            > from
                .checked_add_months(Months::new(30 * 12))
                .unwrap_or(NaiveDate::MAX)
        {
            Self::Year
        } else if to
            > from
                .checked_add_months(Months::new(3 * 12))
                .unwrap_or(NaiveDate::MAX)
        {
            Self::Month
        } else if to
            > from
                .checked_add_months(Months::new(4))
                .unwrap_or(NaiveDate::MAX)
        {
            Self::Week
        } else {
            Self::Day
        }
    }
}

/// A concrete bucketing of an inclusive date range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketSpec {
    pub unit: BucketUnit,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// Which integer field an engine should histogram, and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistogramField {
    pub field: &'static str,
    pub interval: u32,
    /// Key of bucket 0 in the field's units.
    pub origin: u32,
}

impl BucketSpec {
    pub fn new(unit: BucketUnit, from: NaiveDate, to: NaiveDate) -> Self {
        Self { unit, from, to }
    }

    pub fn histogram_field(&self) -> HistogramField {
        match self.unit {
            BucketUnit::Year => HistogramField {
                field: "year",
                interval: 1,
                origin: u32::try_from(self.from.year()).unwrap_or(0),
            },
            BucketUnit::Month => HistogramField {
                field: "ym",
                interval: 1,
                origin: ym_number(self.from),
            },
            BucketUnit::Week => HistogramField {
                field: "day",
                interval: 7,
                origin: day_number(self.from),
            },
            BucketUnit::Day => HistogramField {
                field: "day",
                interval: 1,
                origin: day_number(self.from),
            },
        }
    }

    /// Number of buckets covering `from..=to`.
    pub fn len(&self) -> usize {
        self.index_of(self.to).map_or(0, |i| i + 1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bucket index for a date, or `None` outside the range.
    pub fn index_of(&self, date: NaiveDate) -> Option<usize> {
        if date < self.from || date > self.to {
            return None;
        }
        Some(self.index_of_day(day_number(date)))
    }

    /// Bucket index for a day number known to be within the range.
    pub fn index_of_day(&self, day: u32) -> usize {
        let date = date_from_day(day);
        let h = self.histogram_field();
        let key = match self.unit {
            BucketUnit::Year => u32::try_from(date.year()).unwrap_or(0),
            BucketUnit::Month => ym_number(date),
            BucketUnit::Week | BucketUnit::Day => day,
        };
        (key.saturating_sub(h.origin) / h.interval) as usize
    }

    /// Index for a histogram key returned by an engine on [`Self::histogram_field`].
    pub fn index_of_key(&self, key: u32) -> Option<usize> {
        let h = self.histogram_field();
        let idx = (key.checked_sub(h.origin)? / h.interval) as usize;
        (idx < self.len()).then_some(idx)
    }

    /// First date of bucket `index` (clamped to `from`).
    pub fn bucket_start(&self, index: usize) -> NaiveDate {
        let i = index as u32;
        let start = match self.unit {
            BucketUnit::Year => {
                NaiveDate::from_ymd_opt(self.from.year() + i as i32, 1, 1).expect("valid year")
            }
            BucketUnit::Month => date_from_ym(ym_number(self.from) + i),
            BucketUnit::Week => self.from + Days::new(u64::from(i) * 7),
            BucketUnit::Day => self.from + Days::new(u64::from(i)),
        };
        start.max(self.from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn day_and_ym_numbers_round_trip_before_1970() {
        let date = d("1789-04-15");
        assert_eq!(date_from_day(day_number(date)), date);
        assert_eq!(day_number(d("1700-01-02")), 1);
        assert_eq!(ym_number(d("1896-07-10")), 1896 * 12 + 6);
        assert_eq!(date_from_ym(1896 * 12 + 6), d("1896-07-01"));
    }

    #[test]
    fn auto_bucket_follows_span_table() {
        assert_eq!(
            BucketUnit::auto(d("1789-01-01"), d("1922-12-31")),
            BucketUnit::Year
        );
        assert_eq!(
            BucketUnit::auto(d("1860-01-01"), d("1870-12-31")),
            BucketUnit::Month
        );
        assert_eq!(
            BucketUnit::auto(d("1896-06-01"), d("1896-12-31")),
            BucketUnit::Week
        );
        assert_eq!(
            BucketUnit::auto(d("1896-06-01"), d("1896-08-31")),
            BucketUnit::Day
        );
    }

    #[test]
    fn week_buckets_start_at_from() {
        let spec = BucketSpec::new(BucketUnit::Week, d("1896-06-01"), d("1896-12-31"));
        assert_eq!(spec.len(), 31);
        assert_eq!(spec.index_of(d("1896-06-07")), Some(0));
        assert_eq!(spec.index_of(d("1896-06-08")), Some(1));
        assert_eq!(spec.bucket_start(1), d("1896-06-08"));
        assert_eq!(spec.index_of(d("1897-01-01")), None);
        let h = spec.histogram_field();
        assert_eq!((h.field, h.interval), ("day", 7));
        assert_eq!(spec.index_of_key(h.origin + 14), Some(2));
    }

    #[test]
    fn year_and_month_buckets() {
        let years = BucketSpec::new(BucketUnit::Year, d("1789-04-15"), d("1922-12-31"));
        assert_eq!(years.len(), 134);
        assert_eq!(years.bucket_start(0), d("1789-04-15"));
        assert_eq!(years.bucket_start(1), d("1790-01-01"));
        let months = BucketSpec::new(BucketUnit::Month, d("1860-11-15"), d("1861-02-01"));
        assert_eq!(months.len(), 4);
        assert_eq!(months.index_of(d("1861-01-31")), Some(2));
    }
}
