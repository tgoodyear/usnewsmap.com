//! The overview: six short sections, in this order: the collection, scans
//! being fetched, volunteer computing, results, accuracy and throughput.
//! Each has a number or two, a plain sentence and at most one small chart.
//! It is all the public page shows; the LAN page puts its operational
//! panels under it.

use std::fmt::Write as _;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc, Weekday};

use super::boinc_week::{nice, ticks};
use super::ecosystem::summary_stage;
use super::fmt::{esc, int};
use crate::boinc_week::Bucket;
use crate::throughput::{Counted, StepRate, DAY_HOURS};
use crate::view::View;

/// Chronicling America's size: about 24 million pages in 3,010 batches at
/// the damage audit's full pass of 11 October 2026
/// (`docs/design/12-onprem-ocr-pipeline.md`, Step 6, "Full pass").
const LOC_PAGES: u64 = 24_000_000;

/// What volunteers' computers do for each app, in plain words, as a count
/// of jobs: "1,234 {phrase}". Apps not listed are "other jobs".
const APP_WORK: &[(&str, &str)] = &[
    (
        "usnm_gold",
        "jobs testing a volunteer's OCR against known text",
    ),
    (
        "usnm_gold_native",
        "jobs testing a volunteer's OCR against known text",
    ),
    ("usnm_ndlocr", "jobs reading Japanese pages"),
    ("usnm_ocr", "jobs reading newspaper pages with OCR"),
    (
        "usnm_quality",
        "jobs scoring page quality by comparing a page's OCR texts",
    ),
    (
        "usnm_qwen",
        "jobs checking quality with an AI vision model that reads snippets of pages",
    ),
];

fn figure(o: &mut String, value: &str, label: &str) {
    let _ = write!(
        o,
        "<div class=\"fig\"><span class=\"n\">{value}</span><span class=\"l\">{}</span></div>",
        esc(label)
    );
}

fn open(o: &mut String, id: &str, heading: &str) {
    let _ = write!(
        o,
        "<section class=\"wide overview\" id=\"{id}\"><h2>{}</h2>",
        esc(heading)
    );
}

fn stage(v: &View, id: &str) -> Option<u64> {
    summary_stage(v, id).and_then(|s| s.count)
}

/// The data-flow catalog's window for its total stages, in hours: 14 days
/// (`LIVE_WINDOW_HOURS` in `scripts/corpus/data_catalog.py`).
const FORTNIGHT_HOURS: u64 = 336;

/// A total stage's 14-day count, if the catalog had every hour of the window.
/// The stage's detail says `Last 336 hours, N reported.`; with fewer than all
/// reported the count is an undercount, so it isn't shown as 14 days.
fn fortnight(v: &View, id: &str) -> Option<u64> {
    let s = summary_stage(v, id)?;
    let (window, reported) = reported_hours(&s.detail)?;
    (window == FORTNIGHT_HOURS && reported == window)
        .then_some(s.count)
        .flatten()
}

/// `(336, 330)` from `Last 336 hours, 330 reported. ...`.
fn reported_hours(detail: &str) -> Option<(u64, u64)> {
    let (window, rest) = detail.strip_prefix("Last ")?.split_once(" hours, ")?;
    let (reported, _) = rest.split_once(" reported.")?;
    Some((window.parse().ok()?, reported.parse().ok()?))
}

fn count_or(n: Option<u64>) -> String {
    n.map_or_else(|| "not known".to_owned(), int)
}

fn pct(part: u64, whole: u64) -> String {
    let p = part as f64 / whole as f64 * 100.0;
    if p >= 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.2}%")
    }
}

fn step<'a>(v: &'a View, id: &str) -> Option<&'a StepRate> {
    v.throughput.steps.iter().find(|s| s.id == id)
}

/// A step's last 7 days, one a day, oldest first, from whole hours.
fn days(s: &StepRate) -> Vec<Counted> {
    s.days.clone()
}

/// US Eastern wall time at `at`: daylight time from the second Sunday of
/// March at 2 AM to the first Sunday of November at 2 AM, standard otherwise.
fn eastern(at: DateTime<Utc>) -> NaiveDateTime {
    let year = at.year();
    let sunday = |month, nth| {
        NaiveDate::from_weekday_of_month_opt(year, month, Weekday::Sun, nth).unwrap_or_default()
    };
    // 2 AM standard time is 07:00 UTC; 2 AM daylight time is 06:00 UTC.
    let begins = sunday(3, 2)
        .and_hms_opt(7, 0, 0)
        .unwrap_or_default()
        .and_utc();
    let ends = sunday(11, 1)
        .and_hms_opt(6, 0, 0)
        .unwrap_or_default()
        .and_utc();
    let offset = if (begins..ends).contains(&at) { -4 } else { -5 };
    (at + Duration::hours(offset)).naive_utc()
}

/// The note above a daily chart: what it counts, and where its days end.
fn daily_note(o: &mut String, what: &str, values: usize, start: Option<DateTime<Utc>>) {
    let _ = write!(o, "<p class=\"note\">{}, last {values} days. ", esc(what));
    let ends = |i: usize| {
        start.map(|s| {
            eastern(s + Duration::days(i as i64))
                .format("%-I %p")
                .to_string()
        })
    };
    match ends(values) {
        // Bars are fixed 24-hour intervals, so across a clock change their
        // Eastern end times differ.
        Some(end) if (1..=values).all(|i| ends(i).as_deref() == Some(end.as_str())) => {
            let _ = write!(
                o,
                "Each bar is the 24 hours up to {end} Eastern on the day shown, the newest on the right.</p>"
            );
        }
        Some(end) => {
            let _ = write!(
                o,
                "Each bar is 24 hours, the newest on the right; the newest ends at {end} Eastern on the day shown, and a clock change moves the others by an hour.</p>"
            );
        }
        None => o.push_str("Each bar is 24 hours, the newest on the right.</p>"),
    }
}

/// Seven bars, a day each, ending at the last full hour, on a scale with
/// the day each ends on under it. Their tips come from the page's script,
/// in Eastern time, like the step charts'.
fn daily_bars(
    o: &mut String,
    key: &str,
    label: &str,
    unit: &str,
    values: &[Counted],
    start: Option<DateTime<Utc>>,
) {
    const TOP: f64 = 8.0;
    // The plot's height, and room under it for the day names.
    const H: f64 = 100.0;
    const AXIS: f64 = 22.0;
    // Pixels one character of an axis label takes, about, at 11px.
    const CHAR: f64 = 6.2;
    let top = values
        .iter()
        .filter_map(|d| d.value)
        .fold(0.0_f64, |a, v| a.max(v));
    let scale = nice(top.ceil() as u64);
    let room = (int(scale).len() as f64 * CHAR + 12.0).ceil();
    let at = start.map_or_else(String::new, |t| {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
    // Each chart is drawn at both widths, like the week charts, with the
    // same left gutter and right edge, and the stylesheet shows one.
    for (class, units, edge, gutter, fill) in [
        ("wide", 1040.0, 12.0, 56.0_f64, 0.5),
        ("narrow", 360.0, 8.0, 52.0, 0.7),
    ] {
        let left = gutter.max(room);
        let slot = (units - left - edge) / values.len().max(1) as f64;
        let width = slot * fill;
        let y = |n: f64| TOP + (H - TOP) * (1.0 - n / scale as f64);
        let _ = write!(
        o,
        "<svg class=\"spark daily {class}\" viewBox=\"0 0 {units} {}\" role=\"img\" tabindex=\"0\" data-chart=\"{key}-{class}\" \
         data-start=\"{at}\" data-hours=\"24\" aria-label=\"{}. Left and right arrow keys step through the days.\">",
        H + AXIS,
        esc(label)
    );
        for v in ticks(scale) {
            let _ = write!(
                o,
                "<line class=\"{}\" x1=\"{left}\" x2=\"{}\" y1=\"{:.1}\" y2=\"{:.1}\"/>\
             <text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{}</text>",
                if v == 0 { "wk-axis" } else { "wk-grid" },
                units - edge,
                y(v as f64),
                y(v as f64),
                left - 6.0,
                y(v as f64) + 4.0,
                int(v)
            );
        }
        for (i, v) in values.iter().enumerate() {
            let from = start.map(|s| s + Duration::days(i as i64));
            let window = from.map_or_else(String::new, |f| {
                format!(
                    "{} to {} UTC: ",
                    f.format("%b %-d %H:%M"),
                    (f + Duration::days(1)).format("%b %-d %H:%M")
                )
            });
            let x = left + i as f64 * slot;
            let (what, drawn) = match v.value {
            None => (
                "unknown".to_owned(),
                format!(
                    "<rect class=\"unk\" x=\"{:.1}\" y=\"{TOP}\" width=\"{width:.1}\" height=\"{:.1}\"/>",
                    x + (slot - width) / 2.0,
                    H - TOP
                ),
            ),
            Some(n) => {
                let h = if n > 0.0 {
                    (H - y(n)).max(2.0)
                } else {
                    0.0
                };
                let partial = v.hours_known < DAY_HOURS as u32;
                (
                    if partial {
                        format!(
                            "{} {unit} in the {} of 24 hours counted",
                            int(n.round() as u64),
                            v.hours_known
                        )
                    } else {
                        format!("{} {unit}", int(n.round() as u64))
                    },
                    if h > 0.0 {
                        format!(
                            "<rect class=\"v{}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{width:.1}\" height=\"{h:.1}\"/>",
                            if partial { " part" } else { "" },
                            x + (slot - width) / 2.0,
                            H - h
                        )
                    } else {
                        String::new()
                    },
                )
            }
        };
            // The day it ends on, in Eastern, centred under the bar.
            let name = from.map_or_else(String::new, |f| {
                format!(
                    "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>",
                    x + slot / 2.0,
                    H + 15.0,
                    eastern(f + Duration::days(1)).format("%b %-d")
                )
            });
            let _ = write!(
            o,
            "<g class=\"bar\"><title>{}{}</title><rect class=\"hit\" x=\"{x:.1}\" y=\"0\" width=\"{slot:.1}\" height=\"{H}\"/>{drawn}</g>{name}",
            esc(&window),
            esc(&what)
        );
        }
        o.push_str("</svg>");
    }
}

fn collection(v: &View, o: &mut String) {
    open(o, "collection", "The collection");
    let held = v.fetch.total.pages;
    let ours = stage(v, "indexed");
    o.push_str("<div class=\"figures\">");
    figure(o, &int(held), "page scans fetched so far");
    figure(o, &count_or(ours), "pages where search uses our OCR text");
    o.push_str("</div>");
    warming_note(v, o);
    let _ = write!(
        o,
        "<p>Chronicling America, the Library of Congress's collection of digitized newspapers, has about \
         24 million pages, and usnewsmap.com searches all of them. We have fetched the original scans of {} of \
         them, and our own OCR (text read from the scan by software) has replaced the Library's text on {}.",
        pct(held, LOC_PAGES),
        ours.map_or_else(|| "a number of pages not known right now".to_owned(), |n| pct(n, LOC_PAGES)),
    );
    if let Some(b) = v
        .fetch
        .bulk
        .value
        .as_ref()
        .filter(|b| b.state == "done" && b.done > 0 && b.done == b.archives)
    {
        let _ = write!(
            o,
            " We also have the Library's own OCR text for every batch ({} archives).",
            int(b.archives)
        );
    }
    o.push_str("</p></section>");
}

/// Pages queued but not fetched, by what they were picked for.
fn left_by_kind(v: &View) -> (u64, u64) {
    let mut ocr = 0;
    let mut quality = 0;
    for m in &v.fetch.queue {
        if !matches!(m.state, "queued" | "fetching" | "cooling") {
            continue;
        }
        let left = m.pages.saturating_sub(m.fetched);
        if m.kind == "quality" {
            quality += left;
        } else {
            ocr += left;
        }
    }
    (ocr, quality)
}

/// How the fetcher is holding back for the Library's site.
#[derive(Debug, PartialEq)]
enum Hold {
    Until(DateTime<Utc>),
    /// Slowed with no end set: until the pacer is reset.
    Indefinite,
}

fn hold(v: &View) -> Option<Hold> {
    let p = v.fetch.pacer.value.as_ref()?;
    let slowed = p.slowed.as_ref().filter(|s| s.is_object());
    let slowed_until = slowed
        .and_then(|s| s.get("until_ms"))
        .and_then(serde_json::Value::as_u64);
    let until = p.hold_until_ms.max(slowed_until.unwrap_or(0));
    let at = i64::try_from(until)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .filter(|at| *at > v.generated_at);
    match (at, slowed.is_some() && slowed_until.is_none()) {
        (_, true) => Some(Hold::Indefinite),
        (Some(at), false) => Some(Hold::Until(at)),
        (None, false) => None,
    }
}

/// While the dashboard reads the fetcher's files after a start, its counts
/// grow from zero.
fn warming_note(v: &View, o: &mut String) {
    if v.warming.is_some() {
        o.push_str(
            "<p class=\"stale\">The dashboard has just restarted and is still reading the fetcher's \
             files, so these counts are partial for a few minutes.</p>",
        );
    } else if v.fetch.index_error.is_some() {
        // Not the error itself: it names paths.
        o.push_str(
            "<p class=\"stale\">The fetcher's records can't be read right now, so these counts may \
             be out of date.</p>",
        );
    }
}

fn fetching(v: &View, o: &mut String) {
    open(o, "fetching", "Scans being fetched");
    let f = &v.fetch;
    let left =
        (f.queue_configured && f.queue_error.is_none()).then_some(f.queue_summary.pages_left);
    o.push_str("<div class=\"figures\">");
    figure(
        o,
        &int(f.last_day.pages),
        "pages fetched in the last 24 hours",
    );
    figure(o, &count_or(left), "pages still to fetch");
    o.push_str("</div>");
    warming_note(v, o);
    o.push_str("<p>");
    let now = match f.activity {
        "running" => format!(
            "We are fetching page scans from the Library of Congress now, about {} pages an hour.",
            int(f.last_hour.pages)
        ),
        "queued" => "More page scans from the Library of Congress are queued, waiting to be fetched.".to_owned(),
        "idle" if left == Some(0) => "No page scans from the Library of Congress are queued right now.".to_owned(),
        "idle" if left.is_some() => "The page scans still to fetch from the Library of Congress are waiting to be retried after failed attempts.".to_owned(),
        _ => "We can't tell right now whether page scans are being fetched from the Library of Congress.".to_owned(),
    };
    o.push_str(&esc(&now));
    match hold(v) {
        Some(Hold::Until(until)) => {
            let _ = write!(
                o,
                " The Library's site has been limiting our requests, so fetching is slowed or paused until {}.",
                super::fmt::time(until)
            );
        }
        Some(Hold::Indefinite) => o.push_str(
            " The Library's site has been limiting our requests, so fetching is slowed until further notice.",
        ),
        None => {}
    }
    let (ocr, quality) = left_by_kind(v);
    match (ocr, quality) {
        (0, 0) => {}
        (0, _) => o.push_str(" All of them are for the quality checks."),
        (_, 0) => o.push_str(" All of them are for our OCR."),
        _ => {
            let _ = write!(
                o,
                " Still to fetch: {} pages for our OCR and {} for the quality checks.",
                int(ocr),
                int(quality)
            );
        }
    }
    if let (Some(left), true) = (left, f.last_day.pages > 0) {
        let days = left.div_ceil(f.last_day.pages);
        let _ = write!(
            o,
            " At the last day's pace, that takes about {} days.",
            int(days)
        );
    }
    o.push_str("</p>");
    if let Some(s) = step(v, "scan_fetch") {
        let daily = days(s);
        daily_note(
            o,
            "Pages fetched a day",
            daily.len(),
            v.throughput.days_start,
        );
        daily_bars(
            o,
            "fetch-days",
            "Pages fetched a day, last 7 days",
            "pages",
            &daily,
            v.throughput.days_start,
        );
    }
    o.push_str("</section>");
}

/// What `n` jobs of `app` were, in words.
fn work(app: &str) -> &'static str {
    APP_WORK
        .iter()
        .find(|(a, _)| *a == app)
        .map_or("other jobs", |(_, w)| *w)
}

fn volunteers(v: &View, o: &mut String) {
    open(o, "volunteers", "Volunteer computing");
    let week = v.boinc_week.week.as_ref().filter(|w| !w.buckets.is_empty());
    if let Some(w) = week {
        // The last eight full buckets: the last 24 hours to the last boundary.
        let full: Vec<&Bucket> = w.buckets.iter().filter(|b| !b.partial).collect();
        let day = &full[full.len().saturating_sub(8)..];
        let last = full.last().copied().or(w.buckets.last());
        let done: u64 = day.iter().map(|b| b.completed_total()).sum();
        o.push_str("<div class=\"figures\">");
        if let Some(b) = last {
            figure(
                o,
                &int(b.volunteers),
                "volunteers active in the last 3 hours",
            );
        }
        figure(o, &int(done), "jobs finished in the last 24 hours");
        o.push_str("</div>");
        let mut mix: std::collections::BTreeMap<&str, u64> = std::collections::BTreeMap::new();
        for b in day {
            for (app, n) in &b.completed {
                *mix.entry(work(app)).or_default() += n;
            }
        }
        let mut mix: Vec<_> = mix.into_iter().filter(|(_, n)| *n > 0).collect();
        mix.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        o.push_str(
            "<p>People lend their computers through BOINC to read and check newspaper pages.",
        );
        if !mix.is_empty() {
            let parts: Vec<String> = mix
                .iter()
                .map(|(w, n)| format!("{} {w}", int(*n)))
                .collect();
            let _ = write!(o, " In the last 24 hours: {}.", esc(&join(&parts)));
        }
        o.push_str("</p>");
    } else {
        o.push_str(
            "<p>People lend their computers through BOINC to read and check newspaper pages.</p>",
        );
    }
    o.push_str(&super::boinc_week::charts(&v.boinc_week));
    o.push_str("</section>");
}

/// `a, b and c`.
fn join(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn results(v: &View, o: &mut String) {
    open(o, "results", "Results");
    // The data-flow snapshot's 14-day totals: the catalog adds up the same
    // hourly counts as the throughput section, whole hours, and refreshes
    // them hourly. A total the snapshot doesn't carry fresh, or has not every
    // hour of, reads "not known". The collect step isn't shown: it adds up
    // the controller's finished ticks and runs below what is imported
    // (`docs/operations/onprem-ocr.md`).
    let imported = fortnight(v, "imported");
    let searchable = stage(v, "indexed");
    let scored = fortnight(v, "quality_loaded");
    o.push_str("<ol class=\"flow\">");
    for (n, label) in [
        (
            imported,
            "OCR pages imported into our text collection, last 14 days",
        ),
        (searchable, "pages where search uses our OCR text, so far"),
    ] {
        let _ = write!(
            o,
            "<li><span class=\"n\">{}</span><span class=\"l\">{}</span></li>",
            count_or(n),
            esc(label)
        );
    }
    o.push_str("</ol>");
    let _ = write!(
        o,
        "<p>Volunteers' OCR comes back to our server, where each page is checked before it is imported. \
         The second number is a running total, so it also counts pages imported earlier. An automatic \
         check scores our text for each page against the Library's. Search uses ours when it scores \
         clearly better, so not every imported page is used. In the same 14 days the two kinds of \
         quality check, comparing texts and the AI vision model, loaded results for {} pages between \
         them (a page both read counts twice); the scores let us rank pages for OCR by how poor the \
         Library's text is.</p>",
        count_or(scored)
    );
    o.push_str("</section>");
}

/// The accuracy figures are measured, not computed here: `docs/reports/
/// 2026-10-boinc-quality-pilot/report.md` §4.1 (the 345 clean Chronicling
/// America snippets, Tesseract 5.5 reading each on its own, 3.68%; the
/// Library's ALTO, 9.22%) and `docs/ocr/gold-set.md` (the gold bundle's
/// version 1 baseline, 142 snippets, 11,336 edits in 125,622 characters).
fn accuracy(o: &mut String) {
    open(o, "accuracy", "Accuracy");
    o.push_str("<div class=\"figures\">");
    figure(
        o,
        "3.7%",
        "characters wrong in Tesseract, the engine our OCR runs, on 345 Chronicling America snippets",
    );
    figure(
        o,
        "9.2%",
        "characters wrong in the Library's OCR, on the same 345 snippets",
    );
    o.push_str("</div>");
    o.push_str(
        "<p>The snippets are lines and passages cut from 18 Chronicling America pages, compared with \
         people's transcriptions from a public dataset. Tesseract read each snippet on its own; our OCR \
         reads whole pages, so it can also make layout mistakes this test leaves out. A separate test of \
         our OCR worker used 142 snippets from six datasets of historical newspapers in English, French, \
         Finnish, Swedish and German. It got 9.0% of characters wrong. Most of that text is English from \
         British newspapers, so the result can't be compared with the two figures above. We have no human \
         transcriptions for other languages, such as Czech, Serbian, Polish, Russian, Yiddish and \
         Japanese, so we haven't measured our OCR's accuracy on them.</p></section>",
    );
}

fn throughput(v: &View, o: &mut String) {
    open(o, "throughput", "Pages per day");
    let imported = step(v, "ocr_import");
    let daily = imported.map(days).unwrap_or_default();
    // Only days with every hour counted: a gap isn't a slow day.
    let whole: Vec<f64> = daily
        .iter()
        .filter(|d| d.hours_known == DAY_HOURS as u32)
        .filter_map(|d| d.value)
        .collect();
    let average = (!whole.is_empty()).then(|| whole.iter().sum::<f64>() / whole.len() as f64);
    o.push_str("<div class=\"figures\">");
    figure(
        o,
        &average.map_or_else(|| "not known".to_owned(), |a| int(a.round() as u64)),
        &if whole.len() == daily.len() || whole.is_empty() {
            "OCR pages imported a day, 7-day average".to_owned()
        } else {
            format!(
                "OCR pages imported a day, average of the {} fully counted days of the last 7",
                whole.len()
            )
        },
    );
    o.push_str(
        "</div><p>A page goes from scan to imported text; the number above counts the last step, \
         just before search.</p>",
    );
    if imported.is_some() {
        daily_note(
            o,
            "OCR pages imported a day",
            daily.len(),
            v.throughput.days_start,
        );
        daily_bars(
            o,
            "import-days",
            "OCR pages imported a day, last 7 days",
            "pages",
            &daily,
            v.throughput.days_start,
        );
    }
    o.push_str("<table class=\"rates\"><thead><tr><th>Last 24 hours</th><th class=\"r\">Pages</th></tr></thead><tbody>");
    for (id, label) in [
        ("scan_fetch", "Scans fetched"),
        ("ocr_import", "OCR pages imported"),
        ("quality_load", "Pages scored by the quality checks"),
    ] {
        // The scan fetch's own 24 hours, as in its section, not full hours;
        // a step's only if all 24 were counted.
        let n = if id == "scan_fetch" {
            Some(v.fetch.last_day.pages as f64)
        } else {
            step(v, id)
                .filter(|s| s.day_hours_known == DAY_HOURS as u32)
                .and_then(|s| s.last_day)
        };
        let _ = write!(
            o,
            "<tr><td>{}</td><td class=\"r\">{}</td></tr>",
            esc(label),
            n.map_or_else(|| "not known".to_owned(), |n| int(n.round() as u64))
        );
    }
    o.push_str("</tbody></table></section>");
}

pub(super) fn sections(v: &View) -> String {
    let mut o = String::with_capacity(64 * 1024);
    collection(v, &mut o);
    fetching(v, &mut o);
    volunteers(v, &mut o);
    results(v, &mut o);
    accuracy(&mut o);
    throughput(v, &mut o);
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_read_as_a_sentence() {
        assert_eq!(join(&[]), "");
        assert_eq!(join(&["a".into()]), "a");
        assert_eq!(join(&["a".into(), "b".into(), "c".into()]), "a, b and c");
    }

    #[test]
    fn a_slowdown_with_no_end_is_said_so() {
        let mut v = super::super::tests::summary_view();
        let now = v.generated_at;
        let later = (now + Duration::hours(2)).timestamp_millis() as u64;
        let pacer = |slowed: Option<serde_json::Value>, hold_until_ms: u64| crate::view::FileView {
            configured: true,
            value: Some(crate::fetch::PacerState {
                slowed,
                hold_until_ms,
                ..Default::default()
            }),
            error: None,
            modified: None,
        };
        v.fetch.pacer = pacer(None, 0);
        assert_eq!(hold(&v), None);
        v.fetch.pacer = pacer(None, later);
        assert!(matches!(hold(&v), Some(Hold::Until(_))));
        v.fetch.pacer = pacer(Some(serde_json::json!({"at": "x", "until_ms": null})), 0);
        assert_eq!(hold(&v), Some(Hold::Indefinite));
        v.fetch.pacer = pacer(Some(serde_json::json!({"at": "x", "until_ms": 1})), 0);
        assert_eq!(hold(&v), None);
    }

    #[test]
    fn a_day_with_hours_missing_is_marked_and_left_out_of_the_average() {
        let mut v = super::super::tests::summary_view();
        let whole = Counted {
            value: Some(240.0),
            hours_known: 24,
        };
        let part = Counted {
            value: Some(10.0),
            hours_known: 3,
        };
        v.throughput.steps = vec![StepRate {
            id: "ocr_import",
            days: vec![whole, whole, whole, whole, whole, whole, part],
            ..StepRate::default()
        }];
        let mut o = String::new();
        throughput(&v, &mut o);
        assert!(o.contains(">240<"), "{o}");
        assert!(o.contains("average of the 6 fully counted days"));
        assert!(o.contains("in the 3 of 24 hours counted"));
        assert!(o.contains("class=\"v part\""));
    }

    #[test]
    fn eastern_time_follows_the_united_states_rule() {
        let at = |s: &str| {
            eastern(DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc))
                .format("%Y-%m-%d %H:%M")
                .to_string()
        };
        // Standard time in winter, daylight time in summer.
        assert_eq!(at("2026-11-12T00:00:00Z"), "2026-11-11 19:00");
        assert_eq!(at("2026-07-04T16:00:00Z"), "2026-07-04 12:00");
        // 2026: daylight time began on 8 March at 2 AM and ended on 1 November at 2 AM.
        assert_eq!(at("2026-03-08T06:59:59Z"), "2026-03-08 01:59");
        assert_eq!(at("2026-03-08T07:00:00Z"), "2026-03-08 03:00");
        assert_eq!(at("2026-11-01T05:59:59Z"), "2026-11-01 01:59");
        assert_eq!(at("2026-11-01T06:00:00Z"), "2026-11-01 01:00");
    }

    #[test]
    fn a_daily_chart_has_a_scale_and_the_day_each_bar_ends_on() {
        let start = DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let day = |n: f64| Counted {
            value: Some(n),
            hours_known: 24,
        };
        let values = [
            day(30_000.0),
            day(0.0),
            day(37_387.0),
            Counted {
                value: None,
                hours_known: 0,
            },
            day(12_000.0),
            day(500.0),
            Counted {
                value: Some(10.0),
                hours_known: 3,
            },
        ];
        let mut o = String::new();
        daily_note(&mut o, "Pages fetched a day", values.len(), Some(start));
        assert!(
            o.contains("last 7 days. Each bar is the 24 hours up to 7 PM Eastern on the day shown"),
            "{o}"
        );
        // Across a clock change the bars end at different Eastern hours.
        let mut o = String::new();
        let change = DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        daily_note(&mut o, "Pages fetched a day", values.len(), Some(change));
        assert!(o.contains("the newest ends at 7 PM Eastern"), "{o}");
        assert!(o.contains("clock change"), "{o}");
        let mut o = String::new();
        daily_bars(&mut o, "k", "Pages", "pages", &values, Some(start));
        // The scale: a round top and gridlines under it, on the axis's labels.
        for v in ["0", "10,000", "20,000", "30,000", "40,000", "50,000"] {
            assert!(o.contains(&format!(">{v}</text>")), "{v}: {o}");
        }
        assert!(o.contains("class=\"wk-axis\""));
        // A day's name is the day its 24 hours end on, in Eastern: the first
        // ends at 7 PM on 5 November (EST), the last at 7 PM on 11 November.
        for name in [
            "Nov 5", "Nov 6", "Nov 7", "Nov 8", "Nov 9", "Nov 10", "Nov 11",
        ] {
            assert!(o.contains(&format!(">{name}</text>")), "{name}: {o}");
        }
        // Tips, the arrow keys and the unknown and partial days are as before.
        assert_eq!(o.matches("<g class=\"bar\">").count(), 14);
        assert_eq!(o.matches("class=\"unk\"").count(), 2);
        assert_eq!(o.matches("class=\"v part\"").count(), 2);
        // Each width has its own key, so a refresh restores focus to the shown one.
        assert!(o.contains("data-chart=\"k-wide\"") && o.contains("data-chart=\"k-narrow\""));
        assert!(o.contains("data-start=\"2026-11-05T00:00:00Z\""));
        assert!(o.contains("Nov 5 00:00 to Nov 6 00:00 UTC: 30,000 pages"));
        // A day with none draws no bar; the scale still reaches the biggest.
        assert_eq!(o.matches("class=\"v\"").count(), 8);
        // Nothing is drawn past the chart's own width.
        assert!(o.contains("viewBox=\"0 0 360 ") && o.contains("viewBox=\"0 0 1040 "));
    }

    /// A view whose snapshot carries the catalog's 14-day total stages, each
    /// as the catalog words it, with `reported` of the 336 hours.
    fn view_with_totals(reported: u64) -> View {
        let mut v = super::super::tests::summary_view();
        let detail = |text: &str| format!("Last 336 hours, {reported} reported. {text}");
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"imported","label":"OCR pages in committed imports, last 14 days","count":14015,
             "state":"observed","detail":detail("From ocr-sync's log."),"max_age_secs":10800},
            // The tick-based count runs below the import's and isn't shown.
            {"id":"collected","label":"OCR pages collected through hand-off checks, last 14 days","count":9374,
             "state":"observed","detail":detail("Jobs the hand-off marked published."),"max_age_secs":10800},
            {"id":"quality_loaded","label":"Quality and Qwen pages loaded, last 14 days","count":553466,
             "state":"observed","detail":detail("Pages of finished units loaded."),"max_age_secs":10800},
            {"id":"indexed","label":"Indexed","count":21290,"state":"observed",
             "detail":"Serving index pages: pages whose searchable text is our OCR."}
        ]);
        let snapshot =
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap();
        v.pipeline.stage_freshness = snapshot
            .stages
            .iter()
            .map(|s| crate::pipeline::StageFreshness {
                id: s.id.clone(),
                age_secs: Some(5400),
                stale: false,
                historical: false,
            })
            .collect();
        v.pipeline.snapshot = Some(snapshot);
        v
    }

    #[test]
    fn results_are_the_catalogs_14_day_totals_and_say_why_the_two_numbers_differ() {
        // The 7-day step counts differ; Results must not use them.
        let mut v7 = view_with_totals(336);
        let hours = crate::throughput::CHART_HOURS as u32;
        v7.throughput.steps = vec![StepRate {
            id: "ocr_import",
            detail: crate::throughput::StepDetail {
                last_week: Counted {
                    value: Some(7265.0),
                    hours_known: hours,
                },
                ..Default::default()
            },
            ..StepRate::default()
        }];
        let mut o = String::new();
        results(&v7, &mut o);
        assert!(o.contains(">14,015<"), "{o}");
        assert!(!o.contains("7,265"), "{o}");
        assert!(o.contains("OCR pages imported into our text collection, last 14 days"));
        assert!(!o.contains("last 7 days") && !o.contains("same 7 days"));
        assert!(o.contains(">21,290<") && o.contains("so far"));
        assert!(o.contains("running total"));
        // Quality and Qwen loads together, said as two kinds of check.
        assert!(o.contains("In the same 14 days"), "{o}");
        assert!(
            o.contains("loaded results for 553,466 pages between them"),
            "{o}"
        );
        assert!(o.contains("a page both read counts twice"));
        // The controller-tick count is not shown.
        assert!(!o.contains("9,374") && !o.contains("returned and checked"));
        let mut o = String::new();
        throughput(&v7, &mut o);
        assert!(!o.contains("returned and checked"));
        // The same through the public page's redaction.
        let mut public = view_with_totals(336);
        crate::public::redact(&mut public);
        let mut o = String::new();
        results(&public, &mut o);
        assert!(o.contains(">14,015<") && o.contains("553,466"), "{o}");
    }

    #[test]
    fn results_say_not_known_for_a_stale_stage_or_one_missing_hours() {
        // Every hour but one: an undercount, not 14 days.
        let mut o = String::new();
        results(&view_with_totals(335), &mut o);
        assert!(!o.contains("14,015") && !o.contains("553,466"), "{o}");
        assert_eq!(o.matches("not known").count(), 2, "{o}");
        assert!(o.contains(">21,290<"), "{o}");
        // A stage the dashboard calls stale: the snapshot's old count stays out.
        let mut v = view_with_totals(336);
        v.pipeline
            .stage_freshness
            .iter_mut()
            .filter(|f| f.id != "indexed")
            .for_each(|f| f.stale = true);
        let mut o = String::new();
        results(&v, &mut o);
        assert!(!o.contains("14,015") && !o.contains("553,466"), "{o}");
        assert_eq!(o.matches("not known").count(), 2, "{o}");
        // No snapshot at all.
        let mut v = super::super::tests::summary_view();
        v.pipeline.snapshot = None;
        let mut o = String::new();
        results(&v, &mut o);
        assert_eq!(o.matches("not known").count(), 3, "{o}");
    }

    #[test]
    fn reported_hours_read_the_catalogs_detail() {
        assert_eq!(
            reported_hours("Last 336 hours, 330 reported. From ocr-sync's log."),
            Some((336, 330))
        );
        for bad in [
            "",
            "Last 336 hours.",
            "Last x hours, 3 reported.",
            "Last 336 hours, 3 seen.",
            "Newest hour only.",
        ] {
            assert_eq!(reported_hours(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_accuracy_pair_is_one_set_of_snippets_and_the_gold_set_is_context() {
        let mut o = String::new();
        accuracy(&mut o);
        // The headline: the same 345 snippets for both.
        assert!(o.contains(">3.7%<") && o.contains(">9.2%<"));
        assert!(o.contains("on 345 Chronicling America snippets"));
        assert!(o.contains("on the same 345 snippets"));
        // The gold set is in the text, not beside the pair.
        assert!(!o.contains(">9.0%<"));
        assert!(o.contains("142 snippets") && o.contains("It got 9.0% of characters wrong"));
        // Snippets, not whole pages, and the languages with no ground truth.
        assert!(o.contains("reads whole pages"));
        assert!(o.contains("haven't measured"));
        for lang in [
            "Czech", "Serbian", "Polish", "Russian", "Yiddish", "Japanese",
        ] {
            assert!(o.contains(lang), "{lang}");
        }
    }

    #[test]
    fn every_live_app_has_words() {
        for app in [
            "usnm_ocr",
            "usnm_quality",
            "usnm_qwen",
            "usnm_gold",
            "usnm_gold_native",
            "usnm_ndlocr",
        ] {
            assert_ne!(work(app), "other jobs", "{app}");
        }
        assert_eq!(work("usnm_surya"), "other jobs");
    }
}
