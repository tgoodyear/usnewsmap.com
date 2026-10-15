//! Native cross-platform BOINC worker binary for `usnm_qwen` (#533).
//!
//! Runs directly in a BOINC slot on Linux, macOS, and Windows:
//! 1. Resolves slot files (supporting both `<copy_file/>` and `<soft_link>` XML
//!    pointers, as well as `<sticky/><no_delete/>` GGUF model files in the slot
//!    or `USNM_QWEN_MODEL_DIR` / `/opt/qwen/models`).
//! 2. Locates `llama-server` (`USNM_LLAMA_SERVER`, slot directory, `./llama-server[.exe]`,
//!    or `/usr/local/bin/llama-server`) and spawns it on `127.0.0.1:<free_port>`
//!    with `-m`, `--mmproj`, `--ctx-size 4096`, `--temp 0`, `--seed 42`,
//!    `--parallel 1`, `--no-webui`, and `-ngl 99` (GPU) or `0` (CPU).
//! 3. Waits for `/health`, posts each crop PNG to `/v1/chat/completions`,
//!    updates `fraction_done`, writes `readings.json`, `result.json`, and
//!    `diag.txt`, kills `llama-server` on drop, and writes `boinc_finish_called`
//!    (`"0\n"`).
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use image::GenericImageView as _;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::MAX_SCAN,
    qwen::{
        Crop, Engine, Job, Model, Outcome, Reading, Readings, ResultManifest, MAX_CROP_TEXT,
        MAX_ERROR, MAX_JOB_JSON, PROMPT, PROMPT_VERSION, VERSION,
    },
    surya::Host,
};
use usnm_ocr_quality::slot::{
    read_bounded, resolve_slot_file, tail, write_atomic, Diag, DIAG_FILE, FINISH_FILE,
    FRACTION_FILE, PAGE_FILE, RESULT_FILE,
};

pub const JOB_FILE: &str = "qwen.json";
pub const READINGS_FILE: &str = "readings.json";
pub const ENV_LLAMA_IMAGE: &str = "USNM_QWEN_LLAMA_IMAGE";
pub const ENV_WEIGHTS: &str = "USNM_QWEN_WEIGHTS";
pub const ENV_MODEL_DIR: &str = "USNM_QWEN_MODEL_DIR";
pub const ENV_LLAMA_SERVER: &str = "USNM_LLAMA_SERVER";
pub const DEFAULT_MODEL_DIR: &str = "/opt/qwen/models";
const CTX_SIZE: u32 = 4096;
const LOAD_DEADLINE: Duration = Duration::from_secs(300);

fn main() -> anyhow::Result<()> {
    run_slot(Path::new("."))
}

fn ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
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

fn boinc_info(work: &Path, host: &mut Host, diag: &mut Diag) {
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
    host.gpu_type = tag(&xml, "gpu_type")
        .filter(|s| s.len() <= 32)
        .map(str::to_owned);
    host.gpu_device_num = tag(&xml, "gpu_device_num").and_then(|s| s.parse().ok());
    host.ncpus = tag(&xml, "ncpus").and_then(|s| s.parse().ok());
    diag.section("boinc (init_data.xml, selected fields)", &body, 2048);
}

fn runtime_info(host: &mut Host, diag: &mut Diag) {
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
    diag.section("runtime", &body, 2048);
}

fn quick(program: &str, args: &[&str], deadline: Duration) -> Result<(bool, String), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program}: no answer in {}s", deadline.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{program}: {e}")),
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{program}: {e}"))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&err);
    }
    Ok((out.status.success(), text))
}

#[derive(Debug, Clone)]
struct GpuState {
    cuda_device: Option<u32>,
    free_mb: Option<u64>,
    used_before_mb: Option<u64>,
    use_gpu: bool,
    device_label: String,
}

fn detect_gpu(host: &mut Host, diag: &mut Diag) -> GpuState {
    let mut body = String::new();
    let plan = host.plan_class.as_deref().unwrap_or("");
    let gpu_type = host.gpu_type.as_deref().unwrap_or("");

    // Check NVIDIA via nvidia-smi first.
    if let Ok((true, out)) = quick(
        "nvidia-smi",
        &[
            "--query-gpu=index,name,memory.total,memory.free,memory.used,driver_version",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(10),
    ) {
        let _ = writeln!(body, "nvidia-smi:\n{}", out.trim());
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        if !lines.is_empty() {
            let wanted = u32::try_from(host.gpu_device_num.unwrap_or(0)).unwrap_or(0);
            let chosen = lines
                .iter()
                .find_map(|l| {
                    let i: u32 = l.split(',').next()?.trim().parse().ok()?;
                    (i == wanted).then_some((i, *l))
                })
                .or_else(|| {
                    let i: u32 = lines[0].split(',').next()?.trim().parse().ok()?;
                    Some((i, lines[0]))
                });
            if let Some((idx, line)) = chosen {
                let parts: Vec<&str> = line.split(',').map(str::trim).collect();
                host.gpu = parts.get(1).map(|s| s.chars().take(64).collect());
                host.gpu_mem_total_mib = parts.get(2).and_then(|s| s.parse().ok());
                host.gpu_driver = parts.get(5).map(|s| s.chars().take(32).collect());
                let free_mb = parts.get(3).and_then(|s| s.parse().ok());
                let used_before_mb = parts.get(4).and_then(|s| s.parse().ok());
                diag.section("gpu", &body, 2048);
                return GpuState {
                    cuda_device: Some(idx),
                    free_mb,
                    used_before_mb,
                    use_gpu: true,
                    device_label: "cuda".into(),
                };
            }
        }
    }

    if plan.contains("apple_gpu")
        || plan.contains("metal")
        || gpu_type.eq_ignore_ascii_case("apple")
        || gpu_type.eq_ignore_ascii_case("apple_gpu")
    {
        let _ = writeln!(body, "Apple Metal active via plan_class/gpu_type");
        diag.section("gpu", &body, 1024);
        return GpuState {
            cuda_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: true,
            device_label: "metal".into(),
        };
    }

    if plan.contains("vulkan") {
        let _ = writeln!(body, "Vulkan active via plan_class");
        diag.section("gpu", &body, 1024);
        return GpuState {
            cuda_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: true,
            device_label: "vulkan".into(),
        };
    }

    let _ = writeln!(body, "running on CPU");
    diag.section("gpu", &body, 1024);
    GpuState {
        cuda_device: None,
        free_mb: None,
        used_before_mb: None,
        use_gpu: false,
        device_label: "cpu".into(),
    }
}

fn gpu_used_mib(dev: u32) -> Option<u64> {
    let (ok, out) = quick(
        "nvidia-smi",
        &[
            &format!("--id={dev}"),
            "--query-gpu=memory.used",
            "--format=csv,noheader,nounits",
        ],
        Duration::from_secs(5),
    )
    .ok()?;
    if !ok {
        return None;
    }
    out.lines().next()?.trim().parse().ok()
}

pub struct Loaded {
    pub job: Job,
    pub page: image::DynamicImage,
}

fn check_engine(job: &Job) -> anyhow::Result<()> {
    if let Ok(llama) = std::env::var(ENV_LLAMA_IMAGE) {
        if !llama.is_empty() {
            ensure!(
                llama == job.engine.llama,
                "unit is for llama image {}, host has {llama}",
                job.engine.llama
            );
        }
    }
    if let Ok(weights) = std::env::var(ENV_WEIGHTS) {
        if !weights.is_empty() {
            ensure!(
                weights == job.engine.weights,
                "unit is for weights {}, host has {weights}",
                job.engine.weights
            );
        }
    }
    Ok(())
}

pub fn prepare(work: &Path) -> anyhow::Result<Loaded> {
    let job_path = resolve_slot_file(work, JOB_FILE);
    let job: Job = serde_json::from_slice(&read_bounded(&job_path, MAX_JOB_JSON)?)?;
    job.validate()?;
    check_engine(&job)?;
    let page_bytes = read_bounded(&resolve_slot_file(work, PAGE_FILE), MAX_SCAN)?;
    ensure!(
        job.input.matches(&page_bytes),
        "input page checksum mismatch"
    );
    let page = image::load_from_memory(&page_bytes).context("decoding page.jpg")?;
    let (w, h) = page.dimensions();
    for c in &job.crops {
        let [_, _, x1, y1] = c.rect;
        ensure!(
            x1 <= w && y1 <= h,
            "crop {} {:?} is outside a {w}x{h} page",
            c.id,
            c.rect
        );
    }
    Ok(Loaded { job, page })
}

pub fn crop_png(page: &image::DynamicImage, c: &Crop) -> anyhow::Result<Vec<u8>> {
    let [x0, y0, x1, y1] = c.rect;
    ensure!(x0 < x1 && y0 < y1, "empty crop {}", c.id);
    let sub = page.crop_imm(x0, y0, x1 - x0, y1 - y0);
    let mut out = std::io::Cursor::new(Vec::new());
    sub.write_to(&mut out, image::ImageFormat::Png)
        .with_context(|| format!("encoding crop {}", c.id))?;
    Ok(out.into_inner())
}

pub fn clean_reading(s: &str) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let trimmed = out.trim().len();
    if trimmed < out.len() {
        out = out.trim().to_owned();
    }
    if out.len() > MAX_CROP_TEXT {
        let mut end = MAX_CROP_TEXT;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    out
}

/// Locates `(model_path, mmproj_path)` for `m` either in the BOINC slot
/// directory (via direct files or `<soft_link>` XML pointers) or in
/// `USNM_QWEN_MODEL_DIR` / `/opt/qwen/models`.
pub fn locate_model_files(work: &Path, m: &Model) -> Option<(PathBuf, PathBuf)> {
    let slot_model = resolve_slot_file(work, &m.file);
    let slot_mmproj = resolve_slot_file(work, &m.mmproj);
    if slot_model.is_file() && slot_mmproj.is_file() {
        return Some((slot_model, slot_mmproj));
    }
    let dir = std::env::var_os(ENV_MODEL_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL_DIR));
    let dir_model = dir.join(&m.file);
    let dir_mmproj = dir.join(&m.mmproj);
    if dir_model.is_file() && dir_mmproj.is_file() {
        return Some((dir_model, dir_mmproj));
    }
    None
}

/// Chooses the best available model from `models` whose files exist in the
/// slot directory or model directory.
pub fn resolve_model<'a>(
    work: &Path,
    models: &'a [Model],
    gpu_free_mb: Option<u64>,
    use_gpu: bool,
) -> anyhow::Result<(&'a Model, PathBuf, PathBuf)> {
    let available: Vec<(&'a Model, PathBuf, PathBuf)> = models
        .iter()
        .filter_map(|m| {
            let (mp, mmp) = locate_model_files(work, m)?;
            Some((m, mp, mmp))
        })
        .collect();
    if available.is_empty() {
        let names: Vec<&str> = models.iter().map(|m| m.file.as_str()).collect();
        bail!(
            "no model files found in slot {} or model dir for {:?}",
            work.display(),
            names
        );
    }
    let chosen = match gpu_free_mb {
        Some(free) => available
            .iter()
            .find(|(m, _, _)| u64::from(m.min_gpu_mb) <= free)
            .or_else(|| available.last())
            .unwrap(),
        None if use_gpu => available.first().unwrap(),
        None => available.last().unwrap(),
    };
    Ok((chosen.0, chosen.1.clone(), chosen.2.clone()))
}

/// Finds `llama-server` (`llama-server` or `llama-server.exe`) from
/// `USNM_LLAMA_SERVER`, the slot directory (supporting `<soft_link>`), or
/// standard locations.
pub fn find_llama_server(work: &Path) -> anyhow::Result<PathBuf> {
    if let Some(env_path) = std::env::var_os(ENV_LLAMA_SERVER) {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Ok(p);
        }
        bail!("USNM_LLAMA_SERVER={} does not exist", p.display());
    }
    for name in ["llama-server", "llama-server.exe"] {
        let p = resolve_slot_file(work, name);
        if p.is_file() {
            return Ok(p);
        }
    }
    for cand in [
        PathBuf::from("./llama-server"),
        PathBuf::from("./llama-server.exe"),
        PathBuf::from("/usr/local/bin/llama-server"),
    ] {
        if cand.is_file() {
            return Ok(cand);
        }
    }
    bail!(
        "llama-server binary not found in slot {} or USNM_LLAMA_SERVER",
        work.display()
    )
}

fn free_port() -> anyhow::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

pub fn http_request(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    timeout: Duration,
) -> anyhow::Result<(u16, Vec<u8>)> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    let len = body.map_or(0, <[u8]>::len);
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\r\n"
    )?;
    if let Some(b) = body {
        s.write_all(b)?;
    }
    let mut raw = Vec::new();
    s.take(1024 * 1024).read_to_end(&mut raw)?;
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed HTTP response")?;
    let head = std::str::from_utf8(&raw[..sep]).context("HTTP header UTF-8")?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse().ok())
        .context("HTTP status code")?;
    let mut payload = raw[sep + 4..].to_vec();
    if head.lines().any(|l| {
        l.to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
    }) {
        payload = unchunk(&payload)?;
    }
    Ok((status, payload))
}

fn unchunk(mut b: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let crlf = b
            .windows(2)
            .position(|w| w == b"\r\n")
            .context("chunk header")?;
        let len_s = std::str::from_utf8(&b[..crlf])?
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let len = usize::from_str_radix(len_s, 16).context("chunk length")?;
        b = &b[crlf + 2..];
        if len == 0 {
            return Ok(out);
        }
        ensure!(b.len() >= len + 2, "truncated chunk");
        out.extend_from_slice(&b[..len]);
        b = &b[len + 2..];
    }
}

pub fn chat_completion(
    port: u16,
    png: &[u8],
    max_tokens: u32,
    timeout: Duration,
) -> anyhow::Result<(String, u32)> {
    let url = format!("data:image/png;base64,{}", BASE64.encode(png));
    let body = serde_json::to_vec(&serde_json::json!({
        "messages": [{
            "role": "user",
            "content": [
                {"type": "image_url", "image_url": {"url": url}},
                {"type": "text", "text": PROMPT},
            ]
        }],
        "max_tokens": max_tokens,
        "temperature": 0.0,
        "top_p": 1.0,
        "seed": 42,
    }))?;
    let (status, resp) = http_request(port, "POST", "/v1/chat/completions", Some(&body), timeout)?;
    if status != 200 {
        bail!(
            "llama-server status {status}: {}",
            tail(&String::from_utf8_lossy(&resp), 256)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&resp).context("chat response JSON")?;
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .context("chat response has no choices[0].message.content")?;
    let tokens = v
        .pointer("/usage/completion_tokens")
        .and_then(|t| t.as_u64())
        .and_then(|t| u32::try_from(t).ok())
        .unwrap_or(0);
    Ok((clean_reading(text), tokens))
}

struct Server {
    child: Child,
    port: u16,
    log_path: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    fn start(
        work: &Path,
        server_bin: &Path,
        model_path: &Path,
        mmproj_path: &Path,
        gpu: &GpuState,
        threads: u32,
        load_deadline: Duration,
    ) -> anyhow::Result<(Self, u64)> {
        let port = free_port()?;
        let log_path = work.join("llama-server.log");
        let log = std::fs::File::create(&log_path)?;
        let log_err = log.try_clone()?;
        let ngl = if gpu.use_gpu { "99" } else { "0" };
        let mut cmd = Command::new(server_bin);
        cmd.args([
            "-m",
            &model_path.to_string_lossy(),
            "--mmproj",
            &mmproj_path.to_string_lossy(),
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--ctx-size",
            &CTX_SIZE.to_string(),
            "--temp",
            "0",
            "--seed",
            "42",
            "--parallel",
            "1",
            "--no-webui",
            "-ngl",
            ngl,
            "-t",
            &threads.max(1).to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

        if let Some(dev) = gpu.cuda_device {
            cmd.env("CUDA_VISIBLE_DEVICES", dev.to_string());
        }
        if let Some(bin_dir) = server_bin.parent().filter(|p| !p.as_os_str().is_empty()) {
            #[cfg(target_os = "linux")]
            {
                let cur = std::env::var_os("LD_LIBRARY_PATH").unwrap_or_default();
                let mut paths = vec![bin_dir.to_path_buf()];
                paths.extend(std::env::split_paths(&cur));
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("LD_LIBRARY_PATH", joined);
                }
            }
            #[cfg(target_os = "macos")]
            {
                let cur = std::env::var_os("DYLD_LIBRARY_PATH").unwrap_or_default();
                let mut paths = vec![bin_dir.to_path_buf()];
                paths.extend(std::env::split_paths(&cur));
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("DYLD_LIBRARY_PATH", joined);
                }
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }

        let started = Instant::now();
        let child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", server_bin.display()))?;
        let mut srv = Self {
            child,
            port,
            log_path,
        };
        loop {
            if let Some(status) = srv.child.try_wait()? {
                let log_tail = std::fs::read_to_string(&srv.log_path).unwrap_or_default();
                bail!(
                    "llama-server exited ({status}) before ready: {}",
                    tail(&log_tail, 512)
                );
            }
            if let Ok((200, _)) = http_request(port, "GET", "/health", None, Duration::from_secs(2))
            {
                return Ok((srv, ms(started)));
            }
            if started.elapsed() > load_deadline {
                let log_tail = std::fs::read_to_string(&srv.log_path).unwrap_or_default();
                bail!(
                    "llama-server did not become healthy in {}s: {}",
                    load_deadline.as_secs(),
                    tail(&log_tail, 512)
                );
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn empty_result(
    job_id: String,
    engine: Engine,
    crops: u32,
    outcome: Outcome,
    error: Option<String>,
) -> (ResultManifest, Readings) {
    let readings = Readings {
        v: VERSION,
        job_id: job_id.clone(),
        device: "none".into(),
        diag: BTreeMap::new(),
        model: None,
        outcome,
        prompt_version: PROMPT_VERSION,
        readings: vec![],
    };
    let result = ResultManifest {
        v: VERSION,
        job_id,
        completed_at: String::new(),
        crops,
        device: "none".into(),
        diag_bytes: 0,
        diag_sha256: String::new(),
        engine,
        engine_ms: 0,
        gpu_mem_peak_mib: None,
        host: Host::default(),
        load_ms: 0,
        model: None,
        outcome,
        peak_rss_kib: None,
        read: 0,
        readings_bytes: 0,
        readings_sha256: String::new(),
        unit_error: error,
    };
    (result, readings)
}

pub fn run_slot(work: &Path) -> anyhow::Result<()> {
    run_slot_with(work, None, None)
}

pub fn run_slot_with(
    work: &Path,
    existing_port: Option<u16>,
    deadline_override: Option<Duration>,
) -> anyhow::Result<()> {
    let readings_out = resolve_slot_file(work, READINGS_FILE);
    let result_out = resolve_slot_file(work, RESULT_FILE);
    let diag_out = resolve_slot_file(work, DIAG_FILE);
    let fraction_out = work.join(FRACTION_FILE);
    let finish_out = work.join(FINISH_FILE);

    for p in [
        &readings_out,
        &result_out,
        &diag_out,
        &fraction_out,
        &finish_out,
    ] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    write_atomic(&fraction_out, b"0.0\n")?;

    let mut diag = Diag::new();
    let mut host = Host::default();
    boinc_info(work, &mut host, &mut diag);
    runtime_info(&mut host, &mut diag);
    let gpu = detect_gpu(&mut host, &mut diag);

    let (mut result, readings) = match prepare(work) {
        Ok(l) => {
            diag.section(
                "unit",
                &format!(
                    "job {} doc {} group {} language {}: {} crops",
                    l.job.id,
                    l.job.doc_id,
                    l.job.group,
                    l.job.language,
                    l.job.crops.len()
                ),
                1024,
            );
            let want_cuda_only = host
                .plan_class
                .as_deref()
                .is_some_and(|p| p.contains("nvidia") || p.contains("cuda"));
            if want_cuda_only && gpu.cuda_device.is_none() {
                diag.section(
                    "skipped",
                    "CUDA plan class assigned, but nvidia-smi sees no GPU",
                    256,
                );
                empty_result(
                    l.job.id.clone(),
                    l.job.engine.clone(),
                    l.job.crops.len() as u32,
                    Outcome::NoGpu,
                    None,
                )
            } else {
                let deadline = deadline_override.unwrap_or_else(|| {
                    Duration::from_secs(u64::from(if gpu.use_gpu {
                        l.job.gpu_timeout_secs
                    } else {
                        l.job.cpu_timeout_secs
                    }))
                });
                run_crops(work, &l, &host, &gpu, existing_port, deadline, &mut diag)
            }
        }
        Err(e) => {
            let msg = format!("{e:#}");
            diag.section("unit error", &msg, 1024);
            let job_path = resolve_slot_file(work, JOB_FILE);
            let raw: Option<serde_json::Value> = std::fs::read(job_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let id = raw
                .as_ref()
                .and_then(|j| j.get("id")?.as_str())
                .filter(|s| usnm_ocr_contract::boinc::is_sha256(s))
                .unwrap_or(&"0".repeat(64))
                .to_owned();
            let z = format!("sha256:{}", "0".repeat(64));
            let engine = raw
                .as_ref()
                .and_then(|j| serde_json::from_value::<Engine>(j.get("engine")?.clone()).ok())
                .unwrap_or_else(|| Engine {
                    image: z.clone(),
                    llama: z.clone(),
                    models: vec![],
                    weights: z,
                });
            let crops = raw
                .and_then(|j| u32::try_from(j.get("crops")?.as_array()?.len()).ok())
                .unwrap_or(1);
            empty_result(
                id,
                engine,
                crops,
                Outcome::UnitError,
                Some(tail(&msg, MAX_ERROR).to_owned()),
            )
        }
    };

    let readings_bytes = serde_json::to_vec_pretty(&readings)?;
    let diag_bytes = diag.finish();
    result.host = host;
    result.completed_at = chrono::Utc::now().to_rfc3339();
    result.readings_bytes = readings_bytes.len() as u64;
    result.readings_sha256 = sha256(&readings_bytes);
    result.diag_bytes = diag_bytes.len() as u64;
    result.diag_sha256 = sha256(&diag_bytes);

    write_atomic(&readings_out, &readings_bytes)?;
    write_atomic(&diag_out, &diag_bytes)?;
    write_atomic(&result_out, &serde_json::to_vec_pretty(&result)?)?;
    write_atomic(&fraction_out, b"1.0\n")?;
    write_atomic(&finish_out, b"0\n")?;
    Ok(())
}

fn run_crops(
    work: &Path,
    l: &Loaded,
    host: &Host,
    gpu: &GpuState,
    existing_port: Option<u16>,
    deadline: Duration,
    diag: &mut Diag,
) -> (ResultManifest, Readings) {
    let (model, model_path, mmproj_path) =
        match resolve_model(work, &l.job.engine.models, gpu.free_mb, gpu.use_gpu) {
            Ok(m) => m,
            Err(e) => {
                diag.section("model resolution failed", &format!("{e:#}"), 1024);
                return empty_result(
                    l.job.id.clone(),
                    l.job.engine.clone(),
                    l.job.crops.len() as u32,
                    Outcome::Failed,
                    None,
                );
            }
        };

    let threads = host
        .ncpus
        .map(|n| n.ceil() as u32)
        .or(host.cpu_threads)
        .unwrap_or(1)
        .max(1);

    let (srv_guard, port, load_ms) = match existing_port {
        Some(p) => (None, p, 0),
        None => {
            let server_bin = match find_llama_server(work) {
                Ok(b) => b,
                Err(e) => {
                    diag.section("llama-server missing", &format!("{e:#}"), 1024);
                    return empty_result(
                        l.job.id.clone(),
                        l.job.engine.clone(),
                        l.job.crops.len() as u32,
                        Outcome::Failed,
                        None,
                    );
                }
            };
            match Server::start(
                work,
                &server_bin,
                &model_path,
                &mmproj_path,
                gpu,
                threads,
                LOAD_DEADLINE.min(deadline),
            ) {
                Ok((srv, ms_loaded)) => {
                    let p = srv.port;
                    (Some(srv), p, ms_loaded)
                }
                Err(e) => {
                    diag.section("llama-server start failed", &format!("{e:#}"), 2048);
                    return empty_result(
                        l.job.id.clone(),
                        l.job.engine.clone(),
                        l.job.crops.len() as u32,
                        Outcome::Failed,
                        None,
                    );
                }
            }
        }
    };

    let started = Instant::now();
    let mut out_readings = Vec::with_capacity(l.job.crops.len());
    let mut outcome = Outcome::Ok;
    let mut gpu_peak_mib: Option<u64> = None;

    for (idx, crop) in l.job.crops.iter().enumerate() {
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            diag.section(
                "timeout",
                &format!(
                    "deadline {}s reached at crop {}",
                    deadline.as_secs(),
                    crop.id
                ),
                512,
            );
            outcome = Outcome::Timeout;
            break;
        }
        let remaining = deadline.saturating_sub(elapsed);
        let png = match crop_png(&l.page, crop) {
            Ok(b) => b,
            Err(e) => {
                diag.section("crop error", &format!("{}: {e:#}", crop.id), 1024);
                outcome = Outcome::Failed;
                break;
            }
        };
        let t_crop = Instant::now();
        match chat_completion(port, &png, l.job.max_tokens(crop), remaining) {
            Ok((text, tokens)) => {
                out_readings.push(Reading {
                    id: crop.id.clone(),
                    ms: ms(t_crop),
                    text,
                    tokens,
                });
                if let Some(dev) = gpu.cuda_device {
                    if let Some(used) = gpu_used_mib(dev) {
                        let delta = used.saturating_sub(gpu.used_before_mb.unwrap_or(0));
                        gpu_peak_mib = Some(gpu_peak_mib.map_or(delta, |p| p.max(delta)));
                    }
                }
                let frac = (idx + 1) as f64 / (l.job.crops.len().max(1)) as f64;
                let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.4}\n").as_bytes());
            }
            Err(e) => {
                let msg = format!("{}: {e:#}", crop.id);
                diag.section("chat completion failed", &msg, 1024);
                outcome = if started.elapsed() >= deadline {
                    Outcome::Timeout
                } else {
                    Outcome::Failed
                };
                break;
            }
        }
    }

    let engine_ms = ms(started);
    if let Some(ref srv) = srv_guard {
        if let Ok(log_text) = std::fs::read_to_string(&srv.log_path) {
            diag.section("llama-server.log (tail)", tail(&log_text, 2048), 2048);
        }
    }
    drop(srv_guard);

    let mut r_diag = BTreeMap::new();
    r_diag.insert("ctx_size".into(), CTX_SIZE.to_string());
    r_diag.insert(
        "n_gpu_layers".into(),
        if gpu.use_gpu { "99" } else { "0" }.into(),
    );
    let device = gpu.device_label.clone();
    let readings = Readings {
        v: VERSION,
        job_id: l.job.id.clone(),
        device: device.clone(),
        diag: r_diag,
        model: Some(model.name.clone()),
        outcome,
        prompt_version: PROMPT_VERSION,
        readings: out_readings,
    };
    let result = ResultManifest {
        v: VERSION,
        job_id: l.job.id.clone(),
        completed_at: String::new(),
        crops: l.job.crops.len() as u32,
        device,
        diag_bytes: 0,
        diag_sha256: String::new(),
        engine: l.job.engine.clone(),
        engine_ms,
        gpu_mem_peak_mib: gpu_peak_mib,
        host: Host::default(),
        load_ms,
        model: Some(model.name.clone()),
        outcome,
        peak_rss_kib: usnm_ocr_quality::slot::peak_rss_kib(),
        read: readings.readings.len() as u32,
        readings_bytes: 0,
        readings_sha256: String::new(),
        unit_error: None,
    };
    (result, readings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::quality::Input;

    fn synthetic_jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = image::GrayImage::from_pixel(w, h, image::Luma([220]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn write_qwen_unit(dir: &Path) -> Job {
        std::fs::create_dir_all(dir).unwrap();
        let page = synthetic_jpeg(200, 200);
        let models = vec![
            Model {
                file: "Qwen3VL-8B-Instruct-Q4_K_M.gguf".into(),
                min_gpu_mb: 7000,
                mmproj: "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf".into(),
                name: "qwen3-vl-8b-q4km".into(),
            },
            Model {
                file: "Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
                min_gpu_mb: 0,
                mmproj: "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
                name: "qwen3-vl-2b-q8".into(),
            },
        ];
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 600,
            crops: vec![
                Crop {
                    rect: [10, 10, 100, 60],
                    id: "c01".into(),
                    loc_text: "first crop".into(),
                    stratum: "wc-low/top".into(),
                },
                Crop {
                    rect: [10, 80, 100, 140],
                    id: "c02".into(),
                    loc_text: "second crop".into(),
                    stratum: "wc-high/bottom".into(),
                },
            ],
            doc_id: "sn85055101_1917-04-03_ed-1_seq-3".into(),
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
                llama: format!("sha256:{}", "b".repeat(64)),
                models,
                weights: format!("sha256:{}", "c".repeat(64)),
            },
            gpu_timeout_secs: 300,
            group: "serbian".into(),
            input: Input::of(&page, "local:page.jpg"),
            language: "srp".into(),
            max_tokens_per_1000px2: 4,
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity().unwrap();
        std::fs::write(dir.join(PAGE_FILE), &page).unwrap();
        std::fs::write(dir.join(JOB_FILE), serde_json::to_vec_pretty(&job).unwrap()).unwrap();
        job
    }

    fn spawn_mock_llama_server(responses: Vec<&'static str>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for reply in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = vec![0u8; 65536];
                let mut n = 0;
                loop {
                    match stream.read(&mut buf[n..]) {
                        Ok(0) => break,
                        Ok(k) => {
                            n += k;
                            if let Some(sep) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = std::str::from_utf8(&buf[..sep]).unwrap_or("");
                                let clen: usize = head
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse().ok())?
                                    })
                                    .unwrap_or(0);
                                if n >= sep + 4 + clen {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                let body = serde_json::to_vec(&serde_json::json!({
                    "choices": [{"message": {"content": reply}}],
                    "usage": {"completion_tokens": 5}
                }))
                .unwrap();
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        port
    }

    #[test]
    fn native_slot_runs_with_soft_links_and_writes_boinc_finish_called() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().join("projects/usnewsmap");
        let slot = tmp.path().join("slot");
        let staging = tmp.path().join("staging");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(&slot).unwrap();

        let job = write_qwen_unit(&staging);

        // Stage sticky 2B model files in project_dir and link them via BOINC <soft_link> in slot.
        let model_phys = project_dir.join("Qwen3-VL-2B-Instruct-Q8_0.gguf");
        let mmproj_phys = project_dir.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf");
        std::fs::write(&model_phys, b"fake-gguf-weights").unwrap();
        std::fs::write(&mmproj_phys, b"fake-mmproj-weights").unwrap();
        std::fs::write(
            slot.join("Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            format!("<soft_link>{}</soft_link>\n", model_phys.display()),
        )
        .unwrap();
        std::fs::write(
            slot.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            format!("<soft_link>{}</soft_link>\n", mmproj_phys.display()),
        )
        .unwrap();

        // Also link page.jpg and qwen.json via <soft_link> to verify input resolution.
        for name in [PAGE_FILE, JOB_FILE] {
            let phys = project_dir.join(format!("phys_{name}"));
            std::fs::copy(staging.join(name), &phys).unwrap();
            std::fs::write(
                slot.join(name),
                format!("<soft_link>{}</soft_link>\n", phys.display()),
            )
            .unwrap();
        }

        let port = spawn_mock_llama_server(vec!["  first reading\n", "second\x07 reading"]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "0\n"
        );
        assert_eq!(
            std::fs::read_to_string(slot.join(FRACTION_FILE)).unwrap(),
            "1.0\n"
        );

        let result_bytes = std::fs::read(slot.join(RESULT_FILE)).unwrap();
        let readings_bytes = std::fs::read(slot.join(READINGS_FILE)).unwrap();
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let result: ResultManifest = serde_json::from_slice(&result_bytes).unwrap();
        assert_eq!(result.outcome, Outcome::Ok);
        assert_eq!(result.job_id, job.id);
        assert_eq!(result.model.as_deref(), Some("qwen3-vl-2b-q8"));

        let readings = result.check(&readings_bytes, &diag_bytes).unwrap();
        assert_eq!(readings.readings.len(), 2);
        assert_eq!(readings.readings[0].id, "c01");
        assert_eq!(readings.readings[0].text, "first reading");
        assert_eq!(readings.readings[1].id, "c02");
        assert_eq!(readings.readings[1].text, "second reading");
    }

    #[test]
    fn apple_gpu_plan_class_and_gpu_type_enable_metal() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>apple_gpu_8b</plan_class><gpu_type>apple_gpu</gpu_type></app_init_data>",
        )
        .unwrap();
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        let gpu = detect_gpu(&mut host, &mut diag);
        assert!(gpu.use_gpu);
        assert_eq!(gpu.device_label, "metal");
    }
}
