//! Ecosystem and corpus summaries, with independent evidence freshness.

use super::fmt::*;
use crate::view::View;
use std::fmt::Write as _;

pub(super) fn pipeline_panel(v: &crate::pipeline::PipelineView) -> String {
    let mut o =
        String::from("<section class=\"wide\" id=\"corpus-data-flow\"><h2>Corpus data flow</h2>");
    o.push_str(&badge(
        if v.stale { "warning" } else { "good" },
        if v.stale {
            "Some evidence old or unknown"
        } else {
            "observed"
        },
    ));
    o.push_str(
        "<p class=\"note\">Partial catalog; stage counts are separate from local worker totals, not whole-corpus totals. \
        <a href=\"/api/pipeline.json\">JSON</a></p>",
    );
    if let Some(s) = &v.snapshot {
        o.push_str(&scroll("Corpus stages"));
        o.push_str("<table class=\"pipeline-table\"><thead><tr><th>Stage / details</th><th class=\"r\">Count</th><th>State</th><th>Age</th></tr></thead><tbody>");
        for stage in &s.stages {
            let state = match stage.state {
                crate::pipeline::StageState::Observed => "observed",
                crate::pipeline::StageState::Unknown => "unknown",
                crate::pipeline::StageState::Blocked => "blocked",
            };
            let freshness = v.stage_freshness.iter().find(|f| f.id == stage.id);
            let _ = write!(
                o,
                "<tr><td><details data-refresh-key=\"stage-{}\"><summary>{}</summary><p>{}</p><p><code>{}</code></p>\
                 <p>Source evidence: {}</p></details></td><td class=\"r\">{}</td><td>{}{}</td><td>{}</td></tr>",
                esc(&stage.id),
                esc(&stage.label),
                esc(&stage.detail),
                esc(&stage.id),
                stage
                    .observed_at
                    .unwrap_or(s.observed_at)
                    .map_or_else(|| "unknown".into(), time),
                stage.count.map_or_else(|| "unknown".into(), int),
                state,
                if stage.state != crate::pipeline::StageState::Unknown
                    && freshness.is_none_or(|f| f.stale)
                {
                    " / stale"
                } else {
                    ""
                },
                freshness
                    .and_then(|f| f.age_secs)
                    .map_or_else(|| "unknown".into(), |age| span(age.max(0) as u64)),
            );
        }
        let _ = write!(
            o,
            "</tbody></table></div><details class=\"pipeline-meta\" data-refresh-key=\"corpus-metadata\"><summary>Coverage, provenance and issues ({})</summary>\
             <p>Cataloged runs: {}; cataloged run pages: {}. Cataloged artifact locations: {}; \
             verified: {}; missing: {}.</p>",
            s.issues.len(),
            int(s.runs.total),
            int(s.runs.pages),
            int(s.artifacts.locations),
            int(s.artifacts.verified),
            int(s.artifacts.missing)
        );
        if let Some(e) = &v.last_error {
            let _ = write!(o, "<p class=\"stale\">{}</p>", esc(e));
        }
        let _ = write!(
            o,
            "<p>Producer: {} (schema {}). Source evidence: {}. Generated: {}. \
             Retrieved: {}. Evidence age: {}.</p><p>Coverage: {}</p>",
            esc(&s.producer),
            s.schema_version,
            s.observed_at.map_or_else(|| "unknown".into(), time),
            time(s.generated_at),
            v.retrieved_at.map_or_else(|| "never".into(), time),
            v.age_secs
                .map_or_else(|| "unknown".into(), |n| span(n.max(0) as u64)),
            esc(&s.coverage.join(", "))
        );
        for issue in &s.issues {
            let _ = write!(o, "<p>{}: {}</p>", esc(&issue.code), esc(&issue.detail));
        }
        o.push_str("</details>");
    } else {
        o.push_str("<p>No valid catalog snapshot available.</p>");
        if let Some(e) = &v.last_error {
            let _ = write!(o, "<p class=\"stale\">{}</p>", esc(e));
        }
    }
    o.push_str("</section>");
    o
}

pub(crate) fn summary_stage<'a>(v: &'a View, id: &str) -> Option<&'a crate::pipeline::Stage> {
    let pipeline = &v.pipeline;
    let snapshot = pipeline.snapshot.as_ref()?;
    let stage = snapshot.stages.iter().find(|s| s.id == id)?;
    let freshness = pipeline.stage_freshness.iter().find(|s| s.id == id)?;
    (stage.state == crate::pipeline::StageState::Observed
        && stage.count.is_some()
        && !freshness.stale)
        .then_some(stage)
}

fn summary_count(v: &View, id: &str) -> String {
    summary_stage(v, id)
        .and_then(|s| s.count)
        .map_or_else(|| "not currently observed".into(), int)
}

fn summary_card(o: &mut String, title: &str, headline: &str, body: &str, note: &str) {
    let _ = write!(
        o,
        "<article class=\"ecosystem-card\"><h3>{}</h3><p class=\"headline\">{}</p>\
         <div>{body}</div><p class=\"note\">{note}</p></article>",
        esc(title),
        esc(headline),
    );
}

pub(super) fn ecosystem_panel(v: &View) -> String {
    let now = v.generated_at;
    let mut o = String::from(
        "<section class=\"wide\" id=\"ecosystem\"><h2>Across the ecosystem</h2>\
         <p>Acquire originals, run OCR and comparisons, then validate and publish. \
         These lanes run independently: cached pages, worker completions and volunteer tasks \
         are different populations, not one progress bar.</p><div class=\"ecosystem-cards\">",
    );
    let f = &v.fetch;
    let all_runs = format!(
        "{} pages fetched across all runs ({}).",
        int(f.total.pages),
        bytes(f.total.bytes)
    );
    let qs = &f.queue_summary;
    let queue_line = if f.queue_configured && f.queue_error.is_none() {
        format!(
            " Fetch queue: {} manifests waiting, {} cooling off, {} fetched; {} queued pages left.",
            int((qs.queued + qs.fetching) as u64),
            int(qs.cooling as u64),
            int(qs.fetched as u64),
            int(qs.pages_left)
        )
    } else {
        String::new()
    };
    match f.activity {
        "running" => summary_card(
            &mut o,
            "Fetching originals",
            &format!("{} pages in the last hour", int(f.last_hour.pages)),
            &format!(
                "{} {all_runs}{queue_line} {}",
                fetch_badge(f),
                if f.status.as_ref().is_some_and(|s| s.error.is_some()) {
                    "Fetcher reports an error; see operational details."
                } else {
                    ""
                }
            ),
            &format!(
                "{} source holds and {} failed attempts in the last hour. \
                Failures are attempts, not unique lost pages. Scan caching is not public-mirror verification.",
                int(f.holds_last_hour as u64),
                int(f.failures_last_hour as u64)
            ),
        ),
        "idle" | "queued" => summary_card(
            &mut o,
            "Fetching originals",
            if f.activity == "idle" {
                "Idle: nothing queued"
            } else {
                "Manifests waiting"
            },
            &format!("{} {all_runs}{queue_line}", fetch_badge(f)),
            &format!(
                "{} pages fetched in the last day. The queue looks for new manifests every ten minutes \
                 without rewriting the fetcher's status.",
                int(f.last_day.pages)
            ),
        ),
        _ => summary_card(
            &mut o,
            "Fetching originals",
            "Current activity unknown",
            &format!("Fetcher status is missing, old or invalid. {all_runs}"),
            "Historical download details remain available below; no current rate is inferred.",
        ),
    }
    let fresh_nodes: Vec<_> = v
        .nodes
        .iter()
        .filter(|n| crate::view::node_status_fresh(n, now))
        .collect();
    let ocr = summary_stage(v, "boinc_usnm_ocr_in_progress").and_then(|s| s.count);
    let mut workers = String::new();
    if !fresh_nodes.is_empty() {
        let done: u64 = fresh_nodes.iter().map(|n| n.pages_per_hour).sum();
        let _ = write!(
            workers,
            " Configured direct workers still reporting: {} ({} pages in the last hour).",
            int(fresh_nodes.len() as u64),
            int(done)
        );
    }
    summary_card(
        &mut o,
        "OCR",
        &ocr.map_or_else(
            || "Current activity unknown".into(),
            |n| format!("{} usnm_ocr replicas assigned", int(n)),
        ),
        &format!(
            "OCR runs as the BOINC <code>usnm_ocr</code> app: pages are claimed from the on-prem OCR \
             queue, bundled into workunits, run by volunteers and collected on-prem. Unsent \
             usnm_ocr replicas: {}. Direct queue workers are retired (#1053).{workers}",
            summary_count(v, "boinc_usnm_ocr_unsent")
        ),
        "Assigned replicas are not unique pages or active devices. \
         Completion means collected output, not accepted or searchable text.",
    );
    let boinc_current = summary_stage(v, "boinc_unsent").is_some()
        && summary_stage(v, "boinc_in_progress").is_some();
    summary_card(&mut o, "Volunteer compute",
        &if boinc_current { format!("{} assigned result replicas", summary_count(v, "boinc_in_progress")) }
         else { "Current activity unknown".into() },
        &format!("{} unsent result replicas reported by the project. \
            Volunteer execution is a separate lane from local OCR.", summary_count(v, "boinc_unsent")),
        "Assigned does not mean actively computing. Replicas are not unique pages; \
         device eligibility, completion throughput and source cache age are not provided by this feed.");
    let mut research = String::new();
    let mut assigned = None::<u64>;
    for (app, name) in [
        ("quality", "Quality comparisons"),
        ("qwen", "Qwen crop readings"),
        ("iris", "Iris pilot readings"),
        ("gold", "Reference checks"),
        ("qwen_train", "Training work"),
    ] {
        let id = format!("boinc_usnm_{app}_in_progress");
        if let Some(count) = summary_stage(v, &id).and_then(|s| s.count) {
            assigned = Some(assigned.unwrap_or(0).saturating_add(count));
        }
        let _ = write!(
            research,
            "<li>{}: {} assigned replicas</li>",
            name,
            summary_count(v, &id)
        );
    }
    summary_card(
        &mut o,
        "Experiments and evaluation",
        &assigned.map_or_else(
            || "Current activity unknown".into(),
            |n| format!("{} assigned evaluation replicas", int(n)),
        ),
        &format!("<ul>{research}</ul>"),
        "These are subsets of volunteer work, not additional totals. Scheduler assignments show \
         activity, not experiment success, quality gains or approved rollout. Campaign readiness, \
         paired results and acceptance decisions are not connected to this dashboard.",
    );
    let ssets: Vec<_> = v.serial_set.iter().filter(|s| !s.stale).collect();
    if ssets.is_empty() {
        summary_card(
            &mut o,
            "Other source acquisition",
            "Current activity unknown",
            "Serial Set collection has no fresh report.",
            "This source extracts existing PDF text; it does not use the OCR queue.",
        );
    } else {
        let done: u64 = ssets.iter().map(|s| s.done).sum();
        let total: u64 = ssets.iter().map(|s| s.status.to_fetch).sum();
        let pages: u64 = ssets.iter().map(|s| s.status.pages).sum();
        let running = ssets.iter().filter(|s| s.state == "running").count();
        summary_card(
            &mut o,
            "Other source acquisition",
            &format!("{} Serial Set volumes processed (reported)", int(done)),
            &format!(
                "{} of {} planned volumes reported, containing {} extracted pages. \
                Collectors running: {}.",
                int(done),
                int(total),
                int(pages),
                int(running as u64)
            ),
            "Reports may overlap and include empty/incomplete volumes. \
             PDF text extraction is separate from OCR and search publication.",
        );
    }
    let snapshot = v.pipeline.snapshot.as_ref();
    let historical_stage = |id: &str| {
        snapshot.and_then(|s| {
            s.stages.iter().find(|stage| {
                stage.id == id
                    && stage.state == crate::pipeline::StageState::Observed
                    && stage.count.is_some()
            })
        })
    };
    let mut coverage = String::new();
    if let Some(s) = snapshot {
        if historical_stage("artifact_verified").is_some() {
            let _ = write!(
                coverage,
                "Cataloged artifact locations: {} ({} verified). ",
                int(s.artifacts.locations),
                int(s.artifacts.verified)
            );
        } else {
            coverage.push_str("Artifact inventory is not connected. ");
        }
        if historical_stage("run_history").is_some() {
            let _ = write!(
                coverage,
                "Recorded runs: {}; pages: {}. ",
                int(s.runs.total),
                int(s.runs.pages)
            );
        } else {
            coverage.push_str("Run history is not connected. ");
        }
        coverage.push_str("Imported evidence only, not corpus coverage.");
    } else {
        coverage.push_str("No valid catalog snapshot is loaded.");
    }
    let mut publishing = String::new();
    for (id, label) in [
        ("accepted", "Accepted"),
        ("imported", "Curated imports"),
        ("indexed", "Search inclusion"),
    ] {
        let _ = write!(publishing, "<li>{}: {}</li>", label, summary_count(v, id));
    }
    let publication_connected = ["accepted", "imported", "indexed"]
        .iter()
        .all(|id| summary_stage(v, id).and_then(|s| s.count).is_some());
    summary_card(
        &mut o,
        "Validation and publication",
        if publication_connected {
            "Publication counts reported"
        } else {
            "Coverage is incomplete"
        },
        &format!("<ul>{publishing}</ul>{coverage}"),
        "Execution counts do not establish OCR quality gains or publication. \
         Historical observations keep their original age when live counters refresh.",
    );
    o.push_str("</div>");
    if v.pipeline.last_error.is_some() {
        o.push_str(
            "<p class=\"note\">Some volunteer/catalog evidence is old, unknown or unreadable. \
            Source errors and individual ages are in operational details.</p>",
        );
    }
    if let Some(s) = snapshot {
        let _ = write!(
            o,
            "<p class=\"note\">Volunteer/catalog snapshot generated {} ({} ago). \
            Live-counter freshness is independent of historical evidence.</p>",
            time(s.generated_at),
            span((now - s.generated_at).num_seconds().max(0) as u64)
        );
    } else {
        o.push_str("<p class=\"note\">Volunteer/catalog snapshot unavailable. Local collection reports are independent.</p>");
    }
    o.push_str("</section>");
    o
}
