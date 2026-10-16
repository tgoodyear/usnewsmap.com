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
  *"SELECT id FROM host"*) printf '1\n2\n3\n4\n';;
  *"usnm_spot_"*) printf '%s\n' "${SPOT_HOST:-2}";;
  *"SELECT r.hostid"*) printf '%s\n' "${SOURCE_HOST:-1}";;
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
                "hosts": {IMAGE: [1,2]}, "max_cer": 0.1, "v": usnm_boinc::trust::TRUST_VERSION
            }))
            .unwrap(),
        );
        let prepared = base.join("prepared");
        write(&prepared.join("bundle.tar"), b"fixture bundle");
        write(
            &prepared.join("prepared.json"),
            serde_json::to_vec(&serde_json::json!({
                "bundle": {"bytes": 14, "md5": "0".repeat(32), "name": "bundle.tar",
                    "sha256": boinc::sha256(b"fixture bundle"), "url": null},
                "bundle_version": 1, "manifest_sha256": "b".repeat(64),
                "crops": {"one": ["fixture", {"ref_chars":10,"ref_words":2}]}, "remotes": []
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
    }
    success(&c.command("onboard").arg("--execute").output().unwrap());
    assert_eq!(c.calls().lines().count(), 2);
    assert!(!c.calls().contains("--target_host 1"));
    assert!(!c.calls().contains("--target_host 2"));
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
