use super::*;

const MANIFEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn scope() -> Scope {
    Scope {
        language: "eng".into(),
        profile: "eng".into(),
        psm: 3,
        dpi: 300,
    }
}

fn row(image: &str, host: Option<u64>, cer: Option<f64>) -> String {
    serde_json::json!({
        "completed_at": "2026-10-15T00:00:00Z",
        "bundle_version": 1,
        "engine_image": image,
        "host_id": host,
        "manifest_sha256": MANIFEST,
        "crops": 2,
        "skipped": 0,
        "complete_loss_crops": 0,
        "profiles": ["eng"],
        "psm": 3,
        "dpi": 300,
        "summary": [
            {"cer": cer, "crops": 2, "ref_chars": 10, "source": "all", "wer": cer},
            {"cer": 0.0, "crops": 1, "ref_chars": 5, "source": "bln600", "wer": 0.0},
        ],
    })
    .to_string()
}

#[test]
fn builds_from_reports_and_refuses_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a.jsonl");
    let b = tmp.path().join("b.jsonl");
    std::fs::write(
        &a,
        [
            row(&format!("sha256:{}", "a".repeat(64)), Some(1), Some(0.05)),
            row(&format!("sha256:{}", "a".repeat(64)), Some(2), Some(0.30)),
            row(&format!("sha256:{}", "a".repeat(64)), None, Some(0.01)),
            row(&format!("sha256:{}", "a".repeat(64)), Some(3), None),
        ]
        .join("\n"),
    )
    .unwrap();
    let image_a = format!("sha256:{}", "a".repeat(64));
    let image_b = format!("sha256:{}", "b".repeat(64));
    std::fs::write(&b, row(&image_b, Some(2), Some(0.10))).unwrap();
    let t = Trust::build_without_native(&[a, b], 0.10, MANIFEST, scope()).unwrap();
    assert_eq!(t.manifest_sha256.as_deref(), Some(MANIFEST));
    assert_eq!(t.hosts[&image_a], BTreeSet::from([1]));
    assert_eq!(t.hosts[&image_b], BTreeSet::from([2]));
    assert!(t.refuse(&image_a, Some(1)).is_none());
    assert!(t.refuse(&image_a, Some(2)).is_some());
    assert!(t.refuse(&image_b, Some(1)).is_some());
    assert!(t.refuse("sha256:cc", Some(1)).is_some());
    assert!(t.refuse("sha256:aa", None).is_some());
    let p = tmp.path().join("trust.json");
    std::fs::write(&p, serde_json::to_vec(&t).unwrap()).unwrap();
    assert_eq!(Trust::load(&p).unwrap(), t);
    assert!(Trust::build_without_native(&[], 2.0, MANIFEST, scope()).is_err());
    assert!(Trust::build_without_native(&[], 0.1, "not-a-manifest", scope()).is_err());
}

#[test]
fn newest_failure_revokes_in_any_file_order_and_ties_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("pass");
    let b = tmp.path().join("fail");
    let image = format!("sha256:{}", "a".repeat(64));
    std::fs::write(&a, row(&image, Some(42), Some(0.01))).unwrap();
    std::fs::write(
        &b,
        row(&image, Some(42), Some(0.8)).replace("2026-10-15", "2026-10-16"),
    )
    .unwrap();
    for paths in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
        assert!(Trust::build_without_native(&paths, 0.1, MANIFEST, scope())
            .unwrap()
            .refuse(&image, Some(42))
            .is_some());
    }
    std::fs::write(&b, row(&image, Some(42), None)).unwrap();
    assert!(
        Trust::build_without_native(&[a, b.clone()], 0.1, MANIFEST, scope())
            .unwrap()
            .hosts
            .is_empty()
    );
    std::fs::write(&b, row("mutable:latest", Some(42), Some(0.0))).unwrap();
    assert!(Trust::build_without_native(&[b], 0.1, MANIFEST, scope()).is_err());
}

#[test]
fn gold_issuance_treats_missing_or_legacy_lists_as_trusting_no_host() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("trust.json");
    let image = format!("sha256:{}", "a".repeat(64));
    assert!(Trust::load_or_none_trusted(&p).unwrap().hosts.is_empty());
    for version in [1, 2, 3, 4] {
        let legacy =
            serde_json::json!({"v": version, "max_cer": 0.1, "hosts": {image.clone(): [42]}});
        std::fs::write(&p, legacy.to_string()).unwrap();
        assert!(Trust::load(&p).is_err());
        let t = Trust::load_or_none_trusted(&p).unwrap();
        assert!(t.refuse(&image, Some(42)).is_some());
    }
    std::fs::write(&p, r#"{"v": 99, "max_cer": 0.1, "hosts": {}}"#).unwrap();
    assert!(Trust::load_or_none_trusted(&p).is_err());
    std::fs::write(&p, "not json").unwrap();
    assert!(Trust::load_or_none_trusted(&p).is_err());
    let current = Trust::build_without_native(&[], 0.1, MANIFEST, scope()).unwrap();
    std::fs::write(&p, serde_json::to_vec(&current).unwrap()).unwrap();
    assert_eq!(Trust::load_or_none_trusted(&p).unwrap(), current);
}

#[test]
fn complete_loss_and_incomplete_coverage_revoke_even_below_aggregate_threshold() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reports.jsonl");
    let image = format!("sha256:{}", "a".repeat(64));
    let good = row(&image, Some(42), Some(0.0551));
    for (field, value) in [("complete_loss_crops", 1), ("skipped", 1), ("crops", 0)] {
        let mut failed: serde_json::Value =
            serde_json::from_str(&row(&image, Some(42), Some(0.0827))).unwrap();
        failed[field] = serde_json::json!(value);
        failed["completed_at"] = serde_json::json!("2026-10-16T00:00:00Z");
        std::fs::write(&path, format!("{good}\n{failed}\n")).unwrap();
        assert!(
            Trust::build_without_native(
                std::slice::from_ref(&path),
                0.08734328574534386,
                MANIFEST,
                scope(),
            )
            .unwrap()
            .refuse(&image, Some(42))
            .is_some(),
            "{field}"
        );
    }
}

#[test]
fn rejects_wrong_challenges_and_missing_or_impossible_crop_quality() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reports.jsonl");
    let image = format!("sha256:{}", "a".repeat(64));
    let good: serde_json::Value = serde_json::from_str(&row(&image, Some(42), Some(0.01))).unwrap();
    let mut wrong = good.clone();
    wrong["manifest_sha256"] = serde_json::json!("c".repeat(64));
    std::fs::write(&path, wrong.to_string()).unwrap();
    assert!(
        Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope()).is_err()
    );
    let mut absent = good.clone();
    absent
        .as_object_mut()
        .unwrap()
        .remove("complete_loss_crops");
    std::fs::write(&path, absent.to_string()).unwrap();
    assert!(
        Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope()).is_err()
    );
    let mut impossible = good;
    impossible["complete_loss_crops"] = serde_json::json!(3);
    std::fs::write(&path, impossible.to_string()).unwrap();
    assert!(Trust::build_without_native(&[path], 0.1, MANIFEST, scope()).is_err());
}

#[test]
fn rejects_unscoped_or_mismatched_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reports.jsonl");
    let image = format!("sha256:{}", "a".repeat(64));
    let good: serde_json::Value = serde_json::from_str(&row(&image, Some(42), Some(0.01))).unwrap();
    for (field, value) in [
        ("psm", serde_json::json!(6)),
        ("dpi", serde_json::json!(150)),
    ] {
        let mut wrong = good.clone();
        wrong[field] = value;
        std::fs::write(&path, wrong.to_string()).unwrap();
        assert!(
            Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                .is_err(),
            "{field}"
        );
        wrong.as_object_mut().unwrap().remove(field);
        std::fs::write(&path, wrong.to_string()).unwrap();
        assert!(
            Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                .is_err(),
            "missing {field}"
        );
    }
    let mut missing = good;
    missing.as_object_mut().unwrap().remove("profiles");
    std::fs::write(&path, missing.to_string()).unwrap();
    assert!(Trust::build_without_native(&[path], 0.1, MANIFEST, scope()).is_err());
}

#[test]
fn newest_wrong_or_mixed_profile_report_revokes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("reports.jsonl");
    let image = format!("sha256:{}", "a".repeat(64));
    let good = row(&image, Some(42), Some(0.01));
    for kind in ["wrong-profile", "mixed-profiles"] {
        let mut failed: serde_json::Value = serde_json::from_str(&good).unwrap();
        failed["completed_at"] = serde_json::json!("2026-10-16T00:00:00Z");
        failed["profiles"] = serde_json::json!(["fra"]);
        if kind == "mixed-profiles" {
            failed["profiles"] = serde_json::json!(["eng", "fra"]);
        }
        std::fs::write(&path, format!("{good}\n{failed}\n")).unwrap();
        assert!(
            Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                .unwrap()
                .hosts
                .is_empty()
        );
    }
}

#[test]
fn rejects_missing_or_invalid_policy_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("trust.json");
    let valid = Trust::build_without_native(&[], 0.1, MANIFEST, scope()).unwrap();
    let mut missing = valid.clone();
    missing.scope = None;
    std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    assert!(Trust::load(&path).is_err());
    for (field, value) in [
        ("language", serde_json::json!("english")),
        ("profile", serde_json::json!("")),
        ("psm", serde_json::json!(6)),
        ("dpi", serde_json::json!(150)),
    ] {
        let mut invalid = serde_json::to_value(&valid).unwrap();
        invalid["scope"][field] = value;
        std::fs::write(&path, invalid.to_string()).unwrap();
        assert!(Trust::load(&path).is_err(), "{field}");
        let invalid: Trust = serde_json::from_value(invalid).unwrap();
        assert!(
            Trust::build_without_native(&[], 0.1, MANIFEST, invalid.scope.unwrap()).is_err(),
            "{field}"
        );
    }
}

fn job(image: &str, language: &str, profile: &str) -> usnm_ocr_contract::boinc::Job {
    usnm_ocr_contract::boinc::Job {
        retry_of: None,
        attempt: 0,
        v: 1,
        id: "a".repeat(64),
        message: usnm_ocr_contract::message::OcrMessage {
            v: 1,
            page: "sn83030214/1900-01-01/ed-1/seq-1".into(),
            batch: "batch_test_ver01".into(),
            language: language.into(),
            reason: usnm_ocr_contract::message::Reason::Bulk,
            score: Some(0.5),
            queued_at: "2026-10-17T00:00:00Z".into(),
        },
        batch_version: 1,
        scan: usnm_ocr_contract::boinc::Scan {
            url:
                "https://tile.loc.gov/image-services/iiif/service:ndnp:test/full/full/0/default.jpg"
                    .into(),
            iiif_base: "https://tile.loc.gov/image-services/iiif/service:ndnp:test".into(),
            bytes: 1234,
            sha256: "c".repeat(64),
            md5: None,
        },
        engine: usnm_ocr_contract::boinc::Engine {
            kind: usnm_ocr_contract::routing::EngineKind::Tesseract,
            image: image.into(),
            version: "tesseract 5.5.0".into(),
            profile: profile.into(),
            models: "sha256:001122334455".into(),
            psm: 3,
            dpi: 300,
            timeout_secs: 600,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn native_report_and_outcome(
    workunit_byte: char,
    result_id: u64,
    image: &str,
    host: u64,
    manifest: &str,
    language: &str,
    profile: &str,
    cer: f64,
    received_at: i64,
) -> (String, String) {
    let workunit = format!("usnm_gold_{}", workunit_byte.to_string().repeat(64));
    let report = serde_json::json!({
        "completed_at": "2026-10-18T00:00:00Z",
        "bundle_version": 1,
        "engine_image": image,
        "host_id": host,
        "manifest_sha256": manifest,
        "workunit": workunit,
        "native_result_id": result_id,
        "server_received_at": received_at,
        "language": language,
        "profile": profile,
        "crops": 12,
        "skipped": 0,
        "complete_loss_crops": 0,
        "profiles": [profile],
        "psm": 3,
        "dpi": 300,
        "summary": [
            {"cer": cer, "crops": 12, "ref_chars": 7786, "source": "all", "wer": cer},
        ],
    })
    .to_string();
    let outcome = serde_json::json!({
        "schema_version": 3,
        "app": "usnm_gold",
        "app_version_id": 40,
        "bundle_version": 1,
        "canonical": true,
        "dpi": 300,
        "engine_image": image,
        "host_id": host,
        "language": language,
        "manifest_sha256": manifest,
        "outcome": crate::gold::collect::BOINC_OUTCOME_SUCCESS,
        "exit_status": 0,
        "profile": profile,
        "psm": 3,
        "received_at": received_at,
        "result_id": result_id,
        "sent_at": received_at - 30,
        "server_state": crate::gold::collect::BOINC_SERVER_STATE_OVER,
        "workunit": workunit,
        "workunit_assimilate_state": crate::gold::collect::BOINC_ASSIMILATE_STATE_DONE,
        "workunit_result_count": 1,
    })
    .to_string();
    (report, outcome)
}

#[test]
fn v4_single_scope_english_policy_remains_backward_compatible() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("host-trust.json");
    let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
    let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
    let v4_json = serde_json::json!({
        "hosts": {
            image: [1, 8]
        },
        "manifest_sha256": eng_manifest,
        "scope": {
            "language": "eng",
            "profile": "eng",
            "psm": 3,
            "dpi": 300
        },
        "max_cer": 0.08734328574534386,
        "v": 4
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&v4_json).unwrap()).unwrap();

    let loaded = Trust::load(&path).unwrap();
    assert_eq!(loaded.v, 4);
    assert!(loaded.policies.is_empty());
    assert_eq!(Trust::load_or_none_trusted(&path).unwrap(), loaded);
    assert_eq!(
        loaded.policy_for("eng", "eng").unwrap().hosts[image],
        BTreeSet::from([1, 8])
    );
    assert!(loaded.policy_for("deu", "frak2021").is_none());

    let eng_job = job(image, "eng", "eng");
    let deu_job = job(image, "deu", "frak2021");
    let deu_scope = Scope {
        language: "deu".into(),
        profile: "frak2021".into(),
        psm: 3,
        dpi: 300,
    };
    let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";
    assert!(loaded.refuse_job(&eng_job, Some(1)).is_none());
    assert!(loaded.refuse_job(&eng_job, Some(8)).is_none());
    assert!(loaded.refuse_job(&eng_job, Some(2)).is_some());
    assert!(loaded
        .refuse_job(&deu_job, Some(1))
        .is_some_and(|r| r.contains("outside calibrated trust")));
    assert!(loaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(1))
        .is_none());
    assert!(loaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(8))
        .is_none());
    assert!(loaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(9))
        .is_some());
    // Loading an English-only v4 policy must NOT skip hosts 1 and 8 for a German campaign or
    // an uncalibrated manifest.
    assert!(loaded
        .refuse_campaign(image, deu_manifest, &deu_scope, Some(1))
        .is_some());
    assert!(loaded
        .refuse_campaign(image, deu_manifest, &deu_scope, Some(8))
        .is_some());
    assert!(loaded
        .refuse_campaign(image, deu_manifest, &scope(), Some(1))
        .is_some());

    // A v4 artifact with multi-scope `policies` is rejected.
    let mut invalid_v4 = v4_json;
    invalid_v4["policies"] = serde_json::json!([{
        "hosts": { image: [1, 8] },
        "manifest_sha256": eng_manifest,
        "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
        "max_cer": 0.08734328574534386
    }]);
    std::fs::write(&path, invalid_v4.to_string()).unwrap();
    assert!(Trust::load(&path).is_err());
}

#[test]
fn promoting_deu_frak2021_coexists_without_clobbering_eng_and_enforces_per_scope_trust() {
    let tmp = tempfile::tempdir().unwrap();
    let trust_path = tmp.path().join("trust.json");
    let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
    let other_image = format!("sha256:{}", "f".repeat(64));
    let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
    let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";

    // Start with the existing v4 English policy on disk (hosts 1 and 8).
    let v4_eng = serde_json::json!({
        "hosts": { image: [1, 8] },
        "manifest_sha256": eng_manifest,
        "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
        "max_cer": 0.08734328574534386,
        "v": 4
    });
    std::fs::write(&trust_path, serde_json::to_vec_pretty(&v4_eng).unwrap()).unwrap();

    // Build and promote a calibrated German Fraktur policy (hosts 1 and 9 pass; host 8 fails CER).
    let deu_reports = tmp.path().join("deu-reports.jsonl");
    let deu_outcomes = tmp.path().join("deu-outcomes.jsonl");
    let (r1, o1) = native_report_and_outcome(
        '1',
        101,
        image,
        1,
        deu_manifest,
        "deu",
        "frak2021",
        0.0539,
        1_700_000_100,
    );
    let (r8, o8) = native_report_and_outcome(
        '2',
        102,
        image,
        8,
        deu_manifest,
        "deu",
        "frak2021",
        0.0993,
        1_700_000_200,
    );
    let (r9, o9) = native_report_and_outcome(
        '3',
        103,
        image,
        9,
        deu_manifest,
        "deu",
        "frak2021",
        0.0610,
        1_700_000_300,
    );
    std::fs::write(&deu_reports, [r1, r8, r9].join("\n")).unwrap();
    std::fs::write(&deu_outcomes, [o1, o8, o9].join("\n")).unwrap();

    let deu_scope = Scope {
        language: "deu".into(),
        profile: "frak2021".into(),
        psm: 3,
        dpi: 300,
    };
    let deu_built = Trust::build_with_outcomes(
        std::slice::from_ref(&deu_reports),
        std::slice::from_ref(&deu_outcomes),
        0.06755715386591318,
        deu_manifest,
        deu_scope.clone(),
    )
    .unwrap();
    assert_eq!(deu_built.hosts[image], BTreeSet::from([1, 9]));

    // Promote into the existing trust_path; must merge alongside eng/eng rather than clobbering it.
    let merged = deu_built.promote_to(&trust_path, None).unwrap();
    let reloaded = Trust::load(&trust_path).unwrap();
    assert_eq!(merged, reloaded);
    assert_eq!(reloaded.v, TRUST_VERSION);
    // Primary top-level eng/eng scope and hosts remain intact.
    assert_eq!(reloaded.scope, Some(scope()));
    assert_eq!(reloaded.manifest_sha256.as_deref(), Some(eng_manifest));
    assert_eq!(reloaded.hosts[image], BTreeSet::from([1, 8]));
    assert_eq!(reloaded.policies.len(), 2);

    let eng_policy = reloaded.policy_for("eng", "eng").unwrap();
    assert_eq!(eng_policy.hosts[image], BTreeSet::from([1, 8]));
    assert_eq!(eng_policy.manifest_sha256, eng_manifest);

    let deu_policy = reloaded.policy_for("deu", "frak2021").unwrap();
    assert_eq!(deu_policy.hosts[image], BTreeSet::from([1, 9]));
    assert_eq!(deu_policy.manifest_sha256, deu_manifest);

    let eng_job = job(image, "eng", "eng");
    let deu_job = job(image, "deu", "frak2021");

    // Host 1 is trusted in both scopes.
    assert!(reloaded.refuse_job(&eng_job, Some(1)).is_none());
    assert!(reloaded.refuse_job(&deu_job, Some(1)).is_none());

    // Host 8 is trusted ONLY for eng/eng -> allowed on eng/eng, strictly refused on deu/frak2021.
    assert!(reloaded.refuse_job(&eng_job, Some(8)).is_none());
    assert!(reloaded
        .refuse_job(&deu_job, Some(8))
        .is_some_and(|r| r.contains("in scope deu/frak2021")));

    // Host 9 is trusted ONLY for deu/frak2021 -> allowed on deu/frak2021, strictly refused on eng/eng.
    assert!(reloaded.refuse_job(&deu_job, Some(9)).is_none());
    assert!(reloaded
        .refuse_job(&eng_job, Some(9))
        .is_some_and(|r| r.contains("in scope eng/eng")));

    // Cross-scope profile/language combinations and wrong images are refused even for Host 1.
    assert!(reloaded
        .refuse_job(&job(image, "deu", "eng"), Some(1))
        .is_some_and(|r| r.contains("outside calibrated trust")));
    assert!(reloaded
        .refuse_job(&job(image, "eng", "frak2021"), Some(1))
        .is_some_and(|r| r.contains("outside calibrated trust")));
    assert!(reloaded
        .refuse_job(&job(&other_image, "deu", "frak2021"), Some(1))
        .is_some());

    // Campaign-scoped onboarding eligibility: English skips {1, 8} and offers Gold to {9};
    // German skips {1, 9} and offers Gold to {8}; unscoped `refuse` fails closed on multi-scope.
    assert!(reloaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(1))
        .is_none());
    assert!(reloaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(8))
        .is_none());
    assert!(reloaded
        .refuse_campaign(image, eng_manifest, &scope(), Some(9))
        .is_some());
    assert!(reloaded
        .refuse_campaign(image, deu_manifest, &deu_scope, Some(1))
        .is_none());
    assert!(reloaded
        .refuse_campaign(image, deu_manifest, &deu_scope, Some(9))
        .is_none());
    assert!(reloaded
        .refuse_campaign(image, deu_manifest, &deu_scope, Some(8))
        .is_some());
    assert!(reloaded.refuse(image, Some(1)).is_some());

    // A later terminal client error in deu/frak2021 revokes Host 9 in deu/frak2021 without touching eng/eng.
    let revoke_outcome = serde_json::json!({
        "schema_version": 3,
        "app": "usnm_gold",
        "app_version_id": 40,
        "bundle_version": 1,
        "canonical": false,
        "dpi": 300,
        "engine_image": image,
        "host_id": 9,
        "language": "deu",
        "manifest_sha256": deu_manifest,
        "outcome": crate::gold::collect::BOINC_OUTCOME_CLIENT_ERROR,
        "exit_status": 1,
        "profile": "frak2021",
        "psm": 3,
        "received_at": 1_700_000_400,
        "result_id": 104,
        "sent_at": 1_700_000_350,
        "server_state": crate::gold::collect::BOINC_SERVER_STATE_OVER,
        "workunit": format!("usnm_gold_{}", "4".repeat(64)),
        "workunit_assimilate_state": crate::gold::collect::BOINC_ASSIMILATE_STATE_DONE,
        "workunit_result_count": 1,
    })
    .to_string();
    let existing_outcomes = std::fs::read_to_string(&deu_outcomes).unwrap();
    std::fs::write(
        &deu_outcomes,
        format!("{existing_outcomes}\n{revoke_outcome}"),
    )
    .unwrap();
    let deu_rebuilt = Trust::build_with_outcomes(
        std::slice::from_ref(&deu_reports),
        std::slice::from_ref(&deu_outcomes),
        0.06755715386591318,
        deu_manifest,
        deu_scope,
    )
    .unwrap();
    let updated = deu_rebuilt.promote_to(&trust_path, None).unwrap();
    assert!(updated.refuse_job(&deu_job, Some(9)).is_some());
    assert!(updated.refuse_job(&deu_job, Some(1)).is_none());
    assert!(updated.refuse_job(&eng_job, Some(1)).is_none());
    assert!(updated.refuse_job(&eng_job, Some(8)).is_none());

    // Corrupting `policies` to disagree with top-level primary scope or duplicating a scope fails closed.
    let mut mismatch = serde_json::to_value(&updated).unwrap();
    mismatch["policies"][1]["max_cer"] = serde_json::json!(0.5);
    std::fs::write(&trust_path, mismatch.to_string()).unwrap();
    assert!(Trust::load(&trust_path).is_err());

    let mut duplicate = serde_json::to_value(&updated).unwrap();
    let dup_entry = duplicate["policies"][0].clone();
    duplicate["policies"]
        .as_array_mut()
        .unwrap()
        .push(dup_entry);
    std::fs::write(&trust_path, duplicate.to_string()).unwrap();
    assert!(Trust::load(&trust_path).is_err());
}

#[test]
fn promote_to_preserves_distinct_destination_policies_and_locks_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let out_dir = tmp.path().join("shared");
    let stage_dir = tmp.path().join("stage");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::create_dir_all(&stage_dir).unwrap();
    let out_path = out_dir.join("trust.json");
    let merge_path = stage_dir.join("merge-source.json");
    let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
    let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
    let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";
    let fra_manifest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

    // Existing destination `--out` holds `eng/eng` (hosts {1, 8}) and an older `deu/frak2021` (host {9}).
    let (eng_r1, eng_o1) = native_report_and_outcome(
        '1',
        1,
        image,
        1,
        eng_manifest,
        "eng",
        "eng",
        0.04,
        1_700_000_100,
    );
    let (eng_r8, eng_o8) = native_report_and_outcome(
        '2',
        2,
        image,
        8,
        eng_manifest,
        "eng",
        "eng",
        0.05,
        1_700_000_200,
    );
    let eng_reports = stage_dir.join("eng-reports.jsonl");
    let eng_outcomes = stage_dir.join("eng-outcomes.jsonl");
    std::fs::write(&eng_reports, [eng_r1, eng_r8].join("\n")).unwrap();
    std::fs::write(&eng_outcomes, [eng_o1, eng_o8].join("\n")).unwrap();
    let eng_trust = Trust::build_with_outcomes(
        std::slice::from_ref(&eng_reports),
        std::slice::from_ref(&eng_outcomes),
        0.08734328574534386,
        eng_manifest,
        scope(),
    )
    .unwrap();

    let deu_scope = Scope {
        language: "deu".into(),
        profile: "frak2021".into(),
        psm: 3,
        dpi: 300,
    };
    let (deu_r9, deu_o9) = native_report_and_outcome(
        '3',
        3,
        image,
        9,
        deu_manifest,
        "deu",
        "frak2021",
        0.05,
        1_700_000_300,
    );
    let deu_old_reports = stage_dir.join("deu-old-reports.jsonl");
    let deu_old_outcomes = stage_dir.join("deu-old-outcomes.jsonl");
    std::fs::write(&deu_old_reports, &deu_r9).unwrap();
    std::fs::write(&deu_old_outcomes, &deu_o9).unwrap();
    let deu_old = Trust::build_with_outcomes(
        std::slice::from_ref(&deu_old_reports),
        std::slice::from_ref(&deu_old_outcomes),
        0.06755715386591318,
        deu_manifest,
        deu_scope.clone(),
    )
    .unwrap();
    eng_trust.promote_to(&out_path, None).unwrap();
    deu_old.promote_to(&out_path, None).unwrap();

    // Distinct `--merge` file has `fra/fra` (host {7}) and `deu/frak2021` (hosts {1, 9}),
    // but does NOT contain `eng/eng`.
    let fra_scope = Scope {
        language: "fra".into(),
        profile: "fra".into(),
        psm: 3,
        dpi: 300,
    };
    let (fra_r7, fra_o7) = native_report_and_outcome(
        '4',
        4,
        image,
        7,
        fra_manifest,
        "fra",
        "fra",
        0.03,
        1_700_000_400,
    );
    let fra_reports = stage_dir.join("fra-reports.jsonl");
    let fra_outcomes = stage_dir.join("fra-outcomes.jsonl");
    std::fs::write(&fra_reports, &fra_r7).unwrap();
    std::fs::write(&fra_outcomes, &fra_o7).unwrap();
    let fra_trust = Trust::build_with_outcomes(
        std::slice::from_ref(&fra_reports),
        std::slice::from_ref(&fra_outcomes),
        0.07,
        fra_manifest,
        fra_scope,
    )
    .unwrap();
    let (deu_r1, deu_o1) = native_report_and_outcome(
        '5',
        5,
        image,
        1,
        deu_manifest,
        "deu",
        "frak2021",
        0.05,
        1_700_000_500,
    );
    let deu_mid_reports = stage_dir.join("deu-mid-reports.jsonl");
    let deu_mid_outcomes = stage_dir.join("deu-mid-outcomes.jsonl");
    std::fs::write(&deu_mid_reports, [deu_r1.clone(), deu_r9].join("\n")).unwrap();
    std::fs::write(&deu_mid_outcomes, [deu_o1.clone(), deu_o9].join("\n")).unwrap();
    let deu_mid = Trust::build_with_outcomes(
        std::slice::from_ref(&deu_mid_reports),
        std::slice::from_ref(&deu_mid_outcomes),
        0.06755715386591318,
        deu_manifest,
        deu_scope.clone(),
    )
    .unwrap();
    fra_trust.promote_to(&merge_path, None).unwrap();
    deu_mid.promote_to(&merge_path, None).unwrap();

    // Newly promoted `self` updates `deu/frak2021` to host {1} only (host 9 revoked).
    let deu_new_reports = stage_dir.join("deu-new-reports.jsonl");
    let deu_new_outcomes = stage_dir.join("deu-new-outcomes.jsonl");
    std::fs::write(&deu_new_reports, &deu_r1).unwrap();
    std::fs::write(&deu_new_outcomes, &deu_o1).unwrap();
    let deu_new = Trust::build_with_outcomes(
        std::slice::from_ref(&deu_new_reports),
        std::slice::from_ref(&deu_new_outcomes),
        0.06755715386591318,
        deu_manifest,
        deu_scope,
    )
    .unwrap();

    // Promoting `deu_new` with `--merge merge_path --out out_path` must preserve destination-only
    // `eng/eng` ({1, 8}), merge `fra/fra` ({7}) from `merge_path`, and apply `deu_new` ({1}) with
    // precedence `out < merge_from < self`.
    let promoted = deu_new.promote_to(&out_path, Some(&merge_path)).unwrap();
    assert_eq!(promoted.scope, Some(scope()));
    assert_eq!(
        promoted.policy_for("eng", "eng").unwrap().hosts[image],
        BTreeSet::from([1, 8])
    );
    assert_eq!(
        promoted.policy_for("fra", "fra").unwrap().hosts[image],
        BTreeSet::from([7])
    );
    assert_eq!(
        promoted.policy_for("deu", "frak2021").unwrap().hosts[image],
        BTreeSet::from([1])
    );

    // Holding the destination directory lock blocks/refuses a competing `promote_to` across the
    // entire read–merge–write operation without overwriting `out_path`.
    let held_lock = crate::Lock::take(&out_dir).unwrap();
    assert!(deu_old.promote_to(&out_path, None).is_err());
    drop(held_lock);
    assert_eq!(
        Trust::load(&out_path)
            .unwrap()
            .policy_for("deu", "frak2021")
            .unwrap()
            .hosts[image],
        BTreeSet::from([1])
    );

    // `promote_to` cleanly upgrades a legacy v1–v3 or unscoped v4 `--out` file by discarding
    // uncalibrated legacy eligibility while keeping `Trust::load` strict for collection.
    for version in [1, 2, 3, 4] {
        let legacy = serde_json::json!({
            "v": version,
            "max_cer": 0.1,
            "hosts": { image: [42, 99] }
        });
        std::fs::write(&out_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        assert!(Trust::load(&out_path).is_err());
        let upgraded = eng_trust.promote_to(&out_path, None).unwrap();
        assert_eq!(upgraded, eng_trust);
        assert_eq!(Trust::load(&out_path).unwrap(), eng_trust);
        assert_eq!(
            upgraded.campaign_trusted_hosts(image, eng_manifest, &scope()),
            Some(&BTreeSet::from([1, 8]))
        );
    }

    // Corrupt or unsupported-version `--out` files still fail closed during promotion.
    std::fs::write(&out_path, r#"{"v": 99, "max_cer": 0.1, "hosts": {}}"#).unwrap();
    assert!(eng_trust.promote_to(&out_path, None).is_err());

    // An oversized merged policy is rejected before replacing the destination file, leaving
    // the existing readable trust artifact unchanged.
    std::fs::remove_file(&out_path).unwrap();
    eng_trust.promote_to(&out_path, None).unwrap();
    let before_oversized = std::fs::read(&out_path).unwrap();
    assert!(deu_old
        .promote_to_with_limit(&out_path, None, before_oversized.len())
        .is_err());
    assert_eq!(std::fs::read(&out_path).unwrap(), before_oversized);
    assert_eq!(Trust::load(&out_path).unwrap(), eng_trust);
}

const ADMIT_IMAGE: &str = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
const ADMIT_MANIFEST: &str = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
const ADMIT_CER: f64 = 0.08734328574534386;

fn two_scope_list() -> serde_json::Value {
    let eng = serde_json::json!({
        "hosts": {ADMIT_IMAGE: [1, 8]}, "manifest_sha256": ADMIT_MANIFEST,
        "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
        "max_cer": ADMIT_CER,
    });
    let deu = serde_json::json!({
        "hosts": {ADMIT_IMAGE: [1, 2, 3]}, "manifest_sha256": "7".repeat(64),
        "scope": {"language": "deu", "profile": "frak2021", "psm": 3, "dpi": 300},
        "max_cer": 0.2,
    });
    serde_json::json!({
        "hosts": {ADMIT_IMAGE: [1, 8]}, "manifest_sha256": ADMIT_MANIFEST,
        "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
        "max_cer": ADMIT_CER, "policies": [deu, eng], "v": TRUST_VERSION,
    })
}

#[test]
fn admitting_hosts_adds_to_one_scope_and_keeps_every_other_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("trust.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let admitted = Trust::admit_hosts(
        &path,
        None,
        &scope(),
        ADMIT_MANIFEST,
        ADMIT_CER,
        ADMIT_IMAGE,
        &BTreeSet::from([2, 8]),
    )
    .unwrap()
    .unwrap();
    assert_eq!(admitted.added, BTreeSet::from([2]));
    assert_eq!(
        admitted.before_sha256,
        usnm_ocr_contract::boinc::sha256(&before)
    );
    let after = std::fs::read(&path).unwrap();
    assert_eq!(
        admitted.after_sha256,
        usnm_ocr_contract::boinc::sha256(&after)
    );
    let t = Trust::load(&path).unwrap();
    let eng = t.policy_for("eng", "eng").unwrap();
    assert_eq!(eng.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 8]));
    assert_eq!(
        (eng.max_cer, eng.manifest_sha256.as_str()),
        (ADMIT_CER, ADMIT_MANIFEST)
    );
    assert_eq!(t.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 8]));
    let deu = t.policy_for("deu", "frak2021").unwrap();
    assert_eq!(deu.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 3]));
    assert_eq!(deu.max_cer, 0.2);
    // Again: nothing new, so the file is not rewritten.
    let again = Trust::admit_hosts(
        &path,
        None,
        &scope(),
        ADMIT_MANIFEST,
        ADMIT_CER,
        ADMIT_IMAGE,
        &BTreeSet::from([2]),
    )
    .unwrap()
    .unwrap();
    assert!(again.added.is_empty());
    assert_eq!(again.before_sha256, again.after_sha256);
    assert_eq!(std::fs::read(&path).unwrap(), after);
}

#[test]
fn admitting_into_a_single_scope_list_or_a_new_image_keeps_the_calibration() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("trust.json");
    let list = serde_json::json!({
        "hosts": {ADMIT_IMAGE: [1]}, "manifest_sha256": ADMIT_MANIFEST,
        "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
        "max_cer": ADMIT_CER, "v": 4,
    });
    std::fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
    let other = format!("sha256:{}", "e".repeat(64));
    Trust::admit_hosts(
        &path,
        None,
        &scope(),
        ADMIT_MANIFEST,
        ADMIT_CER,
        &other,
        &BTreeSet::from([5]),
    )
    .unwrap();
    let t = Trust::load(&path).unwrap();
    assert_eq!(t.v, 4);
    assert!(t.policies.is_empty());
    assert_eq!(t.hosts[ADMIT_IMAGE], BTreeSet::from([1]));
    assert_eq!(t.hosts[&other], BTreeSet::from([5]));
    assert_eq!(t.max_cer, ADMIT_CER);
}

#[test]
fn admitting_refuses_a_changed_calibration_an_unknown_scope_or_a_legacy_list() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("trust.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let hosts = BTreeSet::from([2]);
    let fra = Scope {
        language: "fra".into(),
        profile: "fra".into(),
        psm: 3,
        dpi: 300,
    };
    for (scope, manifest, cer, image, hosts) in [
        (
            scope(),
            "c".repeat(64),
            ADMIT_CER,
            ADMIT_IMAGE.to_owned(),
            hosts.clone(),
        ),
        (
            scope(),
            ADMIT_MANIFEST.to_owned(),
            0.09,
            ADMIT_IMAGE.to_owned(),
            hosts.clone(),
        ),
        (
            fra,
            ADMIT_MANIFEST.to_owned(),
            ADMIT_CER,
            ADMIT_IMAGE.to_owned(),
            hosts.clone(),
        ),
        (
            scope(),
            ADMIT_MANIFEST.to_owned(),
            ADMIT_CER,
            "worker:latest".to_owned(),
            hosts.clone(),
        ),
        (
            scope(),
            ADMIT_MANIFEST.to_owned(),
            ADMIT_CER,
            ADMIT_IMAGE.to_owned(),
            BTreeSet::from([0]),
        ),
        (
            scope(),
            ADMIT_MANIFEST.to_owned(),
            ADMIT_CER,
            ADMIT_IMAGE.to_owned(),
            BTreeSet::new(),
        ),
    ] {
        assert!(Trust::admit_hosts(&path, None, &scope, &manifest, cer, &image, &hosts).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    let legacy = serde_json::json!({"hosts": {ADMIT_IMAGE: [1]}, "max_cer": 0.1, "v": 3});
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert!(Trust::admit_hosts(
        &path,
        None,
        &scope(),
        ADMIT_MANIFEST,
        ADMIT_CER,
        ADMIT_IMAGE,
        &BTreeSet::from([2])
    )
    .is_err());
    assert!(Trust::admit_hosts(
        &tmp.path().join("missing.json"),
        None,
        &scope(),
        ADMIT_MANIFEST,
        ADMIT_CER,
        ADMIT_IMAGE,
        &BTreeSet::from([2])
    )
    .is_err());
}

#[test]
fn admitting_against_an_expected_version_writes_nothing_once_the_list_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("trust.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
    let before = std::fs::read(&path).unwrap();
    let hosts = BTreeSet::from([2]);
    let stale = "0".repeat(64);
    let admit = |expected: &str| {
        Trust::admit_hosts(
            &path,
            Some(expected),
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            ADMIT_IMAGE,
            &hosts,
        )
        .unwrap()
    };
    assert!(admit(&stale).is_none());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let current = usnm_ocr_contract::boinc::sha256(&before);
    assert_eq!(admit(&current).unwrap().added, hosts);
}
