//! The step throughput panel: each step's pages an hour and a day, with a
//! week of history, and unknown shown as unknown (#704).

use super::fmt::*;
use crate::throughput::{StepRate, ThroughputView, BIN_HOURS};
use chrono::{DateTime, Utc};
use std::fmt::Write as _;

const BAR: f64 = 6.0;
const CHART_HEIGHT: f64 = 36.0;

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

/// One hover line, whose time the page's script puts in Eastern.
fn bar_title(start: Option<DateTime<Utc>>, i: usize, text: &str) -> String {
    let at = start.map_or_else(String::new, |t| {
        (t + chrono::Duration::hours(i as i64 * BIN_HOURS))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
    format!("<title data-at=\"{at}\">{}</title>", esc(text))
}

/// A week of bars for one count. A bin nobody observed is a pale band of
/// full height, so it can't be read as a quiet period; a zero is a flat tick.
fn chart(s: &StepRate, start: Option<DateTime<Utc>>) -> String {
    let top = s.bins.iter().flatten().fold(0.0_f64, |a, v| a.max(*v));
    let width = s.bins.len() as f64 * BAR;
    let mut o = format!(
        "<svg class=\"spark\" viewBox=\"0 0 {width} {CHART_HEIGHT}\" role=\"img\" aria-label=\"{}, last 7 days, at most {} {} in {BIN_HOURS} hours\">",
        esc(s.label),
        count(top),
        s.unit
    );
    for (i, v) in s.bins.iter().enumerate() {
        let x = i as f64 * BAR;
        let known = s.bin_known.get(i).copied().unwrap_or(0);
        match v {
            None => {
                let _ = write!(
                    o,
                    "<rect class=\"unk\" x=\"{x}\" y=\"0\" width=\"4\" height=\"{CHART_HEIGHT}\">{}</rect>",
                    bar_title(start, i, "unknown")
                );
            }
            Some(v) => {
                let h = if top > 0.0 && *v > 0.0 {
                    (v / top * (CHART_HEIGHT - 1.0)).max(2.0)
                } else {
                    1.0
                };
                let partial = if known < BIN_HOURS as u32 {
                    format!(" ({known} of {BIN_HOURS} hours known)")
                } else {
                    String::new()
                };
                let _ = write!(
                    o,
                    "<rect x=\"{x}\" y=\"{:.1}\" width=\"4\" height=\"{h:.1}\" rx=\"1\">{}</rect>",
                    CHART_HEIGHT - h,
                    bar_title(start, i, &format!("{} {}{partial}", count(*v), s.unit))
                );
            }
        }
    }
    o.push_str("</svg>");
    let _ = write!(
        o,
        " <span class=\"num nowrap\">peak {} per {BIN_HOURS} h</span>",
        count(top)
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
    let mut o = format!(
        "<svg class=\"spark\" viewBox=\"0 0 {} {CHART_HEIGHT}\" role=\"img\" aria-label=\"{}, valid and inconclusive results, last 7 days\">",
        valid.len() as f64 * BAR,
        esc(app)
    );
    let usable = CHART_HEIGHT - 3.0;
    for (i, good) in valid.iter().enumerate() {
        let x = i as f64 * BAR;
        let Some(good) = *good else {
            let _ = write!(
                o,
                "<rect class=\"unk\" x=\"{x}\" y=\"0\" width=\"4\" height=\"{CHART_HEIGHT}\">{}</rect>",
                bar_title(start, i, "unknown")
            );
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
        let hover = format!("{} valid, {} inconclusive", count(good), count(bad));
        if g == 0.0 && b == 0.0 {
            let _ = write!(
                o,
                "<rect x=\"{x}\" y=\"{}\" width=\"4\" height=\"1\">{}</rect>",
                CHART_HEIGHT - 1.0,
                bar_title(start, i, &hover)
            );
            continue;
        }
        if g > 0.0 {
            let _ = write!(
                o,
                "<rect x=\"{x}\" y=\"{:.1}\" width=\"4\" height=\"{g:.1}\" rx=\"1\">{}</rect>",
                CHART_HEIGHT - g,
                bar_title(start, i, &hover)
            );
        }
        if b > 0.0 {
            let gap = if g > 0.0 { 2.0 } else { 0.0 };
            let _ = write!(
                o,
                "<rect class=\"s2\" x=\"{x}\" y=\"{:.1}\" width=\"4\" height=\"{b:.1}\" rx=\"1\">{}</rect>",
                CHART_HEIGHT - g - gap - b,
                bar_title(start, i, &hover)
            );
        }
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

fn seconds_cell(s: &StepRate) -> String {
    let per = if s.id == "controller" { "tick" } else { "page" };
    match (s.seconds_per_page, s.id) {
        (_, "bridge_lock" | "usnm_quality") => "-".to_owned(),
        (Some(n), _) if n >= 100.0 => format!("{}&nbsp;s/{per}", int(n as u64)),
        (Some(n), _) => format!("{n:.1}&nbsp;s/{per}"),
        (None, _) => unknown(),
    }
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
        "<div class=\"table-wide\" tabindex=\"0\" role=\"region\" aria-label=\"Step throughput\">",
    );
    o.push_str(
        "<table class=\"throughput\"><thead><tr><th>Step</th><th class=\"r\">Last hour</th>\
         <th class=\"r\">Last 24 hours</th><th class=\"r\">Seconds</th><th>Limit</th>\
         <th>Last 7 days</th></tr></thead><tbody>",
    );
    for s in &v.steps {
        let _ = write!(
            o,
            "<tr><td><details data-refresh-key=\"throughput-{}\"><summary>{}</summary><p>{}</p></details></td>\
             <td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td>{}</td><td>{}</td></tr>",
            s.id,
            esc(s.label),
            esc(s.scope),
            hour_cell(s),
            day_cell(s),
            seconds_cell(s),
            s.limit.as_deref().map_or_else(|| "<span class=\"muted\">not known</span>".to_owned(), esc),
            chart(s, v.chart_start),
        );
    }
    o.push_str("</tbody></table></div><p class=\"note\">Bars are 4 hours each, oldest left, of full hours, so the last ones are short. \
        A flat tick is a counted zero; a pale full-height band is unknown. Each row has its own scale. Hover a bar for its count.</p>");

    o.push_str(
        "<h3>Results by validation outcome, last 24 hours</h3>\
         <p class=\"note\"><span class=\"key\"></span>valid <span class=\"key s2\"></span>inconclusive \
         &nbsp; Results are replicas, not pages (#1420). Pages are workunits with a canonical result. \
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
             <td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{}</td><td class=\"r\">{} / {}</td><td>{}</td></tr>",
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

    #[test]
    fn with_nothing_connected_every_rate_says_unknown_and_none_says_zero() {
        let html = panel(&empty());
        assert!(html.contains("id=\"step-throughput\""));
        assert!(html.contains("Scan fetch"));
        assert!(html.contains("usnm_qwen"));
        assert!(html.contains("Bridge lock held"));
        assert!(!html.contains(">0 pages<"));
        assert!(!html.contains(">0<"));
        assert!(html.contains("no phase counts yet"));
        assert!(html.contains("never reported"));
        // A chart of unknown bins draws unknown bands and no bars.
        assert_eq!(
            html.matches("class=\"unk\"").count(),
            42 * (empty().steps.len() + empty().quorum.len())
        );
        assert!(!html.contains("<rect x="));
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
        assert!(html.contains("0 pages (2 of 4 hours known)"));
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
        assert!(html.contains("class=\"s2\""));
        assert!(html.contains("156 valid, 409 inconclusive"));
        assert!(html.contains("class=\"key s2\""));
    }
}
