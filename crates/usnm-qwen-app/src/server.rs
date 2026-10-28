//! The `llama-server` process and the HTTP it speaks: locating the binary, starting it on a
//! free local port, and asking it for a reading of each crop.
use crate::{gpu::GpuState, ms, report::redacted_tail};
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use std::{
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    ops::ControlFlow,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::qwen::{MAX_CROP_TEXT, PROMPT};
use usnm_ocr_quality::slot::{resolve_slot_file, BoincControl};

pub const ENV_LLAMA_SERVER: &str = "USNM_LLAMA_SERVER";

pub(crate) const CTX_SIZE: u32 = 4096;
pub(crate) const LOAD_DEADLINE: Duration = Duration::from_secs(300);

/// The library search variable and value that let a bundled `llama-server` find its libraries.
pub(crate) fn library_env(server_bin: &Path) -> Option<(&'static str, std::ffi::OsString)> {
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
) -> anyhow::Result<(String, u32, String)> {
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
    parse_completion(&resp, max_tokens)
}

fn parse_completion(resp: &[u8], max_tokens: u32) -> anyhow::Result<(String, u32, String)> {
    let v: serde_json::Value = serde_json::from_slice(resp).context("chat response JSON")?;
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .context("chat response has no choices[0].message.content")?;
    let tokens_reported = v
        .pointer("/usage/completion_tokens")
        .and_then(|t| t.as_u64())
        .and_then(|t| u32::try_from(t).ok());
    let tokens = tokens_reported.unwrap_or(0);
    let cleaned = clean_reading(text);
    let finish_reason_raw = v.pointer("/choices/0/finish_reason");
    let finish_reason = finish_reason_raw
        .and_then(|v| v.as_str())
        .filter(|s| s.len() <= 128);
    let mut flags = Vec::new();
    if finish_reason_raw.is_some_and(|v| !v.is_null()) && finish_reason.is_none() {
        flags.push("invalid_or_oversized_finish_reason");
    }
    if tokens >= max_tokens || finish_reason == Some("length") {
        flags.push("completion_token_budget_exhausted");
    }
    if cleaned.is_empty() {
        flags.push("empty_reading");
    } else if cleaned.replace("[?]", "").trim().is_empty() {
        flags.push("uncertainty_only");
    }
    let mut previous = None;
    let mut run = 0;
    let mut longest_run = 0;
    for token in cleaned.split_whitespace() {
        run = if previous == Some(token) { run + 1 } else { 1 };
        longest_run = longest_run.max(run);
        previous = Some(token);
    }
    let duplicate_lines = cleaned
        .lines()
        .zip(cleaned.lines().skip(1))
        .any(|(a, b)| !a.trim().is_empty() && a.trim() == b.trim());
    if duplicate_lines {
        flags.push("consecutive_duplicate_lines");
    }
    let metadata = serde_json::to_string(&serde_json::json!({
        "schema": 1,
        "finish_reason": finish_reason,
        "completion_tokens_reported": tokens_reported,
        "max_tokens_requested": max_tokens,
        "stop_strings_requested": [],
        "repetition_penalty_requested": null,
        "response_sha256": usnm_ocr_contract::boinc::sha256(resp),
        "response_bytes": resp.len(),
        "raw_content_sha256": usnm_ocr_contract::boinc::sha256(text.as_bytes()),
        "raw_content_bytes": text.len(),
        "cleaning_changed_content": text != cleaned,
        "longest_identical_token_run": longest_run,
        "generation_flags": flags,
        "scientific_acceptance": "not_established_by_generation_metadata",
    }))?;
    Ok((cleaned, tokens, metadata))
}

pub(crate) struct Server {
    child: Child,
    pub(crate) port: u16,
    pub(crate) log_path: PathBuf,
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
    pub(crate) fn start(
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

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn preserves_length_stop_and_labels_repetition_without_rewriting_reading() {
        let response = br#"{"choices":[{"message":{"content":"word\nword\nword"},"finish_reason":"length"}],"usage":{"completion_tokens":12}}"#;
        let (text, tokens, metadata) = parse_completion(response, 12).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(text, "word\nword\nword");
        assert_eq!(tokens, 12);
        assert_eq!(m["finish_reason"], "length");
        assert_eq!(m["longest_identical_token_run"], 3);
        assert_eq!(
            m["response_sha256"],
            usnm_ocr_contract::boinc::sha256(response)
        );
        assert_eq!(
            m["generation_flags"],
            serde_json::json!([
                "completion_token_budget_exhausted",
                "consecutive_duplicate_lines"
            ])
        );
    }

    #[test]
    fn missing_finish_or_usage_stays_unknown_not_successful_stop() {
        let (text, tokens, metadata) =
            parse_completion(br#"{"choices":[{"message":{"content":" [?] [?] "}}]}"#, 20).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(text, "[?] [?]");
        assert_eq!(tokens, 0);
        assert!(m["finish_reason"].is_null());
        assert!(m["completion_tokens_reported"].is_null());
        assert_eq!(
            m["generation_flags"],
            serde_json::json!(["uncertainty_only"])
        );
        assert_eq!(m["cleaning_changed_content"], true);
    }

    #[test]
    fn unexpected_metadata_is_bounded_and_not_a_successful_stop() {
        let response = serde_json::to_vec(&serde_json::json!({
            "choices": [{"message": {"content": "kept"}, "finish_reason": "x".repeat(8192)}],
            "usage": {"completion_tokens": "x".repeat(8192)}
        }))
        .unwrap();
        let (text, _, metadata) = parse_completion(&response, 20).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(text, "kept");
        assert!(metadata.len() < 2048);
        assert!(m["finish_reason"].is_null());
        assert!(m["completion_tokens_reported"].is_null());
        assert_eq!(
            m["generation_flags"],
            serde_json::json!(["invalid_or_oversized_finish_reason"])
        );
        assert_eq!(
            m["response_sha256"],
            usnm_ocr_contract::boinc::sha256(&response)
        );
    }
}
