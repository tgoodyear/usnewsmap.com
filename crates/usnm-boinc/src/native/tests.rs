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
    Manifest {
        engine_version: "5.5.0".into(),
        files,
        platform: "x86_64-pc-linux-gnu".into(),
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
    let state = format!("/proc/{}/stat", grandchild.trim());
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_to_string(&state).is_ok_and(|s| !s.contains(") Z ")) {
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
