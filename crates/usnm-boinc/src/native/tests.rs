use super::*;
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
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
    other_platform.platform = "no_such_platform".into();
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

#[cfg(unix)]
fn shell(script: &str) -> anyhow::Result<Child> {
    Ok(Command::new("sh")
        .args(["-c", script])
        .process_group(0)
        .spawn()?)
}

/// `cmd` in a job as the worker starts its unit, so the tests use the real spawn.
#[cfg(windows)]
fn shell(script: &str) -> anyhow::Result<Child> {
    use std::os::windows::process::CommandExt as _;
    let mut command = Command::new("cmd");
    command.raw_arg(format!("/D /S /C \"{script}\""));
    Ok(win::spawn_in_job(&mut command, &win::Limits::engine(600))?)
}

/// A command that takes `secs` seconds and does nothing.
#[cfg(unix)]
fn sleeping(secs: u32) -> String {
    format!("sleep {secs}")
}

/// A command that takes `secs` seconds and does nothing: `ping` waits a second between pings.
#[cfg(windows)]
fn sleeping(secs: u32) -> String {
    format!("ping -n {} 127.0.0.1 >nul", secs + 1)
}

#[test]
fn a_finished_unit_ends_the_task_and_a_failed_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let control = BoincControl::noop();
    let done = supervise(dir.path(), &|| shell("exit 0"), &control, false, 1800).unwrap();
    assert_eq!(done, Ended::Finished);
    assert!(supervise(dir.path(), &|| shell("exit 3"), &control, false, 1800).is_err());
}

#[cfg(unix)]
#[test]
fn a_unit_that_exits_at_the_cpu_limit_status_is_reported_as_too_slow() {
    let dir = tempfile::tempdir().unwrap();
    let control = BoincControl::noop();
    let error = supervise(
        dir.path(),
        &|| shell(&format!("exit {CPU_LIMIT_EXIT}")),
        &control,
        false,
        3600,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "too slow: SIGXCPU at timeout_secs 3600");
    assert_eq!(exit_code(&error), CPU_LIMIT_EXIT);
    let other = supervise(dir.path(), &|| shell("exit 3"), &control, false, 3600).unwrap_err();
    assert_eq!(exit_code(&other), 1);
}

#[cfg(unix)]
#[test]
fn an_engine_killed_by_sigxcpu_becomes_a_cpu_limit_and_other_failures_do_not() {
    use std::os::unix::process::ExitStatusExt as _;
    let killed = |signal: i32| {
        anyhow::Error::new(usnm_ocr_worker::engine::Exited {
            status: std::process::ExitStatus::from_raw(signal),
            stderr: String::new(),
        })
        .context("read failed")
    };
    let error = classify_unit_error(killed(24), Some(1800));
    assert_eq!(
        error.root_cause().to_string(),
        "too slow: SIGXCPU at timeout_secs 1800"
    );
    assert_eq!(exit_code(&error), CPU_LIMIT_EXIT);
    // SIGSEGV is a crash, not a slow page, and without a CPU limit nothing is too slow.
    assert_eq!(exit_code(&classify_unit_error(killed(11), Some(1800))), 1);
    assert_eq!(exit_code(&classify_unit_error(killed(24), None)), 1);
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

/// Whether the process runs.
#[cfg(windows)]
fn alive(pid: &str) -> bool {
    win::is_alive(pid.parse().unwrap())
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

#[cfg(unix)]
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
    let ended = supervise(dir.path(), &|| shell(&script), &control, false, 1800).unwrap();
    client.join().unwrap();
    assert_eq!(ended, Ended::Stopped("quit"));
    let grandchild = std::fs::read_to_string(pid).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(grandchild.trim()) {
        assert!(Instant::now() < deadline, "the engine outlived the task");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The job's processes, `cmd` and the `ping` it starts, all end with a quit.
#[cfg(windows)]
#[test]
fn a_quit_stops_the_whole_job_without_finishing() {
    use std::sync::atomic::AtomicU32;
    let (dir, control) = client_slot();
    let path = dir.path().to_owned();
    let unit = AtomicU32::new(0);
    let seen = std::sync::Mutex::new(Vec::new());
    let ended = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(1500));
            *seen.lock().unwrap() = win::group_processes(unit.load(Ordering::SeqCst));
            send(&path, "<quit/>");
        });
        // The ping stands for Tesseract: the stop has to reach it too.
        let start = || {
            let child = shell(&sleeping(60))?;
            unit.store(child.id(), Ordering::SeqCst);
            Ok(child)
        };
        supervise(dir.path(), &start, &control, false, 1800).unwrap()
    });
    assert_eq!(ended, Ended::Stopped("quit"));
    let seen = seen.into_inner().unwrap();
    assert!(seen.len() >= 2, "the job held {seen:?}");
    let deadline = Instant::now() + Duration::from_secs(5);
    while seen.iter().any(|pid| alive(&pid.to_string())) {
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
        shell(&if first { sleeping(60) } else { "exit 0".into() })
    };
    let ended = supervise(dir.path(), &start, &control, true, 1800).unwrap();
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
    #[cfg(unix)]
    let script = format!("sleep 1.5; echo done > {}", marker.display());
    // `echo done>` makes the file; Windows has no sleep that takes a fraction of a second.
    #[cfg(windows)]
    let script = format!("{} & echo done> {}", sleeping(2), marker.display());
    let start = || {
        starts.fetch_add(1, Ordering::SeqCst);
        shell(&script)
    };
    assert_eq!(
        supervise(dir.path(), &start, &control, false, 1800).unwrap(),
        Ended::Finished
    );
    client.join().unwrap();
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(marker.exists());
}

/// macOS has no parent-death signal (`watch_parent`). These run the unit's real layout in
/// subprocesses of this test binary: a stand-in task parent starts the unit in its own process
/// group, as `run_slot` does, and the unit starts an engine-like child in that group. Killing
/// the parent alone must end both, whether the group is running or stopped by a suspension.
#[cfg(target_os = "macos")]
mod parent_death {
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const ROLE_ENV: &str = "USNM_TEST_PARENT_DEATH_ROLE";
    const PIDS_ENV: &str = "USNM_TEST_PARENT_DEATH_PIDS";
    const HELPER: &str = "native::tests::parent_death::helper";

    fn rerun(role: &str, pids: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--nocapture", "--test-threads=1"])
            .env(ROLE_ENV, role)
            .env(PIDS_ENV, pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }

    /// Not a test of its own: the parent and unit roles of the tests below.
    #[test]
    fn helper() {
        let Ok(role) = std::env::var(ROLE_ENV) else {
            return;
        };
        let pids = std::path::PathBuf::from(std::env::var(PIDS_ENV).unwrap());
        match role.as_str() {
            "parent" => {
                use std::os::unix::process::CommandExt;
                let _unit = rerun("unit", &pids).process_group(0).spawn().unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
            "unit" => {
                super::super::watch_parent(super::super::parent_pid());
                let engine = Command::new("/bin/sleep").arg("120").spawn().unwrap();
                let line = format!("{} {}\n", std::process::id(), engine.id());
                std::fs::write(pids.with_extension("tmp"), line).unwrap();
                std::fs::rename(pids.with_extension("tmp"), &pids).unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
            other => panic!("unknown role {other}"),
        }
    }

    fn alive(pid: i32) -> bool {
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()).is_ok()
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(10), "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn the_unit_and_engine_end_with_the_parent(stop_group_first: bool) {
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let mut parent = rerun("parent", &pids).spawn().unwrap();
        wait_until("the unit never started", || pids.exists());
        let text = std::fs::read_to_string(&pids).unwrap();
        let [unit, engine]: [i32; 2] = text
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let group = rustix::process::Pid::from_raw(unit).unwrap();
        if stop_group_first {
            // A suspension stops the whole group, the unit's watching thread with it.
            rustix::process::kill_process_group(group, rustix::process::Signal::STOP).unwrap();
        }
        // The client can kill the parent alone.
        parent.kill().unwrap();
        parent.wait().unwrap();
        wait_until("the unit outlived its parent", || !alive(unit));
        wait_until("the engine outlived its parent", || !alive(engine));
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }

    #[test]
    fn a_running_unit_and_its_engine_end_when_the_parent_is_killed() {
        the_unit_and_engine_end_with_the_parent(false);
    }

    #[test]
    fn a_stopped_unit_and_its_engine_end_when_the_parent_is_killed() {
        the_unit_and_engine_end_with_the_parent(true);
    }
}

/// Windows has no parent-death signal and no process groups: the unit and its engine are in a
/// job that the task's parent holds the only handle to (`win`). These run the unit's real
/// layout in subprocesses of this test binary: a stand-in task parent starts the unit through
/// [`win::spawn_in_job`], as `run_slot` does, and the unit starts an engine-like child, which
/// joins the job. Killing the parent alone must end both, whether the job is running or
/// suspended by the client.
#[cfg(windows)]
mod parent_death {
    use super::super::win;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const ROLE_ENV: &str = "USNM_TEST_PARENT_DEATH_ROLE";
    const PIDS_ENV: &str = "USNM_TEST_PARENT_DEATH_PIDS";
    const HELPER: &str = "native::tests::parent_death::helper";

    fn rerun(role: &str, pids: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--nocapture", "--test-threads=1"])
            .env(ROLE_ENV, role)
            .env(PIDS_ENV, pids)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        command
    }

    /// Not a test of its own: the parent and unit roles of the tests below.
    #[test]
    // The children are left running on purpose: the job ends them with their parent.
    #[allow(clippy::zombie_processes)]
    fn helper() {
        let Ok(role) = std::env::var(ROLE_ENV) else {
            return;
        };
        let pids = std::path::PathBuf::from(std::env::var(PIDS_ENV).unwrap());
        match role.as_str() {
            "parent" => {
                let _unit = win::spawn_in_job(&mut rerun("unit", &pids), &win::Limits::engine(600))
                    .unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
            "unit" => {
                let engine = Command::new("ping")
                    .args(["-n", "120", "127.0.0.1"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                let line = format!("{} {}\n", std::process::id(), engine.id());
                std::fs::write(pids.with_extension("tmp"), line).unwrap();
                std::fs::rename(pids.with_extension("tmp"), &pids).unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
            other => panic!("unknown role {other}"),
        }
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < Duration::from_secs(15), "{what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn the_unit_and_engine_end_with_the_parent(suspend_first: bool) {
        let dir = tempfile::tempdir().unwrap();
        let pids = dir.path().join("pids");
        let mut parent = rerun("parent", &pids).spawn().unwrap();
        wait_until("the unit never started", || pids.exists());
        let text = std::fs::read_to_string(&pids).unwrap();
        let [unit, engine]: [u32; 2] = text
            .split_whitespace()
            .map(|pid| pid.parse().unwrap())
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        assert!(win::is_alive(unit) && win::is_alive(engine));
        if suspend_first {
            // A suspension stops every process of the job, as the client's does.
            win::suspend_or_resume(unit, true);
            win::suspend_or_resume(engine, true);
        }
        // The client can kill the parent alone.
        parent.kill().unwrap();
        parent.wait().unwrap();
        wait_until("the unit outlived its parent", || !win::is_alive(unit));
        wait_until("the engine outlived its parent", || !win::is_alive(engine));
    }

    #[test]
    fn a_running_unit_and_its_engine_end_when_the_parent_is_killed() {
        the_unit_and_engine_end_with_the_parent(false);
    }

    #[test]
    fn a_stopped_unit_and_its_engine_end_when_the_parent_is_killed() {
        the_unit_and_engine_end_with_the_parent(true);
    }
}

#[cfg(windows)]
#[test]
fn a_verbatim_path_loses_its_prefix_and_others_keep_theirs() {
    use crate::plain_path;
    use std::path::PathBuf;
    let plain = |s: &str| plain_path(PathBuf::from(s)).display().to_string();
    assert_eq!(plain(r"\\?\C:\BOINC\slots\0"), r"C:\BOINC\slots\0");
    assert_eq!(plain(r"\\?\UNC\host\share\slot"), r"\\host\share\slot");
    assert_eq!(plain(r"\\?\Volume{1234}\slot"), r"\\?\Volume{1234}\slot");
    assert_eq!(plain(r"C:\already\plain"), r"C:\already\plain");
}

/// The job gives the engine's memory and process limits, and the CPU limit: a process that
/// computes past its limit is ended by the job, not by anything this test does.
#[cfg(windows)]
#[test]
fn a_process_past_the_cpu_limit_is_ended_by_the_job() {
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-Command", "while ($true) { }"])
        .stdin(Stdio::null());
    let mut child = win::spawn_in_job(&mut command, &win::Limits::engine(2)).unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the job didn't end the process"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    // The loop was running when the limit ended it, not stopped at once.
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "ended after {:?} with {status}",
        started.elapsed()
    );
}
