//! The step throughput panel: each step's pages an hour and a day, with a
//! week of history, and unknown shown as unknown (#704).

use super::fmt::*;
use crate::throughput::{trend, Counted, Failures, StepRate, ThroughputView, BIN_HOURS, DAY_HOURS};
use chrono::{DateTime, Duration, Utc};
use std::fmt::Write as _;

/// One bar's slot, wide enough to hit, and the bar drawn in it.
const SLOT: f64 = 6.0;
const CHART_HEIGHT: f64 = 36.0;
const HOUR_SLOT: f64 = 12.0;

fn unknown() -> String {
    "<span class=\"muted\">unknown</span>".to_owned()
}

/// A count: whole numbers as they are, a fraction to one place.
fn count(n: f64) -> String {
    if n.fract() == 0.0 || n >= 100.0 {
        int(n.round() as u64)
    } else {
        format!("{n:.1}")
    }
}

fn maybe(n: Option<f64>) -> String {
    n.map_or_else(unknown, count)
}

/// One run of bars: what they are, and the hours each covers.
struct Bars<'a> {
    /// Stays the same from one refresh to the next, so focus can.
    key: String,
    label: &'a str,
    start: Option<DateTime<Utc>>,
    hours: i64,
    slot: f64,
}

impl Bars<'_> {
    fn open(&self, n: usize, class: &str) -> String {
        let at = self.start.map_or_else(String::new, |t| {
            t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
        format!(
            "<svg class=\"spark{class}\" viewBox=\"0 0 {} {CHART_HEIGHT}\" role=\"img\" tabindex=\"0\" \
             data-chart=\"{}\" data-start=\"{at}\" data-hours=\"{}\" aria-label=\"{}. Left and right arrow keys step through the bars.\">",
            n as f64 * self.slot,
            esc(&self.key),
            self.hours,
            esc(self.label),
        )
    }

    /// The window a bar covers, in UTC: the page's script writes it again in
    /// Eastern, so this is what a browser without scripts shows.
    fn window(&self, i: usize) -> String {
        let Some(start) = self.start else {
            return String::new();
        };
        let from = start + Duration::hours(i as i64 * self.hours);
        let to = from + Duration::hours(self.hours);
        format!(
            "{} to {} UTC: ",
            from.format("%b %-d %H:%M"),
            to.format("%H:%M")
        )
    }

    /// A bar: `drawn` under a title and a hit area the height of the chart,
    /// with no gap to the next, so short bars and zeros can be hovered.
    fn bar(&self, o: &mut String, i: usize, hover: &str, drawn: &str) {
        let _ = write!(
            o,
            "<g class=\"bar\"><title>{}{}</title><rect class=\"hit\" x=\"{}\" y=\"0\" width=\"{}\" height=\"{CHART_HEIGHT}\"/>{drawn}</g>",
            esc(&self.window(i)),
            esc(hover),
            i as f64 * self.slot,
            self.slot,
        );
    }

    /// A slot's drawn bar, leaving a gap to the next.
    fn rect(&self, i: usize, class: &str, y: f64, h: f64) -> String {
        format!(
            "<rect class=\"{class}\" x=\"{}\" y=\"{y:.1}\" width=\"{}\" height=\"{h:.1}\"/>",
            i as f64 * self.slot,
            self.slot * 2.0 / 3.0,
        )
    }

    /// One count to a bar. A bin nobody observed is a pale band of full
    /// height, so it can't be read as a quiet period; a zero is a flat tick.
    fn counts(&self, values: &[Option<f64>], known: &[u32], unit: &str, class: &str) -> String {
        let top = values.iter().flatten().fold(0.0_f64, |a, v| a.max(*v));
        let mut o = self.open(values.len(), class);
        for (i, v) in values.iter().enumerate() {
            let Some(v) = *v else {
                let drawn = self.rect(i, "unk", 0.0, CHART_HEIGHT);
                self.bar(&mut o, i, "unknown", &drawn);
                continue;
            };
            let h = if top > 0.0 && v > 0.0 {
                (v / top * (CHART_HEIGHT - 1.0)).max(2.0)
            } else {
                1.0
            };
            let seen = known.get(i).copied().unwrap_or(self.hours as u32);
            let partial = if i64::from(seen) < self.hours {
                format!(", {seen} of {} hours known", self.hours)
            } else {
                String::new()
            };
            let what = if v == 0.0 {
                "0 (counted)".to_owned()
            } else {
                counted(v, unit)
            };
            let drawn = self.rect(i, "v", CHART_HEIGHT - h, h);
            self.bar(&mut o, i, &format!("{what}{partial}"), &drawn);
        }
        o.push_str("</svg>");
        o
    }
}

/// A week of bars for one count, and its peak.
fn chart(s: &StepRate, start: Option<DateTime<Utc>>) -> String {
    let peak = s
        .bins
        .iter()
        .flatten()
        .fold(None, |a: Option<f64>, v| Some(a.unwrap_or(0.0).max(*v)));
    let bars = Bars {
        key: format!("week-{}", s.id),
        label: &format!("{}, last 7 days, {BIN_HOURS} hours a bar", s.label),
        start,
        hours: BIN_HOURS,
        slot: SLOT,
    };
    let mut o = bars.counts(&s.bins, &s.bin_known, s.unit, "");
    let _ = write!(
        o,
        " <span class=\"num\">peak {} per {BIN_HOURS} h</span>",
        peak.map_or_else(|| "unknown".to_owned(), count)
    );
    o
}

/// Valid and inconclusive replicas, stacked.
fn quorum_chart(
    app: &str,
    valid: &[Option<f64>],
    inconclusive: &[Option<f64>],
    start: Option<DateTime<Utc>>,
) -> String {
    let total =
        |i: usize| valid[i].unwrap_or(0.0) + inconclusive.get(i).copied().flatten().unwrap_or(0.0);
    let top = (0..valid.len()).fold(0.0_f64, |a, i| a.max(total(i)));
    let bars = Bars {
        key: format!("quorum-{app}"),
        label: &format!("{app}, valid and inconclusive results, last 7 days"),
        start,
        hours: BIN_HOURS,
        slot: SLOT,
    };
    let mut o = bars.open(valid.len(), "");
    let usable = CHART_HEIGHT - 3.0;
    for (i, good) in valid.iter().enumerate() {
        let Some(good) = *good else {
            let drawn = bars.rect(i, "unk", 0.0, CHART_HEIGHT);
            bars.bar(&mut o, i, "unknown", &drawn);
            continue;
        };
        let bad = inconclusive.get(i).copied().flatten().unwrap_or(0.0);
        let scale = |v: f64| {
            if top > 0.0 && v > 0.0 {
                (v / top * usable).max(2.0)
            } else {
                0.0
            }
        };
        let (g, b) = (scale(good), scale(bad));
        if g == 0.0 && b == 0.0 {
            let drawn = bars.rect(i, "v", CHART_HEIGHT - 1.0, 1.0);
            bars.bar(&mut o, i, "0 (counted)", &drawn);
            continue;
        }
        let mut drawn = String::new();
        if g > 0.0 {
            drawn.push_str(&bars.rect(i, "v", CHART_HEIGHT - g, g));
        }
        if b > 0.0 {
            let gap = if g > 0.0 { 2.0 } else { 0.0 };
            drawn.push_str(&bars.rect(i, "v s2", CHART_HEIGHT - g - gap - b, b));
        }
        let hover = format!("{} valid, {} inconclusive", count(good), count(bad));
        bars.bar(&mut o, i, &hover, &drawn);
    }
    o.push_str("</svg>");
    o
}

/// A count with its unit, singular when it is one.
fn counted(n: f64, unit: &str) -> String {
    format!(
        "{} {}",
        count(n),
        if n == 1.0 {
            unit.trim_end_matches('s')
        } else {
            unit
        }
    )
}

fn hour_cell(s: &StepRate) -> String {
    let Some(n) = s.last_hour else {
        return unknown();
    };
    if s.id == "bridge_lock" {
        format!("{} s ({:.0}%)", count(n), n / 36.0)
    } else {
        counted(n, s.unit)
    }
}

fn day_cell(s: &StepRate) -> String {
    let Some(n) = s.last_day else {
        return unknown();
    };
    let mut o = if s.id == "bridge_lock" {
        let share = n / (f64::from(s.day_hours_known) * 36.0);
        format!("{} s ({share:.0}%)", count(n))
    } else {
        counted(n, s.unit)
    };
    if let Some(f) = s.failed_day.filter(|f| *f > 0.0) {
        let _ = write!(o, ", {} failed", count(f));
    }
    if s.day_hours_known < 24 {
        let _ = write!(
            o,
            " <span class=\"num\">({} of 24 h known)</span>",
            s.day_hours_known
        );
    }
    o
}

fn per_what(s: &StepRate) -> &'static str {
    match s.unit {
        "ticks" => "tick",
        "units" => "unit",
        _ => "page",
    }
}

fn seconds(n: f64, per: &str) -> String {
    if n >= 100.0 {
        format!("{}&nbsp;s/{per}", int(n as u64))
    } else {
        format!("{n:.1}&nbsp;s/{per}")
    }
}

fn seconds_cell(s: &StepRate) -> String {
    match (s.seconds_per_page, s.id) {
        (_, "bridge_lock") => "-".to_owned(),
        (Some(n), _) => seconds(n, per_what(s)),
        (None, _) => unknown(),
    }
}

/// `n of 7 d`-style coverage, said only when it is partial.
fn coverage(c: Counted, of: i64) -> String {
    if c.value.is_some() && i64::from(c.hours_known) < of {
        format!(
            " <span class=\"num\">({} of {of} h known)</span>",
            c.hours_known
        )
    } else {
        String::new()
    }
}

fn amount(c: Counted, s: &StepRate) -> String {
    c.value.map_or_else(unknown, |n| {
        if s.id == "bridge_lock" {
            format!("{} s", count(n))
        } else {
            counted(n, s.unit)
        }
    })
}

/// The last day against the one before, in words.
fn trend_text(s: &StepRate) -> String {
    let now = Counted {
        value: s.last_day,
        hours_known: s.day_hours_known,
    };
    let before = s.detail.previous_day;
    match (now.value, before.value, trend(now, before)) {
        (None, _, _) | (_, None, _) => {
            "Against the 24 hours before: unknown, one of the two has no count.".to_owned()
        }
        (Some(n), Some(_), None) if n > 0.0 => "Against the 24 hours before: up from 0.".to_owned(),
        (Some(_), Some(_), None) => "Against the 24 hours before: 0 in both.".to_owned(),
        (Some(_), Some(_), Some(pct)) => {
            let hours = if now.hours_known == before.hours_known {
                ""
            } else {
                ", per hour observed"
            };
            format!("Against the 24 hours before: {pct:+.0}%{hours}.")
        }
    }
}

fn counts_block(s: &StepRate) -> String {
    let d = &s.detail;
    let hour = Counted {
        value: s.last_hour,
        hours_known: u32::from(s.last_hour.is_some()),
    };
    let day = Counted {
        value: s.last_day,
        hours_known: s.day_hours_known,
    };
    format!(
        "<table class=\"windows\"><tr><th class=\"r\">1 h</th><th class=\"r\">6 h</th><th class=\"r\">24 h</th><th class=\"r\">7 d</th></tr>\
         <tr><td class=\"r\">{}</td><td class=\"r\">{}{}</td><td class=\"r\">{}{}</td><td class=\"r\">{}{}</td></tr></table><p>{}</p>",
        amount(hour, s),
        amount(d.last_6h, s),
        coverage(d.last_6h, 6),
        amount(day, s),
        coverage(day, DAY_HOURS),
        amount(d.last_week, s),
        coverage(d.last_week, 168),
        trend_text(s),
    )
}

fn seconds_block(s: &StepRate) -> String {
    if s.id == "bridge_lock" {
        return "<p>Not a per-page step. The count above is in seconds.</p>".to_owned();
    }
    let per = per_what(s);
    let d = &s.detail;
    match (s.seconds_per_page, d.seconds_p50, d.seconds_p95) {
        (None, _, _) => "<p>Unknown: no hour here has both seconds and a count.</p>".to_owned(),
        (Some(avg), Some(p50), Some(p95)) => format!(
            "<p>p50 about {}, p95 about {}, average {}, over the last 24 hours. \
             The percentiles come from each page's time in the index, to within about 19%.</p>",
            seconds(p50, per),
            seconds(p95, per),
            seconds(avg, per),
        ),
        (Some(avg), _, _) => format!(
            "<p>Average {}, over the last 24 hours. Average only. This source keeps hourly totals, so p50 and p95 aren't known.</p>",
            seconds(avg, per),
        ),
    }
}

fn rate(failed: Option<f64>, attempts: Option<f64>) -> String {
    match (failed, attempts) {
        (Some(f), Some(a)) if a > 0.0 => format!("{:.1}%", f / a * 100.0),
        (Some(_), Some(_)) => "no attempts".to_owned(),
        _ => "rate unknown".to_owned(),
    }
}

fn failures_block(s: &StepRate) -> String {
    let Some(f) = &s.detail.failures else {
        return "<p>Unknown: no failure count for this step.</p>".to_owned();
    };
    let window = |c: Counted, failed, attempts, of| match c.value {
        Some(n) => format!(
            "{}, {}{}",
            counted(n, f.unit),
            rate(failed, attempts),
            coverage(c, of)
        ),
        None => unknown(),
    };
    let mut o = format!(
        "<p>24 h: {}. 7 d: {}.</p>",
        window(f.day, f.day_failed_of_attempts, f.day_attempts, DAY_HOURS),
        window(f.week, f.week_failed_of_attempts, f.week_attempts, 168),
    );
    o.push_str(&reasons_list(f));
    o
}

fn reasons_list(f: &Failures) -> String {
    if f.reasons.is_empty() {
        return "<p class=\"muted\">No reason recorded for these.</p>".to_owned();
    }
    let mut o = String::new();
    if let Some(n) = f.reasons_from_latest {
        let _ = write!(o, "<p>Reasons in the latest {n} failures:</p>");
    }
    o.push_str("<ul class=\"reasons\">");
    for r in &f.reasons {
        let n = match (r.day, r.week) {
            (Some(d), Some(w)) => format!("{} in 24 h, {} in 7 d", count(d), count(w)),
            (Some(d), None) => count(d),
            (None, Some(w)) => format!("{} in 7 d", count(w)),
            (None, None) => "unknown".to_owned(),
        };
        let _ = write!(
            o,
            "<li>{} <span class=\"num\">({n})</span></li>",
            esc(&r.text)
        );
    }
    o.push_str("</ul>");
    o
}

fn limit_block(s: &StepRate) -> String {
    let d = &s.detail;
    let mut o = format!(
        "<p>{}</p>",
        s.limit
            .as_deref()
            .map_or_else(|| "Limit not known.".to_owned(), esc)
    );
    let mut near = Vec::new();
    if let Some(now) = &d.limit_now {
        near.push(esc(now));
    }
    if let (Some(h), Some(day)) = (d.busy_hour, d.busy_day) {
        near.push(format!(
            "{h:.0}% of the last hour and {day:.0}% of the last 24 hours spent on this step"
        ));
    }
    if let (Some(day), Some(week)) = (d.deferred_day, d.deferred_week) {
        near.push(format!(
            "held back {} times in 24 h, {} in 7 d",
            count(day),
            count(week)
        ));
    }
    if near.is_empty() {
        o.push_str("<p class=\"muted\">How close it is: unknown.</p>");
    } else {
        let _ = write!(o, "<p>How close: {}.</p>", near.join("; "));
    }
    o
}

fn runs_block(s: &StepRate) -> String {
    let d = &s.detail;
    let searched = d.hours_searched;
    let ok = d.last_ok.map_or_else(
        || {
            if searched == 0 {
                unknown()
            } else {
                format!("none in the {searched} hours kept")
            }
        },
        |t| format!("in the hour from {}", time(t)),
    );
    let failed = match (d.last_failed, d.failures.is_some() || d.last_failed_exact) {
        (Some(t), _) if d.last_failed_exact => time(t),
        (Some(t), _) => format!("in the hour from {}", time(t)),
        (None, false) => unknown(),
        (None, true) if searched == 0 => unknown(),
        (None, true) if s.id == "scan_fetch" => "none recorded".to_owned(),
        (None, true) => format!("none in the {searched} hours kept"),
    };
    format!("<p>Last success: {ok}.</p><p>Last failure: {failed}.</p>")
}

fn sources_block(s: &StepRate) -> String {
    let d = &s.detail;
    let mut o = String::new();
    if d.sources.is_empty() {
        o.push_str("<p class=\"muted\">Source unknown.</p>");
    }
    for source in &d.sources {
        let _ = write!(
            o,
            "<p>{}: {}{}</p>",
            source_label(source.name),
            source.received.map_or_else(
                || "never reported".to_owned(),
                |t| format!(
                    "{} {}",
                    if source.name == crate::throughput::INDEX {
                        "read"
                    } else {
                        "reported"
                    },
                    time(t)
                )
            ),
            if source.stale {
                format!(" {}", badge("warning", "stale"))
            } else {
                String::new()
            }
        );
    }
    let _ = write!(
        o,
        "<p>Newest hour counted: {}.</p>",
        d.newest_hour.map_or_else(unknown, time)
    );
    o
}

fn source_label(name: &str) -> &'static str {
    match name {
        "controller" => "Controller journal",
        "ship" => "Shipper and loader logs",
        "boinc" => "BOINC database and step containers",
        _ => "Fetch index",
    }
}

/// What opens under a step's row, which takes no part in the columns above.
fn detail(s: &StepRate, hours_start: Option<DateTime<Utc>>) -> String {
    let known = s
        .detail
        .hours
        .iter()
        .map(|v| u32::from(v.is_some()))
        .collect::<Vec<_>>();
    let bars = Bars {
        key: format!("day-{}", s.id),
        label: &format!("{}, last 24 hours, one hour a bar", s.label),
        start: hours_start,
        hours: 1,
        slot: HOUR_SLOT,
    };
    format!(
        "<div class=\"detail-body\">\
         <div class=\"detail-block wide-block\"><h4>What it is</h4><p>{}</p></div>\
         <div class=\"detail-block\"><h4>Counts</h4>{}</div>\
         <div class=\"detail-block\"><h4>Last 24 hours, hour by hour</h4>{}</div>\
         <div class=\"detail-block\"><h4>Seconds per {}</h4>{}</div>\
         <div class=\"detail-block\"><h4>Failures</h4>{}</div>\
         <div class=\"detail-block\"><h4>Limit</h4>{}</div>\
         <div class=\"detail-block\"><h4>Last runs</h4>{}</div>\
         <div class=\"detail-block\"><h4>Source and freshness</h4>{}</div>\
         </div>",
        esc(s.scope),
        counts_block(s),
        bars.counts(&s.detail.hours, &known, s.unit, " hourly"),
        per_what(s),
        seconds_block(s),
        failures_block(s),
        limit_block(s),
        runs_block(s),
        sources_block(s),
    )
}

pub(super) fn panel(v: &ThroughputView) -> String {
    let mut o = String::with_capacity(48 * 1024);
    o.push_str(
        "<section class=\"wide\" id=\"step-throughput\"><h2>Step throughput</h2>\
         <p class=\"note\">Counted hour by hour from what each step records, in full hours; the current hour isn't in \
         the rates. Unknown means nobody read that hour, not that nothing happened. A step's count \
         is its own: don't add rows, since one page passes through many.</p>",
    );
    if let Some(e) = &v.error {
        let _ = write!(o, "<p class=\"err\">{}</p>", esc(e));
    }
    o.push_str(
        "<div class=\"table-wide steps-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Step throughput\">",
    );
    o.push_str(
        "<table class=\"throughput steps\"><colgroup><col class=\"c-step\"><col class=\"c-hour\">\
         <col class=\"c-day\"><col class=\"c-secs\"><col class=\"c-limit\"><col class=\"c-week\"></colgroup>\
         <thead><tr><th>Step</th><th class=\"r\">Last hour</th>\
         <th class=\"r\">Last 24 hours</th><th class=\"r\">Seconds</th><th>Limit</th>\
         <th>Last 7 days</th></tr></thead>",
    );
    for s in &v.steps {
        let _ = write!(
            o,
            "<tbody><tr><td><details data-refresh-key=\"throughput-{id}\"><summary aria-controls=\"detail-{id}\">{}</summary></details></td>\
             <td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r seconds\">{}</td><td>{}</td><td class=\"chart\">{}</td></tr>\
             <tr class=\"step-detail\" id=\"detail-{id}\"><td colspan=\"6\">{}</td></tr></tbody>",
            esc(s.label),
            hour_cell(s),
            day_cell(s),
            seconds_cell(s),
            s.limit.as_deref().map_or_else(|| "<span class=\"muted\">not known</span>".to_owned(), esc),
            chart(s, v.chart_start),
            detail(s, v.hours_start),
            id = s.id,
        );
    }
    o.push_str("</table></div><p class=\"note\">Bars are 4 hours each, oldest left, of full hours, so the last ones are short. \
        A flat tick is a counted zero; a pale full-height band is unknown. Each row has its own scale. Hover or tap a bar for its \
        window and count, or focus a chart and use the arrow keys. Open a step for its detail.</p>");

    o.push_str(
        "<h3>Results by validation outcome, last 24 hours</h3>\
         <p class=\"note\"><span class=\"key\"></span>valid <span class=\"key s2\"></span>inconclusive \
         &nbsp; Results are replicas, not pages (#1420). \
         An inconclusive result can turn valid when another replica agrees, so recent hours can change.</p>",
    );
    o.push_str("<div class=\"table-wide\" tabindex=\"0\" role=\"region\" aria-label=\"Results by validation outcome\">");
    o.push_str(
        "<table class=\"throughput\"><thead><tr><th>App</th><th class=\"r\">Valid</th><th class=\"r\">Inconclusive</th>\
         <th class=\"r\">Invalid</th><th class=\"r\">Errors</th><th class=\"r\">Waiting for quorum</th>\
         <th class=\"r\">Workunits done</th><th class=\"r\">7 days: valid / inconclusive</th><th>Last 7 days</th></tr></thead><tbody>",
    );
    for q in &v.quorum {
        let _ = write!(
            o,
            "<tr><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td>\
             <td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{} / {}</td><td class=\"chart\">{}</td></tr>",
            q.app,
            maybe(q.valid_day),
            maybe(q.inconclusive_day),
            maybe(q.invalid_day),
            maybe(q.error_day),
            maybe(q.waiting_day),
            maybe(q.pages_day),
            maybe(q.valid_week),
            maybe(q.inconclusive_week),
            quorum_chart(q.app, &q.valid_bins, &q.inconclusive_bins, v.chart_start),
        );
    }
    o.push_str("</tbody></table></div>");

    o.push_str("<h3>Controller phases, last 24 hours</h3>");
    if v.phases.is_empty() {
        o.push_str("<p class=\"muted\">Unknown: no phase counts yet.</p>");
    } else {
        o.push_str("<div class=\"table-wide\" tabindex=\"0\" role=\"region\" aria-label=\"Controller phases\">");
        o.push_str("<table class=\"throughput\"><thead><tr><th>Phase</th><th class=\"r\">Runs</th><th class=\"r\">Failed</th>\
            <th class=\"r\">Seconds</th><th>Share</th></tr></thead><tbody>");
        let all: f64 = v.phases.iter().map(|p| p.seconds).sum();
        for p in &v.phases {
            let _ = write!(
                o,
                "<tr><td>{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td></tr>",
                esc(&p.phase),
                count(p.runs),
                count(p.failed),
                count(p.seconds.round()),
                meter(
                    p.seconds as u64,
                    all as u64,
                    &format!("{:.0}%", pct(p.seconds as u64, all as u64))
                )
            );
        }
        o.push_str("</tbody></table></div>");
    }

    o.push_str("<p class=\"note\">Sources: scan fetch, the fetch index, read here");
    for s in &v.sources {
        let _ = write!(
            o,
            "; {}, {}{}",
            esc(&s.name),
            s.received.map_or_else(
                || "never reported".to_owned(),
                |t| format!("reported {}", time(t))
            ),
            if s.stale {
                format!(" {}", badge("warning", "stale"))
            } else {
                String::new()
            }
        );
    }
    o.push_str(". <a href=\"/api/view.json\">JSON</a></p></section>");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throughput::{view, Throughput};
    use std::collections::BTreeMap;

    fn empty() -> ThroughputView {
        view(&Throughput::default(), &BTreeMap::new(), None, Utc::now())
    }

    /// The number of bars on the page with nothing connected: a week for each
    /// step and app, and a day for each step.
    fn bars(v: &ThroughputView) -> usize {
        42 * (v.steps.len() + v.quorum.len()) + 24 * v.steps.len()
    }

    #[test]
    fn with_nothing_connected_every_rate_says_unknown_and_none_says_zero() {
        let html = panel(&empty());
        assert!(html.contains("id=\"step-throughput\""));
        assert!(html.contains("Scan fetch"));
        assert!(html.contains("usnm_qwen"));
        assert!(html.contains("Bridge lock held"));
        assert!(!html.contains(">0 pages<"));
        assert!(!html.contains(">0<"));
        assert!(!html.contains("peak 0"));
        assert!(html.contains("peak unknown per 4 h"));
        assert!(html.contains("no phase counts yet"));
        assert!(html.contains("never reported"));
        // A chart of unknown bins draws unknown bands and no bars.
        assert_eq!(html.matches("class=\"unk\"").count(), bars(&empty()));
        assert!(!html.contains("class=\"v\""));
    }

    #[test]
    fn every_bar_has_a_title_and_a_hit_area_the_height_of_the_chart() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "stage").unwrap();
        s.bins[40] = Some(0.0);
        s.bin_known[40] = 4;
        s.bins[41] = Some(1200.0);
        s.bin_known[41] = 4;
        let html = panel(&v);
        let n = bars(&v);
        assert_eq!(html.matches("<g class=\"bar\"><title>").count(), n);
        assert_eq!(
            html.matches("<rect class=\"hit\"").count(),
            n,
            "one hit area a bar"
        );
        assert_eq!(html.matches("y=\"0\" width=").count(), n);
        // Hit areas touch: a slot is as wide as the step between them.
        assert!(html.contains("x=\"6\" y=\"0\" width=\"6\""));
        assert!(html.contains(": 0 (counted)</title>"));
        assert!(html.contains(": 1,200 pages</title>"));
        assert!(html.contains(": unknown</title>"));
        assert_eq!(html.matches("data-hours=\"4\"").count(), 16);
        assert_eq!(html.matches("data-hours=\"1\"").count(), 13);
        assert_eq!(html.matches("tabindex=\"0\" data-chart=").count(), 29);
    }

    #[test]
    fn a_bar_title_names_its_window_in_utc_until_the_script_runs() {
        let start = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 11, 1, 20, 0, 0).unwrap();
        let bars = Bars {
            key: "k".into(),
            label: "l",
            start: Some(start),
            hours: 4,
            slot: SLOT,
        };
        assert_eq!(bars.window(0), "Nov 1 20:00 to 00:00 UTC: ");
        assert_eq!(bars.window(1), "Nov 2 00:00 to 04:00 UTC: ");
    }

    #[test]
    fn a_zero_is_drawn_as_a_tick_and_text_is_escaped() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "stage").unwrap();
        s.last_hour = Some(1.0);
        s.last_day = Some(0.0);
        s.day_hours_known = 3;
        s.limit = Some("<b>limit</b>".into());
        s.bins[41] = Some(0.0);
        s.bin_known[41] = 2;
        let html = panel(&v);
        assert!(html.contains("1 page<"));
        assert!(html.contains("(3 of 24 h known)"));
        assert!(html.contains("&lt;b&gt;limit&lt;/b&gt;"));
        assert!(!html.contains("<b>limit"));
        assert!(html.contains("height=\"1.0\""));
        assert!(html.contains("0 (counted), 2 of 4 hours known"));
    }

    #[test]
    fn the_detail_is_its_own_row_and_the_columns_are_fixed() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "stage").unwrap();
        s.scope = "the step's own words";
        s.seconds_per_page = Some(1395.0);
        let html = panel(&v);
        assert!(html.contains("<colgroup>"));
        assert_eq!(
            html.matches("<tbody><tr><td><details").count(),
            v.steps.len()
        );
        assert_eq!(html.matches("<td colspan=\"6\">").count(), v.steps.len());
        assert_eq!(html.matches("<details").count(), v.steps.len());
        // The step cell holds only its name; the words are in the row below.
        let row = html.find("throughput-stage").unwrap();
        let words = html.find("the step&#39;s own words").unwrap();
        assert!(html[row..words].contains("class=\"step-detail\""));
        assert!(html.contains("1,395&nbsp;s/page"));
        assert!(html.contains("class=\"r seconds\""));
        assert!(html.contains("<td class=\"chart\">"));
        assert!(html.contains("aria-controls=\"detail-stage\""));
        assert!(html.contains("id=\"detail-stage\""));
        assert!(crate::html::assets::STYLE.contains("tbody:has(details[open])"));
    }

    #[test]
    fn the_detail_says_what_isnt_known() {
        let html = panel(&empty());
        assert!(html.contains("no failure count for this step"));
        assert!(html.contains("How close it is: unknown."));
        assert!(html.contains("Against the 24 hours before: unknown"));
        assert!(html.contains("Last success: <span class=\"muted\">unknown</span>."));
        assert!(html.contains("Last failure: <span class=\"muted\">unknown</span>."));
        assert!(html.contains("Newest hour counted: <span class=\"muted\">unknown</span>."));
    }

    #[test]
    fn the_detail_shows_average_only_unless_there_are_percentiles() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "stage").unwrap();
        s.seconds_per_page = Some(2.0);
        assert!(seconds_block(s).contains("Average only"));
        s.detail.seconds_p50 = Some(1.0);
        s.detail.seconds_p95 = Some(150.0);
        let text = seconds_block(s);
        assert!(text.contains("p50 about 1.0&nbsp;s/page"));
        assert!(text.contains("p95 about 150&nbsp;s/page"));
        assert!(!text.contains("Average only"));
    }

    #[test]
    fn the_trend_says_up_down_and_from_zero() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "stage").unwrap();
        s.last_day = Some(120.0);
        s.day_hours_known = 24;
        s.detail.previous_day = Counted {
            value: Some(100.0),
            hours_known: 24,
        };
        assert!(trend_text(s).ends_with(": +20%."));
        s.detail.previous_day.hours_known = 12;
        assert!(trend_text(s).ends_with(": -40%, per hour observed."));
        s.detail.previous_day.value = Some(0.0);
        assert!(trend_text(s).ends_with(": up from 0."));
        s.last_day = Some(0.0);
        assert!(trend_text(s).ends_with(": 0 in both."));
    }

    #[test]
    fn reasons_are_listed_with_their_counts_and_escaped() {
        let f = Failures {
            reasons: vec![crate::throughput::Reason {
                text: "<lock> failed".into(),
                day: Some(2.0),
                week: Some(9.0),
            }],
            ..Failures::default()
        };
        let html = reasons_list(&f);
        assert!(html.contains("&lt;lock&gt; failed"));
        assert!(html.contains("2 in 24 h, 9 in 7 d"));
    }

    #[test]
    fn the_lock_row_shows_its_share_of_the_time() {
        let mut v = empty();
        let s = v.steps.iter_mut().find(|s| s.id == "bridge_lock").unwrap();
        s.last_hour = Some(1800.0);
        s.last_day = Some(3600.0);
        s.day_hours_known = 2;
        let html = panel(&v);
        assert!(html.contains("1,800 s (50%)"));
        assert!(html.contains("3,600 s (50%)"));
    }

    #[test]
    fn inconclusive_results_stack_on_valid_ones_with_a_legend() {
        let mut v = empty();
        let q = v.quorum.iter_mut().find(|q| q.app == "usnm_qwen").unwrap();
        q.valid_bins[41] = Some(156.0);
        q.inconclusive_bins[41] = Some(409.0);
        let html = panel(&v);
        assert!(html.contains("class=\"v s2\""));
        assert!(html.contains("156 valid, 409 inconclusive</title>"));
        assert!(html.contains("class=\"key s2\""));
    }
}
