//! The page: one server-rendered HTML document. Its script fetches it again
//! on a timer and swaps in the panels that changed.
//! Every value from outside goes through [`esc`].

use std::fmt::Write as _;

use crate::view::View;

mod assets;
mod details;
mod ecosystem;
mod fmt;
mod throughput;

use assets::{SCRIPT, STYLE};
use ecosystem::ecosystem_panel;
pub(crate) use ecosystem::summary_stage;
use fmt::time;
pub use fmt::{bytes, esc, int, span};

pub fn render(v: &View) -> String {
    let now = v.generated_at;
    let mut o = String::with_capacity(64 * 1024);
    let _ = write!(
        o,
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <noscript><meta http-equiv=\"refresh\" content=\"{}\"></noscript>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>USNewsMap processing</title><style>{STYLE}</style></head><body data-refresh=\"{}\">\
         <h1>USNewsMap processing</h1><div class=\"sub\">Source acquisition, local OCR, volunteer compute and evaluation. \
         Read-only; no page text. Made {}; refreshes every {}s. \
         JSON: <a href=\"/api/view.json\">/api/view.json</a>.</div><div class=\"grid dashboard\">",
        v.refresh_secs,
        v.refresh_secs,
        time(now),
        v.refresh_secs
    );
    o.push_str(&ecosystem_panel(v));
    o.push_str(&throughput::panel(&v.throughput));
    o.push_str(&crate::diagrams::section(v));
    o.push_str("<section class=\"wide\" id=\"operational-details\"><h2>Operational details</h2>\
        <details class=\"diagnostics\" data-refresh-key=\"operational-details\"><summary>\
        Show workers, queues, page lists, source evidence and reference diagrams</summary><div class=\"grid\">");

    details::at_a_glance(v, &mut o);
    details::pool_and_hosts(v, &mut o);
    details::downloads(v, &mut o);
    details::fetch_queue(v, &mut o);
    details::serial_set(v, &mut o);
    details::nodes(v, &mut o);
    details::whats_next(v, &mut o);
    details::ocred(v, &mut o);
    details::queues(v, &mut o);

    let _ = write!(
        o,
        "</div></details></section></div><script>{SCRIPT}</script></body></html>"
    );
    o
}

#[cfg(test)]
mod tests {
    use super::ecosystem::pipeline_panel;
    use super::fmt::{msg, pct, short_urls};
    use super::*;
    use chrono::Utc;

    #[test]
    fn corpus_panel_escapes_metadata_and_keeps_catalog_counts_separate() {
        let now = Utc::now();
        let mut source = crate::pipeline::tests::fixture(now);
        source["coverage"] = serde_json::json!(["<coverage>"]);
        source["stages"][0]["label"] = serde_json::json!("<stage>");
        source["stages"][0]["detail"] = serde_json::json!("\"details\" & <text>");
        source["issues"][0]["detail"] = serde_json::json!("<script>alert(1)</script>");
        let snapshot = crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), now).unwrap();
        let v = crate::pipeline::PipelineView {
            retrieved_at: Some(now),
            last_error: Some("<error>".into()),
            age_secs: Some(901),
            stale: true,
            stage_freshness: Vec::new(),
            snapshot: Some(snapshot),
        };
        let html = pipeline_panel(&v);
        assert!(html.contains("Corpus data flow"));
        assert!(html.contains("<section class=\"wide\" id=\"corpus-data-flow\">"));
        assert!(html.contains("aria-label=\"Corpus stages\""));
        assert!(html.contains("<table class=\"pipeline-table\">"));
        assert_eq!(html.matches("data-refresh-key=\"stage-").count(), 2);
        assert!(!html.contains("colspan=\"3\""));
        assert!(html.contains("data-refresh-key=\"corpus-metadata\"><summary>"));
        assert!(html.contains("Some evidence old or unknown"));
        assert!(html.contains("separate from local worker totals"));
        for s in [
            "&lt;coverage&gt;",
            "&lt;stage&gt;",
            "&lt;error&gt;",
            "&lt;script&gt;",
        ] {
            assert!(html.contains(s));
        }
        assert!(!html.contains("<script>"));
        assert!(html.contains("1,234"));
        assert!(html.contains("unknown"));
        assert!(html.contains("Source evidence: <time"));
        assert!(html.contains("Cataloged runs: 2; cataloged run pages: 100"));
        assert!(pipeline_panel(&crate::pipeline::PipelineView::default())
            .contains("No valid catalog snapshot"));
    }

    #[test]
    fn blocked_stages_keep_their_stale_warning_and_stable_disclosure_key() {
        let now = Utc::now();
        let mut source = crate::pipeline::tests::fixture(now);
        source["stages"][0]["state"] = serde_json::json!("blocked");
        let snapshot = crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), now).unwrap();
        let v = crate::pipeline::PipelineView {
            snapshot: Some(snapshot),
            ..Default::default()
        };
        let html = pipeline_panel(&v);
        assert!(html.contains("blocked / stale"));
        assert!(html.contains("data-refresh-key=\"stage-scan\""));
        assert!(html.contains("data-refresh-key=\"corpus-metadata\""));
        assert!(SCRIPT.contains("d.dataset.refreshKey ||"));
    }

    fn summary_view() -> View {
        let dir = tempfile::tempdir().unwrap();
        crate::view::build(
            &mut crate::fetch::Fetch::new(dir.path().to_owned(), None),
            &crate::nodes::Nodes::open(
                dir.path().join("state"),
                &[("worker-sentinel".into(), "http://127.0.0.1:1".into())],
            )
            .unwrap(),
            &crate::hosts::Hosts::new(Vec::new()),
            &crate::sset::Ssets::default(),
            None,
            &crate::view::Settings::default(),
            Utc::now(),
        )
    }

    #[test]
    fn publisher_details_distinguish_completion_failure_and_unknown() {
        let mut v = summary_view();
        v.fetch.publish.configured = true;
        v.fetch.publish.value = Some(crate::fetch::PublishStatus {
            schema: 1,
            active_state: Some("inactive".into()),
            finished_at: Some(v.generated_at - chrono::Duration::minutes(1)),
            result: Some("success".into()),
            exit_status: Some(0),
            error: None,
        });
        let html = render(&v);
        assert!(html.contains("Publish fetched"));
        assert!(html.contains(" succeeded</span>"));
        assert!(html.contains("Last completion: <time"));
        assert!(html.contains("does not prove that new pages were published"));
        let status = v.fetch.publish.value.as_mut().unwrap();
        status.result = Some("exit-code".into());
        status.exit_status = Some(1);
        assert!(render(&v).contains(" failed</span>"));
        v.fetch.publish.value.as_mut().unwrap().error = Some("<unavailable>".into());
        let html = render(&v);
        assert!(html.contains(" unknown</span>"));
        assert!(html.contains("&lt;unavailable&gt;"));
        v.fetch.publish.value = None;
        v.fetch.publish.error = Some("cannot read /var/lib/private/publisher.json".into());
        crate::public::redact(&mut v);
        let html = render(&v);
        assert!(html.contains("Publisher metadata unknown"));
        assert!(!html.contains("/var/lib/private"));
    }

    #[test]
    fn ecosystem_summaries_do_not_expose_page_or_batch_identifiers() {
        let mut v = summary_view();
        v.fetch.status = Some(crate::fetch::FetchStatus {
            updated_at: v.generated_at.to_rfc3339(),
            state: "running".into(),
            last_doc_id: "private-page-sentinel".into(),
            manifest: "private-batch-sentinel".into(),
            ..Default::default()
        });
        v.fetch.status_error = None;
        v.fetch.plan_error = None;
        v.fetch.activity = "running";
        v.fetch.last_hour.pages = 1574;
        let summary = ecosystem_panel(&v);
        assert!(summary.contains("1,574 pages in the last hour"));
        assert!(summary.contains("Volunteer compute"));
        assert!(summary.contains("Experiments and evaluation"));
        assert!(summary.contains("Validation and publication"));
        assert!(summary.contains("Current activity unknown"));
        assert!(!summary.contains("private-page-sentinel"));
        assert!(!summary.contains("private-batch-sentinel"));
        assert!(!summary.contains("STALE"));
        let html = render(&v);
        assert!(html.find("Across the ecosystem") < html.find("Operational details"));
        assert!(html
            .contains("<details class=\"diagnostics\" data-refresh-key=\"operational-details\">"));
        assert!(SCRIPT.contains("section:not(#operational-details)"));
        assert!(SCRIPT.contains("'.diagnostics > .grid'"));
        assert!(!SCRIPT.contains(".grid > section"));
    }

    #[test]
    fn summary_uses_fresh_volunteer_counts_without_hiding_unknown_acceptance() {
        let mut v = summary_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_unsent","label":"Unsent","count":12345,"state":"observed","detail":""},
            {"id":"boinc_in_progress","label":"Assigned","count":6789,"state":"observed","detail":""},
            {"id":"boinc_usnm_iris_in_progress","label":"Iris","count":2345,"state":"observed","detail":""}
        ]);
        let snapshot =
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap();
        v.pipeline.stage_freshness = snapshot
            .stages
            .iter()
            .map(|s| crate::pipeline::StageFreshness {
                id: s.id.clone(),
                age_secs: Some(5),
                stale: false,
            })
            .collect();
        v.pipeline.snapshot = Some(snapshot);
        // Historical or unknown stages may mark the evidence bundle stale without hiding fresh lanes.
        v.pipeline.stale = true;
        let html = ecosystem_panel(&v);
        assert!(html.contains("6,789 assigned result replicas"));
        assert!(html.contains("12,345 unsent"));
        assert!(html.contains("Iris pilot readings: 2,345 assigned"));
        assert!(html.contains("Accepted: not currently observed"));
        v.pipeline
            .stage_freshness
            .iter_mut()
            .for_each(|s| s.stale = true);
        let stale = ecosystem_panel(&v);
        assert!(!stale.contains("6,789 assigned result replicas"));
        assert!(!stale.contains("12,345 unsent"));
        assert!(stale.contains("Current activity unknown"));
    }

    #[test]
    fn summaries_exclude_old_or_failed_reports_and_keep_queue_units() {
        let mut v = summary_view();
        let node = |done, at, ok| {
            let mut n = summary_view().nodes.remove(0);
            n.ok = ok;
            n.last_ok = Some(at);
            n.pages_per_hour = done;
            n.last_hour.failed = 2;
            n
        };
        v.nodes = vec![
            node(10, v.generated_at, true),
            node(20, v.generated_at, true),
            node(10000, v.generated_at - chrono::Duration::minutes(4), true),
            node(10000, v.generated_at, false),
        ];
        let summary = ecosystem_panel(&v);
        assert!(summary
            .contains("Configured direct workers still reporting: 2 (30 pages in the last hour)"));
        assert!(summary.contains("Direct queue workers are retired"));
        assert!(!summary.contains("worker-sentinel"));
        v.fetch.status = Some(crate::fetch::FetchStatus {
            state: "running".into(),
            updated_at: (v.generated_at - chrono::Duration::minutes(16)).to_rfc3339(),
            ..Default::default()
        });
        v.fetch.status_error = None;
        v.fetch.last_hour.pages = 123456;
        v.fetch.activity = "unknown";
        assert!(!ecosystem_panel(&v).contains("123,456 pages in the last hour"));
        v.fetch.activity = "running";
        assert!(ecosystem_panel(&v).contains("123,456 pages in the last hour"));
        v.fetch.activity = "idle";
        assert!(ecosystem_panel(&v).contains("Idle: nothing queued"));
        assert!(!ecosystem_panel(&v).contains("123,456 pages in the last hour"));
    }

    #[test]
    fn a_stale_node_and_serial_set_show_no_old_counters() {
        let now = Utc::now();
        let dir = tempfile::tempdir().unwrap();
        let mut nodes = crate::nodes::Nodes::open(
            dir.path().join("state"),
            &[("usnm-ocr-node3".into(), "http://127.0.0.1:1".into())],
        )
        .unwrap();
        nodes.nodes[0].status = Some(usnm_ocr_contract::status::WorkerStatus {
            concurrency: 8,
            last_day: usnm_ocr_contract::status::Outcomes {
                done: 1329,
                ..Default::default()
            },
            ..Default::default()
        });
        nodes.nodes[0].last_ok = Some(now - chrono::Duration::days(6));
        let mut ssets = crate::sset::Ssets::default();
        ssets.put(
            "usnm-sset",
            crate::sset::SsetStatus {
                active: "active".into(),
                to_fetch: 9789,
                curated: 648,
                eta_hours: Some(333.0),
                ..Default::default()
            },
            now - chrono::Duration::days(4),
        );
        let v = crate::view::build(
            &mut crate::fetch::Fetch::new(dir.path().to_owned(), None),
            &nodes,
            &crate::hosts::Hosts::new(Vec::new()),
            &ssets,
            None,
            &crate::view::Settings::default(),
            now,
        );
        assert_eq!(v.nodes[0].state, "retired");
        assert_eq!(v.nodes[0].last_day.done, 0);
        assert_eq!(v.serial_set[0].state, "unknown");
        let html = render(&v);
        assert!(!html.contains("1,329"));
        assert!(html.contains("unknown or stopped"));
        assert!(!html.contains("13d"));
    }

    #[test]
    fn malformed_or_missing_snapshot_suppresses_retained_live_counts() {
        let mut v = summary_view();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"boinc_unsent","label":"Unsent","count":12345,"state":"observed","detail":""},
            {"id":"boinc_in_progress","label":"Assigned","count":6789,"state":"observed","detail":""}
        ]);
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        let mut pipeline = crate::pipeline::Pipeline::new(Some(path.clone()), 900);
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        v.pipeline = pipeline.view(v.generated_at + chrono::Duration::minutes(16));
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        std::fs::write(&path, b"not valid JSON").unwrap();
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(v.pipeline.snapshot.is_some());
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
        assert!(ecosystem_panel(&v).contains("old, unknown or unreadable"));
        std::fs::remove_file(path).unwrap();
        pipeline.poll(v.generated_at);
        v.pipeline = pipeline.view(v.generated_at);
        assert!(!ecosystem_panel(&v).contains("6,789 assigned result replicas"));
    }

    #[test]
    fn missing_catalog_and_runs_only_catalog_are_not_empty_inventories() {
        let mut v = summary_view();
        let mut source = crate::pipeline::tests::fixture(v.generated_at);
        source["stages"] = serde_json::json!([
            {"id":"artifact_verified","label":"Artifacts","count":null,"state":"unknown","detail":""},
            {"id":"run_history","label":"Runs","count":null,"state":"unknown","detail":""}
        ]);
        source["artifacts"] = serde_json::json!({"locations":0,"verified":0,"missing":0});
        source["runs"] = serde_json::json!({"total":0,"pages":0});
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        let unknown = ecosystem_panel(&v);
        assert!(unknown.contains("Artifact inventory is not connected"));
        assert!(unknown.contains("Run history is not connected"));
        assert!(!unknown.contains("Recorded runs: 0"));
        assert!(!unknown.contains("artifact locations: 0"));
        source["runs"] = serde_json::json!({"total":5,"pages":3});
        source["stages"][1]["state"] = "observed".into();
        source["stages"][1]["count"] = 3.into();
        source["stages"][1]["observed_at"] = (v.generated_at - chrono::Duration::days(1))
            .to_rfc3339()
            .into();
        v.pipeline.snapshot = Some(
            crate::pipeline::parse(&serde_json::to_vec(&source).unwrap(), v.generated_at).unwrap(),
        );
        let runs = ecosystem_panel(&v);
        assert!(runs.contains("Recorded runs: 5; pages: 3"));
        assert!(runs.contains("Artifact inventory is not connected"));
        assert!(runs.contains("Imported evidence only, not corpus coverage"));
    }

    #[test]
    fn formats() {
        assert_eq!(
            esc("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1_215_076_923), "1.2 GB");
        assert_eq!(bytes(3_298_534_883_328), "3.3 TB");
        assert_eq!(span(691_999), "8d 0h");
        assert_eq!(span(3_700), "1h 1m");
        assert_eq!(span(42), "42s");
        assert_eq!(int(224_848), "224,848");
        assert_eq!(int(5), "5");
        assert_eq!(pct(1, 0), 0.0);
    }

    #[test]
    fn loc_addresses_lose_their_host() {
        assert_eq!(
            short_urls("https://www.loc.gov/item/sn1/1904-09-08/ed-1/?fo=json returned HTTP 503"),
            "/item/sn1/1904-09-08/ed-1/?fo=json returned HTTP 503"
        );
        assert_eq!(
            short_urls(
                "https://tile.loc.gov/storage-services/service/ndnp/ga/batch_x/data/0123.jp2: 404"
            ),
            "…/ga/batch_x/data/0123.jp2: 404"
        );
        assert_eq!(
            short_urls("https://example.com/x and https://loc.gov"),
            "https://example.com/x and "
        );
        assert_eq!(
            msg("a <b> https://www.loc.gov/item/x/", None),
            "<td class=\"msg\"><details class=\"more\"><summary><span class=\"clamp\">a &lt;b&gt; /item/x/</span></summary>\
             <div>a &lt;b&gt; https://www.loc.gov/item/x/</div></details></td>"
        );
    }
}
