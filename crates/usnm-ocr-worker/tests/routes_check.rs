//! The CLI must refuse the same data the worker's shared gate refuses.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn routes_check(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_usnm-ocr-worker"))
        .env_remove("APPLICATIONINSIGHTS_CONNECTION_STRING")
        .env_remove("USNM_OCR_ENGINES")
        .current_dir(dir)
        .args(["routes", "check"])
        .args(args)
        .output()
        .unwrap()
}

fn log(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// CI's `routes check`: the committed routes, catalog and profile lock pass.
#[test]
fn routes_check_passes_on_the_committed_files() {
    let output = routes_check(&repo(), &[]);
    assert!(output.status.success(), "{}", log(&output));
}

#[test]
fn routes_check_refuses_a_model_swap_under_the_same_profile_id() {
    let dir = tempfile::tempdir().unwrap();
    let engines = dir.path().join("ocr/engines");
    for rel in ["tesseract/models.lock", "ndlocr-lite/models.lock"] {
        std::fs::create_dir_all(engines.join(rel).parent().unwrap()).unwrap();
        std::fs::copy(repo().join("ocr/engines").join(rel), engines.join(rel)).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("ocr/routing")).unwrap();
    std::fs::copy(
        repo().join("ocr/routing/routes.toml"),
        dir.path().join("ocr/routing/routes.toml"),
    )
    .unwrap();

    let output = routes_check(dir.path(), &[]);
    assert!(!output.status.success(), "no lock yet: {}", log(&output));
    let output = routes_check(dir.path(), &["--update-lock"]);
    assert!(output.status.success(), "{}", log(&output));
    let lock = std::fs::read_to_string(engines.join("profiles.lock")).unwrap();
    assert_eq!(
        lock,
        std::fs::read_to_string(repo().join("ocr/engines/profiles.lock")).unwrap()
    );
    assert!(routes_check(dir.path(), &[]).status.success());

    // A new eng.traineddata under the same `eng` profile id.
    let tesseract = engines.join("tesseract/models.lock");
    let pins = std::fs::read_to_string(&tesseract).unwrap();
    let line = pins
        .lines()
        .find(|l| l.starts_with("tessdata/eng.traineddata "))
        .unwrap();
    let swapped = format!("{} {}", line.rsplit_once(' ').unwrap().0, "0".repeat(64));
    std::fs::write(&tesseract, pins.replace(line, &swapped)).unwrap();
    let cases: [&[&str]; 2] = [&[], &["--update-lock"]];
    for args in cases {
        let output = routes_check(dir.path(), args);
        assert!(!output.status.success(), "{args:?}");
        let log = log(&output);
        assert!(log.contains("tesseract-5.5 profile eng: locked"), "{log}");
        assert!(log.contains("new profile with its own id"), "{log}");
    }
    assert_eq!(
        std::fs::read_to_string(engines.join("profiles.lock")).unwrap(),
        lock
    );
}

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
            .arg("--lock")
            .arg(repo().join("ocr/engines/profiles.lock"))
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
