//! The pipeline's traces and metrics reach an Application Insights ingestion
//! endpoint with an Entra bearer token, under the `usnm-ingest` role, and
//! shutdown flushes them.

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::{Tracer as _, TracerProvider as _};
use usnm_ingest::telemetry::SERVICE;
use usnm_telemetry::providers;
use usnm_telemetry::testing::{fake_ingestion, Plain};

#[test]
#[cfg(unix)]
fn japanese_supervision_refuses_to_launch_without_telemetry() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("launched");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_usnm-ingest"))
        .env_remove("APPLICATIONINSIGHTS_CONNECTION_STRING")
        .current_dir(directory.path())
        .args([
            "--curated",
            "unused-curated",
            "--reference",
            "unused-reference",
        ])
        .args([
            "ja-ocr-run",
            "--",
            "/bin/sh",
            "-c",
            "touch \"$1\"",
            "runner",
        ])
        .arg(&marker)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!marker.exists(), "an unmonitored runner was launched");
    let messages = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        messages.contains("Japanese OCR supervision requires usable"),
        "{messages}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exports_with_a_bearer_token_and_flushes_on_shutdown() {
    let (endpoint, seen) = fake_ingestion().await;
    let conn = format!(
        "InstrumentationKey=00000000-0000-0000-0000-000000000000;IngestionEndpoint={endpoint}"
    );
    let (tracer, meter) = providers(&conn, Plain::with_token("tok"), SERVICE).unwrap();
    meter
        .meter("usnm-ingest")
        .u64_counter("ingest.docs_sent")
        .build()
        .add(3, &[]);
    tracer.tracer("usnm-ingest").in_span("release", |_| {});

    // As Telemetry::shutdown does: the SDK blocks while the export runs on
    // this runtime.
    tokio::task::spawn_blocking(move || {
        tracer.shutdown().unwrap();
        meter.shutdown().unwrap();
    })
    .await
    .unwrap();

    let uploads = seen.uploads();
    assert!(!uploads.is_empty(), "nothing was exported");
    for u in &uploads {
        assert_eq!(u.path, "/v2.1/track");
        assert_eq!(u.authorization.as_deref(), Some("Bearer tok"));
    }
    let all = seen.all_json();
    assert!(all.contains("ingest.docs_sent"), "{all}");
    assert!(all.contains("\"release\""), "{all}");
    assert!(all.contains(r#""ai.cloud.role":"usnm-ingest""#), "{all}");
}

/// The ocr-sync failure alert (infra/modules/ocr-sync-alerts.bicep) reads
/// `ingest.command_failures` with its `command` and `outcome` as the
/// metric's properties, the way main records it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_command_failure_is_exported_with_its_command() {
    let (endpoint, seen) = fake_ingestion().await;
    let conn = format!(
        "InstrumentationKey=00000000-0000-0000-0000-000000000000;IngestionEndpoint={endpoint}"
    );
    let (tracer, meter) = providers(&conn, Plain::with_token("tok"), SERVICE).unwrap();
    meter
        .meter("usnm-ingest")
        .u64_counter("ingest.command_failures")
        .build()
        .add(
            1,
            &[
                opentelemetry::KeyValue::new("command", "ocr-sync"),
                opentelemetry::KeyValue::new("outcome", "failed"),
            ],
        );
    tokio::task::spawn_blocking(move || {
        tracer.shutdown().unwrap();
        meter.shutdown().unwrap();
    })
    .await
    .unwrap();

    let all = seen.all_json();
    assert!(all.contains("ingest.command_failures"), "{all}");
    assert!(all.contains(r#""command":"ocr-sync""#), "{all}");
    assert!(all.contains(r#""outcome":"failed""#), "{all}");
}
