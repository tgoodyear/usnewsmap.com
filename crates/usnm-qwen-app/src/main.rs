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

mod boinc;
mod checkpoint;
mod gpu;
mod locks;
mod report;
mod server;
#[cfg(test)]
mod test_support;

use crate::{
    boinc::{
        boinc_info, postpone_probe_timeout, runtime_info, start_boinc_control, PROBE_POSTPONE_SECS,
        TEMPORARY_EXIT_FILE,
    },
    checkpoint::{QwenCheckpoint, CHECKPOINT_FILE},
    gpu::{detect_gpu, gpu_used_mib, vulkan_refusal, GpuState},
    locks::{check_engine, resolve_model},
    report::{diag_block, failure_report, redacted_tail, report_failure},
    server::{chat_completion, find_llama_server, Server, CTX_SIZE, LOAD_DEADLINE},
};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeMap,
    ops::ControlFlow,
    path::Path,
    time::{Duration, Instant},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{MAX_BUNDLE_SCAN, MAX_SCAN},
    qwen::{
        Crop, Engine, Job, Outcome, Reading, Readings, ResultManifest, MAX_ERROR, MAX_JOB_JSON,
        PROMPT_VERSION, VERSION,
    },
    surya::Host,
};
use usnm_ocr_quality::{
    qwen_pages::QwenPages,
    slot::{
        read_bounded, resolve_slot_file, tail, write_atomic, BoincControl, Diag, DIAG_FILE,
        FINISH_FILE, FRACTION_FILE, PAGE_FILE, RESULT_FILE,
    },
};

pub const JOB_FILE: &str = "qwen.json";
pub const READINGS_FILE: &str = "readings.json";

fn main() -> anyhow::Result<()> {
    if run_slot_reporting(Path::new(".")).is_err() {
        std::process::exit(1);
    }
    if std::fs::read_to_string(FINISH_FILE).is_ok_and(|code| code.trim() != "0") {
        std::process::exit(1);
    }
    Ok(())
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

pub(crate) fn ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub struct Loaded {
    pub job: Job,
    /// `page.jpg` as read: one page, or a bundle's pages end to end.
    pub page_bytes: Vec<u8>,
    /// The decoded page, one at a time in a bundle (#794).
    pub pages: QwenPages,
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
    let pages = QwenPages::open(&job, &page_bytes)?;
    Ok(Loaded {
        job,
        page_bytes,
        pages,
    })
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
    let temporary_exit_out = work.join(TEMPORARY_EXIT_FILE);

    for p in [
        &readings_out,
        &result_out,
        &diag_out,
        &fraction_out,
        &finish_out,
        &temporary_exit_out,
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
    if gpu.probe_timed_out {
        // A slow cold driver load is not a missing GPU: retry it a bounded number of times.
        match postpone_probe_timeout(work)? {
            Some(attempt) => {
                diag.section(
                    "boinc control",
                    &format!(
                        "GPU device probe timed out; postponed {PROBE_POSTPONE_SECS} s \
                         (attempt {attempt}), exiting without boinc_finish_called"
                    ),
                    256,
                );
                write_atomic(&diag_out, &diag.finish())?;
                return Ok(());
            }
            None => diag.section(
                "gpu",
                "device probe timed out on every allowed attempt",
                256,
            ),
        }
    }

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
        Ok(mut l) => {
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
                    &mut l,
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
    l: &mut Loaded,
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
        let png = match l
            .pages
            .page_for(&l.job, &l.page_bytes, crop)
            .and_then(|page| crop_png(page, crop))
        {
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
    use crate::locks::EMBEDDED_LLAMA_LOCK;
    #[cfg(windows)]
    use crate::report::DIAG_EXCERPT_BYTES;
    use crate::test_support::{
        synthetic_jpeg, test_weights_lock_bytes, write_qwen_unit, FAKE_GGUF, FAKE_MMPROJ,
    };
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use usnm_ocr_contract::{quality::Input, qwen::Model};

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
