//! The BOINC week panel: three charts on one time axis, queued jobs, active
//! volunteers and machines, and completed jobs, a week in 3-hour buckets.
//! Drawn here; the page's script adds the shared crosshair and the tip, from
//! the tips written into the panel. Counts and app names only.

use std::fmt::Write as _;

use chrono::{DateTime, NaiveDateTime};

use super::fmt::{esc, int, time};
use crate::boinc_week::{Bucket, WeekView, BUCKET_HOURS};

const WIDTH: f64 = 1040.0;
const LEFT: f64 = 48.0;
const RIGHT: f64 = 12.0;
const TOP: f64 = 8.0;
/// Room for the day labels under the last chart.
const AXIS: f64 = 22.0;

/// US Eastern wall time at `at`, from the offset the server gave.
fn local(at: i64, offset_minutes: i32) -> NaiveDateTime {
    DateTime::from_timestamp(at + i64::from(offset_minutes) * 60, 0)
        .unwrap_or_default()
        .naive_utc()
}

fn when(b: &Bucket) -> NaiveDateTime {
    local(b.start, b.utc_offset_minutes)
}

/// A round top for an axis: 1, 2, 2.5 or 5 times a power of ten.
fn nice(max: u64) -> u64 {
    let max = max.max(1);
    let mut p = 1u64;
    while p.saturating_mul(10) <= max {
        p *= 10;
    }
    // 2.5 of a 1 isn't a whole number.
    let quarter = if p >= 10 { 5 * p / 2 } else { 2 * p };
    [p, 2 * p, quarter, 5 * p, 10 * p]
        .into_iter()
        .find(|top| *top >= max)
        .unwrap_or(10 * p)
}

/// The gridlines' values: fifths of the top (quarters of a 2), else halves.
fn ticks(top: u64) -> Vec<u64> {
    let lead = top / 10u64.pow(top.max(1).ilog10());
    let parts = if lead == 2 && !top.is_multiple_of(25) {
        4
    } else {
        5
    };
    match [parts, 2].into_iter().find(|n| top.is_multiple_of(*n)) {
        Some(n) => (0..=n).map(|i| top / n * i).collect(),
        None => vec![0, top],
    }
}

struct Chart<'a> {
    buckets: &'a [Bucket],
    height: f64,
    top: u64,
    axis: bool,
}

impl Chart<'_> {
    fn step(&self) -> f64 {
        (WIDTH - LEFT - RIGHT) / self.buckets.len() as f64
    }

    fn x(&self, i: usize) -> f64 {
        LEFT + self.step() * (i as f64 + 0.5)
    }

    fn y(&self, v: u64) -> f64 {
        TOP + (self.height - TOP) * (1.0 - v as f64 / self.top as f64)
    }

    /// The frame: gridlines with their values, a line at each Eastern
    /// midnight, and under the last chart the days.
    fn open(&self, o: &mut String, label: &str, ys: &str) {
        let total = self.height + if self.axis { AXIS } else { 6.0 };
        let _ = write!(
            o,
            "<svg class=\"wk\" viewBox=\"0 0 {WIDTH} {total}\" role=\"img\" aria-label=\"{}\" data-ys=\"{ys}\">",
            esc(label)
        );
        for v in ticks(self.top) {
            let y = self.y(v);
            let _ = write!(
                o,
                "<line class=\"{}\" x1=\"{LEFT}\" x2=\"{}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/>\
                 <text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
                if v == 0 { "wk-axis" } else { "wk-grid" },
                WIDTH - RIGHT,
                LEFT - 6.0,
                y + 4.0,
                int(v)
            );
        }
        for (i, b) in self.buckets.iter().enumerate() {
            let at = when(b);
            if at.format("%H").to_string() != "00" {
                continue;
            }
            let x = LEFT + self.step() * i as f64;
            let _ = write!(
                o,
                "<line class=\"wk-grid\" x1=\"{x:.1}\" x2=\"{x:.1}\" y1=\"{TOP}\" y2=\"{}\"/>",
                self.height
            );
            if self.axis {
                let _ = write!(
                    o,
                    "<text x=\"{:.1}\" y=\"{:.1}\">{}</text>",
                    x + 3.0,
                    self.height + 15.0,
                    at.format("%a %b %-d")
                );
            }
        }
    }

    fn line(&self, o: &mut String, class: &str, value: impl Fn(&Bucket) -> u64) {
        let d: String = self
            .buckets
            .iter()
            .enumerate()
            .map(|(i, b)| {
                format!(
                    "{}{:.1},{:.1}",
                    if i == 0 { 'M' } else { 'L' },
                    self.x(i),
                    self.y(value(b))
                )
            })
            .collect();
        let _ = write!(o, "<path class=\"wk-line {class}\" d=\"{d}\"/>");
    }

    /// Bars with a rounded top; the bucket still filling is faded.
    fn bars(&self, o: &mut String, value: impl Fn(&Bucket) -> u64) {
        let w = (self.step() - 2.0).max(2.0);
        let r = (w / 2.0).min(4.0);
        for (i, b) in self.buckets.iter().enumerate() {
            let v = value(b);
            if v == 0 {
                continue;
            }
            let h = (self.height - self.y(v)).max(r);
            let x = self.x(i) - w / 2.0;
            let _ = write!(
                o,
                "<path class=\"wk-bar{}\" d=\"M{x:.1},{base:.1}v{up:.1}q0,{r1:.1} {r:.1},{r1:.1}h{mid:.1}q{r:.1},0 {r:.1},{r:.1}v{down:.1}z\"/>",
                if b.partial { " wk-partial" } else { "" },
                base = self.height,
                up = -(h - r),
                r1 = -r,
                mid = w - 2.0 * r,
                down = h - r,
            );
        }
    }

    /// The crosshair and, for line charts, a dot per line, placed by the script.
    fn close(&self, o: &mut String, dots: &[&str]) {
        let _ = write!(
            o,
            "<line class=\"wk-x\" x1=\"0\" x2=\"0\" y1=\"{TOP}\" y2=\"{}\" display=\"none\"/>",
            self.height
        );
        for class in dots {
            let _ = write!(
                o,
                "<circle class=\"wk-dot {class}\" r=\"4\" cx=\"0\" cy=\"0\" display=\"none\"/>"
            );
        }
        o.push_str("</svg>");
    }

    /// Each line's y at each bucket, for the dots: `;` between lines.
    fn ys(&self, values: &[&dyn Fn(&Bucket) -> u64]) -> String {
        values
            .iter()
            .map(|f| {
                self.buckets
                    .iter()
                    .map(|b| format!("{:.1}", self.y(f(b))))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .collect::<Vec<_>>()
            .join(";")
    }
}

/// `Wed Nov 11, 3 PM to 6 PM ET`.
fn window(buckets: &[Bucket], i: usize) -> String {
    let b = &buckets[i];
    let end = buckets.get(i + 1).map_or_else(
        || {
            local(
                b.start + i64::from(BUCKET_HOURS) * 3600,
                b.utc_offset_minutes,
            )
        },
        when,
    );
    format!(
        "{} to {} ET",
        when(b).format("%a %b %-d, %-I %p"),
        end.format("%-I %p")
    )
}

fn row(o: &mut String, label: &str, value: &str, app: bool) {
    let _ = write!(
        o,
        "<div class=\"r{}\"><span>{}</span><span>{value}</span></div>",
        if app { " app" } else { "" },
        esc(label)
    );
}

/// A kind of job, by its app, in a word or two.
pub(super) fn app_label(app: &str) -> &'static str {
    match app {
        "usnm_ocr" => "Page OCR",
        "usnm_quality" => "Text comparison",
        "usnm_qwen" => "AI snippet reading",
        "usnm_gold" | "usnm_gold_native" => "Test jobs",
        "usnm_ndlocr" => "Japanese OCR",
        "usnm_surya" => "Retired engine trial",
        _ => "Other",
    }
}

/// Counts by kind of job, largest first: what [`by_app`] puts in a tip.
fn kinds(apps: &std::collections::BTreeMap<String, u64>) -> Vec<(&'static str, u64)> {
    let mut kinds: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
    for (app, n) in apps {
        *kinds.entry(app_label(app)).or_default() += n;
    }
    let mut kinds: Vec<_> = kinds.into_iter().filter(|(_, n)| *n > 0).collect();
    kinds.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    kinds
}

/// The same for the table: `AI snippet reading 1,700, Page OCR 5`.
fn in_words(apps: &std::collections::BTreeMap<String, u64>) -> String {
    kinds(apps)
        .iter()
        .map(|(name, n)| format!("{name} {}", int(*n)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn by_app(o: &mut String, apps: &std::collections::BTreeMap<String, u64>) {
    for (name, n) in kinds(apps) {
        row(o, name, &int(n), true);
    }
}

/// A bucket's tip: its window, then each chart's numbers, apps under their totals.
fn tip(buckets: &[Bucket], i: usize) -> String {
    let b = &buckets[i];
    let mut o = format!(
        "<b>{}{}</b>",
        esc(&window(buckets, i)),
        if b.partial { " (so far)" } else { "" }
    );
    row(&mut o, "Queued", &int(b.queued_total()), false);
    by_app(&mut o, &b.queued);
    row(&mut o, "Volunteers", &int(b.volunteers), false);
    row(
        &mut o,
        "Volunteer computers",
        &int(b.volunteer_hosts),
        false,
    );
    row(&mut o, "All computers", &int(b.hosts), false);
    row(&mut o, "Completed", &int(b.completed_total()), false);
    by_app(&mut o, &b.completed);
    o
}

/// The tips and the geometry the script needs, as JSON that can't end the
/// `<script>` element it sits in.
fn data(buckets: &[Bucket], step: f64) -> String {
    let tips: Vec<String> = (0..buckets.len()).map(|i| tip(buckets, i)).collect();
    serde_json::json!({"left": LEFT, "n": buckets.len(), "step": step, "tips": tips})
        .to_string()
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
}

/// The charts, their notes and the table, for a section of the overview.
pub(super) fn charts(v: &WeekView) -> String {
    let mut o = String::with_capacity(64 * 1024);
    o.push_str("<div class=\"boinc-week\">");
    let Some(w) = v.week.as_ref().filter(|w| !w.buckets.is_empty()) else {
        let _ = write!(
            o,
            "<p class=\"muted\">No counts from the BOINC server yet.</p>{}</div>",
            v.error
                .as_deref()
                .map_or_else(String::new, |e| format!("<p class=\"err\">{}</p>", esc(e)))
        );
        return o;
    };
    let b = &w.buckets;
    let last = &b[b.len() - 1];
    let generated = local(
        w.generated_at,
        w.generated_utc_offset_minutes
            .unwrap_or(last.utc_offset_minutes),
    );
    let _ = write!(
        o,
        "<p class=\"note\">The last 7 days in 3-hour steps, {} to {} ET. Counted {}.",
        when(&b[0]).format("%a %b %-d, %-I %p"),
        generated.format("%a %b %-d, %-I:%M %p"),
        v.generated_at.map_or_else(String::new, time),
    );
    if v.stale {
        o.push_str(" <span class=\"stale\">Not counted for over 20 minutes.</span>");
    }
    o.push_str("</p>");

    let max = |f: &dyn Fn(&Bucket) -> u64| b.iter().map(f).max().unwrap_or(0);
    let queued = |x: &Bucket| x.queued_total();
    let volunteers = |x: &Bucket| x.volunteers;
    let volunteer_hosts = |x: &Bucket| x.volunteer_hosts;
    let hosts = |x: &Bucket| x.hosts;
    let completed = |x: &Bucket| x.completed_total();

    o.push_str(
        "<div class=\"wk-charts table-wide\" tabindex=\"0\" role=\"region\" aria-label=\"BOINC week charts\">\
         <h3>Queued jobs</h3>\
         <p class=\"note\">Jobs waiting for a volunteer's computer</p>",
    );
    let c = Chart {
        buckets: b,
        height: 150.0,
        top: nice(max(&queued)),
        axis: false,
    };
    c.open(
        &mut o,
        "Queued jobs, the last 7 days in 3-hour steps",
        &c.ys(&[&queued]),
    );
    c.line(&mut o, "s1", queued);
    c.close(&mut o, &["s1"]);

    o.push_str(
        "<h3>Active volunteers and computers</h3><p class=\"note\">\
         <span class=\"key wk-key\"></span>Volunteers \
         <span class=\"key wk-key s2\"></span>Volunteer computers \
         <span class=\"key wk-key s3\"></span>All computers, the project's own included</p>",
    );
    let top = nice(max(&volunteers).max(max(&volunteer_hosts)).max(max(&hosts)));
    let c = Chart {
        buckets: b,
        height: 130.0,
        top,
        axis: false,
    };
    c.open(
        &mut o,
        "Active volunteers, volunteer computers and all computers, the last 7 days in 3-hour steps",
        &c.ys(&[&volunteers, &volunteer_hosts, &hosts]),
    );
    c.line(&mut o, "s1", volunteers);
    c.line(&mut o, "s2", volunteer_hosts);
    c.line(&mut o, "s3", hosts);
    c.close(&mut o, &["s1", "s2", "s3"]);

    o.push_str(
        "<h3>Completed jobs</h3><p class=\"note\">Jobs a computer returned successfully</p>",
    );
    let c = Chart {
        buckets: b,
        height: 150.0,
        top: nice(max(&completed)),
        axis: true,
    };
    c.open(
        &mut o,
        "Completed jobs, the last 7 days in 3-hour steps",
        "",
    );
    c.bars(&mut o, completed);
    c.close(&mut o, &[]);
    let _ = write!(
        o,
        "<script type=\"application/json\" class=\"wk-data\">{}</script></div>",
        data(b, c.step())
    );

    o.push_str(
        "<p class=\"note\">A volunteer or computer counts as active if it was sent a job or returned one \
         in those 3 hours. The project's own accounts and computers don't count as volunteers. The newest \
         3 hours are still filling, so their bar is faded and the figures above leave them out. Hover or \
         tap a chart for the numbers by kind of job.</p>",
    );

    o.push_str(
        "<details data-refresh-key=\"boinc-week-table\"><summary>Table view</summary>\
         <div class=\"scroll table-wide\" tabindex=\"0\" role=\"region\" aria-label=\"BOINC week table\">\
         <table><thead><tr><th>3-hour period (ET)</th><th class=\"r\">Queued</th><th class=\"r\">Volunteers</th>\
         <th class=\"r\">Volunteer computers</th><th class=\"r\">All computers</th><th class=\"r\">Completed</th>\
         <th>Queued, by kind</th><th>Completed, by kind</th></tr></thead><tbody>",
    );
    for (i, x) in b.iter().enumerate() {
        let _ = write!(
            o,
            "<tr><td>{}{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td>\
             <td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td><td>{}</td></tr>",
            esc(&window(b, i)),
            if x.partial { " (so far)" } else { "" },
            int(x.queued_total()),
            int(x.volunteers),
            int(x.volunteer_hosts),
            int(x.hosts),
            int(x.completed_total()),
            esc(&in_words(&x.queued)),
            esc(&in_words(&x.completed)),
        );
    }
    o.push_str("</tbody></table></div></details></div>");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn axis_tops_are_round() {
        assert_eq!(nice(0), 1);
        assert_eq!(nice(7), 10);
        assert_eq!(nice(9), 10);
        assert_eq!(nice(18), 20);
        assert_eq!(nice(21), 25);
        assert_eq!(nice(2023), 2500);
        assert_eq!(nice(5000), 5000);
        assert_eq!(nice(2), 2);
        assert_eq!(nice(3), 5);
        assert_eq!(ticks(1), [0, 1]);
        assert_eq!(ticks(2), [0, 1, 2]);
        assert_eq!(ticks(10), [0, 2, 4, 6, 8, 10]);
        assert_eq!(ticks(20), [0, 5, 10, 15, 20]);
        assert_eq!(ticks(25), [0, 5, 10, 15, 20, 25]);
        assert_eq!(ticks(2500), [0, 500, 1000, 1500, 2000, 2500]);
    }

    #[test]
    fn draws_three_charts_on_one_axis_with_tips_and_a_table() {
        let now = Utc::now();
        let doc = crate::boinc_week::tests::document(now);
        let week = crate::boinc_week::parse(&serde_json::to_vec(&doc).unwrap(), now).unwrap();
        let v = WeekView {
            generated_at: DateTime::from_timestamp(week.generated_at, 0),
            week: Some(week),
            ..WeekView::default()
        };
        let html = charts(&v);
        assert!(html.contains("class=\"boinc-week\""));
        assert_eq!(html.matches("<svg class=\"wk\"").count(), 3);
        assert_eq!(html.matches("class=\"wk-line").count(), 4);
        assert_eq!(html.matches("class=\"wk-bar").count(), 56);
        assert_eq!(html.matches("wk-bar wk-partial").count(), 1);
        assert_eq!(html.matches("<tr><td>").count(), 56);
        // The table has what the tips have, for keyboards and screen readers.
        assert!(html.contains("<td>AI snippet reading 1,700</td>"));
        // Day labels on the last chart only, one a day.
        assert!(html.matches("<text x=").count() >= 7);
        let data = html.split("class=\"wk-data\">").nth(1).unwrap();
        let data = data.split("</script>").next().unwrap();
        assert!(!data.contains('<'));
        let d: serde_json::Value = serde_json::from_str(data).unwrap();
        assert_eq!(d["n"], 56);
        let tip = d["tips"][55].as_str().unwrap();
        assert!(tip.contains("(so far)"));
        assert!(tip.contains("AI snippet reading"));
        assert!(!tip.contains("usnm_"));
        assert_eq!(app_label("usnm_surya"), "Retired engine trial");
        assert_eq!(app_label("usnm_new_thing"), "Other");
        assert!(tip.contains("All computers"));
        assert!(!html.contains("stale"));
        let none = charts(&WeekView::default());
        assert!(none.contains("No counts from the BOINC server yet"));
    }
}
