//! Adapter process lifetime, bounded protocol reads and awaited cleanup.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::bail;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use usnm_ocr_contract::engine::{decode, FromAdapter, Hello, Layout, CONTRACT, MAX_LINE};

use super::{ContractOptions, Launch, LABEL};
use crate::engine::{
    checked, podman_command, tail, ContainerGuard, Limits, PageDir, Sandbox, ENGINE_USER,
};

/// One instance's directories: where the worker writes and reads them,
/// and the paths the adapter sees.
#[derive(Debug)]
pub(super) struct Dirs {
    pub(super) host_in: PathBuf,
    pub(super) host_out: PathBuf,
    pub(super) seen_in: String,
    pub(super) seen_out: String,
    /// Under the engine runtime, the slot's MCS level.
    pub(super) level: Option<String>,
    /// Held until the instance is retired.
    slot: Option<Slot>,
}

/// An instance's scratch: a temporary directory, or an engine runtime
/// slot that [`Sandbox::release`] empties.
#[derive(Debug)]
#[allow(dead_code)]
enum Slot {
    Local(tempfile::TempDir),
    Podman(PageDir),
}

/// A running adapter.
/// Cleanups of instances dropped without [`Instance::retire`] (a page
/// cancelled at shutdown), which [`super::ContractEngine::shutdown`] waits for.
pub(super) type Cleanups = Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>;

pub(super) struct Instance {
    pub(super) child: Option<Child>,
    /// None once closed: the contract's "finish and exit".
    pub(super) stdin: Option<ChildStdin>,
    pub(super) stdout: BufReader<ChildStdout>,
    /// The last 50 lines of stderr, for a failure's message.
    stderr: Arc<Mutex<VecDeque<String>>>,
    pub(super) dirs: Dirs,
    pub(super) hello: Hello,
    /// Removes the container when the instance goes.
    container: Option<ContainerGuard>,
    sandbox: Option<Sandbox>,
    cleanups: Cleanups,
    pub(super) jobs: u32,
    /// Start to `hello`.
    pub(super) load_seconds: f64,
    /// The last page's request to its reply (or to giving up on one),
    /// without the worker's work before and after: the conformance
    /// suite's deadline case times the adapter by it.
    pub(super) last_exchange: Option<Duration>,
}

impl std::fmt::Debug for Instance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Instance")
            .field("dirs", &self.dirs)
            .field("jobs", &self.jobs)
            .finish()
    }
}

impl Instance {
    /// Stops the instance and waits for it: closes its input (the contract's
    /// "finish and exit"), gives it 10 s, kills it, removes its container,
    /// then frees its scratch slot. Dropping an instance only starts a
    /// kill; this finishes before a replacement or the worker's exit.
    pub(super) async fn retire(mut self) {
        self.close_input().await;
        let (child, container, slot) = (
            self.child.take(),
            self.container.take(),
            self.dirs.slot.take(),
        );
        cleanup(child, container, slot, self.sandbox.clone()).await;
    }

    /// Closes the instance's stdin. Tokio's `shutdown` on a pipe only
    /// flushes it; the adapter sees EOF when the pipe is dropped.
    pub(super) async fn close_input(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.flush().await;
        }
    }

    pub(super) fn stderr_tail(&self) -> String {
        let lines = self
            .stderr
            .lock()
            .map(|l| l.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        tail(&lines, 1500)
    }
}

/// One line from the adapter, at most [`MAX_LINE`] bytes; none at EOF.
pub(super) async fn read_line(r: &mut BufReader<ChildStdout>) -> anyhow::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf().await?;
        if buf.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let (take, done) = match buf.iter().position(|b| *b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        line.extend_from_slice(&buf[..take]);
        r.consume(take);
        if line.len() > MAX_LINE {
            bail!("a line over {MAX_LINE} bytes");
        }
        if done {
            return Ok(Some(line));
        }
    }
}

/// stderr lines kept per instance, the longest kept, and how many lines a
/// second are kept before the rest are dropped and counted (13 §13.4.1).
pub(super) const STDERR_LINES: usize = 50;
pub(super) const STDERR_LINE: usize = 4096;
const STDERR_PER_SEC: u32 = 200;

/// Reads an adapter's stderr as it arrives, in fixed chunks, so neither a
/// long line nor a flood can grow the worker's memory or stall the
/// adapter: each line is cut at [`STDERR_LINE`] bytes, lines over
/// [`STDERR_PER_SEC`] a second are dropped and counted, and the last
/// [`STDERR_LINES`] are kept for a failure's message.
pub(super) async fn drain_reader(
    mut err: impl tokio::io::AsyncRead + Unpin,
    keep: Arc<Mutex<VecDeque<String>>>,
) {
    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 8192];
    let mut line: Vec<u8> = Vec::new();
    let (mut window, mut in_window, mut dropped) = (std::time::Instant::now(), 0u32, 0u64);
    let mut push = |line: &[u8], keep: &Arc<Mutex<VecDeque<String>>>| {
        if window.elapsed() >= Duration::from_secs(1) {
            if dropped > 0 {
                tracing::warn!(dropped, "engine stderr lines dropped over the rate limit");
            }
            (window, in_window, dropped) = (std::time::Instant::now(), 0, 0);
        }
        in_window += 1;
        if in_window > STDERR_PER_SEC {
            dropped += 1;
            return;
        }
        if let Ok(mut k) = keep.lock() {
            if k.len() == STDERR_LINES {
                k.pop_front();
            }
            k.push_back(String::from_utf8_lossy(line).into_owned());
        }
    };
    loop {
        let n = match err.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\n' {
                push(&line, &keep);
                line.clear();
            } else if line.len() < STDERR_LINE {
                line.push(b);
            }
        }
    }
    if !line.is_empty() {
        push(&line, &keep);
    }
}

/// Starts an instance and reads its `hello`.
pub(super) async fn spawn(
    opts: &ContractOptions,
    sandbox: Option<&Sandbox>,
    cleanups: &Cleanups,
) -> anyhow::Result<Instance> {
    let started = std::time::Instant::now();
    let threads = opts.threads.max(1).to_string();
    let (mut cmd, dirs, container) = match (&opts.launch, sandbox) {
        (
            Launch::Local {
                program,
                args,
                scratch,
            },
            _,
        ) => {
            let tmp = tempfile::Builder::new()
                .prefix("usnm-eng-")
                .tempdir_in(scratch)?;
            let (i, o) = (tmp.path().join("in"), tmp.path().join("out"));
            std::fs::create_dir(&i)?;
            std::fs::create_dir(&o)?;
            let mut cmd = tokio::process::Command::new(program);
            cmd.args(args)
                .args(["--contract", &CONTRACT.to_string(), "--threads", &threads]);
            cmd.env("OMP_THREAD_LIMIT", &threads);
            let dirs = Dirs {
                seen_in: i.display().to_string(),
                seen_out: o.display().to_string(),
                host_in: i,
                host_out: o,
                level: None,
                slot: Some(Slot::Local(tmp)),
            };
            (cmd, dirs, None)
        }
        (
            Launch::Podman {
                podman,
                remote,
                image,
            },
            Some(sb),
        ) => {
            let (slot, level) = sb.page_dir().await?;
            let (i, o) = (slot.path.join("in"), slot.path.join("out"));
            // The container user can read in/ and search out/, and gets
            // rwx only on each page's out/<run_id>/ (the default entries
            // the slot gives new directories): it can't rename or replace
            // a page's directory.
            for (d, acl) in [(&i, "r-x"), (&o, "--x")] {
                std::fs::create_dir(d)?;
                let e = sb.info.engine_uid;
                checked(
                    tokio::process::Command::new("setfacl")
                        .args(["-m", &format!("u:{e}:{acl},m::rwx")])
                        .arg(d),
                )
                .await?;
                if let Some(level) = &level {
                    checked(
                        tokio::process::Command::new("chcon")
                            .args(["-t", "container_file_t", "-l", level])
                            .arg(d),
                    )
                    .await?;
                }
            }
            let name = format!("usnm-eng-{}", crate::queue::random_token());
            let mut cmd = podman_command(podman, Some(&remote.url));
            let Limits {
                memory_mib,
                cpus,
                pids,
            } = remote.limits;
            let mem = format!("{memory_mib}m");
            // 13 §13.6.1.
            cmd.args(["run", "--interactive", "--init", "--pull=never"])
                .args(["--name", &name, "--label", LABEL])
                .args([
                    "--network=none",
                    "--http-proxy=false",
                    "--no-hosts",
                    "--read-only",
                ])
                .args(["--tmpfs", "/tmp:rw,nosuid,nodev,size=64m,mode=1777"])
                .args(["--user", &format!("{ENGINE_USER}:{ENGINE_USER}")])
                .args(["--cap-drop=all", "--security-opt=no-new-privileges"])
                .args(["--memory", &mem, "--memory-swap", &mem])
                .args([
                    "--cpus",
                    &cpus.to_string(),
                    "--pids-limit",
                    &pids.to_string(),
                ])
                .args(["--ulimit", "nofile=1024:1024", "--ulimit", "core=0"])
                .args(["--log-driver=none", "--stop-timeout=10"])
                .args([
                    "--env",
                    "HOME=/tmp",
                    "--env",
                    &format!("OMP_THREAD_LIMIT={threads}"),
                ]);
            if let Some(level) = &level {
                cmd.args(["--security-opt", &format!("label=level:{level}")]);
            }
            cmd.args(["--volume", &format!("{}:/in:ro", i.display())])
                .args(["--volume", &format!("{}:/out:rw", o.display())])
                .arg(image)
                .args(["--contract", &CONTRACT.to_string(), "--threads", &threads]);
            let guard = ContainerGuard {
                podman: podman.clone(),
                url: Some(remote.url.clone()),
                name,
                armed: true,
            };
            let dirs = Dirs {
                host_in: i,
                host_out: o,
                seen_in: "/in".into(),
                seen_out: "/out".into(),
                level,
                slot: Some(Slot::Podman(slot)),
            };
            (cmd, dirs, Some(guard))
        }
        (Launch::Podman { .. }, None) => bail!("no engine runtime"),
    };
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            cleanup(None, container, dirs.slot, sandbox.cloned()).await;
            return Err(anyhow::Error::new(e).context("starting the engine instance"));
        }
    };
    let (Some(stdin), Some(out)) = (child.stdin.take(), child.stdout.take()) else {
        cleanup(Some(child), container, dirs.slot, sandbox.cloned()).await;
        bail!("the engine instance has no stdin or stdout");
    };
    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(err) = child.stderr.take() {
        // Read as it comes, so a full stderr pipe never stalls the
        // adapter; keep the last 50 lines of at most 4 KiB.
        tokio::spawn(drain_reader(err, stderr.clone()));
    }
    let mut inst = Instance {
        child: Some(child),
        stdin: Some(stdin),
        stdout: BufReader::new(out),
        stderr,
        dirs,
        hello: placeholder_hello(),
        container,
        sandbox: sandbox.cloned(),
        cleanups: cleanups.clone(),
        jobs: 0,
        load_seconds: 0.0,
        last_exchange: None,
    };
    match read_hello(&mut inst, opts.start_timeout).await {
        Ok(h) => {
            inst.hello = h;
            inst.load_seconds = started.elapsed().as_secs_f64();
            Ok(inst)
        }
        Err(e) => {
            // Awaited, so a failed start leaves no container behind.
            inst.retire().await;
            Err(e)
        }
    }
}

/// Stands in until an instance's `hello` is read.
fn placeholder_hello() -> Hello {
    use usnm_ocr_contract::engine::{Adapter, Cancel, EngineInfo, ModelsLoaded};
    Hello {
        adapter: Adapter {
            lang: String::new(),
            name: String::new(),
            version: String::new(),
        },
        cancel: Cancel::Kill,
        contract: 0,
        deterministic: false,
        engine: EngineInfo {
            components: None,
            family: String::new(),
            id: String::new(),
            version: String::new(),
        },
        granularity: usnm_alto::Granularity::Page,
        layout: Layout::Boxes,
        models: Vec::new(),
        models_loaded: ModelsLoaded::AtStart,
        profiles: Vec::new(),
    }
}

/// The instance's first line, which must be its `hello`.
async fn read_hello(inst: &mut Instance, timeout: Duration) -> anyhow::Result<Hello> {
    let first = tokio::time::timeout(timeout, read_line(&mut inst.stdout))
        .await
        .map_err(|_| anyhow::anyhow!("no hello from the engine in {timeout:?}"))??;
    let Some(first) = first else {
        if let Some(c) = inst.child.as_mut() {
            let _ = tokio::time::timeout(Duration::from_secs(2), c.wait()).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        bail!("the engine exited before its hello: {}", inst.stderr_tail());
    };
    match decode(&first) {
        Ok(FromAdapter::Hello(h)) => Ok(h),
        Ok(_) => bail!("the engine's first line isn't a hello"),
        Err(e) => bail!("the engine's first line: {e}"),
    }
}

/// Stops an instance's process (10 s to exit after its input closed,
/// then a kill), removes its container, then frees its scratch slot.
async fn cleanup(
    child: Option<Child>,
    container: Option<ContainerGuard>,
    slot: Option<Slot>,
    sandbox: Option<Sandbox>,
) {
    if let Some(mut child) = child {
        if tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
    if let Some(mut c) = container {
        c.finish().await;
    }
    match (slot, sandbox) {
        (Some(Slot::Podman(dir)), Some(sb)) => sb.release(dir).await,
        (other, _) => drop(other),
    }
}

impl Drop for Instance {
    /// An instance dropped without [`Instance::retire`]: a page cut short.
    /// The same cleanup runs as a task the engine's shutdown waits for.
    fn drop(&mut self) {
        let (mut child, container, slot) = (
            self.child.take(),
            self.container.take(),
            self.dirs.slot.take(),
        );
        if let Some(c) = child.as_mut() {
            let _ = c.start_kill();
        }
        if child.is_none() && container.is_none() && slot.is_none() {
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            let task = rt.spawn(cleanup(child, container, slot, self.sandbox.clone()));
            if let Ok(mut c) = self.cleanups.lock() {
                c.retain(|h| !h.is_finished());
                c.push(task);
            }
        }
    }
}
