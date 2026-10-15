//! The CLI must refuse the same data the worker's shared gate refuses.

use std::process::Command;

#[test]
fn routes_check_cli_reports_gate_failures() {
    let dir = tempfile::tempdir().unwrap();
    let engines = dir.path().join("engines.toml");
    let routes = dir.path().join("routes.toml");
    std::fs::write(
        &routes,
        "version=1\n[[route]]\nid='test'\ncandidates=[{engine='tesseract-5.5',profile='eng'}]\n",
    )
    .unwrap();
    for (old, new, expected) in [
        ("production = true", "production = true", true),
        ("production = true", "production = false", false),
        ("license = \"Apache-2.0\"", "license = \"unknown\"", false),
        ("author = \"Tesseract OCR project\"", "author = \"\"", false),
        (
            "name = \"tessdata/eng.traineddata\"",
            "name = \"tessdata/eng.traineddata\"\nrestricted = \"excluded\"",
            false,
        ),
        (
            "name = \"tessdata/eng.traineddata\"",
            "name = \"tessdata/eng.traineddata\"\nundecided = \"pending\"",
            false,
        ),
    ] {
        std::fs::write(&engines, usnm_ocr_route::ENGINES_TOML.replacen(old, new, 1)).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_usnm-ocr-worker"))
            .env_remove("APPLICATIONINSIGHTS_CONNECTION_STRING")
            .args(["routes", "check", "--engines"])
            .arg(&engines)
            .arg("--routes")
            .arg(&routes)
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            expected,
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
