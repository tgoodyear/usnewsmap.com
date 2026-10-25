//! The pipeline diagrams (#296): one SVG per data set, drawn by hand in
//! `src/diagrams/*.svg` and embedded here. They are static; only the state
//! of each step (the bar on a box, its state word and a live line in a few
//! boxes) comes from the [`View`], through the `@@cls:<step>@@`,
//! `@@lbl:<step>@@` and `@@txt:<step>@@` markers in the files.
//!
//! The SVG files are repo content and trusted. Every live value that goes
//! into one passes through [`esc`].

use std::collections::BTreeMap;

use crate::html::{esc, int};
use crate::view::View;

/// One data set's diagram, with the four questions it answers.
pub struct Diagram {
    pub id: &'static str,
    pub heading: &'static str,
    pub svg: &'static str,
    /// Source, needs OCR, where it is stored, what makes it searchable.
    pub answers: [(&'static str, &'static str); 4],
}

pub const LEGEND: &str = include_str!("diagrams/legend.svg");

pub const DIAGRAMS: [Diagram; 5] = [
    Diagram {
        id: "loc-kept",
        heading: "LoC pages whose OCR text is kept",
        svg: include_str!("diagrams/loc-kept.svg"),
        answers: [
            (
                "Comes from",
                "Chronicling America batches on loc.gov, read once by the ingest job in Azure, paced.",
            ),
            (
                "Needs OCR",
                "No: LoC's own text is searched. Pages the damage pass scores above 0.25 are the exception \
                 and follow the next two diagrams.",
            ),
            (
                "Stored",
                "Azure only: curated/ (Parquet), then the search index in qw-index and reference/. \
                 This diagram covers the original-text publication path, not scan caching.",
            ),
            (
                "Searchable when",
                "Once a release has published the batch. No further step.",
            ),
        ],
    },
    Diagram {
        id: "loc-keep",
        heading: "LoC keep set, read again on the nodes",
        svg: include_str!("diagrams/loc-keep.svg"),
        answers: [
            (
                "Comes from",
                "LoC's tile.loc.gov storage service: the batch's JP2 for each page. Only a page the batch \
                 route cannot place goes through the loc.gov item API and the IIIF JPEG. The scan fetcher \
                 fetches each scan once, paced, and holds when LoC asks it to. The keep set is every page \
                 not detected as English, plus the worst English batches.",
            ),
            (
                "Needs OCR",
                "Yes. The on-prem OCR queue (ocr_priority for pages users find, ocr_bulk for the rest) feeds \
                 the BOINC usnm_ocr app: pages are claimed from the queue, bundled into workunits, read by \
                 volunteers and collected on-prem. The direct queue workers are retired (#1053).",
            ),
            (
                "Stored",
                "Home: manifests/ (the plan and the hourly queue-ocr-* manifests) and keep/ on the scan \
                 store (scans are kept for good). Azure: ocr-out (deleted after 30 days), then \
                 curated/ocr-local/.",
            ),
            (
                "Searchable when",
                "ocr-sync has imported the output and a release indexes that batch with USNM_OCR_LOCAL \
                 enabled, selecting our text where the quality rule prefers it. Updating previously \
                 published batches requires a full rebuild. Until then LoC's text stays searchable.",
            ),
        ],
    },
    Diagram {
        id: "loc-buffer",
        heading: "LoC buffer set, English pages read again",
        svg: include_str!("diagrams/loc-buffer.svg"),
        answers: [
            (
                "Comes from",
                "The same route as the keep set. These are the other badly damaged English pages, fetched \
                 after the keep set.",
            ),
            (
                "Needs OCR",
                "Yes, but only the pages the seed plan queues. Same queue, BOINC app and engine as the \
                 keep set.",
            ),
            (
                "Stored",
                "Home: buffer/ on the scan store, a rolling buffer. The fetcher waits while it is at its \
                 cap. Azure as for the keep set.",
            ),
            (
                "Searchable when",
                "As for the keep set: ocr-sync, then an enabled release indexing that batch; \
                 a full rebuild is needed for previously published batches.",
            ),
        ],
    },
    Diagram {
        id: "serial-set",
        heading: "Serial Set from GovInfo",
        svg: include_str!("diagrams/serial-set.svg"),
        answers: [
            (
                "Comes from",
                "GovInfo's sitemaps, MODS and PDFs, public and without a key. usnm-sset fetches one \
                 request at a time, paced. Run again to pick up new or changed volumes.",
            ),
            (
                "Needs OCR",
                "No. pdftotext reads the text layer of each PDF. The OCR queues and engines are not used.",
            ),
            (
                "Stored",
                "Home: a raw store of MODS and page texts on the host that runs it (the PDFs are deleted \
                 after reading). Azure: curated/congress/serialset/.",
            ),
            (
                "Searchable when",
                "A release run with --congress (USNM_CONGRESS, off by default) builds the Congress index. \
                 It does not wait for the #283 rebuild.",
            ),
        ],
    },
    Diagram {
        id: "volunteer",
        heading: "Volunteer compute and experiments",
        svg: include_str!("diagrams/volunteer.svg"),
        answers: [
            ("Comes from", "Explicitly staged campaign inputs, including originals or crops. \
                Local OCR is not a prerequisite for volunteer full-page OCR."),
            ("Needs OCR", "Depends on the campaign: OCR, quality comparisons, reference checks \
                and training are distinct workloads. Assignment does not prove active computation."),
            ("Stored", "Campaign inputs and outputs use the project storage and controlled mirror. \
                The public scheduler feed does not verify mirror coverage or execution results."),
            ("Searchable when", "Only after the relevant validation, acceptance, import and publication \
                gates. Scheduler counts do not establish those outcomes; campaign results are not \
                connected to this dashboard."),
        ],
    },
];

/// A step's state: the CSS token for its bar and the word shown.
#[derive(Debug, Clone, PartialEq)]
pub struct StepState {
    pub class: &'static str,
    pub label: &'static str,
    /// A live line some boxes show; empty for the rest.
    pub text: String,
    pub rate: Option<String>,
    pub rate_scope: &'static str,
}

impl StepState {
    fn new(class: &'static str) -> StepState {
        let label = match class {
            "done" => "done",
            "running" => "running",
            "waiting" => "waiting",
            "not-built" => "not built",
            "failed" => "failed",
            _ => "not tracked here",
        };
        StepState {
            class,
            label,
            text: String::new(),
            rate: None,
            rate_scope: "No independent throughput counter is connected for this step.",
        }
    }
    fn status_line(&self) -> String {
        if self.class == "running" || matches!(self.label, "queued" | "assigned") {
            format!(
                "{}; {}",
                self.label,
                self.rate.as_deref().unwrap_or("rate unknown")
            )
        } else {
            self.label.into()
        }
    }
    fn with(class: &'static str, text: String) -> StepState {
        StepState {
            text,
            ..StepState::new(class)
        }
    }
}

/// The state of every step the dashboard can say something about. A step
/// not in the map is drawn as "not tracked here": the dashboard is on the
/// LAN and reads nothing in Azure (12 §12.2.5).
pub fn steps(v: &View) -> BTreeMap<&'static str, StepState> {
    let mut m = BTreeMap::new();

    // Nothing in Azure is read here, so the ingest, the release, the
    // published index, the rebuild and the index with our text are left
    // out and draw as "not tracked here".
    // Pages users find go first in each hourly queue-ocr manifest (#1082),
    // which the fetch queue shows; nothing here says how many there are.

    // The plan or a queued manifest is on the scan store, so the damage
    // pass that fed it and the copy that put it there are done.
    let plan = (v.fetch.plan_error.is_none() && !v.fetch.sets.is_empty())
        || v.fetch.queue.iter().any(|m| m.error.is_none());
    let manifest = StepState::new(if plan { "done" } else { "waiting" });
    m.insert("manifest", manifest.clone());
    m.insert("pull", manifest);
    if plan {
        m.insert("damage", StepState::new("done"));
    }

    // The fetcher takes the keep set first, then the buffer set, so the
    // run's state belongs to the first set that isn't complete; a set
    // behind it is waiting. A finished set is done whatever the run says.
    let run = crate::view::fetch_status_fresh(v)
        .then(|| v.fetch.status.as_ref().map(|s| s.state.as_str()))
        .flatten();
    let mut active_taken = false;
    for (set, fetch_key, store_key) in [
        ("keep", "fetch-keep", "store-keep"),
        ("buffer", "fetch-buffer", "store-buffer"),
    ] {
        let Some(s) = v.fetch.sets.iter().find(|s| s.name == set) else {
            continue;
        };
        let complete = s.fetched >= s.total;
        let (fetch_class, store_class) = if complete {
            ("done", "done")
        } else if active_taken {
            ("waiting", "waiting")
        } else {
            active_taken = true;
            match run {
                Some("running") => ("running", "running"),
                Some("failed") => ("failed", "waiting"),
                Some("done" | "waiting" | "cooling" | "budget" | "stopped") => {
                    ("waiting", "waiting")
                }
                _ => ("none", "waiting"),
            }
        };
        m.insert(
            fetch_key,
            StepState::with(
                fetch_class,
                format!("{} of {} pages", int(s.fetched), int(s.total)),
            ),
        );
        m.insert(store_key, StepState::new(store_class));
    }

    // Queues, from the nodes' look.
    if let Some(q) = v
        .azure
        .queues
        .as_ref()
        .filter(|q| !v.azure.queues_stale && q.error.is_none())
    {
        if let (Some(&bulk), Some(&prio)) = (q.counts.get("ocr-bulk"), q.counts.get("ocr-priority"))
        {
            m.insert(
                "queues",
                StepState::with(
                    "waiting",
                    format!("ocr-bulk {}, ocr-priority {}", int(bulk), int(prio)),
                ),
            );
            if bulk.saturating_add(prio) > 0 {
                m.get_mut("queues").unwrap().label = "queued";
            }
        }
    }

    // OCR: the BOINC usnm_ocr app's assigned replicas.
    if let Some(count) =
        crate::html::summary_stage(v, "boinc_usnm_ocr_in_progress").and_then(|s| s.count)
    {
        let mut step = StepState::with(
            if count > 0 { "running" } else { "waiting" },
            format!("{} usnm_ocr replicas assigned", int(count)),
        );
        step.rate_scope =
            "Scheduler stock, not throughput: assigned replicas are not unique pages \
            or active devices. Completion and collect rates are not connected.";
        m.insert("nodes", step);
    }
    for key in ["fetch-keep", "fetch-buffer", "store-keep", "store-buffer"] {
        if let Some(step) = m.get_mut(key).filter(|s| s.class == "running") {
            step.rate = Some(format!("{} pages/h*", int(v.fetch.last_hour.pages)));
            step.rate_scope =
                "Last-hour scan-fetch completions across keep + buffer, not a per-set rate. \
                The fetch and store boxes share this counter; do not add them.";
        }
    }

    // The Serial Set, from the status its node pushes.
    if let Some(x) = v.serial_set.iter().find(|x| !x.stale) {
        let st = &x.status;
        let class = match x.state.as_str() {
            "finished" => "done",
            "running" => "running",
            _ => "waiting",
        };
        m.insert(
            "sset-fetch",
            StepState::with(
                class,
                format!("{} of {} volumes", int(x.done), int(st.to_fetch)),
            ),
        );
        m.insert(
            "sset-curated",
            StepState::with(
                if st.curated == 0 { "waiting" } else { class },
                format!("{} volumes, {} pages", int(st.curated), int(st.pages)),
            ),
        );
        if st.started.is_some_and(|at| at <= v.generated_at)
            && st.volumes_per_hour.is_finite()
            && st.volumes_per_hour >= 0.0
        {
            let step = m.get_mut("sset-fetch").unwrap();
            step.rate = Some(format!("{:.1} volumes/h*", st.volumes_per_hour));
            step.rate_scope = "Reported run-to-date average since collector start, not last hour. \
                Includes curated, empty and incomplete volumes; not successful uploads per hour.";
        }
    }
    for (key, id, label) in [
        ("boinc-ready", "boinc_unsent", "queued"),
        ("boinc-assigned", "boinc_in_progress", "assigned"),
    ] {
        if let Some(count) = crate::html::summary_stage(v, id).and_then(|s| s.count) {
            let mut step = StepState::with("waiting", format!("{} result replicas", int(count)));
            step.label = if count > 0 { label } else { "waiting" };
            step.rate_scope = "Scheduler stock, not throughput. Assigned replicas are not active devices \
                or unique pages; source cache age is unavailable. Completion/dispatch rates are not connected.";
            m.insert(key, step);
        }
    }
    m
}

fn step_title<'a>(svg: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = svg.split_once(&format!("data-state=\"@@cls:{key}@@\""))?;
    let (_, rest) = rest.split_once("class=\"ttl\"")?;
    let (_, rest) = rest.split_once('>')?;
    let (title, _) = rest.split_once("</text>")?;
    Some(title.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' '))
}

/// The boxes of a diagram with a live state, as plain text for screen
/// readers: each box's title and what it says.
pub fn summary(svg: &str, steps: &BTreeMap<&'static str, StepState>) -> String {
    let mut out = String::new();
    let mut rest = svg;
    while let Some(i) = rest.find("data-state=\"@@cls:") {
        let after = &rest[i + "data-state=\"@@cls:".len()..];
        let Some(j) = after.find("@@") else { break };
        let key = &after[..j];
        rest = &after[j..];
        let title = rest
            .find("class=\"ttl\"")
            .and_then(|t| rest[t..].split_once('>'))
            .and_then(|(_, r)| r.split_once("</text>"))
            .map(|(t, _)| t.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' '));
        let Some(title) = title else { continue };
        let (label, text) = steps.get(key).map_or(("not tracked here".into(), ""), |s| {
            (s.status_line(), s.text.as_str())
        });
        out.push_str(&format!("<li>{}: {}", title, esc(&label)));
        if !text.is_empty() {
            out.push_str(&format!(" ({})", esc(text)));
        }
        out.push_str("</li>");
    }
    out
}

/// A diagram with its markers filled in from `steps`. A marker for a step
/// not in the map reads "not tracked here"; none is left in the output.
pub fn fill(svg: &str, steps: &BTreeMap<&'static str, StepState>) -> String {
    let mut out = String::with_capacity(svg.len() + 512);
    let mut rest = svg;
    while let Some(i) = rest.find("@@") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("@@") else {
            out.push_str("@@");
            rest = after;
            continue;
        };
        let (kind, step) = after[..j].split_once(':').unwrap_or(("", ""));
        let state = steps.get(step);
        match kind {
            "cls" => out.push_str(state.map_or("none", |s| s.class)),
            "lbl" => out.push_str(&esc(&state.map_or_else(
                || StepState::new("none").status_line(),
                StepState::status_line,
            ))),
            "txt" => out.push_str(&esc(state.map_or("", |s| s.text.as_str()))),
            _ => {}
        }
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

fn mobile_flow(svg: &str, steps: &BTreeMap<&'static str, StepState>) -> String {
    let filled = fill(svg, steps);
    let mut out = String::from("<ol class=\"mobile-flow\" aria-label=\"Pipeline steps\">");
    let mut cards = Vec::new();
    for group in filled
        .split("<g ")
        .skip(1)
        .filter_map(|s| s.split_once("</g>").map(|(g, _)| g))
    {
        if !group.contains("class=\"box") {
            continue;
        }
        let lines: Vec<_> = group
            .split("<text ")
            .skip(1)
            .filter_map(|s| {
                let (_, text) = s.split_once('>')?;
                let (text, _) = text.split_once("</text>")?;
                (!text.trim().is_empty()).then_some(text)
            })
            .collect();
        if let Some((title, body)) = lines.split_first() {
            let order = title
                .split_whitespace()
                .next()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(if group.contains("class=\"box x\"") {
                    0
                } else {
                    usize::MAX
                });
            // Text is trusted SVG content or already escaped by fill().
            cards.push((
                order,
                format!("<li><h4>{title}</h4><p>{}</p></li>", body.join(" ")),
            ));
        }
    }
    cards.sort_by_key(|(order, _)| *order);
    for (_, card) in cards {
        out.push_str(&card);
    }
    out.push_str("</ol>");
    out
}

/// The "Pipelines" section.
pub fn section(v: &View) -> String {
    let st = steps(v);
    let mut o = String::with_capacity(48 * 1024);
    o.push_str(
        "<section class=\"wide\" id=\"pipelines\"><details data-refresh-key=\"pipelines\"><summary><h2>Pipelines</h2> — diagrams and rates</summary>\
         <p class=\"note\">One diagram for each data set: where it comes from, whether it needs OCR, where \
         each copy is stored and what still has to happen before it is searchable, plus the separate \
         volunteer lane. The drawings are \
         fixed; the bar on each box and the italic line under it are the live state from this page's \
         own data. On phones, step cards show the same boxes in step order; branches and storage \
         lanes remain in the wide-screen maps. Rates marked * are shared counters or run averages, explained below each diagram. \
         Rate unknown means a throughput counter is missing, not zero. Steps without telemetry \
         show as not tracked here, not presumed idle.</p>",
    );
    o.push_str(LEGEND);
    for d in &DIAGRAMS {
        let _ = std::fmt::Write::write_fmt(
            &mut o,
            format_args!(
                "<h3 id=\"pipeline-{}\">{}</h3><figure style=\"margin:0\">{}</figure>\
                 {}<ul class=\"sr diagram-state\" aria-label=\"Live state of each step\">{}</ul><table class=\"kv\">",
                d.id,
                esc(d.heading),
                fill(d.svg, &st),
                mobile_flow(d.svg, &st),
                summary(d.svg, &st)
            ),
        );
        for (k, val) in d.answers {
            let _ = std::fmt::Write::write_fmt(
                &mut o,
                format_args!("<tr><td>{}</td><td>{}</td></tr>", esc(k), esc(val)),
            );
        }
        o.push_str("</table>");
        let active: Vec<_> = st
            .iter()
            .filter(|(key, step)| {
                d.svg.contains(&format!("@@cls:{key}@@"))
                    && (step.class == "running" || matches!(step.label, "queued" | "assigned"))
            })
            .collect();
        if !active.is_empty() {
            o.push_str(
                "<table class=\"step-rates\"><caption>Active steps: rates and measurement scope</caption>\
                <thead><tr><th>Step</th><th>Rate</th><th>Window / scope</th></tr></thead><tbody>",
            );
            for (key, step) in active {
                let _ = std::fmt::Write::write_fmt(
                    &mut o,
                    format_args!(
                        "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                        esc(step_title(d.svg, key).unwrap_or(key)),
                        esc(step.rate.as_deref().unwrap_or("rate unknown")),
                        esc(step.rate_scope)
                    ),
                );
            }
            o.push_str("</tbody></table>");
        }
    }
    o.push_str("</details></section>");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::Fetch;
    use crate::hosts::Hosts;
    use crate::nodes::Nodes;
    use crate::sset::{SsetStatus, Ssets};
    use crate::view::{build, SetView, Settings, SsetView};
    use chrono::Utc;
    use quick_xml::events::Event;
    use quick_xml::Reader;

    fn empty_view() -> View {
        let dir = tempfile::tempdir().unwrap();
        let mut fetch = Fetch::new(dir.path().to_owned(), None);
        let nodes = Nodes::open(dir.path().join("state"), &[]).unwrap();
        build(
            &mut fetch,
            &nodes,
            &Hosts::new(Vec::new()),
            &Ssets::default(),
            None,
            &Settings::default(),
            Utc::now(),
        )
    }

    fn all_svgs() -> Vec<(&'static str, &'static str)> {
        let mut v = vec![("legend", LEGEND)];
        v.extend(DIAGRAMS.iter().map(|d| (d.id, d.svg)));
        v
    }

    /// Parses to the end, with matching tags; returns the element names.
    fn well_formed(xml: &str) -> Vec<String> {
        let mut r = Reader::from_str(xml);
        let mut names = Vec::new();
        loop {
            match r.read_event() {
                Ok(Event::Eof) => break,
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    names.push(e.name().as_ref().to_owned());
                }
                Ok(_) => {}
                Err(e) => panic!("not well-formed: {e}"),
            }
        }
        names
    }

    #[test]
    fn each_svg_is_well_formed_and_described() {
        for (name, svg) in all_svgs() {
            let names = well_formed(svg);
            assert_eq!(names.first().map(String::as_str), Some("svg"), "{name}");
            assert!(svg.contains("viewBox="), "{name} has no viewBox");
            assert!(svg.contains("<title id="), "{name} has no title");
            assert!(svg.contains("<desc id="), "{name} has no description");
            assert!(svg.contains("role=\"img\""), "{name}");
            // Plain SVG: no scripts, external files or hard-coded fonts.
            for bad in ["<script", "<image", "xlink:href", "http://fonts", "@import"] {
                assert!(!svg.contains(bad), "{name} has {bad}");
            }
        }
    }

    #[test]
    fn filled_diagrams_are_well_formed_with_no_marker_left() {
        let st = steps(&empty_view());
        for (name, svg) in all_svgs() {
            let out = fill(svg, &st);
            assert!(!out.contains("@@"), "{name} keeps a marker");
            well_formed(&out);
        }
        // With nothing read, the steps the dashboard can't see say so.
        let out = fill(DIAGRAMS[1].svg, &st);
        assert!(out.contains("not tracked here"));
        // Pages users find are queued now (#1082).
        assert!(!out.contains("data-state=\"not-built\""));
    }

    #[test]
    fn state_comes_from_the_view() {
        let mut v = empty_view();
        v.fetch.sets.push(SetView {
            name: "keep",
            total: 10,
            fetched: 10,
            bytes: 0,
        });
        v.fetch.sets.push(SetView {
            name: "buffer",
            total: 1_000,
            fetched: 5,
            bytes: 0,
        });
        v.serial_set.push(SsetView {
            name: "usnm-sset".into(),
            received: Utc::now(),
            age_secs: 0,
            stale: false,
            state: "running".into(),
            status: SsetStatus {
                to_fetch: 900,
                curated: 120,
                pages: 50_000,
                ..SsetStatus::default()
            },
            done: 120,
        });
        let st = steps(&v);
        assert_eq!(st["fetch-keep"].class, "done");
        assert_eq!(st["fetch-keep"].text, "10 of 10 pages");
        assert_eq!(st["fetch-buffer"].text, "5 of 1,000 pages");
        assert_eq!(st["sset-fetch"].class, "running");
        assert_eq!(st["sset-fetch"].text, "120 of 900 volumes");
        assert_eq!(st["sset-curated"].text, "120 volumes, 50,000 pages");
        let out = fill(DIAGRAMS[3].svg, &st);
        assert!(out.contains("data-state=\"running\""));
        assert!(out.contains("120 of 900 volumes"));
    }

    #[test]
    fn live_text_is_escaped() {
        let mut st = BTreeMap::new();
        st.insert("fetch-keep", StepState::with("done", "<b>&".into()));
        let out = fill("<text>@@txt:fetch-keep@@</text>", &st);
        assert_eq!(out, "<text>&lt;b&gt;&amp;</text>");
    }

    #[test]
    fn mobile_steps_keep_descriptions_rates_and_logical_order() {
        let mut st = steps(&empty_view());
        st.insert("nodes", StepState::with("running", "<worker>&".into()));
        let flow = mobile_flow(DIAGRAMS[1].svg, &st);
        assert!(!flow.contains("<svg"));
        assert!(!flow.contains("@@"));
        assert!(flow.contains("&lt;worker&gt;&amp;"));
        assert!(flow.contains("running; rate unknown"));
        assert!(flow.find("1  Damage pass") < flow.find("2  Manifest copy"));
        assert!(flow.find("6  Queues") < flow.find("7  BOINC usnm_ocr"));
        assert!(flow.contains("collected on-prem"));
        assert!(!flow.contains("node 3"));
        assert_eq!(
            flow.matches("<li>").count(),
            DIAGRAMS[1].svg.matches("class=\"box ").count()
        );
    }

    #[test]
    fn the_page_has_every_diagram_once() {
        let html = crate::html::render(&empty_view());
        assert!(html.contains("<h2>Pipelines</h2>"));
        for d in &DIAGRAMS {
            assert_eq!(
                html.matches(&format!("<title id=\"{}-t\">", d.id)).count(),
                1
            );
            assert!(html.contains(&esc(d.heading)));
        }
        assert!(html.find("Across the ecosystem") < html.find("Pipelines"));
        assert!(html.find("Pipelines") < html.find("Operational details"));
        assert!(
            html.contains("<details data-refresh-key=\"pipelines\"><summary><h2>Pipelines</h2>")
        );
        // Element ids are unique across the inlined SVGs.
        let mut ids: Vec<&str> = html
            .split(" id=\"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate element ids");
    }

    #[test]
    fn nothing_private_is_written() {
        let mut text: Vec<String> = all_svgs().iter().map(|(_, s)| s.to_string()).collect();
        for d in &DIAGRAMS {
            for (k, v) in d.answers {
                text.push(format!("{k} {v} {}", d.heading));
            }
        }
        for t in text {
            for word in t.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
                let o: Vec<u8> = word
                    .split('.')
                    .map_while(|p| p.parse::<u8>().ok())
                    .collect();
                if word.split('.').count() == 4 && o.len() == 4 {
                    let private = o[0] == 10
                        || (o[0] == 172 && (16..32).contains(&o[1]))
                        || (o[0] == 192 && o[1] == 168);
                    assert!(!private, "private address {word}");
                }
            }
            for name in ["pve", "unifi", ".tjg.fyi"] {
                assert!(!t.to_lowercase().contains(name), "internal name {name}");
            }
        }
    }

    #[test]
    fn a_stale_status_and_an_unread_step_are_not_shown_as_live() {
        let mut v = empty_view();
        v.serial_set.push(SsetView {
            name: "usnm-sset".into(),
            received: Utc::now(),
            age_secs: 99_999,
            stale: true,
            state: "running".into(),
            status: SsetStatus::default(),
            done: 0,
        });
        let st = steps(&v);
        assert!(!st.contains_key("sset-fetch"));
        for k in ["rebuild", "index-ocr", "ingest", "release-loc", "index-loc"] {
            assert!(!st.contains_key(k), "{k} is not read from anywhere");
        }
    }

    #[test]
    fn an_empty_set_is_complete_and_the_run_state_goes_to_the_next() {
        let mut v = empty_view();
        for (name, total) in [("keep", 0), ("buffer", 50)] {
            v.fetch.sets.push(SetView {
                name,
                total,
                fetched: 0,
                bytes: 0,
            });
        }
        v.fetch.status = Some(crate::fetch::FetchStatus {
            state: "cooling".into(),
            updated_at: v.generated_at.to_rfc3339(),
            ..Default::default()
        });
        v.fetch.status_error = None;
        v.fetch.plan_error = None;
        let st = steps(&v);
        assert_eq!(st["fetch-keep"].class, "done");
        assert_eq!(st["fetch-buffer"].class, "waiting");
        v.fetch.status.as_mut().unwrap().state = "failed".into();
        assert_eq!(steps(&v)["fetch-buffer"].class, "failed");
    }

    #[test]
    fn active_fetch_rates_are_shared_last_hour_counts_not_per_set_estimates() {
        let mut v = empty_view();
        v.fetch.sets.push(SetView {
            name: "keep",
            total: 100,
            fetched: 50,
            bytes: 0,
        });
        v.fetch.sets.push(SetView {
            name: "buffer",
            total: 100,
            fetched: 0,
            bytes: 0,
        });
        v.fetch.status_error = None;
        v.fetch.plan_error = None;
        v.fetch.status = Some(crate::fetch::FetchStatus {
            state: "running".into(),
            updated_at: v.generated_at.to_rfc3339(),
            ..Default::default()
        });
        v.fetch.last_hour.pages = 1234;
        let st = steps(&v);
        for key in ["fetch-keep", "store-keep"] {
            assert_eq!(st[key].rate.as_deref(), Some("1,234 pages/h*"));
            assert!(st[key].rate_scope.contains("not a per-set rate"));
            assert!(st[key].status_line().contains("running; 1,234 pages/h*"));
        }
        assert_eq!(st["fetch-buffer"].class, "waiting");
        assert!(st["fetch-buffer"].rate.is_none());
        assert!(summary(DIAGRAMS[1].svg, &st).contains("1,234 pages/h*"));
        v.fetch.status.as_mut().unwrap().updated_at =
            (v.generated_at - chrono::Duration::minutes(16)).to_rfc3339();
        assert_ne!(steps(&v)["fetch-keep"].class, "running");
        assert!(steps(&v)["fetch-keep"].rate.is_none());
        v.fetch.status.as_mut().unwrap().updated_at = v.generated_at.to_rfc3339();
        v.fetch.status_error = Some("missing".into());
        assert!(steps(&v)["fetch-keep"].rate.is_none());
    }

    #[test]
    fn serial_set_rate_is_a_run_average_not_an_upload_rate() {
        let mut v = empty_view();
        v.serial_set.push(SsetView {
            name: "collector".into(),
            received: v.generated_at,
            age_secs: 0,
            stale: false,
            state: "running".into(),
            status: SsetStatus {
                started: Some(v.generated_at - chrono::Duration::hours(2)),
                volumes_per_hour: 16.25,
                curated: 30,
                ..Default::default()
            },
            done: 32,
        });
        let st = steps(&v);
        assert_eq!(st["sset-fetch"].rate.as_deref(), Some("16.2 volumes/h*"));
        assert!(st["sset-fetch"].rate_scope.contains("run-to-date"));
        assert!(st["sset-curated"].status_line().contains("rate unknown"));
        v.serial_set[0].status.volumes_per_hour = f64::NAN;
        assert!(steps(&v)["sset-fetch"].rate.is_none());
        v.serial_set[0].stale = true;
        assert!(!steps(&v).contains_key("sset-fetch"));
    }

    #[test]
    fn queue_stock_is_not_throughput_and_errors_do_not_imply_zero() {
        let mut v = empty_view();
        v.azure.queues_stale = false;
        v.azure.queues = Some(usnm_ocr_contract::status::QueueStats {
            counts: [("ocr-bulk".into(), 607), ("ocr-priority".into(), 0)].into(),
            ..Default::default()
        });
        let st = steps(&v);
        assert_eq!(st["queues"].label, "queued");
        assert_eq!(st["queues"].status_line(), "queued; rate unknown");
        assert_ne!(st["queues"].class, "running");
        v.azure.queues.as_mut().unwrap().error = Some("failed".into());
        assert!(!steps(&v).contains_key("queues"));
        v.azure.queues.as_mut().unwrap().error = None;
        v.azure.queues.as_mut().unwrap().counts.remove("ocr-bulk");
        assert!(!steps(&v).contains_key("queues"));
    }

    #[test]
    fn volunteer_assignments_have_unknown_rate_and_never_prove_active_computation() {
        let mut v = empty_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_unsent","label":"Ready","count":500,"state":"observed","detail":""},
            {"id":"boinc_in_progress","label":"Assigned","count":100,"state":"observed","detail":""}
        ]);
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        v.pipeline.stage_freshness = ["boinc_unsent", "boinc_in_progress"]
            .iter()
            .map(|id| crate::pipeline::StageFreshness {
                id: (*id).into(),
                age_secs: Some(5),
                stale: false,
            })
            .collect();
        let st = steps(&v);
        assert_eq!(st["boinc-assigned"].label, "assigned");
        assert_ne!(st["boinc-assigned"].class, "running");
        assert_eq!(st["boinc-assigned"].text, "100 result replicas");
        assert!(fill(DIAGRAMS[4].svg, &st).contains("assigned; rate unknown"));
        assert!(!st.contains_key("boinc-validation"));
        v.pipeline
            .stage_freshness
            .iter_mut()
            .for_each(|s| s.stale = true);
        assert!(!steps(&v).contains_key("boinc-assigned"));
    }

    #[test]
    fn ocr_step_is_the_boinc_app_not_the_retired_workers() {
        let mut v = empty_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_usnm_ocr_in_progress","label":"Assigned","count":79,"state":"observed","detail":""}
        ]);
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        v.pipeline.stage_freshness = vec![crate::pipeline::StageFreshness {
            id: "boinc_usnm_ocr_in_progress".into(),
            age_secs: Some(5),
            stale: false,
        }];
        let st = steps(&v);
        assert_eq!(st["nodes"].class, "running");
        assert_eq!(st["nodes"].text, "79 usnm_ocr replicas assigned");
        assert!(st["nodes"].rate.is_none());
        v.pipeline.stage_freshness[0].stale = true;
        assert!(!steps(&v).contains_key("nodes"));
        for svg in [DIAGRAMS[1].svg, DIAGRAMS[2].svg] {
            assert!(!svg.contains("node 3"));
            assert!(!svg.contains("Configured OCR workers"));
            assert!(svg.contains("BOINC usnm_ocr"));
        }
        assert!(!DIAGRAMS[3].svg.contains("node 3"));
    }

    #[test]
    fn screen_readers_get_the_live_state_as_text() {
        let st = steps(&empty_view());
        let s = summary(DIAGRAMS[3].svg, &st);
        assert!(s.contains("<li>"));
        assert!(s.contains("not tracked here"));
        let html = crate::html::render(&empty_view());
        assert_eq!(
            html.matches("class=\"sr diagram-state\"").count(),
            DIAGRAMS.len()
        );
    }
}
