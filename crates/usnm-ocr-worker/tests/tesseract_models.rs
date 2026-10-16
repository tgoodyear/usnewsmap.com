use std::collections::BTreeSet;

use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag};
use usnm_ocr_worker::engine::{
    candidates, models_digest, models_from_lock, route, CONTRACT, MODELS_LOCK, ROUTE, ROUTE_VERSION,
};

const ADDED: &[&str] = &[
    "spa", "fra", "ita", "yid", "heb", "dan", "nor", "fin", "swe", "lit", "slv", "hun", "slk",
];

fn installed() -> BTreeSet<String> {
    MODELS_LOCK
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter_map(|name| name.strip_prefix("tessdata/")?.strip_suffix(".traineddata"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn corpus_languages_route_to_pinned_models() {
    let installed = installed();
    for (marc, profile) in [
        ("spa", "spa"),
        ("fre", "fra"),
        ("ita", "ita"),
        ("yid", "yid"),
        ("heb", "heb"),
        ("dan", "dan"),
        ("nor", "nor"),
        ("fin", "fin"),
        ("swe", "swe"),
        ("lit", "lit"),
        ("slv", "slv"),
        ("hun", "hun"),
        ("slo", "slk"),
    ] {
        let iso3 = usnm_core::lang::marc_to_iso3(marc).unwrap();
        assert_eq!(
            candidates(iso3).first().map(|r| r.profile.as_str()),
            Some(profile)
        );
        assert_eq!(
            route(iso3, &installed).as_ref().map(|r| r.profile.as_str()),
            Some(profile)
        );
        assert!(models_digest(MODELS_LOCK, profile).is_some());
        assert_eq!(route(iso3, &BTreeSet::new()), None);
    }
    for iso3 in ["nor", "nob", "nno"] {
        assert_eq!(
            candidates(iso3)
                .iter()
                .map(|r| r.profile.as_str())
                .collect::<Vec<_>>(),
            ["nor"]
        );
        assert_eq!(
            route(iso3, &installed).as_ref().map(|r| r.profile.as_str()),
            Some("nor")
        );
    }
    assert_eq!(
        candidates("yid")
            .iter()
            .map(|r| r.profile.as_str())
            .collect::<Vec<_>>(),
        ["yid", "heb"]
    );
    let heb_only = BTreeSet::from(["heb".to_owned()]);
    assert_eq!(
        route("yid", &heb_only).as_ref().map(|r| r.profile.as_str()),
        Some("heb")
    );
}

#[test]
fn survey_and_production_pins_match() {
    let survey = include_str!("../../../scripts/ocr-survey/models.lock");
    for model in ADDED {
        let name = format!("tessdata/{model}.traineddata");
        let line = MODELS_LOCK
            .lines()
            .find(|line| line.split_whitespace().next() == Some(name.as_str()))
            .unwrap();
        let fields: Vec<_> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[1], format!("https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/e12c65a915945e4c28e237a9b52bc4a8f39a0cec/{model}.traineddata"));
        assert_eq!(fields[2].len(), 64);
        assert!(fields[2].bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(survey.lines().any(|entry| entry == line), "{model}");
    }
}

#[test]
fn adding_unrelated_models_preserves_existing_profile_identity() {
    let original_lock = MODELS_LOCK
        .lines()
        .filter(|line| {
            !ADDED.iter().any(|model| {
                line.split_whitespace().next()
                    == Some(format!("tessdata/{model}.traineddata").as_str())
            })
        })
        .collect::<Vec<_>>()
        .join("\n");
    // Golden digests computed from the production lock before #625.
    for (profile, expected) in [
        ("frak2021", "153518ad562f"),
        ("deu_latf+deu", "6a02c20e94d7"),
        ("deu", "62a3ec1ae209"),
        ("srp+srp_latn", "6fc3800498d3"),
        ("srp", "bf24b59a108b"),
        ("srp_latn", "ff15d2a541b6"),
        ("eng", "75ba1066e1a4"),
        ("rus", "6b20bc5ce792"),
        ("ces", "4b440da3fcf7"),
        ("pol", "fb9b58d34a38"),
    ] {
        let old = models_digest(&original_lock, profile).unwrap();
        let new = models_digest(MODELS_LOCK, profile).unwrap();
        assert_eq!(old, Digest::short(expected).unwrap());
        assert_eq!(old, new, "{profile}");
        assert_eq!(
            models_from_lock(&original_lock, profile),
            models_from_lock(MODELS_LOCK, profile),
            "{profile}"
        );
        // Hold the job's image pin fixed: republishing an image changes its
        // identity, but merely extending the lock must not change old jobs.
        let tag = |models| {
            OcrEngineTag::new(Fields {
                family: "tesseract",
                version: "5.5.0",
                profile,
                image: Digest::short(&"ab".repeat(32)).unwrap(),
                models,
                route: ROUTE,
                route_version: ROUTE_VERSION,
                contract: CONTRACT,
            })
            .unwrap()
        };
        let mut job: usnm_ocr_contract::boinc::Job = serde_json::from_value(serde_json::json!({
            "v": 1,
            "id": "",
            "message": {
                "v": 1,
                "page": "sn83045462/1890-05-03/ed-1/seq-7",
                "batch": "nbu_gage",
                "language": "deu",
                "reason": "bulk",
                "score": 0.62,
                "queued_at": "2026-10-10T12:00:00Z"
            },
            "batch_version": 1,
            "scan": {
                "url": "https://tile.loc.gov/example.jp2",
                "iiif_base": "https://tile.loc.gov/example",
                "sha256": "cd".repeat(32),
                "bytes": 1024,
                "md5": null
            },
            "engine": {
                "image": format!("sha256:{}", "ab".repeat(32)),
                "version": "5.5.0",
                "profile": profile,
                "models": old.to_string(),
                "psm": 3,
                "dpi": 300,
                "timeout_secs": 120
            }
        }))
        .unwrap();
        let original_identity = job.identity().unwrap();
        job.engine.models = new.to_string();
        assert_eq!(job.identity().unwrap(), original_identity, "{profile}");
        job.engine.models = "sha256:000000000000".into();
        assert_ne!(job.identity().unwrap(), original_identity, "{profile}");
        assert_eq!(tag(old), tag(new));
    }
}
