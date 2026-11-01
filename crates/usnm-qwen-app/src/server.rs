//! The `llama-server` process and the HTTP it speaks: locating the binary, starting it on a
//! free local port, and asking it for a reading of each crop.
use crate::{
    gpu::{GpuState, NVIDIA_6GB_LAYERS},
    ms,
    report::redacted_tail,
};
use anyhow::{bail, ensure, Context};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest as _, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write as _},
    net::{TcpListener, TcpStream},
    ops::ControlFlow,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use usnm_ocr_contract::qwen::{loop_cut, tail_repetition, MAX_CROP_TEXT, PROMPT};
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
    image_min_tokens: Option<u32>,
    guard: bool,
    timeout: Duration,
) -> anyhow::Result<(String, u32, String)> {
    let url = format!("data:image/png;base64,{}", BASE64.encode(png));
    let mut request = serde_json::json!({
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
    });
    if guard {
        request["stream"] = true.into();
        let body = serde_json::to_vec(&request)?;
        return guarded_completion(port, &body, max_tokens, image_min_tokens, timeout);
    }
    let body = serde_json::to_vec(&request)?;
    let (status, resp) = http_request(port, "POST", "/v1/chat/completions", Some(&body), timeout)?;
    if status != 200 {
        bail!(
            "llama-server status {status}: {}",
            redacted_tail(&String::from_utf8_lossy(&resp), 256)
        );
    }
    parse_completion(&resp, max_tokens, image_min_tokens)
}

/// The response body of a chunked reply, as a plain stream.
struct Dechunk<R> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Dechunk<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            // The CRLF that ends the previous chunk comes first, except before the first.
            while line.trim().is_empty() {
                line.clear();
                if self.inner.read_line(&mut line)? == 0 {
                    return Err(std::io::ErrorKind::UnexpectedEof.into());
                }
            }
            let size = line.trim().split(';').next().unwrap_or("");
            self.left = usize::from_str_radix(size, 16)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            if self.left == 0 {
                self.done = true;
                return Ok(0);
            }
        }
        let want = buf.len().min(self.left);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        self.left -= n;
        Ok(n)
    }
}

/// What a streamed completion delivered before it ended or was stopped.
#[derive(Debug, Default)]
struct Streamed {
    raw: String,
    finish_reason: Option<String>,
    finish_invalid: bool,
    tokens_reported: Option<u32>,
    deltas: u32,
    stopped: bool,
    digest: Sha256,
    bytes: usize,
}

/// The most a guarded stream may deliver; `http_request`'s limit for a whole reply.
const MAX_STREAM: usize = 1024 * 1024;

/// Reads server-sent events until the reply ends or the reading starts to
/// repeat, checked every eight tokens from the twelfth, as Study 5 did.
fn read_stream(mut events: impl BufRead, deadline: Instant) -> anyhow::Result<Streamed> {
    let mut out = Streamed::default();
    let mut line = String::new();
    loop {
        ensure!(Instant::now() < deadline, "completion deadline reached");
        line.clear();
        if events.read_line(&mut line)? == 0 {
            return Ok(out);
        }
        out.digest.update(line.as_bytes());
        out.bytes += line.len();
        ensure!(out.bytes <= MAX_STREAM, "completion stream over size limit");
        let Some(data) = line.trim().strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            return Ok(out);
        }
        let v: serde_json::Value = serde_json::from_str(data).context("stream event JSON")?;
        if let Some(e) = v.get("error") {
            bail!(
                "llama-server stream error: {}",
                redacted_tail(&e.to_string(), 256)
            );
        }
        if let Some(t) = v
            .pointer("/usage/completion_tokens")
            .and_then(|t| t.as_u64())
            .and_then(|t| u32::try_from(t).ok())
        {
            out.tokens_reported = Some(t);
        }
        match v.pointer("/choices/0/finish_reason") {
            None | Some(serde_json::Value::Null) => {}
            Some(f) => match f.as_str().filter(|s| s.len() <= 128) {
                Some(f) => out.finish_reason = Some(f.to_owned()),
                None => out.finish_invalid = true,
            },
        }
        if let Some(delta) = v
            .pointer("/choices/0/delta/content")
            .and_then(|c| c.as_str())
        {
            if !delta.is_empty() {
                out.raw.push_str(delta);
                out.deltas += 1;
                if out.deltas >= 12 && out.deltas % 8 == 0 && tail_repetition(&out.raw).is_some() {
                    out.stopped = true;
                    return Ok(out);
                }
            }
        }
    }
}

/// One crop's reading through a streamed request. Closing the connection
/// is what stops the server's generation, so a looping reading costs only
/// the tokens up to the check that caught it.
fn guarded_completion(
    port: u16,
    body: &[u8],
    max_tokens: u32,
    image_min_tokens: Option<u32>,
    timeout: Duration,
) -> anyhow::Result<(String, u32, String)> {
    let deadline = Instant::now() + timeout;
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(timeout))?;
    s.set_write_timeout(Some(timeout))?;
    write!(
        s,
        "POST /v1/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )?;
    s.write_all(body)?;
    let mut reader = BufReader::new(s);
    let mut head = String::new();
    reader.read_line(&mut head)?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .context("HTTP status code")?;
    let mut chunked = false;
    loop {
        let mut h = String::new();
        ensure!(reader.read_line(&mut h)? > 0, "malformed HTTP response");
        if h.trim().is_empty() {
            break;
        }
        chunked |= h
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked");
    }
    let got = if chunked {
        let plain = Dechunk {
            inner: reader,
            left: 0,
            done: false,
        };
        if status != 200 {
            return reply_error(status, plain);
        }
        read_stream(BufReader::new(plain), deadline)?
    } else {
        if status != 200 {
            return reply_error(status, reader);
        }
        read_stream(reader, deadline)?
    };
    let sha = hex_digest(&got.digest);
    let invalid = got.finish_invalid;
    generation_record(
        &got.raw,
        Generation {
            finish_reason: got.finish_reason.as_deref(),
            finish_invalid: invalid,
            tokens_reported: got.tokens_reported.or(Some(got.deltas)),
            max_tokens,
            image_min_tokens,
            response_sha256: sha,
            response_bytes: got.bytes,
            guard: Some(got.stopped),
        },
    )
}

fn reply_error<T>(status: u16, body: impl Read) -> anyhow::Result<T> {
    let mut text = Vec::new();
    body.take(64 * 1024).read_to_end(&mut text)?;
    bail!(
        "llama-server status {status}: {}",
        redacted_tail(&String::from_utf8_lossy(&text), 256)
    )
}

fn hex_digest(digest: &Sha256) -> String {
    digest
        .clone()
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn parse_completion(
    resp: &[u8],
    max_tokens: u32,
    image_min_tokens: Option<u32>,
) -> anyhow::Result<(String, u32, String)> {
    let v: serde_json::Value = serde_json::from_slice(resp).context("chat response JSON")?;
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .context("chat response has no choices[0].message.content")?;
    let tokens_reported = v
        .pointer("/usage/completion_tokens")
        .and_then(|t| t.as_u64())
        .and_then(|t| u32::try_from(t).ok());
    let finish_reason_raw = v.pointer("/choices/0/finish_reason");
    let finish_reason = finish_reason_raw
        .and_then(|v| v.as_str())
        .filter(|s| s.len() <= 128);
    generation_record(
        text,
        Generation {
            finish_reason,
            finish_invalid: finish_reason_raw.is_some_and(|v| !v.is_null())
                && finish_reason.is_none(),
            tokens_reported,
            max_tokens,
            image_min_tokens,
            response_sha256: usnm_ocr_contract::boinc::sha256(resp),
            response_bytes: resp.len(),
            guard: None,
        },
    )
}

/// How one completion ended, for its generation record.
struct Generation<'a> {
    finish_reason: Option<&'a str>,
    finish_invalid: bool,
    tokens_reported: Option<u32>,
    max_tokens: u32,
    image_min_tokens: Option<u32>,
    response_sha256: String,
    response_bytes: usize,
    /// Under the loop guard, whether it stopped the generation.
    guard: Option<bool>,
}

/// The reading and its generation record. Without the guard the reading is
/// the raw text as read; with it, the raw text cut by the `stop` rule.
fn generation_record(text: &str, g: Generation<'_>) -> anyhow::Result<(String, u32, String)> {
    let tokens = g.tokens_reported.unwrap_or(0);
    let cut = g.guard.and_then(|_| loop_cut(text));
    let cleaned = clean_reading(cut.as_deref().unwrap_or(text));
    let mut flags = Vec::new();
    if g.finish_invalid {
        flags.push("invalid_or_oversized_finish_reason");
    }
    let budget_exhausted = tokens >= g.max_tokens || g.finish_reason == Some("length");
    if budget_exhausted {
        flags.push("completion_token_budget_exhausted");
    }
    let tail = tail_repetition(text);
    if tail.is_some() {
        flags.push("tail_periodic_repetition");
        if budget_exhausted {
            flags.push("repetition_loop_at_token_budget");
        }
    }
    if cut.is_some() {
        flags.push("repetition_loop_guard_cut");
    }
    if cleaned.is_empty() {
        flags.push("empty_reading");
    } else if cleaned.replace("[?]", "").trim().is_empty() {
        flags.push("uncertainty_only");
    }
    let mut previous = None;
    let mut run = 0;
    let mut longest_run = 0;
    for token in text.split_whitespace() {
        run = if previous == Some(token) { run + 1 } else { 1 };
        longest_run = longest_run.max(run);
        previous = Some(token);
    }
    let duplicate_lines = text
        .lines()
        .zip(text.lines().skip(1))
        .any(|(a, b)| !a.trim().is_empty() && a.trim() == b.trim());
    if duplicate_lines {
        flags.push("consecutive_duplicate_lines");
    }
    let mut record = serde_json::json!({
        "schema": 2,
        "finish_reason": g.finish_reason,
        "completion_tokens_reported": g.tokens_reported,
        "max_tokens_requested": g.max_tokens,
        "stop_strings_requested": [],
        "repetition_penalty_requested": null,
        "image_min_tokens_requested": g.image_min_tokens,
        "response_sha256": g.response_sha256,
        "response_bytes": g.response_bytes,
        "raw_content_sha256": usnm_ocr_contract::boinc::sha256(text.as_bytes()),
        "raw_content_bytes": text.len(),
        "cleaning_changed_content": text != cleaned,
        "longest_identical_token_run": longest_run,
        "repetition_scope": "raw_content_before_cleaning",
        "tail_repetition": tail.map(|(period, span)| serde_json::json!({"period_chars": period, "span_chars": span})),
        "generation_flags": flags,
        "scientific_acceptance": "not_established_by_generation_metadata",
    });
    if let Some(stopped) = g.guard {
        record["schema"] = 3.into();
        record["loop_guard"] = serde_json::json!({"rule": "stop", "stopped_generation": stopped});
    }
    Ok((cleaned, tokens, serde_json::to_string(&record)?))
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
        image_min_tokens: Option<u32>,
        control: &BoincControl,
    ) -> anyhow::Result<(Self, u64)> {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            bail!("boinc control requested exit ({reason})");
        }
        let port = free_port()?;
        let log_path = work.join("llama-server.log");
        let log = std::fs::File::create(&log_path)?;
        let log_err = log.try_clone()?;
        ensure!(
            !gpu.partial_offload || (gpu.use_gpu && gpu.vulkan_device.is_some()),
            "NVIDIA 6 GB offload requires an assigned hardware Vulkan GPU"
        );
        let ngl = match (gpu.use_gpu, gpu.partial_offload) {
            (true, true) => NVIDIA_6GB_LAYERS,
            (true, false) => 99,
            (false, _) => 0,
        };
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
            &ngl.to_string(),
            "-t",
            &threads.max(1).to_string(),
        ]);
        if let Some(n) = image_min_tokens {
            cmd.args(["--image-min-tokens", &n.to_string()]);
        }
        if gpu.partial_offload {
            cmd.arg("--no-mmproj-offload");
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));

        if let Some(dev) = gpu.cuda_device {
            cmd.env("CUDA_VISIBLE_DEVICES", dev.to_string());
        }
        usnm_ocr_quality::slot::apply_gpu_cache_env(&mut cmd, work);
        // --device takes the names --list-devices printed. GGML_VK_VISIBLE_DEVICES would index
        // the unfiltered physical-device list instead, which can include llvmpipe.
        // GPU projectors select their backend separately; the partial profile keeps it on CPU.
        if let Some(dev) = gpu.vulkan_device {
            let name = format!("Vulkan{dev}");
            cmd.args(["--device", &name]);
            if gpu.partial_offload {
                cmd.env_remove("MTMD_BACKEND_DEVICE");
            } else {
                cmd.env("MTMD_BACKEND_DEVICE", &name);
            }
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
mod guard_tests {
    use super::*;
    use std::io::Cursor;

    fn event(delta: &str) -> String {
        let v =
            serde_json::json!({"choices": [{"delta": {"content": delta}, "finish_reason": null}]});
        format!("data: {v}\n\n")
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn chunked(body: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for piece in body.as_bytes().chunks(17) {
            out.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            out.extend_from_slice(piece);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
        out
    }

    #[test]
    fn a_natural_stop_is_kept_whole() {
        let mut body = ["The ship ", "sailed ", "home."].map(event).concat();
        let end = serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}], "usage": {"completion_tokens": 3}});
        body.push_str(&format!("data: {end}\n\ndata: [DONE]\n\n"));
        let got = read_stream(Cursor::new(body), far()).unwrap();
        assert!(!got.stopped);
        assert_eq!(got.raw, "The ship sailed home.");
        assert_eq!(got.finish_reason.as_deref(), Some("stop"));
        assert_eq!(got.tokens_reported, Some(3));
    }

    #[test]
    fn a_repeating_reading_stops_at_the_first_check_that_sees_it() {
        let body = std::iter::once(event("Head line. "))
            .chain((0..500).map(|_| event("[?] ")))
            .collect::<String>();
        let got = read_stream(Cursor::new(body), far()).unwrap();
        assert!(got.stopped);
        assert_eq!(got.deltas, 16);
        assert!(got.finish_reason.is_none());
        let (text, tokens, metadata) = generation_record(
            &got.raw,
            Generation {
                finish_reason: None,
                finish_invalid: false,
                tokens_reported: Some(got.deltas),
                max_tokens: 1304,
                image_min_tokens: None,
                response_sha256: hex_digest(&got.digest),
                response_bytes: got.bytes,
                guard: Some(true),
            },
        )
        .unwrap();
        assert_eq!(text, "Head line. [?]");
        assert_eq!(tokens, 16);
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(m["schema"], 3);
        assert_eq!(m["loop_guard"]["stopped_generation"], true);
        assert!(m["generation_flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "repetition_loop_guard_cut"));
    }

    #[test]
    fn a_repetition_too_short_for_a_check_is_still_cut() {
        let mut body = (0..11).map(|_| event("0123456789 ")).collect::<String>();
        body.push_str("data: [DONE]\n\n");
        let got = read_stream(Cursor::new(body), far()).unwrap();
        assert!(!got.stopped);
        let (text, ..) = generation_record(
            &got.raw,
            Generation {
                finish_reason: Some("stop"),
                finish_invalid: false,
                tokens_reported: Some(got.deltas),
                max_tokens: 1304,
                image_min_tokens: None,
                response_sha256: hex_digest(&got.digest),
                response_bytes: got.bytes,
                guard: Some(false),
            },
        )
        .unwrap();
        assert_eq!(text, "0123456789");
    }

    #[test]
    fn stream_errors_and_the_deadline_end_the_read() {
        let e = read_stream(Cursor::new("data: {\"error\": {\"code\": 500}}\n\n"), far());
        assert!(format!("{:#}", e.unwrap_err()).contains("stream error"));
        let past = Instant::now() - Duration::from_secs(1);
        assert!(read_stream(Cursor::new(event("x")), past).is_err());
    }

    #[test]
    fn chunked_replies_read_as_a_plain_stream() {
        let body = event("one ") + &event("two") + "data: [DONE]\n\n";
        let wire = chunked(&body);
        let plain = Dechunk {
            inner: Cursor::new(wire),
            left: 0,
            done: false,
        };
        let got = read_stream(BufReader::new(plain), far()).unwrap();
        assert_eq!(got.raw, "one two");
    }

    #[test]
    fn the_connection_closes_when_the_guard_stops_the_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut seen = Vec::new();
            let mut buf = [0u8; 4096];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut buf).unwrap();
                seen.extend_from_slice(&buf[..n]);
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .unwrap();
            // A generation that goes on until the client hangs up.
            for _ in 0..100_000 {
                if s.write_all(
                    &chunked(&event("[?] "))[..]
                        .strip_suffix(b"0\r\n\r\n")
                        .map(<[u8]>::to_vec)
                        .unwrap(),
                )
                .is_err()
                {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        });
        let (text, tokens, _) =
            chat_completion(port, b"png", 4096, None, true, Duration::from_secs(30)).unwrap();
        assert_eq!(text, "[?]");
        assert!(tokens < 64, "stopped early, after {tokens} tokens");
        assert!(server.join().unwrap(), "the client hung up");
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn server_launch_uses_partial_layers_and_cpu_projector_only_for_the_explicit_profile() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path();
        let bin = work.join("llama-server");
        std::fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.args\"\n\
             printf '%s' \"${MTMD_BACKEND_DEVICE-unset}\" > \"$0.device\"\nexit 23\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let control = BoincControl::start(work);
        for partial in [false, true] {
            let gpu = GpuState {
                cuda_device: None,
                vulkan_device: Some(3),
                free_mb: Some(5600),
                used_before_mb: None,
                use_gpu: true,
                partial_offload: partial,
                device_label: "vulkan".into(),
                probe_timed_out: false,
            };
            let result = Server::start(
                work,
                &bin,
                &work.join("model.gguf"),
                &work.join("mmproj.gguf"),
                &gpu,
                2,
                Duration::from_secs(2),
                None,
                &control,
            );
            let error = match result {
                Ok(_) => panic!("fixture server must exit before becoming ready"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("before ready"));
            let text = std::fs::read_to_string(bin.with_extension("args")).unwrap();
            let args: Vec<_> = text.lines().collect();
            let value = |name| args[args.iter().position(|arg| *arg == name).unwrap() + 1];
            assert_eq!(value("-ngl"), if partial { "24" } else { "99" });
            assert_eq!(value("--device"), "Vulkan3");
            assert_eq!(args.contains(&"--no-mmproj-offload"), partial);
            assert_eq!(
                std::fs::read_to_string(bin.with_extension("device")).unwrap(),
                if partial { "unset" } else { "Vulkan3" }
            );
        }
    }

    #[test]
    fn preserves_length_stop_and_labels_repetition_without_rewriting_reading() {
        let response = br#"{"choices":[{"message":{"content":"word\nword\nword"},"finish_reason":"length"}],"usage":{"completion_tokens":12}}"#;
        let (text, tokens, metadata) = parse_completion(response, 20, None).unwrap();
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
        let (text, tokens, metadata) = parse_completion(
            br#"{"choices":[{"message":{"content":" [?] [?] "}}]}"#,
            20,
            None,
        )
        .unwrap();
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
        let (text, _, metadata) = parse_completion(&response, 20, None).unwrap();
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

    #[test]
    fn repetition_after_reading_size_limit_is_still_diagnosed() {
        let prefix: String = (0..2000).map(|i| format!("word{i} ")).collect();
        assert!(prefix.len() > MAX_CROP_TEXT);
        let raw = format!("{prefix}\n{}", "tailword\n".repeat(50));
        let response = serde_json::to_vec(&serde_json::json!({
            "choices": [{"message": {"content": raw}, "finish_reason": "stop"}]
        }))
        .unwrap();
        let (reading, _, metadata) = parse_completion(&response, 20000, None).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(reading, clean_reading(&raw));
        assert!(!reading.contains("tailword"));
        assert_eq!(m["longest_identical_token_run"], 50);
        assert_eq!(m["repetition_scope"], "raw_content_before_cleaning");
        assert_eq!(
            m["generation_flags"],
            serde_json::json!(["tail_periodic_repetition", "consecutive_duplicate_lines"])
        );
        assert_eq!(m["raw_content_bytes"], raw.len());
        assert_eq!(m["cleaning_changed_content"], true);
    }

    fn completion(content: &str, finish: &str, tokens: u32) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "choices": [{"message": {"content": content}, "finish_reason": finish}],
            "usage": {"completion_tokens": tokens}
        }))
        .unwrap()
    }

    #[test]
    fn loop_to_the_token_budget_is_labelled_and_kept_verbatim() {
        // The 1.14 NVIDIA reading of a clean Yiddish crop (#529 benchmark, job 87a106ca…, c10).
        let raw = format!("שְׁפָרִים,{}", " אֵיזֶר עַרְבְּעַיִם,".repeat(33));
        let (reading, _, metadata) =
            parse_completion(&completion(&raw, "length", 844), 844, None).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(reading, raw.trim());
        assert_eq!(
            m["generation_flags"],
            serde_json::json!([
                "completion_token_budget_exhausted",
                "tail_periodic_repetition",
                "repetition_loop_at_token_budget"
            ])
        );
        assert_eq!(m["tail_repetition"]["period_chars"], 21);
        assert!(m["image_min_tokens_requested"].is_null());
    }

    #[test]
    fn repeated_print_that_stops_is_not_a_loop() {
        // A shelf of book spines, read to a natural stop.
        let raw = "SHAKESPEARE'S\nWORKS\n".repeat(6);
        let (_, _, metadata) =
            parse_completion(&completion(&raw, "stop", 80), 1474, Some(1024)).unwrap();
        let m: serde_json::Value = serde_json::from_str(&metadata).unwrap();
        assert_eq!(
            m["generation_flags"],
            serde_json::json!(["tail_periodic_repetition"])
        );
        assert_eq!(m["image_min_tokens_requested"], 1024);
    }
}
