//! A page's provenance (13 §13.8, #307 item 6): the `run_id` minted
//! before the engine call, and the runs record the worker writes between
//! the page's ALTO and its overlay part.

use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Utc};
use sha2::{Digest as _, Sha256};
use usnm_core::ocr_engine::EngineTag;
use usnm_ocr_contract::message::OcrMessage;
use usnm_ocr_contract::runs::RunRecord;

use crate::engine::{ReadInfo, CONTRACT, ROUTE, ROUTE_VERSION};

/// A page's `run_id`, `<node>.<UTC stamp>` with the stamp to the
/// microsecond (13 §13.4.3). Strictly increasing within the process, so
/// two pages started in the same microsecond still get different ids and
/// output names.
pub fn mint_run_id(node: &str) -> String {
    static LAST: AtomicI64 = AtomicI64::new(0);
    let now = Utc::now().timestamp_micros();
    let mut prev = LAST.load(Ordering::Relaxed);
    let us = loop {
        let next = now.max(prev + 1);
        match LAST.compare_exchange_weak(prev, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break next,
            Err(p) => prev = p,
        }
    };
    let t = DateTime::<Utc>::from_timestamp_micros(us).unwrap_or_else(Utc::now);
    format!("{node}.{}", t.format("%Y%m%dT%H%M%S%6fZ"))
}

/// The engine's family, version and profile, and the route, its version
/// and the contract, from the `ocr_engine` tag; a tag in an older form
/// (`tesseract-5.5.1/deu`) gives what it holds.
fn tag_fields(ocr_engine: &str, ocr_source: &str) -> (String, String, String, String, u32, u32) {
    match EngineTag::parse(ocr_engine) {
        Ok(EngineTag::Tag(t)) => (
            t.family().to_owned(),
            t.version().to_owned(),
            t.profile().to_owned(),
            t.route().to_owned(),
            t.route_version(),
            t.contract(),
        ),
        _ => {
            let family = ocr_source.strip_prefix("usnm-").unwrap_or(ocr_source);
            let (engine, profile) = ocr_engine.split_once('/').unwrap_or((ocr_engine, ""));
            let version = engine
                .strip_prefix(family)
                .map(|v| v.trim_start_matches('-'))
                .unwrap_or("");
            (
                family.to_owned(),
                version.to_owned(),
                profile.to_owned(),
                ROUTE.to_owned(),
                ROUTE_VERSION,
                CONTRACT,
            )
        }
    }
}

/// What the worker knows about the kept read besides the engine's side.
#[derive(Debug, Clone)]
pub struct Read<'a> {
    pub run_id: &'a str,
    pub doc_id: &'a str,
    pub node: &'a str,
    pub worker_version: &'a str,
    pub message: &'a OcrMessage,
    /// The row's `ocr_source` and `ocr_engine`.
    pub ocr_source: &'a str,
    pub ocr_engine: &'a str,
    pub info: &'a ReadInfo,
    /// The stored page and its text (normalized).
    pub page: &'a usnm_alto::Page,
    pub text: &'a str,
    /// The sha256 of the scan the engine read.
    pub image_sha256: &'a str,
    pub dpi: u32,
    /// The engine call, start to result.
    pub seconds_wall: f64,
    /// The node's clock when it wrote the ALTO.
    pub finished_at: DateTime<Utc>,
}

/// The kept read's row (13 §13.8.3). The worker routes one engine and one
/// profile per page, so the record has this one row, selected by route.
pub fn record(r: &Read<'_>) -> RunRecord {
    let (family, engine_version, profile, route_id, routing_version, contract) =
        tag_fields(r.ocr_engine, r.ocr_source);
    let (blocks, lines, words) = r.page.counts();
    let count = |n: usize| Some(u32::try_from(n).unwrap_or(u32::MAX));
    let ratio = |v: Option<f64>| v.filter(|v| (0.0..=1.0).contains(v));
    let m = r.message;
    RunRecord {
        run_id: r.run_id.to_owned(),
        doc_id: r.doc_id.to_owned(),
        node: r.node.to_owned(),
        worker_version: r.worker_version.to_owned(),
        contract,
        ocr_engine: r.ocr_engine.to_owned(),
        engine_id: r.info.engine_id.clone(),
        family,
        engine_version,
        adapter: r.info.adapter.clone(),
        image_ref: r.info.image_ref.clone(),
        image_digest: r.info.image_digest.clone(),
        models_digest: r.info.models_digest.clone(),
        models_json: r.info.models_json.clone(),
        profile,
        options_json: r.info.options_json.clone(),
        scale: 1.0,
        tiles: 0,
        route_id,
        routing_version,
        routing_digest: None,
        candidate_rank: 0,
        selection: Some("route".into()),
        selected: true,
        status: "ok".into(),
        error_kind: None,
        seconds_wall: if r.seconds_wall.is_finite() {
            r.seconds_wall.max(0.0)
        } else {
            0.0
        },
        seconds_cpu: None,
        peak_mem_mb: None,
        instance_peak_mem_mb: None,
        oom: false,
        instance_restarts: 0,
        model_load_seconds: r
            .info
            .model_load_seconds
            .filter(|v| v.is_finite() && *v >= 0.0),
        models_loaded: r.info.models_loaded.to_owned(),
        cold_start: r.info.cold_start,
        image_sha256: r.image_sha256.to_owned(),
        image_width: r.page.width.round().clamp(0.0, f64::from(u32::MAX)) as u32,
        image_height: r.page.height.round().clamp(0.0, f64::from(u32::MAX)) as u32,
        dpi: r.dpi,
        dpi_source: "assumed".into(),
        granularity: r.page.granularity.as_str().to_owned(),
        reading_order: None,
        markup: r.info.markup.to_owned(),
        blocks: count(blocks),
        lines: count(lines),
        words: count(words),
        mean_conf: ratio(r.page.mean_conf()),
        text_chars: count(r.text.chars().count()),
        text_sha256: Some(hex(&Sha256::digest(r.text.as_bytes()))),
        engine_stats_json: r.info.engine_stats_json.clone(),
        damage_before: ratio(m.score),
        // Against the page's language alone, as the survey scores; none
        // for a language without a word list (`jpn`, `und`).
        damage_after: ratio(
            usnm_core::damage::score_forced(r.text, &m.language).and_then(|s| s.damage_rate),
        ),
        language: m.language.clone(),
        script: None,
        reason: m.reason.as_str().to_owned(),
        queued_at: DateTime::parse_from_rfc3339(&m.queued_at)
            .ok()
            .map(|t| t.with_timezone(&Utc)),
        node_finished_at: r.finished_at,
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_ids_are_unique_and_name_the_node() {
        let ids: Vec<String> = (0..1000).map(|_| mint_run_id("nuc1")).collect();
        let set: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(set.len(), ids.len());
        for id in &ids {
            let (node, stamp) = id.split_once('.').unwrap();
            assert_eq!(node, "nuc1");
            assert_eq!(stamp.len(), 22, "{stamp}");
            assert!(stamp.ends_with('Z') && stamp.as_bytes()[8] == b'T');
        }
        // Increasing, so a later page's names sort after.
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn tag_fields_read_both_forms() {
        let (f, v, p, r, rv, c) = tag_fields(
            "tesseract/5.5.0 profile=frak2021 image=sha256:3f9a0c1d2e4b models=sha256:8c2d71aa09fe route=minimal@r1 contract=0",
            "usnm-tesseract",
        );
        assert_eq!(
            (f.as_str(), v.as_str(), p.as_str(), r.as_str(), rv, c),
            ("tesseract", "5.5.0", "frak2021", "minimal", 1, 0)
        );
        let (f, v, p, ..) = tag_fields("tesseract-5.5.1/deu_latf+deu", "usnm-tesseract");
        assert_eq!(
            (f.as_str(), v.as_str(), p.as_str()),
            ("tesseract", "5.5.1", "deu_latf+deu")
        );
    }
}
