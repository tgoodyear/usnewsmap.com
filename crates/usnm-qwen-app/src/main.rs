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
//!    (`"0\n"` on success, `"1\n"` on failure). Failures exit nonzero and
//!    automatically report bounded diagnostics through standard BOINC stderr.
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use image::GenericImageView as _;
use sha2::Digest as _;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    ops::ControlFlow,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{MAX_BUNDLE_SCAN, MAX_SCAN},
    qwen::{
        Crop, Engine, Job, Model, Outcome, Reading, Readings, ResultManifest, MAX_CROP_TEXT,
        MAX_ERROR, MAX_JOB_JSON, PROMPT, PROMPT_VERSION, VERSION,
    },
    surya::Host,
};
use usnm_ocr_quality::slot::{
    read_bounded, resolve_slot_file, tail, write_atomic, BoincControl, Diag, DIAG_FILE,
    FINISH_FILE, FRACTION_FILE, PAGE_FILE, RESULT_FILE,
};

pub const JOB_FILE: &str = "qwen.json";
pub const READINGS_FILE: &str = "readings.json";
pub const CHECKPOINT_FILE: &str = "qwen_checkpoint.json";
pub const ENV_LLAMA_IMAGE: &str = "USNM_QWEN_LLAMA_IMAGE";
pub const ENV_WEIGHTS: &str = "USNM_QWEN_WEIGHTS";
pub const ENV_MODEL_DIR: &str = "USNM_QWEN_MODEL_DIR";
pub const ENV_LLAMA_SERVER: &str = "USNM_LLAMA_SERVER";
pub const DEFAULT_MODEL_DIR: &str = "/opt/qwen/models";
pub const EMBEDDED_LLAMA_LOCK: &[u8] = include_bytes!("../../../ocr/boinc/qwen/llama.lock");
pub const EMBEDDED_WEIGHTS_LOCK: &[u8] = include_bytes!("../../../ocr/boinc/qwen/weights.lock");

/// Whether `bytes` holds a carriage return. A CRLF checkout (Windows CI's default) would embed
/// locks that never match the LF copies shipped in the slot (.gitattributes pins them to LF).
const fn has_cr(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            return true;
        }
        i += 1;
    }
    false
}
const _: () = assert!(
    !has_cr(EMBEDDED_LLAMA_LOCK) && !has_cr(EMBEDDED_WEIGHTS_LOCK),
    "llama.lock or weights.lock was checked out with CRLF; see .gitattributes"
);
const CTX_SIZE: u32 = 4096;
const LOAD_DEADLINE: Duration = Duration::from_secs(300);

fn start_boinc_control(work: &Path) -> BoincControl {
    let control = BoincControl::start(work);
    #[cfg(unix)]
    {
        use usnm_ocr_quality::slot::ChildAction;
        control.set_child_hook(Some(std::sync::Arc::new(|pgid, action| {
            if let Some(pid) = rustix::process::Pid::from_raw(pgid as i32) {
                let sig = match action {
                    ChildAction::Suspend => rustix::process::Signal::STOP,
                    ChildAction::Resume => rustix::process::Signal::CONT,
                    ChildAction::Terminate => rustix::process::Signal::TERM,
                };
                let _ = rustix::process::kill_process_group(pid, sig);
            }
        })));
    }
    control
}

fn main() -> anyhow::Result<()> {
    if run_slot_reporting(Path::new(".")).is_err() {
        std::process::exit(1);
    }
    if std::fs::read_to_string(FINISH_FILE).is_ok_and(|code| code.trim() != "0") {
        std::process::exit(1);
    }
    Ok(())
}

fn report_failure(work: &Path, report: &str) -> std::io::Result<()> {
    // Windows BOINC starts native apps without inherited stderr handles.
    #[cfg(windows)]
    {
        let mut stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(work.join("stderr.txt"))?;
        writeln!(stderr, "{report}")?;
        stderr.flush()
    }
    #[cfg(not(windows))]
    {
        let _ = work;
        writeln!(std::io::stderr().lock(), "{report}")
    }
}

fn run_slot_reporting(work: &Path) -> anyhow::Result<()> {
    run_slot(work).inspect_err(|e| {
        // The error chain and whatever diag.txt was written, so the server sees why.
        let mut evidence = format!("== error\n{e:#}\n");
        if let Ok(diag) = std::fs::read(resolve_slot_file(work, DIAG_FILE)) {
            evidence.push_str(&String::from_utf8_lossy(&diag));
        }
        let _ = report_failure(
            work,
            &format!(
                "USNM_QWEN_FAILURE {{\"v\":1,\"phase\":\"slot_io_or_serialization\",\"exit_status\":1}}\n{}",
                diag_block(evidence.as_bytes())
            ),
        );
    })
}

/// The last `n` bytes of a log, redacted before the cut: a cut can then split only an
/// already-redacted `.../<file>`, never a name, and a long single-line message (an HTTP error
/// body) keeps its end.
fn redacted_tail(s: &str, n: usize) -> String {
    tail(&redact_paths(s), n).to_string()
}

/// Most of a failed task's diag.txt that its stderr carries. BOINC uploads no output files
/// for a failed task, so this is the only way the reason reaches the server.
const DIAG_EXCERPT_BYTES: usize = 6144;
const DIAG_HEAD_BYTES: usize = 2048;

/// Replaces absolute paths (`/home/user/...`, `C:\\Users\\...`, `~/...`, `\\\\server\\...`)
/// with `.../<file>`, so the excerpt names files without exposing a volunteer's name or
/// directory layout. Paths can hold spaces and brackets (`C:\\Users\\Alice Smith\\...`,
/// `Program Files (x86)`), so a path opened by a quote runs to the closing quote, and an
/// unquoted one runs through the last chunk on its line that has a path separator. This errs
/// toward redacting too much.
fn redact_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    out
}

fn path_starts_at(s: &str) -> bool {
    let b = s.as_bytes();
    let sep = |c: u8| c == b'/' || c == b'\\';
    if s.starts_with("~/") || s.starts_with("\\\\") {
        return true;
    }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && sep(b[2]) {
        return true;
    }
    // `/x/...`: a slash, a name, another slash (not `//` or a lone `/`).
    b.first() == Some(&b'/')
        && b.len() > 2
        && b[1] != b'/'
        && !b[1].is_ascii_whitespace()
        && s[1..]
            .split(char::is_whitespace)
            .next()
            .is_some_and(|t| t.contains('/'))
}

fn redact_line(line: &str) -> String {
    let is_boundary = |c: char| c.is_whitespace() || "\"'`=,([{<:;|".contains(c);
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut prev: Option<char> = None;
    while let Some((i, c)) = rest.char_indices().find(|&(i, c)| {
        let before = if i == 0 {
            prev
        } else {
            rest[..i].chars().next_back()
        };
        before.is_none_or(is_boundary)
            && (c == '/' || c == '~' || c == '\\' || c.is_ascii_alphabetic())
            && path_starts_at(&rest[i..])
    }) {
        let _ = c;
        out.push_str(&rest[..i]);
        let quote = if i == 0 {
            prev
        } else {
            rest[..i].chars().next_back()
        }
        .filter(|q| "\"'`".contains(*q));
        let tail = &rest[i..];
        let body_end = tail.find('\n').unwrap_or(tail.len());
        // Through the last whitespace-separated chunk that holds a separator.
        let mut sep_end = 0;
        let mut pos = 0;
        for chunk in tail[..body_end].split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            if word.contains(['/', '\\']) {
                sep_end = pos + word.len();
            }
            pos += chunk.len();
        }
        // A quoted path ends at the last matching quote on its line, not the first: names
        // like O'Connor hold the quote character. Whichever reaches further wins.
        let end = match quote.and_then(|q| tail[..body_end].rfind(q)) {
            Some(close) => close.max(sep_end),
            None => sep_end,
        };
        let path = &tail[..end];
        let name = path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        out.push_str(".../");
        out.push_str(name);
        prev = path.chars().next_back();
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The diag excerpt for a failed task's stderr: paths redacted, at most
/// `DIAG_EXCERPT_BYTES`, keeping the head (host, GPU and unit sections) and the tail (the
/// llama-server log and the reason the unit stopped).
fn diag_block(diag: &[u8]) -> String {
    let text = redact_paths(&String::from_utf8_lossy(diag));
    let body = if text.len() <= DIAG_EXCERPT_BYTES {
        text
    } else {
        let mut head = DIAG_HEAD_BYTES;
        while !text.is_char_boundary(head) {
            head -= 1;
        }
        let mut tail = text.len() - (DIAG_EXCERPT_BYTES - DIAG_HEAD_BYTES);
        while !text.is_char_boundary(tail) {
            tail += 1;
        }
        format!(
            "{}\n[... {} bytes omitted ...]\n{}",
            &text[..head],
            tail - head,
            &text[tail..]
        )
    };
    format!(
        "USNM_QWEN_DIAG_BEGIN\n{}\nUSNM_QWEN_DIAG_END",
        body.trim_end()
    )
}

fn failure_report(result: &ResultManifest, diag: &[u8]) -> String {
    let evidence = String::from_utf8_lossy(diag).to_ascii_lowercase();
    let short = |s: Option<&str>| {
        s.map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || "._- ".contains(*c))
                .take(64)
                .collect::<String>()
        })
    };
    let digest = |s: &str| {
        s.strip_prefix("sha256:")
            .filter(|s| usnm_ocr_contract::boinc::is_sha256(s))
            .map(|s| format!("sha256:{s}"))
    };
    let report = serde_json::json!({
        "v": 1, "exit_status": 1, "outcome": result.outcome,
        "device": short(Some(&result.device)), "model": short(result.model.as_deref()),
        "llama": digest(&result.engine.llama), "weights": digest(&result.engine.weights),
        "runtime": short(result.host.runtime.as_deref()),
        "plan_class": short(result.host.plan_class.as_deref()),
        "gpu_driver": short(result.host.gpu_driver.as_deref()),
        "gpu_memory_mib": result.host.gpu_mem_total_mib,
        "gpu_peak_mib": result.gpu_mem_peak_mib, "peak_rss_kib": result.peak_rss_kib,
        "load_ms": result.load_ms, "engine_ms": result.engine_ms,
        "runtime_missing": evidence.contains("no such file") || evidence.contains("os error 2"),
        "access_denied": evidence.contains("permission denied") || evidence.contains("os error 5"),
        "cuda_error": evidence.contains("cuda error"),
        "out_of_memory": evidence.contains("out of memory"),
        "runtime_exited": evidence.contains("exited"),
        "diag_sha256": result.diag_sha256,
    });
    // Failed BOINC tasks need not upload declared outputs. Only this fixed-schema
    // summary travels via stderr; never include prompts, readings or raw paths.
    format!("USNM_QWEN_FAILURE {report}")
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

/// One device from `llama-server --list-devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VulkanDevice {
    index: u32,
    name: String,
    total_mib: u64,
    free_mib: u64,
}

/// Software rasterizers Mesa and others expose as Vulkan devices; never a GPU class's device.
const SOFTWARE_VULKAN: [&str; 3] = ["llvmpipe", "lavapipe", "swiftshader"];

/// Parses `  Vulkan0: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)` lines
/// and returns the device BOINC assigned. BOINC numbers a vendor's GPUs among themselves
/// (`gpu_device_num` for `gpu_type intel` counts Intel GPUs only), so `wanted` is an ordinal
/// among the listed hardware devices of `vendor`, not a `VulkanN` index. There is no fallback to
/// another device: one BOINC did not assign may be running another task.
fn vulkan_device(list: &str, vendor: &str, wanted: u32) -> Option<VulkanDevice> {
    let devices: Vec<VulkanDevice> = list
        .lines()
        .filter_map(|l| {
            let rest = l.trim().strip_prefix("Vulkan")?;
            let (index, rest) = rest.split_once(": ")?;
            let index: u32 = index.parse().ok()?;
            let rest = rest.strip_suffix(" MiB free)")?;
            let open = rest.rfind(" (")?;
            let (name, mem) = (&rest[..open], &rest[open + 2..]);
            let (total, free) = mem.split_once(" MiB, ")?;
            Some(VulkanDevice {
                index,
                name: name.trim().to_string(),
                total_mib: total.trim().parse().ok()?,
                free_mib: free.trim().parse().ok()?,
            })
        })
        .filter(|d| {
            let n = d.name.to_ascii_lowercase();
            let matches_vendor = if vendor == "amd" {
                n.split(|c: char| !c.is_ascii_alphanumeric())
                    .any(|word| matches!(word, "amd" | "ati" | "radeon"))
                    || n.contains("advanced micro devices")
            } else {
                n.contains(vendor)
            };
            !SOFTWARE_VULKAN.iter().any(|s| n.contains(s)) && matches_vendor
        })
        .collect();
    devices.into_iter().nth(usize::try_from(wanted).ok()?)
}

/// The library search variable and value that let a bundled `llama-server` find its libraries.
fn library_env(server_bin: &Path) -> Option<(&'static str, std::ffi::OsString)> {
    let bin_dir = server_bin.parent().filter(|p| !p.as_os_str().is_empty())?;
    #[cfg(target_os = "linux")]
    let lib_var = "LD_LIBRARY_PATH";
    #[cfg(target_os = "macos")]
    let lib_var = "DYLD_LIBRARY_PATH";
    #[cfg(target_os = "windows")]
    let lib_var = "PATH";
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let lib_var = "LD_LIBRARY_PATH";
    let cur = std::env::var_os(lib_var).unwrap_or_default();
    let mut paths = vec![bin_dir.to_path_buf()];
    paths.extend(std::env::split_paths(&cur));
    std::env::join_paths(paths).ok().map(|j| (lib_var, j))
}

/// `llama-server --list-devices`, bounded to 30 seconds.
fn list_devices(server_bin: &Path) -> Result<String, String> {
    let mut cmd = Command::new(server_bin);
    cmd.arg("--list-devices");
    let out = device_probe(&mut cmd, server_bin)?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(text)
}

fn device_probe(cmd: &mut Command, server_bin: &Path) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((var, value)) = library_env(server_bin) {
        cmd.env(var, value);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", server_bin.display()))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > Duration::from_secs(30) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GPU device probe: no answer in 30s".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "GPU device probe failed: {}",
            redacted_tail(&String::from_utf8_lossy(&out.stderr), 1024)
        ));
    }
    Ok(out)
}

fn assigned_amd_device(work: &Path, host: &Host, list: &str) -> Result<VulkanDevice, String> {
    let init = std::fs::read_to_string(work.join("init_data.xml")).map_err(|e| e.to_string())?;
    let opencl_index = tag(&init, "gpu_opencl_dev_index")
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("missing assigned OpenCL index")?;
    if host.gpu_device_num.is_none_or(|n| n < 0)
        || !host
            .gpu_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("ati") || t.eq_ignore_ascii_case("amd"))
    {
        return Err("missing AMD resource assignment".into());
    }
    let server = find_llama_server(work).map_err(|e| e.to_string())?;
    let dir = server.parent().ok_or("missing Vulkan bundle directory")?;
    let helper = resolve_slot_file(
        work,
        if cfg!(windows) {
            "usnm-amd-device-map.exe"
        } else {
            "usnm-amd-device-map"
        },
    );
    let out = device_probe(
        Command::new(helper).arg(dir).arg(opencl_index.to_string()),
        &server,
    )?;
    let text = std::str::from_utf8(&out.stdout)
        .map_err(|e| e.to_string())?
        .trim();
    let index = text
        .strip_prefix("Vulkan")
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or("invalid PCI device mapping")?;
    let matches: Vec<_> = (0..256)
        .map_while(|n| vulkan_device(list, "amd", n))
        .filter(|d| d.index == index)
        .collect();
    if matches.len() != 1 {
        return Err("PCI-verified AMD device missing or duplicated in Vulkan listing".into());
    }
    Ok(matches[0].clone())
}

/// Why a Vulkan unit can't run on this host, if it can't: no hardware device, or less free
/// device memory than the job's largest model needs (#786).
fn vulkan_refusal(gpu: &GpuState, job: &Job) -> Option<String> {
    gpu.vulkan_device
        .is_none()
        .then(|| {
            "Vulkan plan class assigned, but llama-server lists no hardware Vulkan device"
                .to_string()
        })
        .or_else(|| {
            let need = job
                .engine
                .models
                .iter()
                .map(|m| u64::from(m.min_gpu_mb))
                .max()?;
            let free = gpu.free_mb?;
            (free < need)
                .then(|| format!("Vulkan device has {free} MiB free; the model needs {need} MiB"))
        })
}

#[derive(Debug, Clone)]
struct GpuState {
    cuda_device: Option<u32>,
    /// The `VulkanN` ordinal of the assigned device, as `llama-server --list-devices` names it and
    /// `--device` takes it (#786).
    vulkan_device: Option<u32>,
    free_mb: Option<u64>,
    used_before_mb: Option<u64>,
    use_gpu: bool,
    device_label: String,
}

fn detect_gpu(work: &Path, host: &mut Host, diag: &mut Diag) -> GpuState {
    let mut body = String::new();
    let plan = host.plan_class.as_deref().unwrap_or("");
    let gpu_type = host.gpu_type.as_deref().unwrap_or("");

    if plan == "mt" || plan == "docker_mt" || plan == "cpu" {
        let _ = writeln!(body, "CPU-only plan class ({plan})");
        diag.section("gpu", &body, 1024);
        return GpuState {
            cuda_device: None,
            vulkan_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: false,
            device_label: "cpu".into(),
        };
    }

    if plan.contains("vulkan") {
        // No CPU fallback inside a GPU class: without a hardware Vulkan device the GPU is
        // `None` and the unit ends `no_gpu` (#786).
        let devices = find_llama_server(work)
            .map_err(|e| format!("{e:#}"))
            .and_then(|bin| list_devices(&bin));
        let wanted = u32::try_from(host.gpu_device_num.unwrap_or(0)).ok();
        let (vulkan_device, free_mb) = match &devices {
            Ok(text) => {
                let _ = writeln!(body, "llama-server --list-devices:\n{}", text.trim());
                let vendor = if plan.contains("intel") {
                    "intel"
                } else if plan.contains("nvidia") {
                    "nvidia"
                } else if plan == "amd_vulkan_8b" {
                    "amd"
                } else {
                    "unsupported-vulkan-vendor"
                };
                let selected = if plan == "amd_vulkan_8b" {
                    match assigned_amd_device(work, host, text) {
                        Ok(d) => Some(d),
                        Err(error) => {
                            let _ = writeln!(body, "AMD PCI identity mapping refused: {error}");
                            None
                        }
                    }
                } else {
                    wanted.and_then(|ordinal| vulkan_device(text, vendor, ordinal))
                };
                match selected {
                    Some(d) => {
                        host.gpu = Some(d.name.chars().take(64).collect());
                        host.gpu_mem_total_mib = Some(d.total_mib);
                        (Some(d.index), Some(d.free_mib))
                    }
                    None => {
                        let _ = writeln!(body, "no hardware Vulkan device");
                        (None, None)
                    }
                }
            }
            Err(e) => {
                let _ = writeln!(body, "listing Vulkan devices failed: {e}");
                (None, None)
            }
        };
        let _ = writeln!(body, "Vulkan via plan_class, device {vulkan_device:?}");
        diag.section("gpu", &body, 2048);
        return GpuState {
            cuda_device: None,
            vulkan_device,
            free_mb,
            used_before_mb: None,
            use_gpu: vulkan_device.is_some(),
            device_label: "vulkan".into(),
        };
    }

    // Check NVIDIA via nvidia-smi.
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
                    vulkan_device: None,
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
            vulkan_device: None,
            free_mb: None,
            used_before_mb: None,
            use_gpu: true,
            device_label: "metal".into(),
        };
    }

    let _ = writeln!(body, "running on CPU");
    diag.section("gpu", &body, 1024);
    GpuState {
        cuda_device: None,
        vulkan_device: None,
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
    pub bundle_pages: BTreeMap<String, image::DynamicImage>,
}

/// Per-crop checkpoint state persisted atomically in `qwen_checkpoint.json`
/// so interrupted single-page or multi-page `usnm_qwen` workunits resume from
/// the last completed crop without re-running completed crops.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenCheckpoint {
    pub v: u32,
    pub job_id: String,
    pub engine: Engine,
    pub model: String,
    pub device: String,
    pub readings: Vec<Reading>,
}

impl QwenCheckpoint {
    pub fn load_valid(work: &Path, job: &Job, model: &str, device: &str) -> Option<Self> {
        let bytes = std::fs::read(work.join(CHECKPOINT_FILE)).ok()?;
        let cp: Self = serde_json::from_slice(&bytes).ok()?;
        if cp.v != VERSION
            || cp.job_id != job.id
            || cp.engine != job.engine
            || cp.model != model
            || cp.device != device
            || cp.readings.len() > job.crops.len()
        {
            return None;
        }
        for (got, want) in cp.readings.iter().zip(&job.crops) {
            if got.id != want.id {
                return None;
            }
        }
        Some(cp)
    }

    pub fn save(&self, work: &Path) -> anyhow::Result<()> {
        write_atomic(
            &work.join(CHECKPOINT_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}

#[derive(serde::Deserialize, Default)]
struct LlamaLockFile {
    #[serde(default)]
    assets: BTreeMap<String, LlamaLockAsset>,
    #[serde(default)]
    platforms: BTreeMap<String, LlamaLockAsset>,
}

#[derive(serde::Deserialize, Default)]
struct LlamaLockAsset {
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    llama_server_sha256: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct WeightsLockJson {
    #[serde(default)]
    models: Vec<WeightsLockModelJson>,
}

#[derive(serde::Deserialize)]
struct WeightsLockModelJson {
    file: String,
    sha256: String,
    mmproj: WeightsLockMmprojJson,
}

#[derive(serde::Deserialize)]
struct WeightsLockMmprojJson {
    file: String,
    sha256: String,
}

fn active_llama_lock_bytes(work: &Path) -> anyhow::Result<Vec<u8>> {
    let slot_lock = resolve_slot_file(work, "llama.lock");
    if slot_lock.is_file() {
        let bytes = read_bounded(&slot_lock, 64 * 1024)?;
        #[cfg(not(test))]
        ensure!(
            bytes.as_slice() == EMBEDDED_LLAMA_LOCK,
            "slot llama.lock does not match embedded llama.lock"
        );
        return Ok(bytes);
    }
    Ok(EMBEDDED_LLAMA_LOCK.to_vec())
}

fn active_weights_lock_bytes(work: &Path) -> anyhow::Result<Vec<u8>> {
    let slot_lock = resolve_slot_file(work, "weights.lock");
    if slot_lock.is_file() {
        let bytes = read_bounded(&slot_lock, 64 * 1024)?;
        #[cfg(not(test))]
        ensure!(
            bytes.as_slice() == EMBEDDED_WEIGHTS_LOCK,
            "slot weights.lock does not match embedded weights.lock"
        );
        return Ok(bytes);
    }
    Ok(EMBEDDED_WEIGHTS_LOCK.to_vec())
}

fn llama_lock_identities(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let parsed: LlamaLockFile = serde_json::from_slice(bytes).context("parsing llama.lock JSON")?;
    let mut out = vec![format!("sha256:{}", sha256(bytes))];
    for entry in parsed.assets.values().chain(parsed.platforms.values()) {
        for cand in [&entry.sha256, &entry.llama_server_sha256]
            .into_iter()
            .flatten()
        {
            let hex = cand.strip_prefix("sha256:").unwrap_or(cand);
            if usnm_ocr_contract::boinc::is_sha256(hex) {
                out.push(format!("sha256:{hex}"));
            }
        }
    }
    Ok(out)
}

fn weights_lock_identities(bytes: &[u8]) -> Vec<String> {
    let mut out = vec![format!("sha256:{}", sha256(bytes))];
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut non_comments = String::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            non_comments.push_str(line);
            non_comments.push('\n');
        }
        let d = format!("sha256:{}", sha256(non_comments.as_bytes()));
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

pub fn parse_weights_lock(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).context("weights.lock is not UTF-8")?;
    let trimmed = text.trim();
    let mut map = BTreeMap::new();
    if trimmed.starts_with('{') {
        let parsed: WeightsLockJson =
            serde_json::from_str(trimmed).context("parsing weights.lock JSON")?;
        for m in parsed.models {
            let m_hex = m.sha256.strip_prefix("sha256:").unwrap_or(&m.sha256);
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(m_hex),
                "invalid sha256 for {} in weights.lock",
                m.file
            );
            map.insert(m.file, m_hex.to_owned());
            let mm_hex = m
                .mmproj
                .sha256
                .strip_prefix("sha256:")
                .unwrap_or(&m.mmproj.sha256);
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(mm_hex),
                "invalid sha256 for {} in weights.lock",
                m.mmproj.file
            );
            map.insert(m.mmproj.file, mm_hex.to_owned());
        }
        return Ok(map);
    }
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        ensure!(parts.len() >= 2, "malformed line in weights.lock: {line}");
        let file = parts[0];
        let digest = parts[parts.len() - 1]
            .strip_prefix("sha256:")
            .unwrap_or(parts[parts.len() - 1]);
        ensure!(
            usnm_ocr_contract::boinc::is_sha256(digest),
            "invalid sha256 for {file} in weights.lock"
        );
        map.insert(file.to_owned(), digest.to_owned());
    }
    Ok(map)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

fn check_engine(work: &Path, job: &Job) -> anyhow::Result<()> {
    match std::env::var(ENV_LLAMA_IMAGE) {
        Ok(llama) if !llama.is_empty() => {
            ensure!(
                llama == job.engine.llama,
                "unit is for llama image {}, host has {llama}",
                job.engine.llama
            );
        }
        _ => {
            let lock_bytes = active_llama_lock_bytes(work)?;
            let allowed = llama_lock_identities(&lock_bytes)?;
            ensure!(
                allowed.iter().any(|a| a == &job.engine.llama),
                "unit is for llama {}, host llama.lock allows {:?}",
                job.engine.llama,
                allowed
            );
        }
    }
    match std::env::var(ENV_WEIGHTS) {
        Ok(weights) if !weights.is_empty() => {
            ensure!(
                weights == job.engine.weights,
                "unit is for weights {}, host has {weights}",
                job.engine.weights
            );
        }
        _ => {
            let lock_bytes = active_weights_lock_bytes(work)?;
            let allowed = weights_lock_identities(&lock_bytes);
            ensure!(
                allowed.iter().any(|a| a == &job.engine.weights),
                "unit is for weights {}, host weights.lock allows {:?}",
                job.engine.weights,
                allowed
            );
        }
    }
    Ok(())
}

pub fn prepare(work: &Path) -> anyhow::Result<Loaded> {
    let job_path = resolve_slot_file(work, JOB_FILE);
    let job: Job = serde_json::from_slice(&read_bounded(&job_path, MAX_JOB_JSON)?)?;
    job.validate()?;
    check_engine(work, &job)?;
    let max_scan = if job.pages.is_empty() {
        MAX_SCAN
    } else {
        MAX_BUNDLE_SCAN
    };
    let page_bytes = read_bounded(&resolve_slot_file(work, PAGE_FILE), max_scan)?;
    ensure!(
        job.input.matches(&page_bytes),
        "input page checksum mismatch"
    );
    if job.pages.is_empty() {
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
        Ok(Loaded {
            job,
            page,
            bundle_pages: BTreeMap::new(),
        })
    } else {
        let mut bundle_pages = BTreeMap::new();
        let mut first_page: Option<image::DynamicImage> = None;
        for bp in &job.pages {
            let slice = bp.input.extract(&page_bytes, "page")?;
            let img = image::load_from_memory(slice)
                .with_context(|| format!("decoding page.jpg for {}", bp.doc_id))?;
            let (w, h) = img.dimensions();
            for c in &bp.crops {
                let [_, _, x1, y1] = c.rect;
                ensure!(
                    x1 <= w && y1 <= h,
                    "crop {}:{} {:?} is outside a {w}x{h} page",
                    bp.doc_id,
                    c.id,
                    c.rect
                );
            }
            if first_page.is_none() {
                first_page = Some(img.clone());
            }
            bundle_pages.insert(bp.doc_id.clone(), img);
        }
        Ok(Loaded {
            job,
            page: first_page.expect("non-empty bundle pages"),
            bundle_pages,
        })
    }
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

pub fn crop_png_for_loaded(l: &Loaded, c: &Crop) -> anyhow::Result<Vec<u8>> {
    if l.job.pages.is_empty() {
        crop_png(&l.page, c)
    } else {
        let (doc, _) =
            c.id.split_once(':')
                .context("bundle crop id missing doc_id prefix")?;
        let page = l
            .bundle_pages
            .get(doc)
            .with_context(|| format!("bundle page {doc} not found"))?;
        crop_png(page, c)
    }
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
/// slot directory or model directory and verifies their SHA-256 digests against
/// `weights.lock`.
pub fn resolve_model<'a>(
    work: &Path,
    models: &'a [Model],
    gpu_free_mb: Option<u64>,
    use_gpu: bool,
    plan_class: Option<&str>,
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
    let chosen = match plan_class {
        Some(p) if p == "mt" || p == "docker_mt" || p == "cpu" || p.ends_with("_2b") => available
            .iter()
            .find(|(m, _, _)| m.min_gpu_mb == 0 || m.name.contains("2b"))
            .with_context(|| format!("plan class {p} requires a 2B model, none found"))?,
        Some(p) if p.ends_with("_8b") => available
            .iter()
            .find(|(m, _, _)| m.min_gpu_mb > 0 || m.name.contains("8b"))
            .with_context(|| format!("plan class {p} requires an 8B model, none found"))?,
        _ => match gpu_free_mb {
            Some(free) => available
                .iter()
                .find(|(m, _, _)| u64::from(m.min_gpu_mb) <= free)
                .or_else(|| available.last())
                .unwrap(),
            None if use_gpu => available.first().unwrap(),
            None => available.last().unwrap(),
        },
    };

    let weights_bytes = active_weights_lock_bytes(work)?;
    let expected = parse_weights_lock(&weights_bytes)?;
    for (logical, actual_path) in [(&chosen.0.file, &chosen.1), (&chosen.0.mmproj, &chosen.2)] {
        let want = expected
            .get(logical.as_str())
            .with_context(|| format!("{} is missing from weights.lock", logical))?;
        let got = sha256_file(actual_path)?;
        ensure!(
            &got == want,
            "model file {} ({}) sha256 mismatch: expected {want}, got {got}",
            logical,
            actual_path.display()
        );
    }

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
            redacted_tail(&String::from_utf8_lossy(&resp), 256)
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
    control: BoincControl,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.control.set_child_pgid(None);
        #[cfg(unix)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    fn start(
        work: &Path,
        server_bin: &Path,
        model_path: &Path,
        mmproj_path: &Path,
        gpu: &GpuState,
        threads: u32,
        load_deadline: Duration,
        control: &BoincControl,
    ) -> anyhow::Result<(Self, u64)> {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            bail!("boinc control requested exit ({reason})");
        }
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
        usnm_ocr_quality::slot::apply_gpu_cache_env(&mut cmd, work);
        // --device takes the names --list-devices printed. GGML_VK_VISIBLE_DEVICES would index
        // the unfiltered physical-device list instead, which can include llvmpipe.
        // The vision projector picks its backend separately (MTMD_BACKEND_DEVICE, else a
        // default GPU), so it is pinned to the same device.
        if let Some(dev) = gpu.vulkan_device {
            let name = format!("Vulkan{dev}");
            cmd.args(["--device", &name]);
            cmd.env("MTMD_BACKEND_DEVICE", &name);
        }
        if let Some((var, value)) = library_env(server_bin) {
            cmd.env(var, value);
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
        control.set_child_pgid(Some(child.id()));
        let mut srv = Self {
            child,
            port,
            log_path,
            control: control.clone(),
        };
        let mut active_elapsed = Duration::ZERO;
        loop {
            if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
                bail!("boinc control requested exit ({reason})");
            }
            let step_start = Instant::now();
            if let Some(status) = srv.child.try_wait()? {
                let log_tail = std::fs::read_to_string(&srv.log_path).unwrap_or_default();
                bail!(
                    "llama-server exited ({status}) before ready: {}",
                    redacted_tail(&log_tail, 512)
                );
            }
            if let Ok((200, _)) = http_request(port, "GET", "/health", None, Duration::from_secs(2))
            {
                return Ok((srv, ms(started)));
            }
            if active_elapsed > load_deadline {
                let log_tail = std::fs::read_to_string(&srv.log_path).unwrap_or_default();
                bail!(
                    "llama-server did not become healthy in {}s: {}",
                    load_deadline.as_secs(),
                    redacted_tail(&log_tail, 512)
                );
            }
            std::thread::sleep(Duration::from_millis(200));
            if !control.is_suspended() {
                active_elapsed += step_start.elapsed();
            }
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

    let control = start_boinc_control(work);
    let mut diag = Diag::new();
    let mut host = Host::default();
    boinc_info(work, &mut host, &mut diag);
    runtime_info(&mut host, &mut diag);
    let gpu = detect_gpu(work, &mut host, &mut diag);

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

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
                .is_some_and(|p| p.contains("cuda"));
            let vulkan_refused = host
                .plan_class
                .as_deref()
                .is_some_and(|p| p.contains("vulkan"))
                .then(|| vulkan_refusal(&gpu, &l.job))
                .flatten();
            if let Some(reason) = vulkan_refused {
                diag.section("skipped", &reason, 256);
                empty_result(
                    l.job.id.clone(),
                    l.job.engine.clone(),
                    l.job.crops.len() as u32,
                    Outcome::NoGpu,
                    None,
                )
            } else if want_cuda_only && gpu.cuda_device.is_none() {
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
                run_crops(
                    work,
                    &l,
                    &host,
                    &gpu,
                    existing_port,
                    deadline,
                    &control,
                    &mut diag,
                )
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

    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        diag.section(
            "boinc control",
            &format!("exiting without boinc_finish_called: {reason}"),
            256,
        );
        write_atomic(&diag_out, &diag.finish())?;
        return Ok(());
    }

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
    if result.outcome == Outcome::Ok {
        write_atomic(&finish_out, b"0\n")?;
    } else {
        report_failure(
            work,
            &format!(
                "{}\n{}",
                failure_report(&result, &diag_bytes),
                diag_block(&diag_bytes)
            ),
        )?;
        write_atomic(&finish_out, b"1\n")?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_crops(
    work: &Path,
    l: &Loaded,
    host: &Host,
    gpu: &GpuState,
    existing_port: Option<u16>,
    deadline: Duration,
    control: &BoincControl,
    diag: &mut Diag,
) -> (ResultManifest, Readings) {
    if control.wait_if_suspended_or_should_exit().is_break() {
        return empty_result(
            l.job.id.clone(),
            l.job.engine.clone(),
            l.job.crops.len() as u32,
            Outcome::Failed,
            None,
        );
    }

    let (model, model_path, mmproj_path) = match resolve_model(
        work,
        &l.job.engine.models,
        gpu.free_mb,
        gpu.use_gpu,
        host.plan_class.as_deref(),
    ) {
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

    let threads = if gpu.use_gpu {
        usnm_ocr_quality::slot::gpu_threads(host.ncpus, host.cpu_threads)
    } else {
        host.ncpus
            .map(|n| n.ceil() as u32)
            .or(host.cpu_threads)
            .unwrap_or(1)
            .max(1)
    };

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
                control,
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
    let mut cp = match QwenCheckpoint::load_valid(work, &l.job, &model.name, &gpu.device_label) {
        Some(existing) => {
            if !existing.readings.is_empty() {
                diag.section(
                    "checkpoint",
                    &format!(
                        "resumed from checkpoint: {} of {} crops already complete",
                        existing.readings.len(),
                        l.job.crops.len()
                    ),
                    256,
                );
                let frac = existing.readings.len() as f64 / (l.job.crops.len().max(1)) as f64;
                let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.4}\n").as_bytes());
            }
            existing
        }
        None => QwenCheckpoint {
            v: VERSION,
            job_id: l.job.id.clone(),
            engine: l.job.engine.clone(),
            model: model.name.clone(),
            device: gpu.device_label.clone(),
            readings: Vec::with_capacity(l.job.crops.len()),
        },
    };
    let mut outcome = Outcome::Ok;
    let mut gpu_peak_mib: Option<u64> = None;

    for (idx, crop) in l.job.crops.iter().enumerate().skip(cp.readings.len()) {
        if control.wait_if_suspended_or_should_exit().is_break() {
            outcome = Outcome::Failed;
            break;
        }
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
        let png = match crop_png_for_loaded(l, crop) {
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
                cp.readings.push(Reading {
                    id: crop.id.clone(),
                    ms: ms(t_crop),
                    text,
                    tokens,
                });
                let _ = cp.save(work);
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
                if control.should_exit().is_some() {
                    outcome = Outcome::Failed;
                    break;
                }
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

    if outcome == Outcome::Ok && cp.readings.len() == l.job.crops.len() {
        let _ = std::fs::remove_file(work.join(CHECKPOINT_FILE));
    }
    let out_readings = cp.readings;

    let engine_ms = ms(started);
    if let Some(ref srv) = srv_guard {
        if let Ok(log_text) = std::fs::read_to_string(&srv.log_path) {
            diag.section(
                "llama-server.log (tail)",
                &redacted_tail(&log_text, 2048),
                2048,
            );
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

    const FAKE_GGUF: &[u8] = b"fake-gguf-weights";
    const FAKE_MMPROJ: &[u8] = b"fake-mmproj-weights";

    fn test_weights_lock_bytes() -> Vec<u8> {
        let gguf_sum = sha256(FAKE_GGUF);
        let mmproj_sum = sha256(FAKE_MMPROJ);
        format!(
            "# synthetic weights.lock for unit tests\n\
             Qwen3VL-8B-Instruct-Q4_K_M.gguf https://example.invalid/8b.gguf {gguf_sum}\n\
             mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf https://example.invalid/8b.mmproj {mmproj_sum}\n\
             Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.gguf {gguf_sum}\n\
             mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.mmproj {mmproj_sum}\n"
        )
        .into_bytes()
    }

    fn write_qwen_unit(dir: &Path) -> Job {
        std::fs::create_dir_all(dir).unwrap();
        let page = synthetic_jpeg(200, 200);
        let weights_lock = test_weights_lock_bytes();
        std::fs::write(dir.join("weights.lock"), &weights_lock).unwrap();
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
                llama: format!("sha256:{}", sha256(EMBEDDED_LLAMA_LOCK)),
                models,
                weights: format!("sha256:{}", sha256(&weights_lock)),
            },
            gpu_timeout_secs: 300,
            group: "serbian".into(),
            input: Input::of(&page, "local:page.jpg"),
            language: "srp".into(),
            max_tokens_per_1000px2: 4,
            pages: vec![],
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
    fn failed_slot_marks_computation_error_and_reports_only_bounded_facts() {
        let tmp = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        std::fs::write(tmp.path().join("stderr.txt"), "existing BOINC diagnostic\n").unwrap();
        run_slot_with(tmp.path(), None, None).unwrap();
        #[cfg(windows)]
        {
            let stderr = std::fs::read_to_string(tmp.path().join("stderr.txt")).unwrap();
            assert!(stderr.starts_with("existing BOINC diagnostic\nUSNM_QWEN_FAILURE "));
            assert_eq!(stderr.matches("USNM_QWEN_FAILURE ").count(), 1);
            assert!(stderr.contains("\"outcome\":\"unit_error\""));
            assert!(
                stderr.contains("USNM_QWEN_DIAG_BEGIN\n")
                    && stderr.contains("\nUSNM_QWEN_DIAG_END")
            );
            assert!(stderr.len() < 2048 + DIAG_EXCERPT_BYTES + 128);
        }
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(FINISH_FILE)).unwrap(),
            "1\n"
        );
        let mut result: ResultManifest =
            serde_json::from_slice(&std::fs::read(tmp.path().join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::UnitError);
        result.engine.llama = "PRIVATE_TOKEN".repeat(1000);
        result.unit_error = Some("PRIVATE_TOKEN raw input text credentials local path".into());
        result.host.runtime = Some("native-windows-x86_64".into());
        result.host.gpu_mem_total_mib = Some(8192);
        for outcome in [
            Outcome::UnitError,
            Outcome::Failed,
            Outcome::NoGpu,
            Outcome::Timeout,
        ] {
            result.outcome = outcome;
            let report = failure_report(&result, b"CUDA error: out of memory PRIVATE_TOKEN");
            assert!(report.len() <= 2048);
            assert!(!report.contains("PRIVATE_TOKEN"));
            assert!(!report.contains("raw input text"));
            let facts: serde_json::Value =
                serde_json::from_str(report.strip_prefix("USNM_QWEN_FAILURE ").unwrap()).unwrap();
            assert_eq!(facts["exit_status"], 1);
            assert_eq!(facts["gpu_memory_mib"], 8192);
            assert_eq!(facts["cuda_error"], true);
            assert_eq!(facts["out_of_memory"], true);
            assert!(facts["llama"].is_null());
        }
    }

    #[test]
    fn path_redaction_covers_spaces_brackets_and_quotes() {
        let cases = [
            (
                "model C:\\Users\\Alice Smith\\AppData\\BOINC\\llama-server.exe failed",
                "model .../llama-server.exe failed",
            ),
            (
                "open \"C:\\Users\\Alice Smith\\x y\\Qwen3VL-8B.gguf\": access denied",
                "open \".../Qwen3VL-8B.gguf\": access denied",
            ),
            (
                "C:\\Program Files (x86)\\BOINC\\boinc.exe started",
                ".../boinc.exe started",
            ),
            (
                "spawning /home/alice smith/boinc/llama-server failed: os error 2",
                "spawning .../llama-server failed: os error 2",
            ),
            ("cwd='/Users/bob/Library/BOINC Data/slots/3'", "cwd='.../3'"),
            (
                "GET http://127.0.0.1:8080/health ok; a/b stays",
                "GET http://127.0.0.1:8080/health ok; a/b stays",
            ),
            ("\\\\nas\\share\\Alice\\f.txt", ".../f.txt"),
            (
                "failed to load 'C:\\Users\\O'Connor\\BOINC\\model.gguf' now",
                "failed to load '.../model.gguf' now",
            ),
            (
                "open \"/home/d'arcy/BOINC data/x.gguf\": denied",
                "open \".../x.gguf\": denied",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact_paths(input), want, "{input}");
        }
        let two = redact_paths("a /home/alice/x\nb C:\\Users\\Bob Jones\\y.gguf\n");
        assert_eq!(two, "a .../x\nb .../y.gguf\n");
        assert!(!two.contains("alice") && !two.contains("Bob"));
    }

    #[test]
    fn log_tails_are_redacted_before_the_cut() {
        let log = "load C:\\Users\\Alice Smith\\AppData\\BOINC\\model.gguf\nready\n";
        // Any cut through the path line leaves no part of the name.
        for n in 1..log.len() {
            let t = redacted_tail(log, n);
            assert!(
                !t.contains("Alice") && !t.contains("Smith") && !t.contains("AppData"),
                "{n}: {t}"
            );
        }
        // A long single-line error body keeps its end.
        let body = format!(
            "{{\"error\":{{\"message\":\"{}model too large for /home/bob/x/llama.gguf\"}}}}",
            "x".repeat(400)
        );
        let t = redacted_tail(&body, 256);
        assert!(t.ends_with("model too large for .../llama.gguf\"}}"), "{t}");
        assert!(!t.contains("bob"));
    }

    #[test]
    fn diag_excerpt_redacts_paths_and_keeps_head_and_tail() {
        let text = "spawning /home/alice/boinc/slots/3/llama-server failed: os error 2\n\
                    model C:\\Users\\bob\\AppData\\BOINC\\Qwen3VL-8B.gguf (\"~/x/y.txt\")\n\
                    GET http://127.0.0.1:8080/health ok; relative a/b stays";
        let r = redact_paths(text);
        assert!(r.contains("spawning .../llama-server failed"), "{r}");
        // Two paths on one line collapse to the last file name: redacting more is the safe side.
        assert!(r.contains("model .../y.txt\")"), "{r}");
        assert!(
            r.contains("http://127.0.0.1:8080/health") && r.contains("a/b stays"),
            "{r}"
        );
        assert!(!r.contains("alice") && !r.contains("bob"));

        let short = diag_block(b"== gpu\nno hardware Vulkan device\n");
        assert_eq!(
            short,
            "USNM_QWEN_DIAG_BEGIN\n== gpu\nno hardware Vulkan device\nUSNM_QWEN_DIAG_END"
        );

        let long = format!("HEAD-MARK{}TAIL-MARK", "x".repeat(20_000));
        let block = diag_block(long.as_bytes());
        assert!(block.len() <= DIAG_EXCERPT_BYTES + 128);
        assert!(block.contains("HEAD-MARK") && block.contains("TAIL-MARK"));
        assert!(block.contains("bytes omitted"));
        // Multi-byte text is cut on character boundaries.
        let wide = "é".repeat(10_000);
        assert!(diag_block(wide.as_bytes()).len() <= DIAG_EXCERPT_BYTES + 128);
    }

    #[test]
    fn early_slot_io_error_uses_the_same_boinc_failure_transport() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(RESULT_FILE)).unwrap();
        #[cfg(windows)]
        std::fs::write(tmp.path().join("stderr.txt"), "existing BOINC diagnostic\n").unwrap();
        assert!(run_slot_reporting(tmp.path()).is_err());
        #[cfg(windows)]
        {
            let stderr = std::fs::read_to_string(tmp.path().join("stderr.txt")).unwrap();
            assert!(stderr.starts_with(
                "existing BOINC diagnostic\nUSNM_QWEN_FAILURE {\"v\":1,\"phase\":\"slot_io_or_serialization\",\"exit_status\":1}\nUSNM_QWEN_DIAG_BEGIN\n== error\n"
            ));
            assert!(stderr.ends_with("\nUSNM_QWEN_DIAG_END\n"));
        }
        assert!(!tmp.path().join(FINISH_FILE).exists());
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

        // Also link page.jpg, qwen.json, and weights.lock via <soft_link> to verify input resolution.
        for name in [PAGE_FILE, JOB_FILE, "weights.lock"] {
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
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(gpu.use_gpu);
        assert_eq!(gpu.device_label, "metal");
    }

    #[test]
    fn vulkan_device_list_skips_software_devices() {
        let list = "load_backend: loaded Vulkan backend\nAvailable devices:\n  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (15834 MiB, 15000 MiB free)\n  Vulkan1: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)\n";
        let d = vulkan_device(list, "intel", 0).unwrap();
        assert_eq!(d.index, 1);
        assert_eq!(d.name, "Intel(R) Iris(R) Xe Graphics (TGL GT2)");
        assert_eq!((d.total_mib, d.free_mib), (7917, 7125));
        let soft = "  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (1 MiB, 1 MiB free)";
        assert!(vulkan_device(soft, "", 0).is_none());
        let two = "  Vulkan0: Intel(R) Arc(TM) A770 Graphics (16384 MiB, 16000 MiB free)\n  Vulkan1: Intel(R) UHD Graphics 770 (7000 MiB, 6500 MiB free)";
        assert_eq!(vulkan_device(two, "intel", 1).unwrap().index, 1);
        // No fallback to an unassigned device.
        assert!(vulkan_device(two, "intel", 5).is_none());
        // BOINC's Intel device 0 is the first Intel device, not Vulkan0.
        let mixed = "  Vulkan0: NVIDIA GeForce RTX 3060 (12288 MiB, 11000 MiB free)\n  Vulkan1: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 7125 MiB free)";
        assert_eq!(vulkan_device(mixed, "intel", 0).unwrap().index, 1);
        assert!(vulkan_device(mixed, "intel", 1).is_none());
        assert_eq!(vulkan_device(mixed, "nvidia", 0).unwrap().index, 0);
        assert!(vulkan_device(mixed, "nvidia", 1).is_none());
        assert!(vulkan_device("no devices", "intel", 0).is_none());
    }

    #[cfg(unix)]
    fn fake_llama_server(slot: &Path, listing: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let bin = slot.join("llama-server");
        std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s\\n' '{listing}'\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn amd_vulkan_assignment_and_memory_are_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let mut job = write_qwen_unit(&slot);
        job.engine.models[0].min_gpu_mb = 7000;
        let listing = "  Vulkan0: NVIDIA GeForce RTX 3060 (12288 MiB, 11000 MiB free)
  Vulkan1: AMD llvmpipe (LLVM 19) (16384 MiB, 16000 MiB free)
  Vulkan2: AMD Radeon RX 9070 XT (16384 MiB, 15000 MiB free)
  Vulkan3: Intel Arc A770 (16384 MiB, 16000 MiB free)
  Vulkan4: Advanced Micro Devices, Inc. [RADV NAVI48] (16384 MiB, 6000 MiB free)";
        fake_llama_server(&slot, listing);
        use std::os::unix::fs::PermissionsExt as _;
        let helper = slot.join("usnm-amd-device-map");
        std::fs::write(
            &helper,
            "#!/bin/sh\ncase \"$2\" in 0) echo Vulkan2;; 1) echo Vulkan4;; *) exit 1;; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for (physical, opencl, expected) in [
            (12, 0, Some(2)),
            (0, 1, Some(4)),
            (0, 2, None),
            (-1, 0, None),
        ] {
            std::fs::write(
                slot.join("init_data.xml"),
                format!("<app_init_data><plan_class>amd_vulkan_8b</plan_class><gpu_type>ATI</gpu_type><gpu_device_num>{physical}</gpu_device_num><gpu_opencl_dev_index>{opencl}</gpu_opencl_dev_index></app_init_data>"),
            )
            .unwrap();
            let mut host = Host::default();
            let mut diag = Diag::new();
            boinc_info(&slot, &mut host, &mut diag);
            let gpu = detect_gpu(&slot, &mut host, &mut diag);
            assert_eq!(gpu.vulkan_device, expected);
            assert_eq!(gpu.cuda_device, None);
            assert_eq!(gpu.use_gpu, expected.is_some());
            assert_eq!(vulkan_refusal(&gpu, &job).is_none(), expected == Some(2));
            if expected == Some(4) {
                assert!(vulkan_refusal(&gpu, &job)
                    .unwrap()
                    .contains("6000 MiB free"));
            }
        }
        // Only the second AMD adapter remains visible. A PCI mapping of the assigned
        // first adapter must not be reinterpreted as an ordinal into this shorter list.
        fake_llama_server(
            &slot,
            "  Vulkan4: AMD Radeon RX 9070 XT (16384 MiB, 15000 MiB free)",
        );
        std::fs::write(slot.join("init_data.xml"), "<app_init_data><plan_class>amd_vulkan_8b</plan_class><gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index></app_init_data>").unwrap();
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        fake_llama_server(&slot, listing);
        for response in ["Vulkan1", "Vulkan0", "Vulkan3", "Vulkan999", "ambiguous"] {
            std::fs::write(&helper, format!("#!/bin/sh\necho {response}\n")).unwrap();
            assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        }
        std::fs::remove_file(&helper).unwrap();
        assert!(!detect_gpu(&slot, &mut host, &mut diag).use_gpu);
        std::fs::write(&helper, "#!/bin/sh\necho Vulkan2\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        for name in [
            "AMD Radeon RX 9070 XT",
            "Radeon RX 9070 XT (RADV NAVI48)",
            "Advanced Micro Devices, Inc.",
            "ATI Radeon",
        ] {
            let line = format!("  Vulkan7: {name} (16384 MiB, 15000 MiB free)");
            assert_eq!(vulkan_device(&line, "amd", 0).unwrap().index, 7);
            assert!(vulkan_device(&line, "amd", 1).is_none());
        }
        for name in [
            "AMD lavapipe",
            "Radeon SwiftShader",
            "Intel Arc",
            "NVIDIA RTX",
        ] {
            let line = format!("  Vulkan0: {name} (16384 MiB, 15000 MiB free)");
            assert!(vulkan_device(&line, "amd", 0).is_none());
        }
        for assignment in [
            "<gpu_type>NVIDIA</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>invalid</gpu_device_num><gpu_opencl_dev_index>0</gpu_opencl_dev_index>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num>",
            "<gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num><gpu_opencl_dev_index>-1</gpu_opencl_dev_index>",
        ] {
            let init = format!(
                "<app_init_data><plan_class>amd_vulkan_8b</plan_class>{assignment}</app_init_data>"
            );
            std::fs::write(slot.join("init_data.xml"), init).unwrap();
            let mut host = Host::default();
            let mut diag = Diag::new();
            boinc_info(&slot, &mut host, &mut diag);
            let gpu = detect_gpu(&slot, &mut host, &mut diag);
            assert!(!gpu.use_gpu);
            assert!(vulkan_refusal(&gpu, &job).is_some());
        }
    }

    #[cfg(unix)]
    #[test]
    fn vulkan_plan_class_uses_the_listed_device_and_refuses_too_little_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_qwen_unit(&slot);
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>intel_vulkan_8b</plan_class><gpu_type>intel_gpu</gpu_type></app_init_data>",
        )
        .unwrap();
        fake_llama_server(
            &slot,
            "  Vulkan0: Intel(R) Iris(R) Xe Graphics (TGL GT2) (7917 MiB, 6000 MiB free)",
        );
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(gpu.use_gpu);
        assert_eq!(gpu.device_label, "vulkan");
        assert_eq!((gpu.vulkan_device, gpu.free_mb), (Some(0), Some(6000)));
        assert_eq!(host.gpu_mem_total_mib, Some(7917));
        let mut big = job.clone();
        big.engine.models[0].min_gpu_mb = 7000;
        assert!(vulkan_refusal(&gpu, &big)
            .unwrap()
            .contains("6000 MiB free"));
        big.engine.models[0].min_gpu_mb = 5000;
        assert!(vulkan_refusal(&gpu, &big).is_none());

        // Only a software device: no GPU, and the unit is refused rather than run on the CPU.
        fake_llama_server(
            &slot,
            "  Vulkan0: llvmpipe (LLVM 19.1.7, 256 bits) (15834 MiB, 15000 MiB free)",
        );
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(!gpu.use_gpu);
        assert!(vulkan_refusal(&gpu, &job)
            .unwrap()
            .contains("no hardware Vulkan device"));
    }

    #[test]
    fn mt_plan_class_forces_cpu_and_2b_plan_class_selects_2b_model() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_qwen_unit(&slot);
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>mt</plan_class><gpu_type>NVIDIA</gpu_type></app_init_data>",
        )
        .unwrap();
        let mut host = Host::default();
        let mut diag = Diag::new();
        boinc_info(&slot, &mut host, &mut diag);
        let gpu = detect_gpu(&slot, &mut host, &mut diag);
        assert!(!gpu.use_gpu);
        assert_eq!(gpu.device_label, "cpu");
        assert!(gpu.cuda_device.is_none());

        // Stage both 8B and 2B models in slot.
        for name in [
            "Qwen3VL-8B-Instruct-Q4_K_M.gguf",
            "Qwen3-VL-2B-Instruct-Q8_0.gguf",
        ] {
            std::fs::write(slot.join(name), FAKE_GGUF).unwrap();
        }
        for name in [
            "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf",
            "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
        ] {
            std::fs::write(slot.join(name), FAKE_MMPROJ).unwrap();
        }

        // Even with 16000 MB free GPU memory, nvidia_cuda_2b selects the 2B model.
        let (chosen_2b, _, _) = resolve_model(
            &slot,
            &job.engine.models,
            Some(16000),
            true,
            Some("nvidia_cuda_2b"),
        )
        .unwrap();
        assert_eq!(chosen_2b.name, "qwen3-vl-2b-q8");

        // mt also selects the 2B model even if free_mb is Some(16000).
        let (chosen_mt, _, _) =
            resolve_model(&slot, &job.engine.models, Some(16000), false, Some("mt")).unwrap();
        assert_eq!(chosen_mt.name, "qwen3-vl-2b-q8");

        for plan in [
            "nvidia_cuda_8b",
            "intel_vulkan_8b",
            "nvidia_vulkan_8b",
            "amd_vulkan_8b",
        ] {
            let (chosen_8b, _, _) =
                resolve_model(&slot, &job.engine.models, Some(16000), true, Some(plan)).unwrap();
            assert_eq!(chosen_8b.name, "qwen3-vl-8b-q4km");
        }
    }

    #[test]
    fn qwen_slot_checkpoints_per_crop_and_resumes_multipage_bundle() {
        use usnm_ocr_contract::{quality::BundleSlice, qwen::BundlePage};

        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        std::fs::create_dir_all(&slot).unwrap();

        let p1 = synthetic_jpeg(200, 200);
        let p2 = synthetic_jpeg(220, 220);
        let mut page_blob = Vec::new();
        let slice1 = BundleSlice::of(0, &p1, "s3://b/p1.jpg");
        page_blob.extend_from_slice(&p1);
        let slice2 = BundleSlice::of(page_blob.len() as u64, &p2, "s3://b/p2.jpg");
        page_blob.extend_from_slice(&p2);

        let weights_lock = test_weights_lock_bytes();
        std::fs::write(slot.join("weights.lock"), &weights_lock).unwrap();
        std::fs::write(slot.join("Qwen3-VL-2B-Instruct-Q8_0.gguf"), FAKE_GGUF).unwrap();
        std::fs::write(
            slot.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            FAKE_MMPROJ,
        )
        .unwrap();

        let models = vec![Model {
            file: "Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            min_gpu_mb: 0,
            mmproj: "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            name: "qwen3-vl-2b-q8".into(),
        }];
        let c1 = Crop {
            rect: [10, 10, 100, 60],
            id: "c01".into(),
            loc_text: "first page crop".into(),
            stratum: "wc-low/top".into(),
        };
        let c2 = Crop {
            rect: [10, 80, 100, 140],
            id: "c01".into(),
            loc_text: "second page crop".into(),
            stratum: "wc-high/bottom".into(),
        };
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 600,
            crops: vec![
                Crop {
                    id: "page_one:c01".into(),
                    ..c1.clone()
                },
                Crop {
                    id: "page_two:c01".into(),
                    ..c2.clone()
                },
            ],
            doc_id: "page_one".into(),
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
                llama: format!("sha256:{}", sha256(EMBEDDED_LLAMA_LOCK)),
                models,
                weights: format!("sha256:{}", sha256(&weights_lock)),
            },
            gpu_timeout_secs: 300,
            group: "pilot".into(),
            input: Input::of(&page_blob, "bundle:page.jpg"),
            language: "eng".into(),
            max_tokens_per_1000px2: 4,
            pages: vec![
                BundlePage {
                    crops: vec![c1],
                    doc_id: "page_one".into(),
                    group: "pilot".into(),
                    input: slice1,
                    language: "eng".into(),
                },
                BundlePage {
                    crops: vec![c2],
                    doc_id: "page_two".into(),
                    group: "pilot".into(),
                    input: slice2,
                    language: "eng".into(),
                },
            ],
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity().unwrap();
        std::fs::write(slot.join(PAGE_FILE), &page_blob).unwrap();
        std::fs::write(
            slot.join(JOB_FILE),
            serde_json::to_vec_pretty(&job).unwrap(),
        )
        .unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>mt</plan_class></app_init_data>",
        )
        .unwrap();

        // Pre-populate a checkpoint with crop 1 of 2 already complete.
        let cp = QwenCheckpoint {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            model: "qwen3-vl-2b-q8".into(),
            device: "cpu".into(),
            readings: vec![Reading {
                id: "page_one:c01".into(),
                ms: 42,
                text: "cached first page crop".into(),
                tokens: 4,
            }],
        };
        cp.save(&slot).unwrap();
        assert!(slot.join(CHECKPOINT_FILE).is_file());

        // Mock server only needs to answer 1 request (the remaining crop 2 of 2).
        let port = spawn_mock_llama_server(vec!["fresh second page crop"]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert!(!slot.join(CHECKPOINT_FILE).exists());
        let result_bytes = std::fs::read(slot.join(RESULT_FILE)).unwrap();
        let readings_bytes = std::fs::read(slot.join(READINGS_FILE)).unwrap();
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let result: ResultManifest = serde_json::from_slice(&result_bytes).unwrap();
        assert_eq!(result.outcome, Outcome::Ok);
        assert_eq!(result.crops, 2);
        assert_eq!(result.read, 2);
        let readings = result.check(&readings_bytes, &diag_bytes).unwrap();
        assert_eq!(readings.readings[0].id, "page_one:c01");
        assert_eq!(readings.readings[0].text, "cached first page crop");
        assert_eq!(readings.readings[1].id, "page_two:c01");
        assert_eq!(readings.readings[1].text, "fresh second page crop");
        let diag_str = String::from_utf8_lossy(&diag_bytes);
        assert!(diag_str.contains("resumed from checkpoint: 1 of 2 crops already complete"));
    }

    #[test]
    fn engine_and_model_lock_verification_rejects_mismatches() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_qwen_unit(&slot);
        prepare(&slot).unwrap();

        // Mismatched job.engine.llama is rejected when USNM_QWEN_LLAMA_IMAGE is unset.
        let mut bad_llama = job.clone();
        bad_llama.engine.llama = format!("sha256:{}", "b".repeat(64));
        bad_llama.id = bad_llama.identity().unwrap();
        std::fs::write(
            slot.join(JOB_FILE),
            serde_json::to_vec_pretty(&bad_llama).unwrap(),
        )
        .unwrap();
        assert!(prepare(&slot).is_err());

        // Mismatched job.engine.weights is rejected when USNM_QWEN_WEIGHTS is unset.
        let mut bad_weights = job.clone();
        bad_weights.engine.weights = format!("sha256:{}", "c".repeat(64));
        bad_weights.id = bad_weights.identity().unwrap();
        std::fs::write(
            slot.join(JOB_FILE),
            serde_json::to_vec_pretty(&bad_weights).unwrap(),
        )
        .unwrap();
        assert!(prepare(&slot).is_err());

        // Restore valid job and verify tampered .gguf file is rejected by resolve_model.
        std::fs::write(
            slot.join(JOB_FILE),
            serde_json::to_vec_pretty(&job).unwrap(),
        )
        .unwrap();
        std::fs::write(
            slot.join("Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            b"tampered-gguf-bytes",
        )
        .unwrap();
        std::fs::write(
            slot.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            FAKE_MMPROJ,
        )
        .unwrap();
        let err = resolve_model(&slot, &job.engine.models, None, false, Some("mt")).unwrap_err();
        assert!(format!("{err:#}").contains("sha256 mismatch"));
    }

    #[test]
    fn boinc_mmap_suspend_and_quit_in_qwen_slot() {
        use usnm_ocr_quality::slot::{
            write_mmap_channel, MMAP_FILE, PROCESS_CONTROL_REQUEST_OFFSET,
        };

        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        write_qwen_unit(&slot);
        let mmap = slot.join(MMAP_FILE);
        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<suspend/>").unwrap();
        let control = start_boinc_control(&slot);
        assert!(control.is_suspended());
        assert_eq!(std::fs::read(&mmap).unwrap()[0], 0);
        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<resume/>").unwrap();
        assert_eq!(
            control.wait_if_suspended_or_should_exit(),
            ControlFlow::Continue(())
        );

        let quit_slot = tmp.path().join("quit_slot");
        write_qwen_unit(&quit_slot);
        let quit_mmap = quit_slot.join(MMAP_FILE);
        write_mmap_channel(&quit_mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        run_slot_with(&quit_slot, None, Some(Duration::from_secs(5))).unwrap();
        assert!(!quit_slot.join(FINISH_FILE).exists());
        assert_eq!(std::fs::read(&quit_mmap).unwrap()[0], 0);
    }
}
