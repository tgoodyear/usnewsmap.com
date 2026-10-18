use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Output};

struct Controller {
    fx: Fixture,
    config: PathBuf,
    project: PathBuf,
    root: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    id: String,
    jobs: PathBuf,
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn script(path: &Path, contents: &str) {
    write(path, contents);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

impl Controller {
    async fn new() -> Self {
        let fx = Fixture::new("eng").await;
        let id = fx.export().await;
        let base = fx.tmp.path();
        let project = base.join("project");
        let bin = project.join("bin");
        let root = base.join("controller");
        let log = base.join("calls");
        let jobs = base.join("candidates.json");
        write(&jobs, serde_json::to_vec(&vec![&id, &id]).unwrap());
        script(
            &bin.join("mysql"),
            r#"#!/bin/sh
case "$*" in
  *"SELECT id FROM host"*) printf '%b' "${HOST_IDS:-1\n2\n3\n4\n}";;
  *"usnm_spot_"*) printf '%s\n' "${SPOT_HOST:-2}";;
  *"SELECT r.hostid"*) printf '%s\n' "${SOURCE_HOST:-1}";;
  *"r.workunitid=w.id"*) printf '%b' "${GOLD_STATUS:-}";;
  *) exit 5;;
esac
"#,
        );
        script(
            &bin.join("dir_hier_path"),
            "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n",
        );
        script(
            &bin.join("create_work"),
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$TEST_CALLS"
exit "${CREATE_STATUS:-0}"
"#,
        );
        write(
            &project.join("config.xml"),
            "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>",
        );
        let defaults = base.join("mysql.cnf");
        write(&defaults, "[client]\npassword=fixture-only\n");
        std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o600)).unwrap();
        let ca = base.join("ca.pem");
        write(&ca, "fixture CA");
        let trust = base.join("trust.json");
        write(
            &trust,
            serde_json::to_vec(&serde_json::json!({
                "hosts": {IMAGE: [1,2]}, "max_cer": 0.1, "v": usnm_boinc::trust::TRUST_VERSION,
                "manifest_sha256": "b".repeat(64),
                "scope": {"language":"eng", "profile":"eng", "psm":3, "dpi":300}
            }))
            .unwrap(),
        );
        let prepared = base.join("prepared");
        write(&prepared.join("bundle.tar"), b"fixture bundle");
        let original = b"fixture original bytes";
        let original_sha = boinc::sha256(original);
        let original_name = format!("{original_sha}.jpg");
        write(&prepared.join("cache").join(&original_name), original);
        use md5::Digest as _;
        let original_md5: String = md5::Md5::digest(original)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        write(
            &prepared.join("prepared.json"),
            serde_json::to_vec(&serde_json::json!({
                "bundle": {"bytes": 14, "md5": "0".repeat(32), "name": "bundle.tar",
                    "sha256": boinc::sha256(b"fixture bundle"), "url": null},
                "bundle_version": 1, "manifest_sha256": "b".repeat(64),
                "crops": {"one": ["fixture", {"ref_chars":10,"ref_words":2}]},
                "remotes": [{"bytes":original.len(), "md5":original_md5,
                    "name":original_name, "sha256":original_sha, "url":"https://tile.loc.gov/fixture.jpg"}]
            }))
            .unwrap(),
        );
        let config = base.join("controller.json");
        write(
            &config,
            serde_json::to_vec(&serde_json::json!({
                "root": root, "project": project, "jobs_root": fx.options.root,
                "mysql_defaults": defaults, "mysql_ca": ca, "database": "fixture",
                "trust": trust, "prepared": prepared,
                "scope": {"language":"eng", "profile":"eng", "psm":3, "dpi":300},
                "engine": {"base":format!("fixture@sha256:{}", "c".repeat(64)),
                    "dpi":300, "image": IMAGE, "psm":3, "timeout_secs":300, "version":"5.5.0"},
                "max_hosts":1, "max_spots":1, "sample_every":1
            }))
            .unwrap(),
        );
        Self {
            fx,
            config,
            project,
            root,
            bin,
            log,
            id,
            jobs,
        }
    }

    fn command(&self, action: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_usnm-boinc"));
        c.args(["host-trust", action, "--config"]).arg(&self.config);
        c.env(
            "PATH",
            format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
        );
        c.env("TEST_CALLS", &self.log);
        c
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    fn spot(&self, execute: bool, fail: bool) -> Output {
        let mut c = self.command("spot-submit");
        c.arg("--jobs").arg(&self.jobs);
        if execute {
            c.arg("--execute");
        }
        if fail {
            c.env("CREATE_STATUS", "9");
        }
        c.output().unwrap()
    }

    fn compare(&self, host: u64) -> Output {
        self.command("spot-compare")
            .arg("--job")
            .arg(&self.id)
            .arg("--original-spool")
            .arg(self.fx.tmp.path().join("original-spool"))
            .arg("--spot-spool")
            .arg(self.fx.tmp.path().join("spot-spool"))
            .env("SPOT_HOST", host.to_string())
            .output()
            .unwrap()
    }

    fn results(&self) {
        let files = self.fx.result(&self.id);
        let spot = format!("usnm_spot_{}_1_2", self.id);
        for (spool, unit) in [
            ("original-spool", format!("usnm_{}", self.id)),
            ("spot-spool", spot),
        ] {
            for (name, from) in [
                ("result.json", &files.0),
                ("text.txt", &files.1),
                ("layout.xml", &files.2),
            ] {
                write(
                    &self
                        .fx
                        .tmp
                        .path()
                        .join(spool)
                        .join(format!("{unit}__file_{name}")),
                    std::fs::read(from).unwrap(),
                );
            }
        }
    }

    fn comparison(&self) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(
                self.root
                    .join("spots")
                    .join(&self.id)
                    .join("comparison.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }
}

fn success(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test]
async fn planning_is_read_only_and_onboarding_is_capped_retry_stable_and_does_not_starve() {
    let c = Controller::new().await;
    let out = c.command("onboard").output().unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[3]");
    success(&c.spot(false, false));
    assert!(!c.root.exists());
    assert!(c.calls().is_empty());
    success(&c.command("onboard").arg("--execute").output().unwrap());
    assert_eq!(c.calls().lines().count(), 1);
    assert!(c.calls().contains("--target_host 3"));
    success(&c.command("onboard").arg("--execute").output().unwrap());
    assert_eq!(c.calls().lines().count(), 2);
    assert!(c.calls().contains("--target_host 4"));
    let onboard = c.root.join("onboard");
    let campaign = std::fs::read_dir(onboard)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let shared = std::fs::metadata(campaign.join("shared/bundle.tar")).unwrap();
    for host in ["3", "4"] {
        let linked = std::fs::metadata(campaign.join(host).join("bundle.tar")).unwrap();
        assert_eq!((shared.dev(), shared.ino()), (linked.dev(), linked.ino()));
        let name = format!("{}.jpg", boinc::sha256(b"fixture original bytes"));
        let linked = std::fs::metadata(campaign.join(host).join("cache").join(&name)).unwrap();
        let source = std::fs::metadata(campaign.join("shared/cache").join(&name)).unwrap();
        assert_eq!((source.dev(), source.ino()), (linked.dev(), linked.ino()));
        assert_eq!(
            std::fs::read(
                c.project
                    .join("download")
                    .join(format!("usnm_gold_input_{name}"))
            )
            .unwrap(),
            b"fixture original bytes"
        );
    }
    success(&c.command("onboard").arg("--execute").output().unwrap());
    assert_eq!(c.calls().lines().count(), 2);
    assert!(!c.calls().contains("--target_host 1"));
    assert!(!c.calls().contains("--target_host 2"));
}

#[tokio::test]
async fn a_failed_gold_attempt_is_recorded_and_the_host_gets_another() {
    let c = Controller::new().await;
    success(&c.command("onboard").arg("--execute").output().unwrap());
    assert!(c.calls().contains("--target_host 3"));
    let host = std::fs::read_dir(c.root.join("onboard"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("3");
    // Unsent or in progress: host 3 keeps its unit and host 4 is next.
    let out = c
        .command("onboard")
        .env("GOLD_STATUS", "NULL\\tNULL\\tNULL\\tNULL\\tNULL\\tNULL\\n")
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[4]");
    // Over with a client error: the diagnostics are kept and host 3 gets a fresh unit.
    let failed = "11\\t5\\t3\\t5\\t255\\t4572726F72202D31\\n";
    let out = c
        .command("onboard")
        .env("GOLD_STATUS", failed)
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[3]");
    assert!(!host.join("failure.json").exists());
    success(
        &c.command("onboard")
            .arg("--execute")
            .env("GOLD_STATUS", failed)
            .output()
            .unwrap(),
    );
    assert_eq!(c.calls().lines().count(), 2);
    assert_eq!(c.calls().matches("--target_host 3").count(), 2);
    let failure: serde_json::Value =
        serde_json::from_slice(&std::fs::read(host.join("failure.json")).unwrap()).unwrap();
    assert_eq!(failure["results"][0]["exit_status"], 255);
    assert_eq!(failure["results"][0]["stderr"], "Error -1");
    assert!(host.join("retry/1/units/0000/submitted").is_file());
    // A success ends the retries; the trust workflow decides from its report.
    let out = c
        .command("onboard")
        .env("GOLD_STATUS", "12\\t5\\t1\\t5\\t0\\t\\n")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[4]");
}

#[tokio::test]
async fn onboarding_missing_or_changed_prepared_original_never_dispatches() {
    let c = Controller::new().await;
    let name = format!("{}.jpg", boinc::sha256(b"fixture original bytes"));
    let original = c.fx.tmp.path().join("prepared/cache").join(name);
    std::fs::remove_file(&original).unwrap();
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .output()
        .unwrap()
        .status
        .success());
    assert!(c.calls().is_empty());
    write(&original, b"changed original bytes");
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .output()
        .unwrap()
        .status
        .success());
    assert!(c.calls().is_empty());
}

#[tokio::test]
async fn assignment_is_opt_in_and_uncertain_gold_is_never_blindly_retried() {
    let c = Controller::new().await;
    write(
        &c.project.join("config.xml"),
        "<boinc><config><!-- <enable_assignment>1</enable_assignment> --></config></boinc>",
    );
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .output()
        .unwrap()
        .status
        .success());
    assert!(!c.spot(true, false).status.success());
    assert!(c.calls().is_empty());
    write(
        &c.project.join("config.xml"),
        "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>",
    );
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .env("CREATE_STATUS", "9")
        .output()
        .unwrap()
        .status
        .success());
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(c.calls().lines().count(), 1);
}

#[tokio::test]
async fn spot_replay_is_idempotent_and_uncertainty_never_retries() {
    let c = Controller::new().await;
    assert!(!c.spot(true, true).status.success());
    assert!(!c.spot(true, false).status.success());
    assert_eq!(c.calls().lines().count(), 1);
    let journal = c.root.join("spots").join(&c.id).join("submission.json");
    let mut s: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
    // Explicit operator reconciliation after finding the exact unit on the server.
    s["stage"] = "submitted".into();
    write(&journal, serde_json::to_vec(&s).unwrap());
    success(&c.spot(true, false));
    assert_eq!(c.calls().lines().count(), 1);
    assert!(c.calls().contains("--target_host 2"));
    assert!(c
        .calls()
        .contains(&format!("usnm_spot_{}_1_2_request_json", c.id)));
}

#[tokio::test]
async fn calibrated_scope_gates_spot_replay_and_revokes_existing_quorum() {
    let c = Controller::new().await;
    let path = c.fx.tmp.path().join("trust.json");
    let valid: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in ["language", "profile"] {
        let mut wrong = valid.clone();
        wrong["scope"][field] = "fra".into();
        write(&path, serde_json::to_vec(&wrong).unwrap());
        assert!(!c.spot(true, false).status.success(), "{field}");
        assert!(c.calls().is_empty());
    }
    write(&path, serde_json::to_vec(&valid).unwrap());
    success(&c.spot(true, false));
    c.results();
    success(&c.compare(2));
    assert_eq!(c.comparison()["agrees"], true);
    for field in ["language", "profile"] {
        let mut wrong = valid.clone();
        wrong["scope"][field] = "fra".into();
        write(&path, serde_json::to_vec(&wrong).unwrap());
        assert!(!c.compare(2).status.success(), "{field}");
        assert_eq!(c.comparison()["status"], "unverified");
        assert_eq!(c.comparison()["agrees"], false);
    }
    assert_eq!(c.calls().lines().count(), 1);
}

#[tokio::test]
async fn independent_canonical_results_validate_and_never_publish_or_change_trust() {
    let c = Controller::new().await;
    success(&c.spot(true, false));
    c.results();
    success(&c.compare(2));
    assert_eq!(c.comparison()["agrees"], true);
    assert_eq!(c.comparison()["quorum"], 2);
    let defaults = c.fx.tmp.path().join("mysql.cnf");
    std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!c.compare(2).status.success());
    assert_eq!(c.comparison()["agrees"], false);
    std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o600)).unwrap();
    success(&c.compare(2));
    std::fs::remove_file(&defaults).unwrap();
    assert!(!c.compare(2).status.success());
    assert_eq!(c.comparison()["agrees"], false);
    write(&defaults, "[client]\n");
    std::fs::set_permissions(&defaults, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!c.compare(1).status.success());
    assert_eq!(c.comparison()["agrees"], false);
    assert_eq!(c.comparison()["status"], "unverified");
    assert!(!c.compare(0).status.success());
    let text =
        c.fx.tmp
            .path()
            .join("spot-spool")
            .join(format!("usnm_spot_{}_1_2__file_text.txt", c.id));
    write(&text, b"CORRUPT");
    assert!(!c.compare(2).status.success());
    assert_eq!(c.comparison()["agrees"], false);
    c.results();
    let trust = c.fx.tmp.path().join("trust.json");
    write(
        &trust,
        serde_json::to_vec(&serde_json::json!({"hosts": {IMAGE: [1]}, "max_cer":0.1, "v":2}))
            .unwrap(),
    );
    assert!(!c.compare(2).status.success());
    assert!(c.fx.stores.out.list("pages").await.unwrap().is_empty());
    assert!(c.fx.stores.out.list("alto").await.unwrap().is_empty());
    assert_eq!(
        usnm_boinc::load(&c.fx.options.root, &c.id).unwrap().stage,
        Stage::Reserved
    );
}

#[tokio::test]
async fn spot_physical_inputs_survive_original_workunit_cleanup() {
    let c = Controller::new().await;
    let source = c.fx.options.root.join("jobs").join(&c.id);
    for (logical, physical) in [("page", "page"), ("request.json", "request_json")] {
        write(
            &c.project
                .join("download")
                .join(format!("{}_{}", c.id, physical)),
            std::fs::read(source.join(logical)).unwrap(),
        );
    }
    success(&c.spot(true, false));
    for (logical, physical) in [("page", "page"), ("request.json", "request_json")] {
        std::fs::remove_file(
            c.project
                .join("download")
                .join(format!("{}_{}", c.id, physical)),
        )
        .unwrap();
        let spot = c
            .project
            .join("download")
            .join(format!("usnm_spot_{}_1_2_{}", c.id, physical));
        assert_eq!(
            std::fs::read(spot).unwrap(),
            std::fs::read(source.join(logical)).unwrap()
        );
    }
    assert_eq!(c.calls().lines().count(), 1);
}
#[tokio::test]
async fn valid_but_different_text_is_a_failed_quorum_and_config_rejects_unsafe_defaults() {
    let c = Controller::new().await;
    success(&c.spot(true, false));
    c.results();
    let spool = c.fx.tmp.path().join("spot-spool");
    let unit = format!("usnm_spot_{}_1_2", c.id);
    let manifest = spool.join(format!("{unit}__file_result.json"));
    let mut m: ResultManifest = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    m.text_sha256 = boinc::sha256(b"DIFFERENT WORDS");
    write(&manifest, serde_json::to_vec(&m).unwrap());
    write(
        &spool.join(format!("{unit}__file_text.txt")),
        b"DIFFERENT WORDS",
    );
    assert!(!c.compare(2).status.success());
    assert_eq!(c.comparison()["status"], "verified");
    assert_eq!(c.comparison()["agrees"], false);
    std::fs::set_permissions(
        c.fx.tmp.path().join("mysql.cnf"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(!c.command("onboard").output().unwrap().status.success());
}

#[tokio::test]
async fn quarantine_view_is_read_only_and_excludes_released_or_failed_jobs() {
    let c = Controller::new().await;
    let path =
        c.fx.options
            .root
            .join("quarantine")
            .join(&c.id)
            .join("reason.json");
    write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "at":"2026-10-16T00:00:00Z", "engine_image":IMAGE, "host_id":null,
            "job": c.id, "reason":"no host id for this result", "workunit": format!("usnm_{}", c.id)
        }))
        .unwrap(),
    );
    let before = std::fs::read(&path).unwrap();
    let rows = usnm_boinc::host_trust::quarantine(&c.fx.options.root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reason, "no host id for this result");
    assert_eq!(rows[0].host_id, None);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut r = usnm_boinc::load(&c.fx.options.root, &c.id).unwrap();
    for stage in [Stage::Published, Stage::Failed] {
        r.stage = stage;
        usnm_boinc::save(&c.fx.options.root, &r).unwrap();
        assert!(usnm_boinc::host_trust::quarantine(&c.fx.options.root)
            .unwrap()
            .is_empty());
    }
    assert!(usnm_boinc::host_trust::quarantine(&c.root)
        .unwrap()
        .is_empty());
    assert!(!c.root.exists());
}

#[tokio::test]
async fn onboarding_checks_only_prepared_campaign_scope_and_manifest() {
    let c = Controller::new().await;
    let base = c.fx.tmp.path();
    let trust_path = base.join("trust.json");
    let prepared_json = base.join("prepared/prepared.json");
    let eng_manifest = "b".repeat(64);
    let deu_manifest = "d".repeat(64);

    // Coexisting English hosts {1, 8} and German hosts {1, 9}.
    let coexisting = serde_json::json!({
        "hosts": { IMAGE: [1, 8] },
        "manifest_sha256": eng_manifest,
        "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
        "max_cer": 0.08734328574534386,
        "policies": [
            {
                "hosts": { IMAGE: [1, 8] },
                "manifest_sha256": eng_manifest,
                "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
                "max_cer": 0.08734328574534386
            },
            {
                "hosts": { IMAGE: [1, 9] },
                "manifest_sha256": deu_manifest,
                "scope": { "language": "deu", "profile": "frak2021", "psm": 3, "dpi": 300 },
                "max_cer": 0.06755715386591318
            }
        ],
        "v": usnm_boinc::trust::TRUST_VERSION
    });
    write(&trust_path, serde_json::to_vec(&coexisting).unwrap());

    let mut cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&c.config).unwrap()).unwrap();
    cfg["max_hosts"] = serde_json::json!(10);
    write(&c.config, serde_json::to_vec(&cfg).unwrap());

    // English campaign skips {1, 8} (does NOT submit Gold to host 8) and targets [9].
    let out = c
        .command("onboard")
        .env("HOST_IDS", "1\\n8\\n9\\n")
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[9]");

    // German campaign skips {1, 9} and targets [8].
    let mut prep: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&prepared_json).unwrap()).unwrap();
    prep["manifest_sha256"] = serde_json::json!(deu_manifest);
    write(&prepared_json, serde_json::to_vec(&prep).unwrap());
    cfg["scope"] =
        serde_json::json!({"language": "deu", "profile": "frak2021", "psm": 3, "dpi": 300});
    write(&c.config, serde_json::to_vec(&cfg).unwrap());
    let out = c
        .command("onboard")
        .env("HOST_IDS", "1\\n8\\n9\\n")
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[8]");

    // Loading the English-only v4 policy must NOT skip hosts 1 and 8 for a German campaign.
    let v4_eng_only = serde_json::json!({
        "hosts": { IMAGE: [1, 8] },
        "manifest_sha256": eng_manifest,
        "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
        "max_cer": 0.08734328574534386,
        "v": 4
    });
    write(&trust_path, serde_json::to_vec(&v4_eng_only).unwrap());
    let out = c
        .command("onboard")
        .env("HOST_IDS", "1\\n8\\n9\\n")
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[1,8,9]");

    // Missing campaign scope on controller config is rejected for onboarding.
    cfg.as_object_mut().unwrap().remove("scope");
    write(&c.config, serde_json::to_vec(&cfg).unwrap());
    assert!(!c
        .command("onboard")
        .env("HOST_IDS", "1\\n8\\n9\\n")
        .output()
        .unwrap()
        .status
        .success());
}

#[tokio::test]
async fn onboarding_upgrade_respects_legacy_pending_and_uncertain_attempts() {
    let c = Controller::new().await;
    let base = c.fx.tmp.path();
    let prepared = usnm_boinc::gold::prepare::read(&base.join("prepared")).unwrap();
    let cfg: usnm_boinc::host_trust::Config =
        serde_json::from_slice(&std::fs::read(&c.config).unwrap()).unwrap();
    let legacy_campaign = boinc::sha256(&serde_json::to_vec(&(&prepared, &cfg.engine)).unwrap());
    let legacy_host3 = c.root.join("onboard").join(&legacy_campaign).join("3");
    let legacy_unit = legacy_host3.join("units/0000");

    // 1. An uncertain `submitting` marker in the pre-upgrade legacy campaign root refuses
    // both planning and execution instead of creating a fresh workunit in the scoped root.
    write(&legacy_unit.join("submitting"), b"");
    assert!(!c.command("onboard").output().unwrap().status.success());
    assert!(!c
        .command("onboard")
        .arg("--execute")
        .output()
        .unwrap()
        .status
        .success());
    assert!(c.calls().is_empty());

    // 2. Reconciling the legacy marker to `submitted` with a pending unit skips host 3 and
    // plans/dispatches host 4 instead of re-submitting host 3.
    let job = usnm_boinc::gold::submit::job(&prepared, &cfg.engine, 0).unwrap();
    write(
        &legacy_unit.join(usnm_boinc::gold::job::JOB_FILE),
        serde_json::to_vec_pretty(&job).unwrap(),
    );
    std::fs::remove_file(legacy_unit.join("submitting")).unwrap();
    write(&legacy_unit.join("submitted"), b"");

    let pending_status = "NULL\\tNULL\\tNULL\\tNULL\\tNULL\\tNULL\\n";
    let out = c
        .command("onboard")
        .env("GOLD_STATUS", pending_status)
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[4]");

    // 3. If the legacy attempt failed terminally, `onboard --execute` records `failure.json`
    // in the legacy root and issues a retry for host 3 in the scoped campaign root.
    let failed_status = "11\\t5\\t3\\t5\\t255\\t4572726F72202D31\\n";
    let out = c
        .command("onboard")
        .env("GOLD_STATUS", failed_status)
        .output()
        .unwrap();
    success(&out);
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "[3]");
    assert!(!legacy_host3.join("failure.json").exists());

    success(
        &c.command("onboard")
            .arg("--execute")
            .env("GOLD_STATUS", failed_status)
            .output()
            .unwrap(),
    );
    assert!(legacy_host3.join("failure.json").is_file());
    assert_eq!(c.calls().lines().count(), 1);
    assert!(c.calls().contains("--target_host 3"));
}
