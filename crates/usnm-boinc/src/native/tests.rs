use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use usnm_ocr_contract::boinc::Job;
use usnm_ocr_quality::slot::{write_mmap_channel, PROCESS_CONTROL_REQUEST_OFFSET};

fn file(sha256: &str) -> BuildFile {
    BuildFile {
        bytes: 10,
        sha256: sha256.to_owned(),
    }
}

fn manifest() -> Manifest {
    let mut files: BTreeMap<_, _> = locked_models()
        .unwrap()
        .into_iter()
        .map(|(name, sum)| (name, file(&sum)))
        .collect();
    files.insert(WORKER_FILE.into(), file(&"a".repeat(64)));
    files.insert(TESSERACT_FILE.into(), file(&"b".repeat(64)));
    for (name, (_, sum)) in locked_notices().unwrap() {
        files.insert(name, file(&sum));
    }
    Manifest {
        engine_version: "5.5.0".into(),
        files,
        platform: PLATFORM.into(),
        v: MANIFEST_VERSION,
    }
}

fn build() -> Build {
    let manifest = manifest();
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    manifest.build(&bytes).unwrap()
}

fn page(image: &str) -> Job {
    let mut job = crate::bundle::work::tests::job(1);
    job.engine.image = image.to_owned();
    job.id = job.identity().unwrap();
    job.validate().unwrap();
    job
}

fn write_page(dir: &Path, image: &str) {
    std::fs::write(
        dir.join("request.json"),
        serde_json::to_vec(&page(image)).unwrap(),
    )
    .unwrap();
}

#[test]
fn the_image_is_the_manifests_digest_and_the_base_the_engines() {
    let manifest = manifest();
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    let build = manifest.clone().build(&bytes).unwrap();
    assert_eq!(build.image, format!("sha256:{}", boinc::sha256(&bytes)));
    assert_eq!(build.base, format!("sha256:{}", "b".repeat(64)));
    // Any file of the build changing changes the image.
    let mut other = manifest;
    other.files.get_mut(WORKER_FILE).unwrap().sha256 = "c".repeat(64);
    let bytes = serde_json::to_vec_pretty(&other).unwrap();
    assert_ne!(other.build(&bytes).unwrap().image, build.image);
}

#[test]
fn a_build_holds_exactly_the_locked_models() {
    let eng = format!("{MODEL_PREFIX}eng.traineddata");
    let mut wrong_model = manifest();
    wrong_model.files.get_mut(&eng).unwrap().sha256 = "c".repeat(64);
    let mut missing_model = manifest();
    missing_model.files.remove(&eng);
    let mut extra_file = manifest();
    extra_file
        .files
        .insert("extra".into(), file(&"c".repeat(64)));
    let mut no_engine = manifest();
    no_engine.files.remove(TESSERACT_FILE);
    let mut other_platform = manifest();
    other_platform.platform = "windows_x86_64".into();
    for bad in [
        wrong_model,
        missing_model,
        extra_file,
        no_engine,
        other_platform,
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn a_build_holds_chr_phoenixs_notice_and_licence() {
    let notices = locked_notices().unwrap();
    assert_eq!(
        notices.keys().collect::<Vec<_>>(),
        [
            "notice_chr_phoenix_LICENSE-Apache-2.0.txt",
            "notice_chr_phoenix_NOTICE.md"
        ]
    );
    // The same SHA256SUMS the engine image pins by hash.
    // Read, not embedded: the engine image's recipe isn't an input of the worker's build.
    let containerfile = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ocr/engines/tesseract/Containerfile"),
    )
    .unwrap();
    let sums = boinc::sha256(NOTICES[0].2.as_bytes());
    assert!(containerfile.contains(&format!("ARG CHR_PHOENIX_SUMS={sums}")));
    // And the files in the repository are the release's.
    for (path, sum) in notices.values() {
        let file = path.rsplit('/').next().unwrap();
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../ocr/models/chr-phoenix")
            .join(file);
        if repo.exists() {
            assert_eq!(&boinc::sha256(&std::fs::read(repo).unwrap()), sum);
        }
    }
    for name in notices.keys() {
        let mut missing = manifest();
        missing.files.remove(name);
        assert!(missing.validate().is_err(), "{name}");
        let mut changed = manifest();
        changed.files.get_mut(name).unwrap().sha256 = "c".repeat(64);
        assert!(changed.validate().is_err(), "{name}");
    }
    manifest().validate().unwrap();
}

#[test]
fn a_page_for_another_build_is_refused() {
    let build = build();
    let dir = tempfile::tempdir().unwrap();
    write_page(dir.path(), &build.image);
    let unit = read_unit(dir.path()).unwrap();
    check_unit(&unit, &build).unwrap();
    // A job for the worker image: a host running this build must not answer for it.
    write_page(dir.path(), &format!("sha256:{}", "2".repeat(64)));
    let unit = read_unit(dir.path()).unwrap();
    let error = check_unit(&unit, &build).unwrap_err().to_string();
    assert!(error.contains("made for build"), "{error}");
}

#[test]
fn a_gold_unit_is_bound_to_the_build_and_its_engine() {
    let build = build();
    let unit = |image: &str, base: &str| Unit {
        image: image.to_owned(),
        base: Some(base.to_owned()),
        timeout_secs: 300,
        restart_after_suspend: true,
    };
    check_unit(&unit(&build.image, &build.base), &build).unwrap();
    assert!(check_unit(&unit(&build.image, "sha256:other"), &build).is_err());
    assert!(check_unit(&unit("sha256:other", &build.base), &build).is_err());
}

#[test]
fn another_apps_unit_is_refused() {
    let build = build();
    for other in [
        crate::assess::JOB_FILE,
        crate::surya::JOB_FILE,
        crate::quality::JOB_FILE,
        crate::qwen::JOB_FILE,
        crate::ndlocr::JOB_FILE,
    ] {
        let dir = tempfile::tempdir().unwrap();
        write_page(dir.path(), &build.image);
        std::fs::write(dir.path().join(other), b"{}").unwrap();
        assert!(read_unit(dir.path()).is_err(), "{other}");
    }
}

fn shell(script: &str) -> anyhow::Result<Child> {
    Ok(Command::new("sh")
        .args(["-c", script])
        .process_group(0)
        .spawn()?)
}

#[test]
fn a_finished_unit_ends_the_task_and_a_failed_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let control = BoincControl::noop();
    let done = supervise(dir.path(), &|| shell("exit 0"), &control, false).unwrap();
    assert_eq!(done, Ended::Finished);
    assert!(supervise(dir.path(), &|| shell("exit 3"), &control, false).is_err());
}

/// A slot with the client's channel, and a task control watching it.
fn client_slot() -> (tempfile::TempDir, BoincControl) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("init_data.xml"), "<app_init_data/>").unwrap();
    let control = control(dir.path());
    (dir, control)
}

fn send(dir: &Path, message: &str) {
    write_mmap_channel(
        &dir.join(MMAP_FILE),
        PROCESS_CONTROL_REQUEST_OFFSET,
        message,
    )
    .unwrap();
}

/// Whether the process runs, a zombie counting as gone.
#[cfg(target_os = "linux")]
fn alive(pid: &str) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

/// Whether the process runs, a zombie counting as gone: `ps` prints nothing for a process
/// that has gone and `Z` for a zombie.
#[cfg(target_os = "macos")]
fn alive(pid: &str) -> bool {
    Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .is_ok_and(|o| {
            let stat = String::from_utf8_lossy(&o.stdout);
            !stat.trim().is_empty() && !stat.trim_start().starts_with('Z')
        })
}

#[test]
fn a_quit_stops_the_whole_group_without_finishing() {
    let (dir, control) = client_slot();
    let path = dir.path().to_owned();
    let client = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        send(&path, "<quit/>");
    });
    // The grandchild stands for Tesseract: the stop has to reach it too.
    let pid = dir.path().join("grandchild");
    let script = format!("sleep 60 & echo $! > {}; wait", pid.display());
    let ended = supervise(dir.path(), &|| shell(&script), &control, false).unwrap();
    client.join().unwrap();
    assert_eq!(ended, Ended::Stopped("quit"));
    let grandchild = std::fs::read_to_string(pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(grandchild.trim()) {
        assert!(Instant::now() < deadline, "the engine outlived the task");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_suspended_gold_unit_starts_again_on_resume() {
    let (dir, control) = client_slot();
    let starts = AtomicUsize::new(0);
    let path = dir.path().to_owned();
    let client = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        send(&path, "<suspend/>");
        std::thread::sleep(Duration::from_millis(600));
        send(&path, "<resume/>");
    });
    let start = || {
        // The first run would take a minute; the run after the resume ends at once.
        let first = starts.fetch_add(1, Ordering::SeqCst) == 0;
        shell(if first { "sleep 60" } else { "exit 0" })
    };
    let ended = supervise(dir.path(), &start, &control, true).unwrap();
    client.join().unwrap();
    assert_eq!(ended, Ended::Finished);
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}

#[test]
fn a_suspended_page_stays_stopped_and_carries_on() {
    let (dir, control) = client_slot();
    let starts = AtomicUsize::new(0);
    let path = dir.path().to_owned();
    let marker = dir.path().join("done");
    let client = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        send(&path, "<suspend/>");
        std::thread::sleep(Duration::from_millis(600));
        send(&path, "<resume/>");
    });
    let script = format!("sleep 1.5; echo done > {}", marker.display());
    let start = || {
        starts.fetch_add(1, Ordering::SeqCst);
        shell(&script)
    };
    assert_eq!(
        supervise(dir.path(), &start, &control, false).unwrap(),
        Ended::Finished
    );
    client.join().unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(marker.exists());
}
