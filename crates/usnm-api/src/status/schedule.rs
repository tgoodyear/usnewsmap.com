//! The ingest job's schedule (`USNM_INGEST_CRON`, the same five-field cron
//! expression `caj-usnm-ingest` runs on, in UTC), for the status page's
//! "next scheduled run". Supports what Container Apps accepts in each
//! field: `*`, numbers, ranges `a-b`, lists `a,b` and steps `*/n`, `a-b/n`.

use chrono::{DateTime, Datelike, Duration, NaiveTime, Timelike, Utc};

/// A parsed cron expression: minute, hour, day of month, month, day of week.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<bool>,
    hours: Vec<bool>,
    days: Vec<bool>,
    months: Vec<bool>,
    weekdays: Vec<bool>,
    /// Day of month and day of week were both restricted: either matches.
    either_day: bool,
}

fn field(text: &str, min: u32, max: u32, name: &str) -> Result<(Vec<bool>, bool), String> {
    let mut set = vec![false; max as usize + 1];
    let bad = || format!("bad {name} field `{text}` in the ingest schedule");
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| bad())?),
            None => (part, 1),
        };
        if step == 0 {
            return Err(bad());
        }
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?)
        } else {
            let v: u32 = range.parse().map_err(|_| bad())?;
            // `5/15` means from 5 to the end, every 15.
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return Err(bad());
        }
        for v in (lo..=hi).step_by(step as usize) {
            set[v as usize] = true;
        }
    }
    Ok((set, text != "*"))
}

impl Cron {
    pub fn parse(text: &str) -> Result<Self, String> {
        let parts: Vec<&str> = text.split_whitespace().collect();
        let [m, h, dom, mon, dow] = parts[..] else {
            return Err(format!(
                "the ingest schedule `{text}` needs five fields (minute hour day month weekday)"
            ));
        };
        let (minutes, _) = field(m, 0, 59, "minute")?;
        let (hours, _) = field(h, 0, 23, "hour")?;
        let (days, dom_set) = field(dom, 1, 31, "day of month")?;
        let (months, _) = field(mon, 1, 12, "month")?;
        let (mut weekdays, dow_set) = field(dow, 0, 7, "day of week")?;
        // 7 is Sunday as well as 0.
        if weekdays[7] {
            weekdays[0] = true;
        }
        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            either_day: dom_set && dow_set,
        })
    }

    fn day_matches(&self, d: chrono::NaiveDate) -> bool {
        if !self.months[d.month() as usize] {
            return false;
        }
        let dom = self.days[d.day() as usize];
        let dow = self.weekdays[d.weekday().num_days_from_sunday() as usize];
        if self.either_day {
            dom || dow
        } else {
            dom && dow
        }
    }

    /// The first time after `after` (to the minute) the schedule fires,
    /// looking up to about four years ahead (for a `29 2` schedule).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let start = after + Duration::minutes(1);
        let first_day = start.date_naive();
        for offset in 0..(366 * 4 + 1) {
            let day = first_day + Duration::days(offset);
            if !self.day_matches(day) {
                continue;
            }
            for h in 0..24u32 {
                if !self.hours[h as usize] {
                    continue;
                }
                for m in 0..60u32 {
                    if !self.minutes[m as usize] {
                        continue;
                    }
                    let t = day.and_time(NaiveTime::from_hms_opt(h, m, 0)?).and_utc();
                    if t >= start.with_second(0)?.with_nanosecond(0)? {
                        return Some(t);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn mondays_at_three_seventeen() {
        let c = Cron::parse("17 3 * * 1").unwrap();
        // Friday 2026-10-02 → Monday 2026-10-05.
        assert_eq!(
            c.next_after(at("2026-10-02T19:30:00Z")),
            Some(at("2026-10-05T03:17:00Z"))
        );
        // Exactly at a firing time: the next one, a week on.
        assert_eq!(
            c.next_after(at("2026-10-05T03:17:00Z")),
            Some(at("2026-10-12T03:17:00Z"))
        );
        // Earlier the same Monday: that day.
        assert_eq!(
            c.next_after(at("2026-10-05T01:00:00Z")),
            Some(at("2026-10-05T03:17:00Z"))
        );
    }

    #[test]
    fn steps_lists_and_ranges() {
        let c = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        assert_eq!(
            c.next_after(at("2026-10-02T17:50:00Z")),
            Some(at("2026-10-05T09:00:00Z"))
        );
        let c = Cron::parse("0 0 1,15 * *").unwrap();
        assert_eq!(
            c.next_after(at("2026-10-02T00:00:00Z")),
            Some(at("2026-10-15T00:00:00Z"))
        );
        // Day of month and day of week both set: either one fires.
        let c = Cron::parse("0 12 13 * 5").unwrap();
        assert_eq!(
            c.next_after(at("2026-10-03T00:00:00Z")),
            Some(at("2026-10-09T12:00:00Z"))
        );
        // Sunday as 7.
        let c = Cron::parse("0 6 * * 7").unwrap();
        assert_eq!(
            c.next_after(at("2026-10-02T00:00:00Z")),
            Some(at("2026-10-04T06:00:00Z"))
        );
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.next_after(at("2026-10-02T00:00:00Z")),
            Some(at("2028-02-29T00:00:00Z"))
        );
    }

    #[test]
    fn refuses_what_it_cant_read() {
        for bad in [
            "",
            "* * * *",
            "61 * * * *",
            "* 24 * * *",
            "*/0 * * * *",
            "a * * * *",
            "5-1 * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }
}
