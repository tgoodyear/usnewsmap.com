//! Cross-platform BOINC slot execution for `usnm_quality` (#522, #533).
//!
//! [`compute`] is the whole computation as a library call: the page's
//! features, the readers' pairwise metrics, the hat, the consensus,
//! findability and confusion pairs, with timings. [`run_slot`] wraps it in a
//! BOINC slot on Linux, macOS or Windows: resolves both direct `<copy_file/>`
//! files and `<soft_link>` XML pointers, collects diagnostics, runs the job's
//! checks and computation, writes `result.json`, `confusions.json`, `diag.txt`
//! and `fraction_done`, and finishes by writing `boinc_finish_called` (`"0\n"`).
use crate::{
    features,
    findability::Places,
    report::{self, PageFeatures, PageInputs, ReaderInput},
};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    ops::ControlFlow,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
use usnm_alto::{Page, ParseError, ParseOptions};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{
        Engine, Host, Job, Outcome, ResultManifest, Texts, Timings, MAX_ALTO, MAX_DIAG, MAX_ERROR,
        MAX_JOB_JSON, MAX_PLACES, MAX_SCAN, MAX_TEXTS,
    },
};

pub const JOB_FILE: &str = "quality.json";
pub const PAGE_FILE: &str = "page.jpg";
pub const ALTO_FILE: &str = "loc.xml";
pub const TEXTS_FILE: &str = "texts.json";
pub const PLACES_FILE: &str = "places.txt";
pub const RESULT_FILE: &str = "result.json";
pub const CONFUSIONS_FILE: &str = "confusions.json";
pub const DIAG_FILE: &str = "diag.txt";
pub const FRACTION_FILE: &str = "fraction_done";
pub const FINISH_FILE: &str = "boinc_finish_called";
pub const MMAP_FILE: &str = "boinc_mmap_file";

/// Upstream BOINC `MSG_CHANNEL_SIZE` (`lib/app_ipc.h`).
pub const MSG_CHANNEL_SIZE: usize = 1024;
/// Upstream BOINC `SHARED_MEM` total size: 8 consecutive `MSG_CHANNEL` blocks.
pub const SHARED_MEM_SIZE: usize = MSG_CHANNEL_SIZE * 8;
/// Offset of `SHARED_MEM::process_control_request` (client -> app).
pub const PROCESS_CONTROL_REQUEST_OFFSET: usize = 0;
/// Offset of `SHARED_MEM::process_control_reply` (app -> client).
pub const PROCESS_CONTROL_REPLY_OFFSET: usize = MSG_CHANNEL_SIZE;
/// Offset of `SHARED_MEM::heartbeat` (client -> app).
pub const HEARTBEAT_OFFSET: usize = MSG_CHANNEL_SIZE * 4;
/// Offset of `SHARED_MEM::app_status` (app -> client).
pub const APP_STATUS_OFFSET: usize = MSG_CHANNEL_SIZE * 7;
/// Default BOINC heartbeat loss timeout when heartbeats are active.
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

/// Lifecycle action applied to a registered child process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildAction {
    Suspend,
    Resume,
    Terminate,
}

pub type ChildHook = Arc<dyn Fn(u32, ChildAction) + Send + Sync>;

fn default_signal_child(pgid: u32, action: ChildAction) {
    #[cfg(unix)]
    {
        let sig = match action {
            ChildAction::Suspend => "-STOP",
            ChildAction::Resume => "-CONT",
            ChildAction::Terminate => "-TERM",
        };
        let group = format!("-{pgid}");
        let ok = std::process::Command::new("kill")
            .args([sig, &group])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            let _ = std::process::Command::new("kill")
                .args([sig, &pgid.to_string()])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pgid, action);
    }
}

struct BoincControlState {
    suspended: bool,
    quit_requested: bool,
    abort_requested: bool,
    heartbeat_lost: bool,
    heartbeat_enabled: bool,
    last_heartbeat: Instant,
    child_pgid: Option<u32>,
    child_hook: Option<ChildHook>,
}

impl BoincControlState {
    fn exit_reason(&self) -> Option<&'static str> {
        if self.abort_requested {
            Some("abort")
        } else if self.quit_requested {
            Some("quit")
        } else if self.heartbeat_lost {
            Some("heartbeat_lost")
        } else {
            None
        }
    }

    fn dispatch_child(&self, action: ChildAction) {
        if let Some(pgid) = self.child_pgid {
            if let Some(ref hook) = self.child_hook {
                hook(pgid, action);
            } else {
                default_signal_child(pgid, action);
            }
        }
    }
}

struct BoincControlInner {
    state: Mutex<BoincControlState>,
    cvar: Condvar,
}

/// Monitors BOINC's shared-memory process-control and heartbeat channels in
/// `boinc_mmap_file` (`lib/app_ipc.h`).
#[derive(Clone)]
pub struct BoincControl {
    inner: Arc<BoincControlInner>,
}

pub type BoincRuntime = BoincControl;

impl BoincControl {
    /// Starts monitoring `boinc_mmap_file` in `work` with the standard 30-second
    /// heartbeat timeout.
    pub fn start(work: &Path) -> Self {
        Self::start_with_heartbeat_timeout(work, HEARTBEAT_TIMEOUT)
    }

    /// Starts monitoring `boinc_mmap_file` in `work` with a custom heartbeat timeout.
    pub fn start_with_heartbeat_timeout(work: &Path, heartbeat_timeout: Duration) -> Self {
        let init_path = resolve_slot_file(work, "init_data.xml");
        let no_heartbeat_check =
            std::fs::read_to_string(init_path).is_ok_and(|xml| xml.contains("<no_heartbeat_check"));
        let mmap_path = resolve_slot_file(work, MMAP_FILE);
        let inner = Arc::new(BoincControlInner {
            state: Mutex::new(BoincControlState {
                suspended: false,
                quit_requested: false,
                abort_requested: false,
                heartbeat_lost: false,
                heartbeat_enabled: false,
                last_heartbeat: Instant::now(),
                child_pgid: None,
                child_hook: None,
            }),
            cvar: Condvar::new(),
        });

        if mmap_path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.len() >= SHARED_MEM_SIZE as u64)
        {
            poll_mmap_once(&mmap_path, &inner, heartbeat_timeout, no_heartbeat_check);
            let weak = Arc::downgrade(&inner);
            let _ = std::thread::Builder::new()
                .name("boinc-mmap-control".into())
                .spawn(move || {
                    while let Some(shared) = weak.upgrade() {
                        poll_mmap_once(&mmap_path, &shared, heartbeat_timeout, no_heartbeat_check);
                        if shared
                            .state
                            .lock()
                            .ok()
                            .and_then(|s| s.exit_reason())
                            .is_some()
                        {
                            break;
                        }
                        drop(shared);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                });
        }

        Self { inner }
    }

    /// Registers a custom callback for suspending, resuming, or terminating a child process group.
    pub fn set_child_hook(&self, hook: Option<ChildHook>) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.child_hook = hook;
        }
    }

    /// Registers (or clears) a child process/group ID for `<suspend/>`, `<resume/>`,
    /// `<quit/>`, `<abort/>`, and heartbeat loss signaling.
    pub fn set_child_pgid(&self, pgid: Option<u32>) {
        if let Ok(mut s) = self.inner.state.lock() {
            s.child_pgid = pgid;
            if pgid.is_some() {
                if s.exit_reason().is_some() {
                    s.dispatch_child(ChildAction::Terminate);
                } else if s.suspended {
                    s.dispatch_child(ChildAction::Suspend);
                }
            }
        }
    }

    /// Returns whether the task is currently suspended by `<suspend/>`.
    pub fn is_suspended(&self) -> bool {
        self.inner.state.lock().is_ok_and(|s| s.suspended)
    }

    /// Returns `Some("quit")`, `Some("abort")`, or `Some("heartbeat_lost")` when
    /// BOINC requested immediate termination or heartbeat was lost.
    pub fn should_exit(&self) -> Option<&'static str> {
        self.inner.state.lock().ok().and_then(|s| s.exit_reason())
    }

    /// Blocks while suspended unless an exit condition (`quit`, `abort`, `heartbeat_lost`)
    /// is triggered.
    pub fn wait_if_suspended_or_should_exit(&self) -> ControlFlow<&'static str, ()> {
        let Ok(mut s) = self.inner.state.lock() else {
            return ControlFlow::Continue(());
        };
        loop {
            if let Some(reason) = s.exit_reason() {
                return ControlFlow::Break(reason);
            }
            if !s.suspended {
                return ControlFlow::Continue(());
            }
            let Ok((guard, _)) = self.inner.cvar.wait_timeout(s, Duration::from_millis(50)) else {
                return ControlFlow::Continue(());
            };
            s = guard;
        }
    }
}

fn poll_mmap_once(
    mmap_path: &Path,
    inner: &BoincControlInner,
    heartbeat_timeout: Duration,
    no_heartbeat_check: bool,
) {
    let Ok(mut file) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(mmap_path)
    else {
        return;
    };
    let Ok(meta) = file.metadata() else {
        return;
    };
    if !meta.is_file() || meta.len() < SHARED_MEM_SIZE as u64 {
        return;
    }

    let mut req_buf = [0u8; MSG_CHANNEL_SIZE];
    if file
        .seek(SeekFrom::Start(PROCESS_CONTROL_REQUEST_OFFSET as u64))
        .is_ok()
        && file.read_exact(&mut req_buf).is_ok()
        && req_buf[0] != 0
    {
        let end = req_buf[1..]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(MSG_CHANNEL_SIZE - 1);
        let msg = String::from_utf8_lossy(&req_buf[1..1 + end]).into_owned();
        if file
            .seek(SeekFrom::Start(PROCESS_CONTROL_REQUEST_OFFSET as u64))
            .is_ok()
        {
            let _ = file.write_all(&[0u8]);
            let _ = file.sync_data();
        }
        if let Ok(mut s) = inner.state.lock() {
            if msg.contains("<quit") {
                s.quit_requested = true;
                s.suspended = false;
                s.dispatch_child(ChildAction::Terminate);
                inner.cvar.notify_all();
            } else if msg.contains("<abort") {
                s.abort_requested = true;
                s.suspended = false;
                s.dispatch_child(ChildAction::Terminate);
                inner.cvar.notify_all();
            } else if msg.contains("<suspend") {
                s.suspended = true;
                s.dispatch_child(ChildAction::Suspend);
                inner.cvar.notify_all();
            } else if msg.contains("<resume") {
                s.suspended = false;
                s.last_heartbeat = Instant::now();
                s.dispatch_child(ChildAction::Resume);
                inner.cvar.notify_all();
            }
        }
    }

    let mut hb_buf = [0u8; MSG_CHANNEL_SIZE];
    if file.seek(SeekFrom::Start(HEARTBEAT_OFFSET as u64)).is_ok()
        && file.read_exact(&mut hb_buf).is_ok()
        && hb_buf[0] != 0
    {
        if file.seek(SeekFrom::Start(HEARTBEAT_OFFSET as u64)).is_ok() {
            let _ = file.write_all(&[0u8]);
            let _ = file.sync_data();
        }
        if let Ok(mut s) = inner.state.lock() {
            s.last_heartbeat = Instant::now();
            s.heartbeat_enabled = true;
        }
    }

    if let Ok(mut s) = inner.state.lock() {
        if !no_heartbeat_check
            && s.heartbeat_enabled
            && !s.suspended
            && !s.heartbeat_lost
            && s.last_heartbeat.elapsed() > heartbeat_timeout
        {
            s.heartbeat_lost = true;
            s.dispatch_child(ChildAction::Terminate);
            inner.cvar.notify_all();
        }
    }
}

/// Writes a BOINC `MSG_CHANNEL` frame (`byte[0] = 1`, `byte[1..]` = NUL-terminated XML)
/// at `offset` in `boinc_mmap_file` (creating/extending it to 8192 bytes if needed).
pub fn write_mmap_channel(mmap_path: &Path, offset: usize, xml: &str) -> anyhow::Result<()> {
    ensure!(
        offset + MSG_CHANNEL_SIZE <= SHARED_MEM_SIZE,
        "invalid MSG_CHANNEL offset {offset}"
    );
    let xml_bytes = xml.as_bytes();
    ensure!(
        xml_bytes.len() < MSG_CHANNEL_SIZE - 1,
        "MSG_CHANNEL payload too large"
    );
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(mmap_path)?;
    if f.metadata()?.len() < SHARED_MEM_SIZE as u64 {
        f.set_len(SHARED_MEM_SIZE as u64)?;
    }
    let mut payload = vec![0u8; MSG_CHANNEL_SIZE - 1];
    payload[..xml_bytes.len()].copy_from_slice(xml_bytes);
    f.seek(SeekFrom::Start((offset + 1) as u64))?;
    f.write_all(&payload)?;
    f.seek(SeekFrom::Start(offset as u64))?;
    f.write_all(&[1u8])?;
    f.sync_data()?;
    Ok(())
}

/// The output files in output template order.
pub fn outputs() -> [&'static str; 3] {
    [RESULT_FILE, CONFUSIONS_FILE, DIAG_FILE]
}

/// The input files in input template order, with the job last.
pub fn inputs() -> [&'static str; 5] {
    [PAGE_FILE, ALTO_FILE, TEXTS_FILE, PLACES_FILE, JOB_FILE]
}

/// Resolves a logical file name in a BOINC slot directory, supporting both
/// `<copy_file/>` direct files and BOINC `<soft_link>...</soft_link>` XML
/// pseudo-symlinks.
pub fn resolve_slot_file(work: &Path, name: &str) -> PathBuf {
    let p = work.join(name);
    if let Ok(meta) = std::fs::metadata(&p) {
        if meta.is_file() && meta.len() <= 4096 {
            if let Ok(text) = std::fs::read_to_string(&p) {
                let trimmed = text.trim();
                if let Some(rest) = trimmed.strip_prefix("<soft_link>") {
                    if let Some(target) = rest.strip_suffix("</soft_link>") {
                        let target_path = Path::new(target.trim());
                        return if target_path.is_absolute() {
                            target_path.to_path_buf()
                        } else {
                            work.join(target_path)
                        };
                    }
                }
            }
        }
    }
    p
}

/// Bounded regular-file read.
pub fn read_bounded(path: &Path, max: usize) -> anyhow::Result<Vec<u8>> {
    let meta = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(meta.is_file(), "{} is not a regular file", path.display());
    ensure!(
        meta.len() <= max as u64,
        "{} is over size limit",
        path.display()
    );
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut bytes = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "{} is over size limit", path.display());
    Ok(bytes)
}

/// Cross-platform atomic file write (write to temporary sibling, sync, close, rename).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("output");
    let tmp = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    {
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o644));
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Reads an ALTO into a page in the image's pixels. LoC's ALTO is in
/// 1/1200 inch; it is read at 1200 dpi (its own units) and then scaled to
/// the image by the page's width and height, as the gold set's `crops.py`
/// scales it, so the units never matter.
pub fn parse_alto(xml: &[u8], width: f64, height: f64) -> anyhow::Result<Page> {
    ensure!(width > 0.0 && height > 0.0, "page has no size");
    let opts = ParseOptions::default();
    let mut page = match usnm_alto::parse(xml, &opts) {
        Ok(p) => p,
        Err(ParseError::NeedsDpi(_)) => usnm_alto::parse(
            xml,
            &ParseOptions {
                dpi: Some(1200.0),
                ..opts
            },
        )
        .context("ALTO")?,
        Err(e) => return Err(anyhow::anyhow!("ALTO: {e}")),
    };
    ensure!(
        page.width > 0.0 && page.height > 0.0,
        "ALTO page has no size"
    );
    page.rescale(width, height);
    Ok(page)
}

/// The diagnostics text, kept under [`MAX_DIAG`]: each section is cut to its
/// own cap and the whole to the limit, with a note where it was cut.
pub struct Diag(String);
impl Default for Diag {
    fn default() -> Self {
        Self::new()
    }
}
impl Diag {
    pub fn new() -> Self {
        Self(String::new())
    }
    pub fn section(&mut self, title: &str, body: &str, cap: usize) {
        let _ = writeln!(self.0, "== {title}");
        let body = body.trim_end();
        if body.len() > cap {
            let mut end = cap;
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            let _ = writeln!(
                self.0,
                "{}\n[cut at {cap} of {} bytes]",
                &body[..end],
                body.len()
            );
        } else if !body.is_empty() {
            let _ = writeln!(self.0, "{body}");
        }
    }
    pub fn finish(mut self) -> Vec<u8> {
        let limit = MAX_DIAG - 64;
        if self.0.len() > limit {
            let mut end = limit;
            while !self.0.is_char_boundary(end) {
                end -= 1;
            }
            self.0.truncate(end);
            self.0.push_str("\n[diagnostics cut at the size limit]\n");
        }
        self.0.into_bytes()
    }
}

/// The last `n` bytes of `s`, from a character boundary.
pub fn tail(s: &str, n: usize) -> &str {
    let mut i = s.len().saturating_sub(n);
    while !s.is_char_boundary(i) {
        i += 1;
    }
    &s[i..]
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim())
}

const INIT_FIELDS: [&str; 10] = [
    "major_version",
    "minor_version",
    "release",
    "app_version",
    "plan_class",
    "gpu_type",
    "gpu_device_num",
    "gpu_opencl_dev_index",
    "ncpus",
    "rsc_memory_bound",
];

pub fn boinc_info(work: &Path, host: &mut Host, diag: &mut Diag) {
    let init_path = resolve_slot_file(work, "init_data.xml");
    let Ok(xml) = std::fs::read_to_string(init_path) else {
        diag.section("boinc", "no init_data.xml in the slot", 256);
        return;
    };
    let mut body = String::new();
    for f in INIT_FIELDS {
        if let Some(v) = tag(&xml, f) {
            if v.len() <= 32
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                let _ = writeln!(body, "{f}: {v}");
            }
        }
    }
    if let (Some(a), Some(b), Some(c)) = (
        tag(&xml, "major_version"),
        tag(&xml, "minor_version"),
        tag(&xml, "release"),
    ) {
        host.boinc_client = Some(format!("{a}.{b}.{c}")).filter(|s| s.len() <= 32);
    }
    host.plan_class = tag(&xml, "plan_class")
        .filter(|s| s.len() <= 32)
        .map(str::to_owned);
    host.ncpus = tag(&xml, "ncpus").and_then(|s| s.parse().ok());
    diag.section("boinc (init_data.xml, selected fields)", &body, 2048);
}

pub fn runtime_info(host: &mut Host, diag: &mut Diag) {
    let mut body = String::new();
    if let Ok(env) = std::fs::read_to_string("/run/.containerenv") {
        for l in env.lines() {
            if let Some(v) = l.strip_prefix("engine=") {
                let v = v.trim_matches('"');
                host.runtime = Some(v.chars().take(32).collect());
                let _ = writeln!(body, "engine: {v}");
            }
            if let Some(v) = l.strip_prefix("rootless=") {
                host.rootless = Some(v.trim() == "1");
                let _ = writeln!(body, "rootless: {v}");
            }
        }
    } else if Path::new("/.dockerenv").exists() {
        host.runtime = Some("docker".into());
        body.push_str("engine: docker (/.dockerenv)\n");
    } else {
        let rt = format!("native-{}-{}", std::env::consts::OS, std::env::consts::ARCH);
        host.runtime = Some(rt.chars().take(32).collect());
        let _ = writeln!(body, "engine: {rt}");
    }
    host.cpu_threads = std::thread::available_parallelism()
        .ok()
        .and_then(|n| u32::try_from(n.get()).ok());
    let _ = writeln!(body, "available parallelism: {:?}", host.cpu_threads);
    if Path::new("/sys/fs/cgroup").exists() {
        for (name, path) in [
            ("cgroup memory.max", "/sys/fs/cgroup/memory.max"),
            ("cgroup memory.swap.max", "/sys/fs/cgroup/memory.swap.max"),
            ("cgroup cpu.max", "/sys/fs/cgroup/cpu.max"),
            ("cgroup pids.max", "/sys/fs/cgroup/pids.max"),
        ] {
            let v = std::fs::read_to_string(path).unwrap_or_else(|e| format!("({e})"));
            let _ = writeln!(body, "{name}: {}", v.trim());
        }
    }
    if let Ok(m) = std::fs::read_to_string("/proc/meminfo") {
        for l in m
            .lines()
            .filter(|l| l.starts_with("MemTotal") || l.starts_with("MemAvailable"))
        {
            let _ = writeln!(body, "{l}");
        }
    }
    diag.section("runtime", &body, 2048);
}

/// What one unit holds, in memory.
#[derive(Debug, Clone)]
pub struct Inputs<'a> {
    pub job: &'a Job,
    /// `page.jpg`.
    pub page: &'a [u8],
    /// `loc.xml`.
    pub alto: &'a [u8],
    pub texts: &'a Texts,
    /// `places.txt`.
    pub places: &'a str,
}

/// What [`compute`] gives: the result (host and time of completion left
/// for the caller) and the confusion pairs as the bytes of
/// `confusions.json`.
#[derive(Debug, Clone, PartialEq)]
pub struct Computed {
    pub result: ResultManifest,
    pub confusions: Vec<u8>,
}

fn ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// How long the computation may run before the unit returns `timeout`.
pub const DEADLINE: Duration = Duration::from_secs(1800);

/// The process's peak resident set in KiB (`VmHWM`), where `/proc` has it.
pub fn peak_rss_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
}

/// The process's CPU time (user plus system) in milliseconds from
/// `/proc/self/stat`, at the kernel's usual 100 ticks a second.
pub fn cpu_ms() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let (_, rest) = stat.rsplit_once(')')?;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some(ticks * 10)
}

/// The computation for one unit: deterministic, single-threaded.
pub fn compute(inputs: &Inputs) -> anyhow::Result<Computed> {
    let started = Instant::now();
    let job = inputs.job;
    job.validate()?;
    inputs.texts.validate(job)?;
    let t = Instant::now();
    let image = image::load_from_memory(inputs.page)
        .context("decoding page.jpg")?
        .to_luma8();
    let (w, h) = image.dimensions();
    let alto = parse_alto(inputs.alto, f64::from(w), f64::from(h))?;
    let decode_ms = ms(t);
    let t = Instant::now();
    let page = PageFeatures {
        image: Some(report::round_image(features::image_features(&image))),
        layout: Some(report::round_layout(features::layout_features(&alto))),
    };
    let features_ms = ms(t);
    let t = Instant::now();
    let places = Places::parse(inputs.places);
    let readers: Vec<ReaderInput> = job
        .readers
        .iter()
        .map(|spec| {
            let text = inputs
                .texts
                .readers
                .iter()
                .find(|r| r.name == spec.name)
                .map(|r| r.text.clone())
                .unwrap_or_default();
            ReaderInput {
                name: spec.name.clone(),
                source: spec.source.clone(),
                text,
            }
        })
        .collect();
    let function_words: BTreeSet<String> = job
        .function_words
        .iter()
        .map(|w| crate::consensus::fold(w))
        .collect();
    let analysis = report::analyse(&PageInputs {
        image: None,
        alto: None,
        readers,
        function_words,
        places: &places,
    });
    let text_ms = ms(t);
    let confusions = serde_json::to_vec_pretty(&analysis.confusions)?;
    let r = analysis.report;
    Ok(Computed {
        result: ResultManifest {
            v: usnm_ocr_contract::quality::VERSION,
            job_id: job.id.clone(),
            completed_at: String::new(),
            confusions_bytes: confusions.len() as u64,
            confusions_sha256: sha256(&confusions),
            diag_bytes: 0,
            diag_sha256: sha256(b""),
            engine: job.engine.clone(),
            hat: Some(r.hat),
            host: Host::default(),
            legibility: Some(r.legibility),
            outcome: Outcome::Ok,
            page: Some(page),
            readers: r.readers,
            timings: Timings {
                cpu_ms: None,
                decode_ms,
                features_ms,
                peak_rss_kib: None,
                text_ms,
                total_ms: ms(started),
            },
            unit_error: None,
        },
        confusions,
    })
}

/// The unit's files, read and checked against the job.
pub struct Loaded {
    pub job: Job,
    pub page: Vec<u8>,
    pub alto: Vec<u8>,
    pub texts: Texts,
    pub places: String,
}

/// Checks the unit before anything runs: the job and every input's checksum.
pub fn prepare(work: &Path) -> anyhow::Result<Loaded> {
    let job_path = resolve_slot_file(work, JOB_FILE);
    let job: Job = serde_json::from_slice(&read_bounded(&job_path, MAX_JOB_JSON)?)?;
    job.validate()?;
    let page = read_bounded(&resolve_slot_file(work, PAGE_FILE), MAX_SCAN)?;
    ensure!(
        job.inputs.page.matches(&page),
        "input page checksum mismatch"
    );
    let alto = read_bounded(&resolve_slot_file(work, ALTO_FILE), MAX_ALTO)?;
    ensure!(
        job.inputs.alto.matches(&alto),
        "input ALTO checksum mismatch"
    );
    let texts_bytes = read_bounded(&resolve_slot_file(work, TEXTS_FILE), MAX_TEXTS)?;
    ensure!(
        job.inputs.texts.matches(&texts_bytes),
        "input texts checksum mismatch"
    );
    let texts: Texts = serde_json::from_slice(&texts_bytes).context("texts.json")?;
    texts.validate(&job)?;
    let places_bytes = read_bounded(&resolve_slot_file(work, PLACES_FILE), MAX_PLACES)?;
    ensure!(
        job.inputs.places.matches(&places_bytes),
        "input places checksum mismatch"
    );
    let places = String::from_utf8(places_bytes).context("places.txt is not UTF-8")?;
    Ok(Loaded {
        job,
        page,
        alto,
        texts,
        places,
    })
}

fn empty(
    job_id: String,
    engine: Engine,
    outcome: Outcome,
    error: Option<String>,
) -> ResultManifest {
    ResultManifest {
        v: usnm_ocr_contract::quality::VERSION,
        job_id,
        completed_at: String::new(),
        confusions_bytes: 0,
        confusions_sha256: String::new(),
        diag_bytes: 0,
        diag_sha256: String::new(),
        engine,
        hat: None,
        host: Host::default(),
        legibility: None,
        outcome,
        page: None,
        readers: vec![],
        timings: Timings::default(),
        unit_error: error,
    }
}

fn compute_with_deadline(
    l: Loaded,
    deadline: Duration,
    control: &BoincControl,
) -> Result<Computed, Box<(Job, anyhow::Error)>> {
    let job = l.job.clone();
    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        return Err(Box::new((
            job,
            anyhow::anyhow!("boinc control requested exit ({reason})"),
        )));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("quality-compute".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let r = compute(&Inputs {
                job: &l.job,
                page: &l.page,
                alto: &l.alto,
                texts: &l.texts,
                places: &l.places,
            });
            let _ = tx.send(r);
        })
        .map_err(|e| {
            Box::new((
                job.clone(),
                anyhow::Error::from(e).context("starting the computation"),
            ))
        })?;
    let mut active_elapsed = Duration::ZERO;
    let poll_step = Duration::from_millis(50);
    loop {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("boinc control requested exit ({reason})"),
            )));
        }
        let remaining = deadline.saturating_sub(active_elapsed);
        if remaining.is_zero() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("computation exceeded {} s", deadline.as_secs()),
            )));
        }
        let step_start = Instant::now();
        match rx.recv_timeout(poll_step.min(remaining)) {
            Ok(Ok(c)) => {
                if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
                    return Err(Box::new((
                        job,
                        anyhow::anyhow!("boinc control requested exit ({reason})"),
                    )));
                }
                return Ok(c);
            }
            Ok(Err(e)) => return Err(Box::new((job, e))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !control.is_suspended() {
                    active_elapsed += step_start.elapsed();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Box::new((
                    job,
                    anyhow::anyhow!("computation thread ended without a result"),
                )));
            }
        }
    }
}

/// Runs a complete `usnm_quality` unit in `work`, writing `fraction_done`,
/// `confusions.json`, `diag.txt`, `result.json`, and `boinc_finish_called`.
pub fn run_slot(work: &Path) -> anyhow::Result<()> {
    let confusions_out = resolve_slot_file(work, CONFUSIONS_FILE);
    let diag_out = resolve_slot_file(work, DIAG_FILE);
    let result_out = resolve_slot_file(work, RESULT_FILE);
    let fraction_out = work.join(FRACTION_FILE);
    let finish_out = work.join(FINISH_FILE);

    for p in [
        &confusions_out,
        &diag_out,
        &result_out,
        &fraction_out,
        &finish_out,
    ] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    write_atomic(&fraction_out, b"0.0\n")?;

    let control = BoincControl::start(work);
    let mut diag = Diag::new();
    let mut host = Host::default();
    boinc_info(work, &mut host, &mut diag);
    runtime_info(&mut host, &mut diag);

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

    let (mut result, confusions) = match prepare(work) {
        Ok(l) => {
            diag.section(
                "unit",
                &format!(
                    "job {} doc {} group {} language {}: readers {:?}",
                    l.job.id,
                    l.job.doc_id,
                    l.job.group,
                    l.job.language,
                    l.job
                        .readers
                        .iter()
                        .map(|r| r.name.as_str())
                        .collect::<Vec<_>>()
                ),
                1024,
            );
            let started = Instant::now();
            match compute_with_deadline(l, DEADLINE, &control) {
                Ok(c) => {
                    diag.section("timings", &format!("{:?}", c.result.timings), 512);
                    (c.result, c.confusions)
                }
                Err(boxed) => {
                    if let Some(reason) = control.should_exit() {
                        diag.section(
                            "boinc control",
                            &format!("exiting without boinc_finish_called: {reason}"),
                            256,
                        );
                        write_atomic(&diag_out, &diag.finish())?;
                        return Ok(());
                    }
                    let (job, e) = *boxed;
                    let outcome = if started.elapsed() >= DEADLINE {
                        Outcome::Timeout
                    } else {
                        Outcome::Failed
                    };
                    diag.section("computation failed", &format!("{e:#}"), 2048);
                    let mut r = empty(job.id.clone(), job.engine.clone(), outcome, None);
                    r.timings.total_ms = ms(started);
                    (r, b"{}".to_vec())
                }
            }
        }
        Err(e) => {
            if let Some(reason) = control.should_exit() {
                diag.section(
                    "boinc control",
                    &format!("exiting without boinc_finish_called: {reason}"),
                    256,
                );
                write_atomic(&diag_out, &diag.finish())?;
                return Ok(());
            }
            let msg = format!("{e:#}");
            diag.section("unit error", &msg, 1024);
            let job_path = resolve_slot_file(work, JOB_FILE);
            let job: Option<serde_json::Value> = std::fs::read(job_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let id = job
                .as_ref()
                .and_then(|j| j.get("id")?.as_str())
                .filter(|s| usnm_ocr_contract::boinc::is_sha256(s))
                .unwrap_or(&"0".repeat(64))
                .to_owned();
            let engine = job
                .and_then(|j| serde_json::from_value::<Engine>(j.get("engine")?.clone()).ok())
                .unwrap_or_else(|| Engine {
                    image: format!("sha256:{}", "0".repeat(64)),
                });
            (
                empty(
                    id,
                    engine,
                    Outcome::UnitError,
                    Some(tail(&msg, MAX_ERROR).to_owned()),
                ),
                b"{}".to_vec(),
            )
        }
    };

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

    result.timings.cpu_ms = cpu_ms();
    result.timings.peak_rss_kib = peak_rss_kib();
    diag.section(
        "process",
        &format!(
            "cpu {:?} ms, peak rss {:?} KiB, deadline {} s",
            result.timings.cpu_ms,
            result.timings.peak_rss_kib,
            DEADLINE.as_secs()
        ),
        256,
    );
    let diag = diag.finish();
    result.host = host;
    result.completed_at = chrono::Utc::now().to_rfc3339();
    result.confusions_bytes = confusions.len() as u64;
    result.confusions_sha256 = sha256(&confusions);
    result.diag_bytes = diag.len() as u64;
    result.diag_sha256 = sha256(&diag);

    write_atomic(&confusions_out, &confusions)?;
    write_atomic(&diag_out, &diag)?;
    write_atomic(&result_out, &serde_json::to_vec_pretty(&result)?)?;
    write_atomic(&fraction_out, b"1.0\n")?;
    write_atomic(&finish_out, b"0\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/test-slot-tmp")
                .join(format!("{label}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn boinc_mmap_suspend_resume_and_quit_consume_messages() {
        let tmp = TestDir::new("suspend-resume");
        let slot = tmp.path();
        let mmap = slot.join(MMAP_FILE);
        std::fs::write(&mmap, vec![0u8; SHARED_MEM_SIZE]).unwrap();

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<suspend/>").unwrap();
        let control = BoincControl::start(slot);
        assert!(control.is_suspended());
        // Verify byte 0 was cleared to 0 so the BOINC client sees the message was consumed.
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);

        let control_wait = control.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let flow = control_wait.wait_if_suspended_or_should_exit();
            tx.send(flow).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(120)).is_err());

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<resume/>").unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            ControlFlow::Continue(())
        );
        waiter.join().unwrap();
        assert!(!control.is_suspended());
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);
    }

    #[test]
    fn boinc_mmap_quit_and_heartbeat_loss_omit_boinc_finish_called() {
        let tmp = TestDir::new("quit-heartbeat");
        let quit_slot = tmp.path().join("quit_slot");
        std::fs::create_dir_all(&quit_slot).unwrap();
        let quit_mmap = quit_slot.join(MMAP_FILE);
        write_mmap_channel(&quit_mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        run_slot(&quit_slot).unwrap();
        assert!(!quit_slot.join(FINISH_FILE).exists());
        assert_eq!(std::fs::read(&quit_mmap).unwrap()[0], 0);

        let hb_slot = tmp.path().join("hb_slot");
        std::fs::create_dir_all(&hb_slot).unwrap();
        let hb_mmap = hb_slot.join(MMAP_FILE);
        write_mmap_channel(&hb_mmap, HEARTBEAT_OFFSET, "<heartbeat/>").unwrap();
        let control =
            BoincControl::start_with_heartbeat_timeout(&hb_slot, Duration::from_millis(80));
        assert_eq!(std::fs::read(&hb_mmap).unwrap()[HEARTBEAT_OFFSET], 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("heartbeat_lost"));
    }
}
