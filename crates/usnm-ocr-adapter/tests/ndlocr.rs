//! Fixtures are produced by the production `ja-ocr/alto.py` converter.
use usnm_ocr_adapter::ndlocr::{self, Geometry, COMMIT};

const XML: &[u8] = include_bytes!("fixtures/ndlocr/page.xml");

#[test]
fn matches_jaocr_alto_byte_for_byte() {
    let alto = ndlocr::ndlocr_to_alto(
        XML,
        Geometry {
            dpi: 300,
            width: Some(8500),
            height: Some(16100),
        },
        "https://tile.loc.gov/a&b",
        COMMIT,
        3,
        "2026-10-05T12:00:00+00:00",
    )
    .unwrap();
    assert_eq!(
        std::str::from_utf8(&alto).unwrap(),
        include_str!("fixtures/ndlocr/page.alto.xml")
    );
    usnm_alto::check(&alto, &Default::default()).unwrap();
    let page = usnm_alto::parse(
        &alto,
        &usnm_alto::ParseOptions {
            dpi: Some(300.0),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        usnm_alto::text(&page),
        include_str!("fixtures/ndlocr/page.txt")
            .replace("\n次の記事", "\n\n次の記事")
            .replace("\nCamp News", "\n\nCamp News")
    );
}

#[test]
fn normalization_matches_japanese_job_not_latin_pipeline() {
    assert_eq!(
        ndlocr::normalize("  か\u{3099}\u{3000}\t新聞\n\n  ﬀ in-\n dustry\u{200d}\u{e000}\0"),
        "が 新聞\nﬀ in-\ndustry"
    );
}

#[test]
fn rejects_missing_page_bad_geometry_dtd_and_unclosed_xml() {
    for xml in [
        "<OCRDATASET/>",
        "<OCRDATASET><PAGE WIDTH=\"0\" HEIGHT=\"10\"/></OCRDATASET>",
        "<!DOCTYPE x><OCRDATASET/>",
        "<OCRDATASET><PAGE WIDTH=\"10\" HEIGHT=\"10\">",
        "<OCRDATASET><PAGE WIDTH=\"10\" HEIGHT=\"10\"/></OCRDATASET><OCRDATASET/>",
    ] {
        assert!(
            ndlocr::ndlocr_to_alto(xml.as_bytes(), Geometry::default(), "u", "e", 1, "t").is_err(),
            "{xml}"
        );
    }
}

#[test]
fn model_identity_is_stable_and_wrong_models_fail_startup() {
    assert!(ndlocr::models_digest().starts_with("sha256:"));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("COMMIT"), COMMIT).unwrap();
    std::fs::create_dir_all(dir.path().join("src/model")).unwrap();
    let name = ndlocr::MODELS_LOCK
        .lines()
        .find(|l| !l.starts_with('#'))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    std::fs::write(dir.path().join(name), "changed bytes").unwrap();
    assert!(format!("{:#}", ndlocr::hello(dir.path()).unwrap_err()).contains("checksum mismatch"));
}

#[cfg(unix)]
fn standin(body: &str) -> (tempfile::TempDir, ndlocr::Ndlocr, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let python = dir.path().join("python");
    std::fs::write(&python, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
    let image = dir.path().join("input");
    std::fs::write(&image, b"\xff\xd8\xfffake JPEG").unwrap();
    let engine = ndlocr::Ndlocr {
        root: dir.path().to_owned(),
        python,
        scratch: dir.path().to_owned(),
    };
    (dir, engine, image)
}

#[cfg(unix)]
#[test]
fn upstream_zero_exit_without_outputs_is_not_success() {
    let (_dir, engine, image) = standin("echo 'Images are not found.'; exit 0");
    let error = engine
        .ocr(
            &image,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(error.to_string().contains("produced no XML"), "{error}");
}

#[cfg(unix)]
#[test]
fn deadline_stops_upstream_and_removes_scratch() {
    let (dir, engine, image) = standin("sleep 30 &\nwait");
    let now = std::time::Instant::now();
    let error = engine
        .ocr(&image, now + std::time::Duration::from_millis(75))
        .unwrap_err();
    assert_eq!(error, usnm_ocr_adapter::ReadError::Timeout);
    assert!(now.elapsed() < std::time::Duration::from_secs(2));
    assert!(!std::fs::read_dir(dir.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("ndlocr-")));
}

#[cfg(unix)]
#[test]
fn reads_extensionless_boinc_image_and_normalizes_upstream_text() {
    let (_dir, engine, image) = standin(&format!(
        r#"
[ "$2" = '--sourceimg' ] || exit 1
case "$3" in *.jpg) ;; *) exit 1 ;; esac
[ "$4" = '--output' ] || exit 1
[ -d "$5" ] || exit 1
cat > "$5/page.xml" <<'XML'
{}
XML
printf '　新聞　　記事\n\n' > "$5/page.txt"
"#,
        std::str::from_utf8(XML).unwrap()
    ));
    let result = engine
        .ocr(
            &image,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(result.text, "新聞 記事");
    assert!(result.xml.starts_with(b"<OCRDATASET>"));
}

#[test]
fn catalog_has_all_pinned_models_and_credits_for_the_default_profile() {
    let catalog =
        usnm_ocr_route::Catalog::parse(include_str!("../../../ocr/engines/engines.toml")).unwrap();
    catalog.check_profile("ndlocr-lite", "default").unwrap();
    let credit = catalog
        .credit(&format!("ndlocr-lite/{COMMIT} default"))
        .unwrap();
    assert_eq!(credit.models.len(), 4);
    assert_eq!(credit.languages, ["jpn"]);
    let models: Vec<String> = ndlocr::MODELS_LOCK
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| l.split_whitespace().next().unwrap().to_owned())
        .collect();
    assert_eq!(
        credit
            .models
            .iter()
            .map(|m| m.name.clone())
            .collect::<Vec<_>>(),
        models
    );
    assert_eq!(credit.models[0].attribution.license, "LicenseRef-DINOv3");
}

#[cfg(unix)]
#[test]
fn decoder_errors_are_bad_input_not_engine_outages() {
    let (_dir, engine, image) =
        standin("echo 'PIL.UnidentifiedImageError: cannot identify image file' >&2; exit 1");
    let error = engine
        .ocr(
            &image,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        )
        .unwrap_err();
    assert!(matches!(error, usnm_ocr_adapter::ReadError::BadInput(_)));
}

#[cfg(unix)]
#[test]
fn contract_scratch_is_inside_the_writable_page_not_the_search_only_parent() {
    use std::os::unix::fs::PermissionsExt;
    use usnm_ocr_adapter::{Reader, Reading, Root};
    let (dir, engine, image) = standin(&format!(
        r#"
cat > "$5/page.xml" <<'XML'
{}
XML
printf '新聞' > "$5/page.txt"
"#,
        std::str::from_utf8(XML).unwrap()
    ));
    let parent = dir.path().join("out");
    let out = parent.join("page");
    std::fs::create_dir_all(&out).unwrap();
    let req = serde_json::from_value(serde_json::json!({
        "deadline_ms": 5000,
        "image": {"dpi":300,"dpi_source":"assumed","height":4000,"width":2400,
                  "path":"/in/page.jpg","sha256":"unused"},
        "language":"jpn","options":{},"out":"/out/page","profile":"default","run_id":"page"
    }))
    .unwrap();
    let mut reader = ndlocr::ContractReader {
        engine_root: engine.root,
        python: engine.python,
        root: Root(dir.path().to_owned()),
    };
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o111)).unwrap();
    let result = reader.read(
        &req,
        &image,
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    );
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(result.unwrap(), Reading::Alto { text, .. } if text == "新聞"));
    assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}
