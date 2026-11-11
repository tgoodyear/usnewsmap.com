//! The overview: six short sections, in this order: the collection, scans
//! being fetched, volunteer computing, results, accuracy and throughput.
//! Each has a number or two, a plain sentence and at most one small chart.
//! It is all the public page shows; the LAN page puts its operational
//! panels under it.

use std::fmt::Write as _;

use chrono::{DateTime, Duration, Utc};

use super::ecosystem::summary_stage;
use super::fmt::{esc, int};
use crate::boinc_week::Bucket;
use crate::throughput::StepRate;
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

/// 7 days of 4-hour bins, as days.
const BINS_PER_DAY: usize = 6;

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

/// A step's count over the last 7 days, if every hour of it was observed.
fn week(v: &View, id: &str) -> Option<u64> {
    let w = step(v, id)?.detail.last_week;
    (w.hours_known as i64 == crate::throughput::CHART_HOURS)
        .then_some(w.value?)
        .map(|n| n.round() as u64)
}

fn step<'a>(v: &'a View, id: &str) -> Option<&'a StepRate> {
    v.throughput.steps.iter().find(|s| s.id == id)
}

/// A step's last 7 days, one value a day, oldest first; `None` for a day
/// nobody observed.
fn days(s: &StepRate) -> Vec<Option<f64>> {
    s.bins
        .chunks(BINS_PER_DAY)
        .map(|c| {
            c.iter()
                .flatten()
                .copied()
                .reduce(|a, b| a + b)
                .filter(|_| c.iter().any(Option::is_some))
        })
        .collect()
}

/// Seven bars, a day each, ending at the last full hour. Their tips come
/// from the page's script, in Eastern time, like the step charts'.
fn daily_bars(
    o: &mut String,
    key: &str,
    label: &str,
    unit: &str,
    values: &[Option<f64>],
    start: Option<DateTime<Utc>>,
) {
    const H: f64 = 72.0;
    const SLOT: f64 = 40.0;
    let top = values.iter().flatten().fold(0.0_f64, |a, v| a.max(*v));
    let at = start.map_or_else(String::new, |t| {
        t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });
    let _ = write!(
        o,
        "<svg class=\"spark daily\" viewBox=\"0 0 {} {H}\" role=\"img\" tabindex=\"0\" data-chart=\"{key}\" \
         data-start=\"{at}\" data-hours=\"24\" aria-label=\"{}. Left and right arrow keys step through the days.\">",
        values.len() as f64 * SLOT,
        esc(label)
    );
    for (i, v) in values.iter().enumerate() {
        let from = start.map(|s| s + Duration::days(i as i64));
        let window = from.map_or_else(String::new, |f| {
            format!(
                "{} to {} UTC: ",
                f.format("%b %-d %H:%M"),
                (f + Duration::days(1)).format("%b %-d %H:%M")
            )
        });
        let x = i as f64 * SLOT;
        let (what, drawn) = match v {
            None => (
                "unknown".to_owned(),
                format!(
                    "<rect class=\"unk\" x=\"{x}\" y=\"0\" width=\"{}\" height=\"{H}\"/>",
                    SLOT * 0.75
                ),
            ),
            Some(n) => {
                let h = if top > 0.0 && *n > 0.0 {
                    (n / top * (H - 1.0)).max(2.0)
                } else {
                    1.0
                };
                (
                    format!("{} {unit}", int(n.round() as u64)),
                    format!(
                        "<rect class=\"v\" x=\"{x}\" y=\"{:.1}\" width=\"{}\" height=\"{h:.1}\"/>",
                        H - h,
                        SLOT * 0.75
                    ),
                )
            }
        };
        let _ = write!(
            o,
            "<g class=\"bar\"><title>{}{}</title><rect class=\"hit\" x=\"{x}\" y=\"0\" width=\"{SLOT}\" height=\"{H}\"/>{drawn}</g>",
            esc(&window),
            esc(&what)
        );
    }
    o.push_str("</svg>");
}

fn collection(v: &View, o: &mut String) {
    open(o, "collection", "The collection");
    let held = v.fetch.total.pages;
    let ours = stage(v, "indexed");
    o.push_str("<div class=\"figures\">");
    figure(o, &int(held), "page scans held");
    figure(o, &count_or(ours), "pages where search uses our OCR text");
    o.push_str("</div>");
    let _ = write!(
        o,
        "<p>Chronicling America, the Library of Congress's collection of digitized newspapers, has about \
         24 million pages, and usnewsmap.com searches all of them. We hold the original scans of {} of \
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

/// When the fetcher holds back for the Library's site, until when.
fn held_until(v: &View) -> Option<DateTime<Utc>> {
    let p = v.fetch.pacer.value.as_ref()?;
    let slowed = p
        .slowed
        .as_ref()
        .and_then(|s| s.get("until_ms"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let until = p.hold_until_ms.max(slowed);
    let at = DateTime::from_timestamp_millis(i64::try_from(until).ok()?)?;
    (at > v.generated_at).then_some(at)
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
    o.push_str("</div><p>");
    let now = match f.activity {
        "running" => format!(
            "We are fetching page scans from the Library of Congress now, about {} pages an hour.",
            int(f.last_hour.pages)
        ),
        "queued" => "More page scans from the Library of Congress are queued, waiting to be fetched.".to_owned(),
        "idle" if left.is_some() => "No page scans from the Library of Congress are queued right now.".to_owned(),
        _ => "We can't tell right now whether page scans are being fetched from the Library of Congress.".to_owned(),
    };
    o.push_str(&esc(&now));
    if let Some(until) = held_until(v) {
        let _ = write!(
            o,
            " The Library's site has been limiting our requests, so fetching is slowed or paused until {}.",
            super::fmt::time(until)
        );
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
        o.push_str("<p class=\"note\">Pages fetched a day, last 7 days; each bar is 24 hours, the newest on the right</p>");
        daily_bars(
            o,
            "fetch-days",
            "Pages fetched a day, last 7 days",
            "pages",
            &days(s),
            v.throughput.chart_start,
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
    // The same hourly counts as the throughput section, so the two agree.
    let collected = week(v, "ocr_collect");
    let imported = week(v, "ocr_import");
    let searchable = stage(v, "indexed");
    let scored = week(v, "quality_load");
    o.push_str("<ol class=\"flow\">");
    for (n, label) in [
        (collected, "OCR pages returned and checked, last 7 days"),
        (imported, "imported into our text collection, last 7 days"),
        (searchable, "used in search so far"),
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
         A search release uses our text for a page when an automatic check scores it clearly better than \
         the Library's. In the same 7 days the quality checks scored {} pages; the scores let us rank \
         pages for OCR by how poor the Library's text is.</p>",
        count_or(scored)
    );
    o.push_str("</section>");
}

fn accuracy(o: &mut String) {
    open(o, "accuracy", "Accuracy");
    o.push_str("<div class=\"figures\">");
    figure(
        o,
        "9.0%",
        "characters wrong in our OCR, on 142 test snippets",
    );
    figure(
        o,
        "9.2%",
        "characters wrong in the Library's OCR, on 345 Chronicling America snippets",
    );
    o.push_str("</div>");
    // docs/ocr/gold-set.md, the gold bundle's version 1 baseline (11,336
    // edits in 125,622 characters); docs/reports/2026-10-boinc-quality-pilot/
    // report.md §4.1, the 345 clean overproof-ca crops.
    o.push_str(
        "<p>Both numbers compare OCR with people's transcriptions from public datasets, on snippets \
         (lines and passages cut from pages) rather than whole pages. The 142 snippets come from six \
         datasets of historical newspapers in English, French, Finnish, Swedish and German, read by our \
         OCR worker in October 2026; the 345 come from 18 Chronicling America pages, where Tesseract, \
         the engine our OCR runs, got 3.7% wrong reading one snippet at a time. We have no \
         transcriptions for other languages, such as Czech, Serbian, Polish, Russian, Yiddish and \
         Japanese, so we don't know how accurate our OCR is on them.</p></section>",
    );
}

fn throughput(v: &View, o: &mut String) {
    open(o, "throughput", "Pages per day");
    let imported = step(v, "ocr_import");
    let daily = imported.map(days).unwrap_or_default();
    let known: Vec<f64> = daily.iter().flatten().copied().collect();
    let total: f64 = known.iter().sum();
    o.push_str("<div class=\"figures\">");
    figure(
        o,
        &if known.is_empty() {
            "not known".to_owned()
        } else {
            int((total / known.len() as f64).round() as u64)
        },
        "OCR pages imported a day, 7-day average",
    );
    o.push_str(
        "</div><p>A page goes from scan to imported text; the number above counts the last step, \
         just before search.</p>",
    );
    if imported.is_some() {
        o.push_str("<p class=\"note\">OCR pages imported a day, last 7 days; each bar is 24 hours, the newest on the right</p>");
        daily_bars(
            o,
            "import-days",
            "OCR pages imported a day, last 7 days",
            "pages",
            &daily,
            v.throughput.chart_start,
        );
    }
    o.push_str("<table class=\"rates\"><thead><tr><th>Last 24 hours</th><th class=\"r\">Pages</th></tr></thead><tbody>");
    for (id, label) in [
        ("scan_fetch", "Scans fetched"),
        ("ocr_collect", "OCR pages returned and checked"),
        ("ocr_import", "OCR pages imported"),
        ("quality_load", "Pages scored by the quality checks"),
    ] {
        // The scan fetch's own 24 hours, as in its section, not full hours.
        let n = if id == "scan_fetch" {
            Some(v.fetch.last_day.pages as f64)
        } else {
            step(v, id).and_then(|s| s.last_day)
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
    fn a_day_is_six_bins_and_unknown_only_if_all_are() {
        let s = StepRate {
            bins: [
                vec![Some(1.0); 6],
                vec![None; 6],
                vec![None, Some(2.0), None, None, None, Some(3.0)],
            ]
            .concat(),
            ..StepRate::default()
        };
        assert_eq!(days(&s), [Some(6.0), None, Some(5.0)]);
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
