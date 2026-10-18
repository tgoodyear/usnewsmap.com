//! The volunteer gold check (#457): score this host's engine on the public gold bundle and report
//! the scores with the bundle version, so results from different hosts and engine builds compare.
//!
//! Two steps, because the BOINC job container has no network (docs/boinc.md):
//! - `gold-stage` (network): verifies the bundle and downloads the original files it refers to
//!   (the BLN600 zip, Library of Congress page images) into a cache, checking each sha256.
//! - `gold-run` (offline): verifies the bundle again, checks the cache, cuts the crops locally,
//!   runs Tesseract on each, scores CER/WER as gold.py does, and writes a report of counts only.
//!   The report holds no page text and no transcription.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use usnm_ocr_survey::{bundle, score};
use usnm_ocr_worker::engine::{self, Engine, Runner, Tesseract, TesseractOptions};

pub mod collect;
pub mod job;
pub mod mirror;
pub mod prepare;
pub mod submit;
pub mod validate;
pub mod work;

/// The report's format version.
pub const REPORT_VERSION: u32 = 1;

/// Opens a bundle directory, or unpacks a bundle tar into `work`, and verifies it.
pub fn open(bundle_path: &Path, work: &Path) -> anyhow::Result<(PathBuf, bundle::Manifest)> {
    let dir = if bundle_path.is_file() {
        let dest = work.join("bundle");
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        std::fs::create_dir_all(&dest)?;
        bundle::unpack(bundle_path, &dest)?
    } else {
        bundle_path.to_path_buf()
    };
    let m = bundle::verify(&dir).context("bundle verification failed")?;
    Ok((dir, m))
}

pub async fn stage(
    bundle_path: &Path,
    cache: &Path,
    work: &Path,
) -> anyhow::Result<bundle::Staged> {
    let (_, m) = open(bundle_path, work)?;
    bundle::stage(&m, cache, Duration::from_secs(1)).await
}

pub struct RunOptions {
    pub bundle: PathBuf,
    pub cache: PathBuf,
    pub work: PathBuf,
    pub tesseract: PathBuf,
    pub command: Option<CommandEngine>,
    pub psm: u8,
    pub dpi: u32,
    pub timeout_secs: u32,
    /// Run at most this many crops (for a quick check); all when none.
    pub limit: Option<usize>,
}

pub struct CommandEngine {
    pub executable: PathBuf,
    pub name: String,
    pub version: String,
}

/// Never include command output in errors: either stream may contain page text.
#[cfg(unix)]
async fn command_text(
    executable: &Path,
    image: &Path,
    language: &str,
    timeout: Duration,
) -> anyhow::Result<String> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(executable)
        .process_group(0)
        .arg(image)
        .arg(language)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("could not start engine command")?;
    struct ProcessGroup(rustix::process::Pid);
    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
        }
    }
    // The guard also covers cancellation and adapters that exit leaving a worker
    // behind. A separate session or container created by an adapter is not a sandbox.
    let _group = ProcessGroup(
        child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            .context("engine process group missing")?,
    );
    let mut stdout = child.stdout.take().context("engine stdout missing")?;
    let output = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .context("could not read engine output")?;
        ensure!(bytes.len() <= 16 * 1024 * 1024, "engine output too large");
        ensure!(child.wait().await?.success(), "engine command failed");
        String::from_utf8(bytes).context("engine output is not UTF-8")
    })
    .await
    .map_err(|_| anyhow::anyhow!("engine command timed out"))?;
    output
}

#[cfg(not(unix))]
async fn command_text(_: &Path, _: &Path, _: &str, _: Duration) -> anyhow::Result<String> {
    anyhow::bail!("external gold engine commands require Unix process-group support")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInfo {
    pub dpi: u32,
    pub name: String,
    pub psm: u8,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CropRun {
    #[serde(flatten)]
    pub result: bundle::CropResult,
    pub elapsed_ms: u64,
    pub profile: String,
}

/// gold-result.json. Counts only: no page text, no transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// Set when the report came from a BOINC unit; absent from a hand run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boinc: Option<BoincInfo>,
    pub bundle_version: u32,
    pub completed_at: String,
    pub crops: Vec<CropRun>,
    pub engine: EngineInfo,
    /// The `--limit` the run had; a limited run is partial and not comparable with a full one.
    pub limit: Option<usize>,
    pub manifest_sha256: String,
    pub report_version: u32,
    pub skipped: Vec<bundle::Skipped>,
    pub summary: Vec<bundle::SourceSummary>,
}

/// Who ran a BOINC unit, as the slot says. The host id is the client's own claim (from the
/// slot's `init_data.xml`), not proof of identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoincInfo {
    pub engine_base: String,
    pub engine_image: String,
    pub host_id: Option<u64>,
    pub job_id: String,
}

pub async fn run(opts: &RunOptions) -> anyhow::Result<Report> {
    std::fs::create_dir_all(&opts.work)?;
    let (dir, m) = open(&opts.bundle, &opts.work)?;
    let crops_dir = opts.work.join("crops");
    let (ready, mut skipped) = bundle::materialize(&m, &dir, &opts.cache, &crops_dir, opts.limit)?;
    let tess = if opts.command.is_none() {
        Some(
            Tesseract::detect(TesseractOptions {
                runner: Runner::Host(opts.tesseract.clone()),
                psm: opts.psm,
                dpi: opts.dpi,
                threads: 1,
                timeout: Duration::from_secs(opts.timeout_secs.into()),
            })
            .await?,
        )
    } else {
        None
    };
    let mut crops = Vec::new();
    for r in ready {
        let profile = if let Some(tess) = &tess {
            let Some(profile) = engine::route(&r.lang, tess.installed()) else {
                skipped.push(bundle::Skipped {
                    crop_id: r.crop_id,
                    reason: format!("no installed model for {}", r.lang),
                });
                continue;
            };
            profile.profile
        } else {
            r.lang.clone()
        };
        let scratch = tempfile::tempdir_in(&opts.work)?;
        let started = Instant::now();
        let result = if let Some(command) = &opts.command {
            command_text(
                &command.executable,
                &r.image,
                &r.lang,
                Duration::from_secs(opts.timeout_secs.into()),
            )
            .await
        } else {
            tess.as_ref()
                .expect("Tesseract detected")
                .ocr(&r.image, &r.lang, scratch.path())
                .await
                .map(|out| out.text)
                .map_err(anyhow::Error::from)
        };
        match result {
            Ok(out) => crops.push(CropRun {
                result: bundle::CropResult {
                    counts: score::cer_wer(&r.transcription, &out),
                    crop_id: r.crop_id,
                    source: r.source,
                },
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                profile,
            }),
            Err(e) => skipped.push(bundle::Skipped {
                crop_id: r.crop_id,
                reason: format!("engine: {e}"),
            }),
        }
    }
    ensure!(
        !crops.is_empty(),
        "no crop could be run: {} skipped",
        skipped.len()
    );
    let results: Vec<_> = crops.iter().map(|c| c.result.clone()).collect();
    Ok(Report {
        boinc: None,
        bundle_version: m.bundle_version,
        completed_at: chrono::Utc::now().to_rfc3339(),
        summary: bundle::summarize(&results),
        crops,
        engine: EngineInfo {
            dpi: opts.dpi,
            name: opts
                .command
                .as_ref()
                .map(|c| c.name.clone())
                .unwrap_or_else(|| {
                    tess.as_ref()
                        .expect("Tesseract detected")
                        .source()
                        .to_string()
                }),
            psm: opts.psm,
            version: opts
                .command
                .as_ref()
                .map(|c| c.version.clone())
                .unwrap_or_else(|| {
                    tess.as_ref()
                        .expect("Tesseract detected")
                        .version()
                        .to_string()
                }),
        },
        limit: opts.limit,
        manifest_sha256: m.digest()?,
        report_version: REPORT_VERSION,
        skipped,
    })
}

#[cfg(all(test, unix))]
mod command_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::Arc;

    fn executable(dir: &Path, script: &str) -> PathBuf {
        let path = dir.join("engine");
        let mut staged = tempfile::NamedTempFile::new_in(dir).unwrap();
        writeln!(staged, "#!/bin/sh\n{script}").unwrap();
        staged
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))
            .unwrap();
        staged.as_file().sync_all().unwrap();
        // Close before publishing the executable, not just before spawning it.
        let staged = staged.into_temp_path();
        std::fs::rename(&staged, &path).unwrap();
        path
    }

    async fn fixture_command(
        executable: &Path,
        image: &Path,
        timeout: Duration,
    ) -> anyhow::Result<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let result = command_text(executable, image, "eng", timeout).await;
            // A different test's pre-exec child can inherit the staging writer
            // across our close/rename. Retry only that spawn error, never a run.
            if result.as_ref().err().is_some_and(|error| {
                error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    error.raw_os_error() == Some(rustix::io::Errno::TXTBSY.raw_os_error())
                })
            }) && Instant::now() < deadline
            {
                tokio::task::yield_now().await;
                continue;
            }
            return result;
        }
    }

    async fn invoke(script: &str, timeout: Duration) -> anyhow::Result<String> {
        let dir = tempfile::tempdir().unwrap();
        let executable = executable(dir.path(), script);
        fixture_command(&executable, Path::new("crop with spaces.png"), timeout).await
    }

    #[tokio::test]
    async fn arguments_and_utf8() {
        assert_eq!(
            invoke("printf '%s|%s|é' \"$1\" \"$2\"", Duration::from_secs(2))
                .await
                .unwrap(),
            "crop with spaces.png|eng|é"
        );
    }

    #[tokio::test]
    async fn executable_fixtures_launch_under_parallel_forks() {
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            tasks.spawn(async {
                for _ in 0..25 {
                    assert_eq!(
                        invoke("printf fixture", Duration::from_secs(10))
                            .await
                            .unwrap(),
                        "fixture"
                    );
                }
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn busy_fixture_exposes_only_the_safe_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let executable = executable(dir.path(), "echo SECRET; echo SECRET >&2");
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable)
            .unwrap();
        let error = command_text(
            &executable,
            Path::new("crop"),
            "eng",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "could not start engine command");
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .raw_os_error(),
            Some(rustix::io::Errno::TXTBSY.raw_os_error())
        );
        assert!(!format!("{error:#}").contains("SECRET"));
        drop(writer);
        assert_eq!(
            fixture_command(&executable, Path::new("crop"), Duration::from_secs(10))
                .await
                .unwrap(),
            "SECRET\n"
        );
    }

    #[tokio::test]
    async fn failures_never_report_output() {
        for (script, timeout, expected) in [
            (
                "echo SECRET; echo SECRET >&2; exit 1",
                Duration::from_secs(2),
                "engine command failed",
            ),
            (
                "printf '\\377'",
                Duration::from_secs(2),
                "engine output is not UTF-8",
            ),
            (
                "exec sleep 2",
                Duration::from_millis(20),
                "engine command timed out",
            ),
        ] {
            let error = invoke(script, timeout).await.unwrap_err();
            // Display the safe error chain (including spawn errno), not output.
            assert_eq!(error.to_string(), expected, "{error:#}");
        }
    }

    fn worker_pipe(dir: &Path) -> (Arc<std::fs::File>, std::fs::File) {
        let fifo = dir.join("worker");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let reader = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&fifo)
            .unwrap();
        // Keep the read end from reporting EOF before the adapter connects.
        let keepalive = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&fifo)
            .unwrap();
        (Arc::new(reader), keepalive)
    }

    async fn pipe_read(fifo: &Arc<std::fs::File>, deadline: Instant) -> std::io::Result<Vec<u8>> {
        let fifo = fifo.clone();
        tokio::task::spawn_blocking(move || {
            let mut bytes = vec![0; 128];
            loop {
                match fifo.as_ref().read(&mut bytes) {
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Ok(n) => {
                        bytes.truncate(n);
                        return Ok(bytes);
                    }
                    Err(error) => return Err(error),
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(std::io::ErrorKind::TimedOut.into());
                }
                let mut fds = [rustix::event::PollFd::new(
                    fifo.as_ref(),
                    rustix::event::PollFlags::IN,
                )];
                let timeout = rustix::event::Timespec::try_from(remaining).unwrap();
                match rustix::event::poll(&mut fds, Some(&timeout)) {
                    Ok(0) => {
                        // Some Unix poll implementations omit a FIFO's final
                        // hangup notification. Check EOF once at the deadline.
                        return match fifo.as_ref().read(&mut bytes) {
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                Err(std::io::ErrorKind::TimedOut.into())
                            }
                            Ok(n) => {
                                bytes.truncate(n);
                                Ok(bytes)
                            }
                            Err(error) => Err(error),
                        };
                    }
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn fifo_wait_is_bounded_without_an_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let (reader, keepalive) = worker_pipe(dir.path());
        assert_eq!(
            pipe_read(&reader, Instant::now() + Duration::from_millis(20))
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
        drop(keepalive);
        assert!(pipe_read(&reader, Instant::now() + Duration::from_secs(2))
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn timeout_stops_a_non_exec_adapters_worker() {
        let dir = tempfile::tempdir().unwrap();
        let (worker, keepalive) = worker_pipe(dir.path());
        let executable = executable(
            dir.path(),
            "{ echo \"$$\"; sh -c 'echo $$; exec sleep 3600' & wait; } > \"$1\"",
        );
        let fifo = dir.path().join("worker");
        let command = tokio::spawn(async move {
            fixture_command(&executable, &fifo, Duration::from_secs(60)).await
        });
        // Nonblocking reads plus a kernel poll deadline cannot strand a syscall
        // on Tokio's blocking pool, even when the adapter never connects.
        let deadline = Instant::now() + Duration::from_secs(10);
        {
            let mut pids = Vec::new();
            while pids.iter().filter(|&&b| b == b'\n').count() < 2 {
                let bytes = pipe_read(&worker, deadline)
                    .await
                    .expect("adapter's worker did not start");
                assert!(
                    !bytes.is_empty(),
                    "adapter closed its pipe before announcing its worker"
                );
                pids.extend_from_slice(&bytes);
                assert!(pids.len() <= 128);
            }
            let pids = String::from_utf8(pids).unwrap();
            let pids: Vec<_> = pids
                .lines()
                .map(|pid| rustix::process::Pid::from_raw(pid.parse().unwrap()).unwrap())
                .collect();
            assert_eq!(pids.len(), 2);
            assert_ne!(pids[0], pids[1]);
            assert_eq!(rustix::process::getpgid(Some(pids[1])).unwrap(), pids[0]);
        }
        drop(keepalive);
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(
            command.await.unwrap().unwrap_err().to_string(),
            "engine command timed out"
        );
        tokio::time::resume();
        assert!(pipe_read(&worker, Instant::now() + Duration::from_secs(5))
            .await
            .expect("adapter's worker is still running")
            .is_empty());
    }
}
