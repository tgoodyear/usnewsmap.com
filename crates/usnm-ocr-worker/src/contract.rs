//! Engines behind the engine contract (ADR-0016 decision 2, 13 §13.4 and
//! §13.6): long-lived instances of an adapter that the worker talks to in
//! JSON lines, one page at a time, instead of a container per page.
//!
//! Off unless `--engine contract` is given; `--engine tesseract` (the
//! default) keeps one Tesseract container per page. This is the first
//! slice of 13 §13.11 step 6: one engine, a fixed pool of instances,
//! deadlines with a kill, restart after a crash, the `hello` checks and
//! both layouts (`alto` and `boxes`). Routing, the lock, the memory
//! budget, the breaker and cgroup metering come later.
//!
//! An instance runs either through the engine account's Podman API socket
//! (`podman --remote run --interactive`, 13 §13.6.1), or, for development
//! and tests only, as a local process.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use usnm_alto::boxes::{self, BoxLimits, Convert};
use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag};
use usnm_ocr_contract::engine::{
    decode, encode, engine_files, DpiSource, FromAdapter, Hello, Image, Layout, PageRequest,
    PageResult, Status, ToAdapter, CONTRACT, MAX_LINE,
};

use crate::engine::{
    checked, podman_command, tail, ContainerGuard, Engine, EngineError, EngineOutput, Limits,
    PageDir, ReadFailed, ReadInfo, Remote, Sandbox, ENGINE_USER, ROUTE, ROUTE_VERSION,
};

/// The conformance runner (13 §13.4.8).
pub mod conformance;

/// The label on instance containers, so stale ones can be found.
const LABEL: &str = "usnm.engine=contract";

/// How an instance is started.
#[derive(Debug, Clone)]
pub enum Launch {
    /// The adapter as a process on this host, with its scratch in
    /// `scratch`: for development and tests. It runs with the worker's
    /// rights, so production uses [`Launch::Podman`].
    Local {
        program: PathBuf,
        args: Vec<String>,
        scratch: PathBuf,
    },
    /// The adapter's image (`<name>@sha256:<digest>` or `sha256:<id>`)
    /// through the engine account's Podman API socket.
    Podman {
        podman: PathBuf,
        remote: Remote,
        image: String,
    },
}

#[derive(Debug, Clone)]
pub struct ContractOptions {
    pub launch: Launch,
    /// Instances, and so pages this engine reads at once.
    pub instances: usize,
    /// Threads per instance (`--threads` and `OMP_THREAD_LIMIT`).
    pub threads: u32,
    /// Start to `hello`.
    pub start_timeout: Duration,
    /// The longest page: the request's `deadline_ms`. The worker kills an
    /// instance with no result 15 s after it.
    pub page_timeout: Duration,
    /// Recycle an instance after this many pages.
    pub max_jobs: u32,
    /// The resolution sent with each scan (IIIF JPEGs carry none).
    pub dpi: u32,
    /// The node, the first part of each `run_id`.
    pub node: String,
}

/// The worker's grace after `deadline_ms` before it kills an instance
/// (13 §13.4.6).
const KILL_GRACE: Duration = Duration::from_secs(15);

/// One instance's directories: where the worker writes and reads them,
/// and the paths the adapter sees.
#[derive(Debug)]
struct Dirs {
    host_in: PathBuf,
    host_out: PathBuf,
    seen_in: String,
    seen_out: String,
    /// Under the engine runtime, the slot's MCS level.
    level: Option<String>,
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
/// cancelled at shutdown), which [`ContractEngine::shutdown`] waits for.
type Cleanups = Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>;

struct Instance {
    child: Option<Child>,
    /// None once closed: the contract's "finish and exit".
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// The last 50 lines of stderr, for a failure's message.
    stderr: Arc<Mutex<VecDeque<String>>>,
    dirs: Dirs,
    hello: Hello,
    /// Removes the container when the instance goes.
    container: Option<ContainerGuard>,
    sandbox: Option<Sandbox>,
    cleanups: Cleanups,
    jobs: u32,
    /// Start to `hello`.
    load_seconds: f64,
    /// The last page's request to its reply (or to giving up on one),
    /// without the worker's work before and after: the conformance
    /// suite's deadline case times the adapter by it.
    last_exchange: Option<Duration>,
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
    async fn retire(mut self) {
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
    async fn close_input(&mut self) {
        if let Some(mut s) = self.stdin.take() {
            let _ = s.flush().await;
        }
    }

    fn stderr_tail(&self) -> String {
        let lines = self
            .stderr
            .lock()
            .map(|l| l.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        tail(&lines, 1500)
    }
}

/// One line from the adapter, at most [`MAX_LINE`] bytes; none at EOF.
async fn read_line(r: &mut BufReader<ChildStdout>) -> anyhow::Result<Option<Vec<u8>>> {
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

/// The width and height of a JPEG, JPEG 2000, PNG or PGM, from its header.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?)).filter(|(w, h)| *w > 0 && *h > 0);
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                return None;
            }
            let marker = b[i + 1];
            if marker == 0xFF {
                i += 1;
                continue;
            }
            let len = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
            // Start of frame (not DHT, JPG, DAC).
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
                return (w > 0 && h > 0).then_some((u32::from(w), u32::from(h)));
            }
            i += 2 + len;
        }
        return None;
    }
    if b.starts_with(b"P5") || b.starts_with(b"P2") {
        let head = std::str::from_utf8(&b[..b.len().min(64)])
            .ok()
            .or_else(|| {
                let end = b
                    .iter()
                    .take(64)
                    .position(|&c| c > 127)
                    .unwrap_or(64.min(b.len()));
                std::str::from_utf8(&b[..end]).ok()
            })?;
        let mut nums = head[2..].split_ascii_whitespace().map(|t| t.parse::<u32>());
        let (w, h) = (nums.next()?.ok()?, nums.next()?.ok()?);
        return (w > 0 && h > 0).then_some((w, h));
    }
    // JP2: the image header box, `ihdr`, holds the height then the width.
    let at = b.windows(4).take(4096).position(|w| w == b"ihdr")?;
    let (h, w) = (be32(at + 4)?, be32(at + 8)?);
    (w > 0 && h > 0).then_some((w, h))
}

/// The sha256 of a profile's model files, as sorted lines
/// `<name> <sha256>` (13 §13.8.1).
fn models_digest(hello: &Hello, profile: &str) -> Digest {
    let shas: BTreeMap<&str, &str> = hello
        .models
        .iter()
        .map(|m| (m.name.as_str(), m.sha256.as_str()))
        .collect();
    let Some(p) = hello.profile(profile) else {
        return Digest::Local;
    };
    let mut lines = Vec::new();
    for m in &p.models {
        match shas.get(m.as_str()) {
            Some(s) => lines.push(format!("{m} {s}\n")),
            None => return Digest::Local,
        }
    }
    lines.sort();
    Digest::short(&crate::prefetch::sha256(lines.concat().as_bytes())).unwrap_or(Digest::Local)
}

/// A page that failed, and whether its instance can serve the next one.
enum PageError {
    /// The adapter answered (`unsupported`, `bad_input`, `failed` or
    /// `timeout` without `restart`): the instance stays.
    Keep(EngineError),
    /// A crash, a broken protocol, no answer in time, or a restart asked
    /// for: the instance is killed and removed.
    Drop(EngineError),
}

/// Any other failure that drops the instance is the protocol's: a limit,
/// a path or the schema broken (13 §13.4.6), unless it says otherwise.
impl<E: Into<anyhow::Error>> From<E> for PageError {
    fn from(e: E) -> Self {
        let e = e.into();
        let e = if e.is::<ReadFailed>() {
            e
        } else {
            ReadFailed::error("failed", "engine_protocol", format!("{e:#}"))
        };
        PageError::Drop(EngineError::Failed(e))
    }
}

/// An engine behind the contract.
#[derive(Debug)]
pub struct ContractEngine {
    catalog: usnm_ocr_route::Catalog,
    opts: ContractOptions,
    sandbox: Option<Sandbox>,
    /// The first instance's `hello`; every later one must match it.
    hello: Hello,
    idle: Mutex<Vec<Instance>>,
    slots: tokio::sync::Semaphore,
    source: String,
    image: Digest,
    cleanups: Cleanups,
}

impl ContractEngine {
    /// Starts the first instance and keeps its `hello`.
    pub async fn start(opts: ContractOptions) -> anyhow::Result<Self> {
        Self::start_with_catalog(opts, usnm_ocr_route::ENGINES.clone()).await
    }

    pub async fn start_with_catalog(
        opts: ContractOptions,
        catalog: usnm_ocr_route::Catalog,
    ) -> anyhow::Result<Self> {
        let (sandbox, image) = match &opts.launch {
            Launch::Local { .. } => (None, Digest::Local),
            Launch::Podman {
                podman,
                remote,
                image,
            } => {
                crate::engine::check_image_ref(image)?;
                let digest = image
                    .rsplit_once("sha256:")
                    .and_then(|(_, h)| Digest::short(h).ok())
                    .unwrap_or(Digest::Local);
                (Some(Sandbox::detect(podman, remote, image).await?), digest)
            }
        };
        let cleanups = Cleanups::default();
        let first = spawn(&opts, sandbox.as_ref(), &cleanups).await?;
        if let Err(e) = first.hello.check() {
            first.retire().await;
            bail!("the engine's hello: {e}");
        }
        let hello = first.hello.clone();
        tracing::info!(
            engine = %hello.engine.id,
            version = %hello.engine.version,
            layout = ?hello.layout,
            granularity = hello.granularity.as_str(),
            profiles = hello.profiles.len(),
            "engine instance ready"
        );
        Ok(Self {
            catalog,
            source: format!("usnm-{}", hello.engine.family),
            slots: tokio::sync::Semaphore::new(opts.instances.max(1)),
            idle: Mutex::new(vec![first]),
            hello,
            sandbox,
            image,
            opts,
            cleanups,
        })
    }

    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// The first profile that reads `language`.
    fn profile(&self, language: &str) -> Option<&str> {
        if crate::engine::EngineKind::for_language(language).family() != self.hello.engine.family {
            return None;
        }
        self.hello
            .profiles
            .iter()
            .find(|p| {
                p.languages.iter().any(|l| l == language)
                    && self
                        .catalog
                        .engine(&self.hello.engine.id)
                        .is_ok_and(|e| e.family == self.hello.engine.family)
                    && self
                        .catalog
                        .check_production(&self.hello.engine.id, &p.models)
                        .is_ok()
            })
            .map(|p| p.id.as_str())
    }

    fn tag(&self, profile: &str) -> anyhow::Result<String> {
        let t = OcrEngineTag::new(Fields {
            family: &self.hello.engine.family,
            version: &self.hello.engine.version,
            profile,
            image: self.image.clone(),
            models: models_digest(&self.hello, profile),
            route: ROUTE,
            route_version: ROUTE_VERSION,
            contract: CONTRACT,
        })
        .map_err(|e| anyhow::anyhow!("no ocr_engine tag: {e:?}"))?;
        Ok(t.to_string())
    }

    /// The runs record's engine fields for a read on `inst` with
    /// `profile` that answered `result`.
    fn read_info(&self, inst: &Instance, profile: &str, result: &PageResult) -> ReadInfo {
        let cold_start = inst.jobs == 0;
        ReadInfo {
            cold_start,
            model_load_seconds: cold_start.then_some(inst.load_seconds),
            markup: result.markup.as_str(),
            engine_stats_json: serde_json::to_string(&result.engine_stats)
                .unwrap_or_else(|_| "{}".into()),
            ..self.hello_info(&inst.hello, profile, cold_start)
        }
    }

    /// The engine fields a read on `inst` with `profile` has before it
    /// answers: what a failed read reports.
    fn instance_info(&self, inst: &Instance, profile: &str) -> ReadInfo {
        let cold_start = inst.jobs == 0;
        ReadInfo {
            model_load_seconds: cold_start.then_some(inst.load_seconds),
            ..self.hello_info(&inst.hello, profile, cold_start)
        }
    }

    /// The engine fields `h` gives for `profile`, before a read.
    fn hello_info(&self, h: &Hello, profile: &str, cold_start: bool) -> ReadInfo {
        let image_ref = match &self.opts.launch {
            Launch::Podman { image, .. } => image.clone(),
            Launch::Local { .. } => "local".to_owned(),
        };
        let shas: BTreeMap<&str, &str> = h
            .models
            .iter()
            .map(|m| (m.name.as_str(), m.sha256.as_str()))
            .collect();
        let models = h.profile(profile).and_then(|p| {
            crate::engine::models_full(
                p.models
                    .iter()
                    .map(|m| (m.as_str(), shas.get(m.as_str()).copied())),
            )
        });
        let (models_digest, models_json) =
            models.unwrap_or_else(|| ("local".to_owned(), "{}".to_owned()));
        ReadInfo {
            engine_id: h.engine.id.clone(),
            adapter: format!(
                "{} {} ({})",
                h.adapter.name, h.adapter.version, h.adapter.lang
            ),
            image_digest: crate::engine::image_digest(&image_ref),
            image_ref,
            models_digest,
            models_json,
            // The worker sends no options until routes carry them.
            options_json: "{}".into(),
            models_loaded: match h.models_loaded {
                usnm_ocr_contract::engine::ModelsLoaded::AtStart => "at_start",
                usnm_ocr_contract::engine::ModelsLoaded::PerPage => "per_page",
            },
            cold_start,
            model_load_seconds: None,
            markup: "none",
            engine_stats_json: "{}".into(),
        }
    }

    async fn instance(&self) -> anyhow::Result<Instance> {
        if let Some(i) = self.idle.lock().ok().and_then(|mut v| v.pop()) {
            return Ok(i);
        }
        let inst = spawn(&self.opts, self.sandbox.as_ref(), &self.cleanups).await?;
        if !inst.hello.matches(&self.hello) {
            bail!("a new instance's hello differs from the first one's");
        }
        Ok(inst)
    }

    async fn give_back(&self, mut inst: Instance) {
        inst.jobs += 1;
        if inst.jobs >= self.opts.max_jobs.max(1) {
            inst.retire().await;
            return;
        }
        if let Ok(mut v) = self.idle.lock() {
            v.push(inst);
        }
    }

    async fn read_page(
        &self,
        inst: &mut Instance,
        image: &Path,
        language: &str,
        profile: &str,
        run_id: &str,
    ) -> Result<EngineOutput, PageError> {
        let run_id = run_id.to_owned();
        // The scan's and the page directory's names come from it.
        if run_id.is_empty() || run_id.contains(['/', '\\']) || run_id.starts_with('.') {
            return Err(PageError::Keep(EngineError::Failed(anyhow::anyhow!(
                "a run id that can't name a file: {run_id:?}"
            ))));
        }
        // Failures before the engine sees the page aren't the instance's.
        let keep = |e: anyhow::Error| PageError::Keep(EngineError::Failed(e));
        let bytes = tokio::fs::read(image)
            .await
            .context("reading the scan")
            .map_err(keep)?;
        let size = image_size(&bytes)
            .context("the scan's size isn't readable")
            .map_err(keep)?;
        let ext = image.extension().and_then(|e| e.to_str()).unwrap_or("img");
        self.read_bytes(inst, &bytes, ext, size, language, profile, &run_id)
            .await
    }

    /// Reads `bytes`, a scan `width` by `height`, on `inst`: the scan in
    /// the instance's `in/`, a page directory in its `out/`, then the
    /// exchange, and both removed after it.
    #[allow(clippy::too_many_arguments)]
    async fn read_bytes(
        &self,
        inst: &mut Instance,
        bytes: &[u8],
        ext: &str,
        (width, height): (u32, u32),
        language: &str,
        profile: &str,
        run_id: &str,
    ) -> Result<EngineOutput, PageError> {
        let keep = |e: anyhow::Error| PageError::Keep(EngineError::Failed(e));
        let name = format!("{run_id}.{ext}");
        let host_image = inst.dirs.host_in.join(&name);
        tokio::fs::write(&host_image, bytes)
            .await
            .map_err(|e| {
                anyhow::Error::new(crate::worker::ScratchError(format!(
                    "writing the scan for the engine: {e}"
                )))
            })
            .map_err(keep)?;
        let host_out = inst.dirs.host_out.join(run_id);
        let made = make_out_dir(&host_out, self.sandbox.as_ref(), inst.dirs.level.as_deref()).await;
        let result = match made {
            Ok(()) => {
                self.exchange(
                    inst,
                    run_id,
                    &name,
                    bytes,
                    (width, height),
                    language,
                    profile,
                    &host_out,
                )
                .await
            }
            // The worker's own scratch, before the engine sees the page.
            Err(e) => Err(PageError::Keep(EngineError::Failed(anyhow::Error::new(
                crate::worker::ScratchError(format!("the page's out directory: {e:#}")),
            )))),
        };
        let _ = tokio::fs::remove_file(&host_image).await;
        let _ = tokio::fs::remove_dir_all(&host_out).await;
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn exchange(
        &self,
        inst: &mut Instance,
        run_id: &str,
        name: &str,
        bytes: &[u8],
        (width, height): (u32, u32),
        language: &str,
        profile: &str,
        host_out: &Path,
    ) -> Result<EngineOutput, PageError> {
        let req = PageRequest {
            deadline_ms: u64::try_from(self.opts.page_timeout.as_millis()).unwrap_or(u64::MAX),
            image: Image {
                dpi: self.opts.dpi,
                dpi_source: DpiSource::Assumed,
                height,
                path: format!("{}/{name}", inst.dirs.seen_in),
                sha256: crate::prefetch::sha256(bytes),
                width,
            },
            language: language.to_owned(),
            options: BTreeMap::new(),
            out: format!("{}/{run_id}/", inst.dirs.seen_out),
            profile: profile.to_owned(),
            run_id: run_id.to_owned(),
            script: None,
        };
        req.check(&inst.dirs.seen_out)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        tracing::debug!(run_id, "page to engine");
        let line = encode(&ToAdapter::Page(req.clone()));
        let asked = std::time::Instant::now();
        inst.last_exchange = None;
        let sent = match inst.stdin.as_mut() {
            Some(stdin) => match stdin.write_all(&line).await {
                Ok(()) => stdin.flush().await,
                Err(e) => Err(e),
            },
            None => Err(std::io::Error::other("input already closed")),
        };
        if let Err(e) = sent {
            return Err(ReadFailed::error(
                "failed",
                "engine_crash",
                format!(
                    "the engine instance closed its input: {e}; {}",
                    inst.stderr_tail()
                ),
            )
            .into());
        }
        let wait = self.opts.page_timeout + KILL_GRACE;
        let reply = tokio::time::timeout(wait, read_line(&mut inst.stdout)).await;
        inst.last_exchange = Some(asked.elapsed());
        let reply = match reply {
            Err(_) => {
                // Killed now, not after retire's grace: the page's budget
                // allows the deadline plus 15 s and no more.
                if let Some(c) = inst.child.as_mut() {
                    let _ = c.start_kill();
                }
                return Err(ReadFailed::error(
                    "timeout",
                    "engine_timeout",
                    format!("no result from the engine in {wait:?}"),
                )
                .into());
            }
            Ok(Err(e)) => return Err(anyhow::anyhow!("engine protocol: {e}").into()),
            Ok(Ok(None)) => {
                // Give its last stderr lines a moment to arrive.
                if let Some(c) = inst.child.as_mut() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), c.wait()).await;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                return Err(ReadFailed::error(
                    "failed",
                    "engine_crash",
                    format!("the engine instance exited; {}", inst.stderr_tail()),
                )
                .into());
            }
            Ok(Ok(Some(l))) => l,
        };
        let result: PageResult = match decode(&reply) {
            Ok(FromAdapter::Result(r)) => r,
            Ok(FromAdapter::Hello(_)) => {
                return Err(anyhow::anyhow!("engine protocol: a second hello").into())
            }
            Err(e) => return Err(anyhow::anyhow!("engine protocol: {e}").into()),
        };
        result
            .check(&req, &inst.hello, false)
            .map_err(|e| anyhow::anyhow!("engine protocol: {e}"))?;
        let message = || {
            result
                .error
                .as_ref()
                .map(|e| format!("{}: {}", e.kind, e.message))
                .unwrap_or_default()
        };
        match result.status {
            Status::Ok => {}
            Status::Unsupported => {
                return Err(PageError::Keep(EngineError::Unsupported(
                    language.to_owned(),
                )))
            }
            Status::BadInput | Status::Failed | Status::Timeout => {
                // The instance is kept unless the adapter asked for a
                // restart; a timeout's stray processes are its own to stop
                // (`cancel = adapter`).
                let (status, kind) = match result.status {
                    Status::BadInput => ("bad_input", "engine"),
                    Status::Timeout => ("timeout", "engine_timeout"),
                    _ => ("failed", "engine"),
                };
                let e = EngineError::Failed(ReadFailed::error(
                    status,
                    kind,
                    format!("engine {}: {}", result.status.as_str(), message()),
                ));
                return Err(if result.restart {
                    PageError::Drop(e)
                } else {
                    PageError::Keep(e)
                });
            }
        }
        // The page's directory is opened once, without following a
        // symlink, and every check and read is relative to it: the adapter
        // may still be running. It holds only regular files, within the
        // job's cap (13 §13.4.7 step 1).
        let dir = open_dir(host_out).map_err(|e| anyhow::anyhow!("engine protocol: {e:#}"))?;
        check_job_dir(&dir, engine_files::JOB_DIR)
            .map_err(|e| anyhow::anyhow!("engine protocol: {e:#}"))?;
        let name = |path: &str| {
            usnm_ocr_contract::engine::file_name_in(&req.out, path)
                .map(str::to_owned)
                .context("a file outside the page's out directory")
        };
        let layout = result.layout.as_ref().context("no layout")?;
        let text = match &result.text {
            Some(t) => {
                String::from_utf8_lossy(&read_at(&dir, &name(&t.path)?, engine_files::TEXT)?)
                    .into_owned()
            }
            None => String::new(),
        };
        let raw = read_at(&dir, &name(&layout.path)?, engine_files::LAYOUT)?;
        let engine = self.tag(profile)?;
        // A layout that doesn't convert is the page's `convert` failure and
        // the instance stays; one that breaks a limit or the declared
        // granularity is a broken protocol, and it goes.
        let convert = |e: String| {
            PageError::Keep(EngineError::Failed(ReadFailed::error(
                "failed",
                "convert",
                format!("convert: {e}"),
            )))
        };
        let full = (f64::from(width), f64::from(height));
        let mut page = match layout.kind {
            Layout::Alto => {
                let opts = usnm_alto::ParseOptions {
                    dpi: Some(layout.alto_dpi.unwrap_or(f64::from(self.opts.dpi))),
                    ..usnm_alto::ParseOptions::default()
                };
                let mut page = usnm_alto::parse(&raw, &opts).map_err(|e| {
                    use usnm_alto::{AltoError, ParseError};
                    // A broken limit is the protocol's; the instance goes.
                    let limit = matches!(
                        e,
                        ParseError::Limit(_)
                            | ParseError::Alto(
                                AltoError::TooLarge
                                    | AltoError::TooDeep
                                    | AltoError::TooManyElements
                                    | AltoError::TooManyAttributes
                            )
                    );
                    if limit {
                        PageError::from(anyhow::anyhow!("engine protocol: {e}"))
                    } else {
                        convert(e.to_string())
                    }
                })?;
                // Geometry at least as fine as the hello declared.
                if page.granularity > inst.hello.granularity {
                    return Err(anyhow::anyhow!(
                        "engine protocol: ALTO with {} geometry from an engine that declared {}",
                        page.granularity.as_str(),
                        inst.hello.granularity.as_str()
                    )
                    .into());
                }
                // A boxed String can hold an entire line, not a word.
                page.granularity = inst.hello.granularity;
                page
            }
            Layout::Boxes => {
                let b = boxes::parse(&raw, &BoxLimits::default()).map_err(|e| {
                    if e.is_protocol() {
                        PageError::from(anyhow::anyhow!("engine protocol: {e}"))
                    } else {
                        convert(e.to_string())
                    }
                })?;
                boxes::to_page(
                    &b,
                    &Convert {
                        granularity: inst.hello.granularity,
                        full,
                        dir: usnm_alto::default_dir(language, None),
                        markup: result.markup,
                    },
                )
                .map_err(|e| {
                    if e.is_protocol() {
                        PageError::from(anyhow::anyhow!("engine protocol: {e}"))
                    } else {
                        convert(e.to_string())
                    }
                })?
            }
        };
        // In pixels of the image sent: ALTO in inch1200 converts at a dpi
        // and may round.
        if (page.width, page.height) != full {
            page.rescale(full.0, full.1);
        }
        let out = EngineOutput {
            text,
            alto: Vec::new(),
            page: Some(page),
            layout_from: match layout.kind {
                Layout::Alto => "ALTO",
                Layout::Boxes => "boxes.json",
            },
            engine,
            read: self.read_info(inst, profile, &result),
        };
        Ok(out)
    }
}

/// Opens a page's out directory, refusing a symlink.
fn open_dir(path: &Path) -> anyhow::Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("opening {}", path.display()))
}

/// Checks that a page's directory holds only regular files, `max` bytes
/// in all (the adapter writes nothing else, 13 §13.4.1).
fn check_job_dir(dir: &rustix::fd::OwnedFd, max: u64) -> anyhow::Result<()> {
    use rustix::fs::{AtFlags, FileType};
    let mut total = 0u64;
    for entry in rustix::fs::Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let st = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            bail!("{:?} in the page's directory isn't a regular file", name);
        }
        total = total.saturating_add(u64::try_from(st.st_size).unwrap_or(u64::MAX));
        if total > max {
            bail!("the page's directory holds over {max} bytes");
        }
    }
    Ok(())
}

/// Reads the regular file `name` in `dir`, at most `max` bytes, without
/// following a symlink.
fn read_at(dir: &rustix::fd::OwnedFd, name: &str, max: u64) -> anyhow::Result<Vec<u8>> {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::io::Read;
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("opening {name}"))?;
    let st = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        bail!("{name} isn't a regular file");
    }
    let mut buf = Vec::new();
    std::fs::File::from(fd)
        .take(max + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        bail!("{name} is over {max} bytes");
    }
    Ok(buf)
}

/// Creates a page's out directory: mode 0700, writable by the container
/// user through the slot's ACL, and labelled at the slot's level.
async fn make_out_dir(
    dir: &Path,
    sandbox: Option<&Sandbox>,
    level: Option<&str>,
) -> anyhow::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    if let Some(sb) = sandbox {
        let e = sb.info.engine_uid;
        // The mode's group bits are the ACL mask: open it again so the
        // inherited entries apply.
        checked(
            tokio::process::Command::new("setfacl")
                .args(["-m", &format!("u:{e}:rwx,m::rwx")])
                .arg(dir),
        )
        .await?;
        if let Some(level) = level {
            checked(
                tokio::process::Command::new("chcon")
                    .args(["-t", "container_file_t", "-l", level])
                    .arg(dir),
            )
            .await?;
        }
    }
    Ok(())
}

/// stderr lines kept per instance, the longest kept, and how many lines a
/// second are kept before the rest are dropped and counted (13 §13.4.1).
const STDERR_LINES: usize = 50;
const STDERR_LINE: usize = 4096;
const STDERR_PER_SEC: u32 = 200;

/// Reads an adapter's stderr as it arrives, in fixed chunks, so neither a
/// long line nor a flood can grow the worker's memory or stall the
/// adapter: each line is cut at [`STDERR_LINE`] bytes, lines over
/// [`STDERR_PER_SEC`] a second are dropped and counted, and the last
/// [`STDERR_LINES`] are kept for a failure's message.
async fn drain_reader(
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
async fn spawn(
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

#[async_trait]
impl Engine for ContractEngine {
    fn source(&self) -> &str {
        &self.source
    }

    fn can_read(&self, language: &str) -> bool {
        self.profile(language).is_some()
    }

    fn routes(&self) -> BTreeMap<String, String> {
        let mut routes = BTreeMap::new();
        for profile in &self.hello.profiles {
            for language in &profile.languages {
                if self.profile(language) == Some(profile.id.as_str()) {
                    routes.insert(language.clone(), profile.id.clone());
                }
            }
        }
        routes
    }

    fn models(&self) -> Vec<String> {
        self.hello
            .models
            .iter()
            .map(|model| model.name.clone())
            .collect()
    }

    fn dpi(&self) -> u32 {
        self.opts.dpi
    }

    fn execution_config(&self) -> serde_json::Value {
        let (runtime, limits) = match &self.opts.launch {
            Launch::Local { .. } => ("local", None),
            Launch::Podman { remote, .. } => ("podman", Some(remote.limits)),
        };
        serde_json::json!({
            "runtime": runtime,
            "limits": limits,
            "dpi": self.opts.dpi,
            "threads": self.opts.threads.max(1),
            "instances": self.opts.instances,
            "start_timeout_seconds": self.opts.start_timeout.as_secs_f64(),
            "page_timeout_seconds": self.opts.page_timeout.as_secs_f64(),
            "max_jobs": self.opts.max_jobs,
        })
    }

    fn describe(&self, language: &str) -> Option<(String, ReadInfo)> {
        let profile = self.profile(language)?;
        let tag = self.tag(profile).ok()?;
        // A guess, before the page has a slot: the instance `instance()`
        // would take now (the last idle one, cold if it hasn't read a page
        // yet, or a new one). The read reports the one it gets.
        let cold_start = self
            .idle
            .lock()
            .map_or(true, |v| v.last().is_none_or(|i| i.jobs == 0));
        Some((tag, self.hello_info(&self.hello, profile, cold_start)))
    }

    async fn shutdown(&self) {
        let idle: Vec<Instance> = self
            .idle
            .lock()
            .map(|mut v| v.drain(..).collect())
            .unwrap_or_default();
        for i in idle {
            i.retire().await;
        }
        // And the instances of pages cut short.
        loop {
            let tasks: Vec<_> = self
                .cleanups
                .lock()
                .map(|mut c| c.drain(..).collect())
                .unwrap_or_default();
            if tasks.is_empty() {
                break;
            }
            for t in tasks {
                let _ = t.await;
            }
        }
    }

    async fn ocr(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
    ) -> Result<EngineOutput, EngineError> {
        let run_id = crate::provenance::mint_run_id(&self.opts.node);
        self.read(image, language, work, &run_id).await
    }

    async fn read(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
        run_id: &str,
    ) -> Result<EngineOutput, EngineError> {
        self.read_traced(image, language, work, run_id, &|_| {})
            .await
    }

    async fn read_traced(
        &self,
        image: &Path,
        language: &str,
        _work: &Path,
        run_id: &str,
        on_instance: &(dyn Fn(ReadInfo) + Send + Sync),
    ) -> Result<EngineOutput, EngineError> {
        let profile = self
            .profile(language)
            .ok_or_else(|| EngineError::Unsupported(language.to_owned()))?
            .to_owned();
        let _permit = self
            .slots
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut inst = self.instance().await?;
        on_instance(self.instance_info(&inst, &profile));
        match self
            .read_page(&mut inst, image, language, &profile, run_id)
            .await
        {
            Ok(out) => {
                self.give_back(inst).await;
                Ok(out)
            }
            Err(PageError::Keep(e)) => {
                self.give_back(inst).await;
                Err(e)
            }
            // The instance is dropped (killed and removed), and the next
            // page starts a new one.
            Err(PageError::Drop(e)) => {
                inst.retire().await;
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_image_sizes_from_headers() {
        assert_eq!(
            image_size(&crate::testimg::text_image("HELLO")),
            Some((260, 122))
        );
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(1000u32.to_be_bytes());
        png.extend(700u32.to_be_bytes());
        assert_eq!(image_size(&png), Some((1000, 700)));
        // A JPEG: SOI, an APP0 segment, then SOF0 with height 7208 and width 5412.
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 0, 0, 0xFF, 0xC0, 0, 11, 8];
        jpg.extend(7208u16.to_be_bytes());
        jpg.extend(5412u16.to_be_bytes());
        jpg.extend([1, 1, 0x11, 0]);
        assert_eq!(image_size(&jpg), Some((5412, 7208)));
        let mut jp2 = b"\0\0\0\x0cjP  \r\n\x87\n\0\0\0\x2dihdr".to_vec();
        jp2.extend(7208u32.to_be_bytes());
        jp2.extend(5412u32.to_be_bytes());
        assert_eq!(image_size(&jp2), Some((5412, 7208)));
        assert_eq!(image_size(b"nothing"), None);
    }

    #[test]
    fn job_directories_hold_only_bounded_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 100]).unwrap();
        let fd = open_dir(dir.path()).unwrap();
        check_job_dir(&fd, 1000).unwrap();
        assert!(check_job_dir(&fd, 99).is_err());
        assert_eq!(read_at(&fd, "a", 100).unwrap().len(), 100);
        assert!(read_at(&fd, "a", 99).is_err());
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("l")).unwrap();
        assert!(read_at(&fd, "l", 1000).is_err(), "a symlink");
        assert!(check_job_dir(&open_dir(dir.path()).unwrap(), 1000).is_err());
        std::fs::remove_file(dir.path().join("l")).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(
            check_job_dir(&open_dir(dir.path()).unwrap(), 1000).is_err(),
            "a directory"
        );
        // The page's directory itself may not be a symlink.
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/link")).unwrap();
        assert!(open_dir(&dir.path().join("sub/link")).is_err());
    }

    /// An adapter that sends its hello and reads requests without
    /// answering.
    async fn silent_engine(dir: &Path) -> ContractEngine {
        script_engine(dir, "cat >/dev/null\n").await
    }

    /// An adapter that sends its hello, then runs `body` on its requests.
    async fn script_engine(dir: &Path, body: &str) -> ContractEngine {
        script_engine_with(dir, "", body).await
    }

    /// [`script_engine`], running `before` before each instance's hello.
    pub(super) async fn script_engine_with(dir: &Path, before: &str, body: &str) -> ContractEngine {
        let hello = r#"{"adapter":{"lang":"sh","name":"silent","version":"0"},"cancel":"kill","contract":1,"deterministic":true,"engine":{"family":"tesseract","id":"silent","version":"1"},"granularity":"page","layout":"boxes","models":[{"name":"test-model","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}],"models_loaded":"at_start","profiles":[{"id":"eng","languages":["eng"],"models":["test-model"],"tunable":[]}],"type":"hello"}"#;
        let script = dir.join("adapter");
        std::fs::write(
            &script,
            format!("#!/bin/sh\n{before}\necho '{hello}'\n{body}"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let mut catalog = usnm_ocr_route::ENGINES.clone();
        let mut engine = catalog.engine[0].clone();
        engine.id = "silent".into();
        engine.family = "tesseract".into();
        engine.model[0].name = "test-model".into();
        catalog.engine.push(engine);
        ContractEngine::start_with_catalog(
            ContractOptions {
                launch: Launch::Local {
                    program: script,
                    args: Vec::new(),
                    scratch: dir.to_owned(),
                },
                instances: 1,
                threads: 1,
                start_timeout: Duration::from_secs(10),
                page_timeout: Duration::from_secs(10),
                max_jobs: 100,
                dpi: 300,
                node: "test1".into(),
            },
            catalog,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn run_metadata_execution_config_excludes_adapter_command_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = silent_engine(dir.path()).await;
        if let Launch::Local { args, .. } = &mut engine.opts.launch {
            args.push("--token=private-credential".into());
        }
        engine.opts.threads = 0;
        let config = engine.execution_config();
        assert_eq!(
            config,
            serde_json::json!({
                "runtime": "local",
                "limits": null,
                "dpi": 300,
                "threads": 1,
                "instances": 1,
                "start_timeout_seconds": 10.0,
                "page_timeout_seconds": 10.0,
                "max_jobs": 100,
            })
        );
        assert!(!config.to_string().contains("private-credential"));
        assert!(!config.to_string().contains(dir.path().to_str().unwrap()));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn contract_profiles_gate_actual_hello_models_and_engine_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = silent_engine(dir.path()).await;
        assert!(e.can_read("eng"));
        let original = e.catalog.clone();
        for code in [true, false] {
            for field in [
                "author",
                "source",
                "credit",
                "restricted",
                "undecided",
                "unknown",
            ] {
                e.catalog = original.clone();
                let a = if code {
                    &mut e.catalog.engine.last_mut().unwrap().attribution
                } else {
                    &mut e.catalog.engine.last_mut().unwrap().model[0].attribution
                };
                match field {
                    "author" => a.author = None,
                    "source" => a.source = None,
                    "credit" => a.credit = None,
                    "restricted" => a.restricted = Some("excluded".into()),
                    "undecided" => a.undecided = Some("pending".into()),
                    "unknown" => a.license = "unknown".into(),
                    _ => unreachable!(),
                }
                assert!(!e.can_read("eng"), "{code} {field}");
                assert!(e.describe("eng").is_none());
            }
        }
        e.catalog = original.clone();
        e.hello.profiles[0].models.push("unrecorded-model".into());
        assert!(!e.can_read("eng"));
        e.hello.profiles[0].models.pop();
        e.catalog.engine.last_mut().unwrap().production = false;
        assert!(!e.can_read("eng"));
        e.catalog = original;
        e.hello.engine.family = "wrong-family".into();
        assert!(!e.can_read("eng"));
        e.shutdown().await;
    }

    #[tokio::test]
    async fn contract_routes_only_the_selected_engine_and_still_checks_licenses() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = silent_engine(dir.path()).await;
        e.hello.profiles[0].languages.push("jpn".into());
        assert!(e.can_read("eng"));
        assert!(!e.can_read("jpn"));
        e.hello.engine.family = "ndlocr-lite".into();
        e.catalog.engine.last_mut().unwrap().family = "ndlocr-lite".into();
        e.hello.profiles[0].id = "default".into();
        assert!(!e.can_read("eng"));
        assert!(e.can_read("jpn"));
        assert!(e
            .describe("jpn")
            .unwrap()
            .0
            .starts_with("ndlocr-lite/1 profile=default "));
        e.catalog.engine.last_mut().unwrap().production = false;
        assert!(!e.can_read("jpn"));
        e.shutdown().await;
    }

    /// A page directory the worker can't make is its own scratch failure,
    /// before the engine sees the page: no `ocr_engine_run` for it.
    #[tokio::test]
    async fn an_out_directory_that_cant_be_made_is_scratch() {
        let dir = tempfile::tempdir().unwrap();
        let e = silent_engine(dir.path()).await;
        let mut inst = e.instance().await.unwrap();
        std::fs::create_dir(inst.dirs.host_out.join("r1")).unwrap();
        let img = crate::testimg::text_image("HELLO");
        let r = e
            .read_bytes(&mut inst, &img, "pgm", (10, 10), "eng", "eng", "r1")
            .await;
        match r {
            Err(PageError::Keep(EngineError::Failed(err))) => {
                assert!(err.is::<crate::worker::ScratchError>(), "{err:#}")
            }
            Err(PageError::Keep(e) | PageError::Drop(e)) => panic!("{e:#}"),
            Ok(_) => panic!("read"),
        }
        inst.retire().await;
        e.shutdown().await;
    }

    /// The first read of a new idle instance is a cold start, before it
    /// (`describe`, used when the read fails) as after it.
    #[tokio::test]
    async fn a_fresh_idle_instance_is_cold() {
        let dir = tempfile::tempdir().unwrap();
        let e = silent_engine(dir.path()).await;
        assert_eq!(e.idle.lock().unwrap().len(), 1);
        assert!(e.describe("eng").unwrap().1.cold_start);
        let inst = e.instance().await.unwrap();
        e.give_back(inst).await;
        assert!(!e.describe("eng").unwrap().1.cold_start);
        // None idle: the read starts one.
        let inst = e.instance().await.unwrap();
        assert!(e.describe("eng").unwrap().1.cold_start);
        inst.retire().await;
        e.shutdown().await;
    }

    /// A read reports the instance it got once it has a slot, not the
    /// guess `describe` made before: here the page waits for the one
    /// instance, which another page has warmed by then.
    #[tokio::test]
    async fn a_read_reports_the_instance_it_got() {
        let dir = tempfile::tempdir().unwrap();
        let e = std::sync::Arc::new(
            script_engine(
                dir.path(),
                r#"while IFS= read -r l; do
  run=$(printf '%s\n' "$l" | sed 's/.*"run_id":"\([^"]*\)".*/\1/')
  printf '{"engine_stats":{},"error":{"kind":"image","message":"unreadable"},"markup":"none","run_id":"%s","status":"bad_input","type":"result","warnings":[]}\n' "$run"
done
"#,
            )
            .await,
        );
        let image = dir.path().join("page.pgm");
        std::fs::write(&image, crate::testimg::text_image("HELLO")).unwrap();
        let read = |run_id: &'static str| {
            let (e, image) = (e.clone(), image.clone());
            let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let s = seen.clone();
            let task = tokio::spawn(async move {
                let on = move |i: ReadInfo| s.lock().unwrap().push(i.cold_start);
                e.read_traced(&image, "eng", Path::new("."), run_id, &on)
                    .await
            });
            (task, seen)
        };
        // The first page holds the only slot and instance.
        let permit = e.slots.acquire().await.unwrap();
        let mut inst = e.instance().await.unwrap();
        assert!(e.describe("eng").unwrap().1.cold_start, "none idle");
        let (second, seen) = read("r2");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(seen.lock().unwrap().is_empty(), "waits for the slot");
        inst.jobs = 1;
        e.give_back(inst).await;
        drop(permit);
        let r = second.await.unwrap();
        assert!(
            matches!(&r, Err(EngineError::Failed(err)) if crate::engine::read_failure(err) == ("bad_input", "engine")),
            "{r:?}"
        );
        assert_eq!(*seen.lock().unwrap(), vec![false], "the warm instance");
        // A new instance's first read is cold.
        let idle = e.idle.lock().unwrap().pop().unwrap();
        idle.retire().await;
        let (third, seen) = read("r3");
        assert!(third.await.unwrap().is_err());
        assert_eq!(*seen.lock().unwrap(), vec![true]);
        e.shutdown().await;
    }

    #[tokio::test]
    async fn stderr_is_bounded() {
        // One 1 MiB line and 1,000 short ones: the kept lines are cut and
        // capped, and the flood is cut by the rate limit.
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "head -c 1048576 /dev/zero | tr '\\0' x; echo; i=0; while [ $i -lt 1000 ]; do echo line $i; i=$((i+1)); done"])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        // Drain the child's stdout through the same reader as stderr.
        let out = child.stdout.take().unwrap();
        let keep = Arc::new(Mutex::new(VecDeque::new()));
        drain_reader(out, keep.clone()).await;
        let k = keep.lock().unwrap();
        assert_eq!(k.len(), STDERR_LINES);
        assert!(k.iter().all(|l| l.len() <= STDERR_LINE));
        assert!(k.back().unwrap().starts_with("line "));
    }
}
