//! The pickup's instruments (08 §8.1.2). They sit on the `usnm-ingest`
//! meter, as the rest of the job's metrics do, so the exported series
//! (`ocr_sync.parts`, `ocr.out.oldest_age_s`) are the ones `ocr-sync` has
//! always written. With no meter provider installed (telemetry off) they
//! record nothing.

use std::sync::OnceLock;

use opentelemetry::metrics::{Counter, Gauge};

pub(crate) struct Metrics {
    /// Node parts `ocr-sync` took, by `outcome` (imported, rejected) and
    /// `reason` (a reject's code).
    pub ocr_sync_parts: Counter<u64>,
    /// The age of the oldest part `ocr-sync` left in `ocr-out`.
    pub ocr_out_oldest_age: Gauge<u64>,
}

/// The instruments, created on first use from the global meter provider
/// (`usnm-ingest`'s `telemetry::init` installs it first).
pub(crate) fn metrics() -> &'static Metrics {
    static METRICS: OnceLock<Metrics> = OnceLock::new();
    METRICS.get_or_init(|| {
        let m = opentelemetry::global::meter("usnm-ingest");
        Metrics {
            ocr_sync_parts: m
                .u64_counter("ocr_sync.parts")
                .with_description("OCR node parts picked up, by outcome and reject reason")
                .build(),
            ocr_out_oldest_age: m
                .u64_gauge("ocr.out.oldest_age_s")
                .with_unit("s")
                .with_description("Age of the oldest part left in ocr-out after a pickup")
                .build(),
        }
    })
}
