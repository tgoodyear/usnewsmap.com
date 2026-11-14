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
mod retry;
mod server;
#[cfg(all(test, unix))]
mod stub_tests;
#[cfg(test)]
mod test_support;
mod verified;

#[cfg(test)]
use crate::locks::resolve_model;
use crate::{
    boinc::{
        boinc_info, postpone_probe_timeout, runtime_info, start_boinc_control, PROBE_POSTPONE_SECS,
        TEMPORARY_EXIT_FILE,
    },
    checkpoint::{QwenCheckpoint, CHECKPOINT_FILE},
    gpu::{detect_gpu, gpu_used_mib, vulkan_refusal, GpuState},
    locks::{check_engine, resolve_model_logged},
    report::{diag_block, failure_report, redacted_tail, report_failure},
    retry::{run_crop, write_temporary_exit, CropEnd, MAX_TEMPORARY_EXITS},
    server::{chat_completion_for, find_llama_server, Launch, Llama, LOAD_DEADLINE},
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
        context_size, image_min_tokens, loop_guard, Crop, Engine, Job, Outcome, Reading, Readings,
        ResultManifest, MAX_ERROR, MAX_JOB_JSON, PROMPT_VERSION, SUPPORTED_PROMPT_VERSIONS,
        VERSION,
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

/// `items` comma-joined when that fits in `max` bytes, else their count.
fn bounded_list(items: &[String], max: usize) -> String {
    let all = items.join(",");
    if all.len() <= max {
        all
    } else {
        format!(
            "{} crops; see the generation_v1 sections of diag.txt",
            items.len()
        )
    }
}

fn empty_result(
    job_id: String,
    engine: Engine,
    crops: u32,
    outcome: Outcome,
    error: Option<String>,
    prompt_version: u32,
) -> (ResultManifest, Readings) {
    let readings = Readings {
        v: VERSION,
        job_id: job_id.clone(),
        device: "none".into(),
        diag: BTreeMap::new(),
        model: None,
        outcome,
        prompt_version,
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
                    l.job.prompt_version,
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
                    l.job.prompt_version,
                )
            } else {
                let deadline = deadline_override.unwrap_or_else(|| {
                    Duration::from_secs(u64::from(if gpu.use_gpu {
                        l.job.gpu_timeout_secs
                    } else {
                        l.job.cpu_timeout_secs
                    }))
                });
                match run_crops(
                    work,
                    &mut l,
                    &host,
                    &gpu,
                    existing_port,
                    deadline,
                    &control,
                    &mut diag,
                ) {
                    Some(done) => done,
                    None => {
                        // The crop kept failing and the client will restart the task, which
                        // resumes from the checkpoint. Like a postponed probe: no result, no
                        // boinc_finish_called.
                        write_atomic(&diag_out, &diag.finish())?;
                        return Ok(());
                    }
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
            let prompt_version = raw
                .as_ref()
                .and_then(|j| u32::try_from(j.get("prompt_version")?.as_u64()?).ok())
                .filter(|v| SUPPORTED_PROMPT_VERSIONS.contains(v))
                .unwrap_or(PROMPT_VERSION);
            let crops = raw
                .and_then(|j| u32::try_from(j.get("crops")?.as_array()?.len()).ok())
                .unwrap_or(1);
            empty_result(
                id,
                engine,
                crops,
                Outcome::UnitError,
                Some(tail(&msg, MAX_ERROR).to_owned()),
                prompt_version,
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
) -> Option<(ResultManifest, Readings)> {
    if control.wait_if_suspended_or_should_exit().is_break() {
        return Some(empty_result(
            l.job.id.clone(),
            l.job.engine.clone(),
            l.job.crops.len() as u32,
            Outcome::Failed,
            None,
            l.job.prompt_version,
        ));
    }

    let mut checks = Vec::new();
    let resolved = resolve_model_logged(
        work,
        &l.job.engine.models,
        gpu.free_mb,
        gpu.use_gpu,
        host.plan_class.as_deref(),
        l.job.model.as_deref(),
        &mut checks,
    );
    if !checks.is_empty() {
        // Logical names from weights.lock only, never paths (#1722).
        let lines: Vec<String> = checks.iter().map(ToString::to_string).collect();
        diag.section("weights check", &lines.join("\n"), 512);
    }
    let (model, model_path, mmproj_path) = match resolved {
        Ok(m) => m,
        Err(e) => {
            diag.section("model resolution failed", &format!("{e:#}"), 1024);
            return Some(empty_result(
                l.job.id.clone(),
                l.job.engine.clone(),
                l.job.crops.len() as u32,
                Outcome::Failed,
                None,
                l.job.prompt_version,
            ));
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

    let launch = Launch {
        work,
        model_path: &model_path,
        mmproj_path: &mmproj_path,
        gpu,
        threads,
        image_min_tokens: image_min_tokens(l.job.prompt_version),
        ctx_size: context_size(l.job.prompt_version),
        control,
    };
    let mut load_ms = 0;
    let mut llama = match existing_port {
        Some(p) => Llama {
            srv: None,
            port: p,
            server_bin: None,
        },
        None => {
            let server_bin = match find_llama_server(work) {
                Ok(b) => b,
                Err(e) => {
                    diag.section("llama-server missing", &format!("{e:#}"), 1024);
                    return Some(empty_result(
                        l.job.id.clone(),
                        l.job.engine.clone(),
                        l.job.crops.len() as u32,
                        Outcome::Failed,
                        None,
                        l.job.prompt_version,
                    ));
                }
            };
            let mut llama = Llama {
                srv: None,
                port: 0,
                server_bin: Some(server_bin),
            };
            match llama.start(&launch, LOAD_DEADLINE.min(deadline)) {
                Ok(ms_loaded) => load_ms = ms_loaded,
                Err(e) => {
                    diag.section("llama-server start failed", &format!("{e:#}"), 2048);
                    return Some(empty_result(
                        l.job.id.clone(),
                        l.job.engine.clone(),
                        l.job.crops.len() as u32,
                        Outcome::Failed,
                        None,
                        l.job.prompt_version,
                    ));
                }
            }
            llama
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
            generation_metadata: BTreeMap::new(),
            temporary_exits: 0,
            llama_restarts: 0,
            restart_log: Vec::new(),
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
        let max_tokens = l.job.max_tokens(crop);
        let min_tokens = image_min_tokens(l.job.prompt_version);
        let guard = loop_guard(l.job.prompt_version);
        let mut restarts_this_run = 0u32;
        let end = run_crop(
            &mut llama,
            |srv| {
                chat_completion_for(
                    srv.port,
                    &png,
                    l.job.prompt_version,
                    max_tokens,
                    min_tokens,
                    guard,
                    deadline.saturating_sub(started.elapsed()),
                )
            },
            |(text, tokens, _)| text.trim().is_empty() && *tokens == 0,
            // The checkpoint remembers restarts and temporary exits by crop, across runs.
            cp.restart_log
                .iter()
                .any(|n| n.starts_with(&format!("{}:", crop.id))),
            |srv| srv.has_exited(),
            |srv| {
                srv.restart(
                    &launch,
                    LOAD_DEADLINE.min(deadline.saturating_sub(started.elapsed())),
                )
            },
            || control.should_exit().is_some() || started.elapsed() >= deadline,
            |note| {
                restarts_this_run += 1;
                cp.note_restart(&format!("{}: {note}", crop.id));
                diag.section(&format!("llama-server restart ({})", crop.id), note, 1024);
                // The restart truncates the log, so keep what the server said before it died.
                if restarts_this_run <= 4 {
                    if let Ok(log_text) = std::fs::read_to_string(work.join("llama-server.log")) {
                        diag.section(
                            "llama-server.log before restart",
                            &redacted_tail(&log_text, 1024),
                            1024,
                        );
                    }
                }
            },
        );
        match end {
            CropEnd::Done((text, tokens, metadata)) => {
                cp.generation_metadata.insert(crop.id.clone(), metadata);
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
            CropEnd::GiveUp(e)
                if control.should_exit().is_none() && started.elapsed() < deadline =>
            {
                let msg = format!("{}: {e:#}", crop.id);
                diag.section("chat completion failed", &msg, 1024);
                // The count is saved before the client is asked, or a lost save would let a
                // broken host restart forever.
                let used = cp.temporary_exits;
                if used < MAX_TEMPORARY_EXITS {
                    cp.temporary_exits = used + 1;
                    cp.log_note(&format!("{}: temporary exit {}", crop.id, used + 1));
                    if cp.save(work).is_ok() && write_temporary_exit(work, used + 1, &msg).is_ok() {
                        diag.section(
                            "boinc control",
                            &format!(
                                "llama-server kept failing at crop {}; asked the client to \
                                 restart the task (temporary exit {}/{MAX_TEMPORARY_EXITS}), \
                                 exiting without boinc_finish_called",
                                crop.id,
                                used + 1
                            ),
                            512,
                        );
                        return None;
                    }
                    let _ = std::fs::remove_file(work.join(TEMPORARY_EXIT_FILE));
                    diag.section("temporary exit", "could not save the exit count", 256);
                } else {
                    diag.section(
                        "temporary exit",
                        &format!("all {MAX_TEMPORARY_EXITS} temporary exits already used"),
                        256,
                    );
                }
                outcome = Outcome::Failed;
                break;
            }
            CropEnd::Failed(e) | CropEnd::GiveUp(e) => {
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
    let mut loop_crops = Vec::new();
    let mut loop_bits = vec![0u8; l.job.crops.len().div_ceil(8)];
    for (i, crop) in l.job.crops.iter().enumerate() {
        let Some(metadata) = cp.generation_metadata.get(&crop.id) else {
            continue;
        };
        if serde_json::from_str::<serde_json::Value>(metadata)
            .ok()
            .and_then(|m| m.get("generation_flags")?.as_array().cloned())
            .is_some_and(|f| {
                f.iter().any(|f| {
                    f == "repetition_loop_at_token_budget" || f == "repetition_loop_guard_cut"
                })
            })
        {
            loop_crops.push(crop.id.clone());
            loop_bits[i / 8] |= 1 << (i % 8);
        }
    }
    let (llama_restarts, temporary_exits) = (cp.llama_restarts, cp.temporary_exits);
    if !cp.restart_log.is_empty() {
        diag.section(
            "llama-server restarts (whole task)",
            &cp.restart_log.join("\n"),
            4096,
        );
    }
    for (crop, metadata) in cp.generation_metadata {
        diag.section(&format!("generation_v1/{crop}"), &metadata, 2048);
    }
    if let Some(ref srv) = llama.srv {
        if let Ok(log_text) = std::fs::read_to_string(&srv.log_path) {
            diag.section(
                "llama-server.log (tail)",
                &redacted_tail(&log_text, 2048),
                2048,
            );
        }
    }
    drop(llama);

    let mut r_diag = BTreeMap::new();
    r_diag.insert(
        "ctx_size".into(),
        context_size(l.job.prompt_version).to_string(),
    );
    r_diag.insert(
        "image_min_tokens".into(),
        image_min_tokens(l.job.prompt_version).map_or("default".into(), |n| n.to_string()),
    );
    r_diag.insert(
        "loop_guard".into(),
        if loop_guard(l.job.prompt_version) {
            "stop"
        } else {
            "none"
        }
        .into(),
    );
    // Marks the crops whose reading ran out of tokens while repeating, or was cut by the
    // loop guard, so they are not mistaken for complete transcriptions.
    r_diag.insert(
        "repetition_loop_crops".into(),
        bounded_list(&loop_crops, 256),
    );
    // Complete even when the names do not fit: bit i%8 of byte i/8 is the job's crop i.
    r_diag.insert(
        "repetition_loop_crop_bits".into(),
        loop_bits
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    );
    if llama_restarts > 0 || temporary_exits > 0 {
        r_diag.insert("llama_restarts".into(), llama_restarts.to_string());
        r_diag.insert("temporary_exits".into(), temporary_exits.to_string());
    }
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
        prompt_version: l.job.prompt_version,
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
    Some((result, readings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locks::EMBEDDED_LLAMA_LOCK;
    #[cfg(windows)]
    use crate::report::DIAG_EXCERPT_BYTES;
    use crate::test_support::{
        qwen_models, synthetic_jpeg, test_weights_lock_bytes, write_qwen_unit, write_unit,
        FAKE_GGUF, FAKE_MMPROJ,
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
                // The loop guard (request versions 3 and 4) reads a streamed reply.
                if String::from_utf8_lossy(&buf[..n]).contains("\"stream\":true") {
                    let event = serde_json::json!({
                        "choices": [{"delta": {"content": reply}, "finish_reason": "stop"}],
                        "usage": {"completion_tokens": 5}
                    });
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {event}\n\ndata: [DONE]\n\n"
                        )
                        .as_bytes(),
                    );
                    continue;
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
    fn a_unit_error_keeps_the_jobs_supported_request_version() {
        for (raw, want) in [
            (2, usnm_ocr_contract::qwen::PROMPT_VERSION_IMAGE_MIN_TOKENS),
            (5, usnm_ocr_contract::qwen::PROMPT_VERSION_XIAOMI),
            (6, PROMPT_VERSION),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let job = serde_json::json!({
                "id": "b".repeat(64),
                "prompt_version": raw,
                "crops": [{}, {}],
            });
            std::fs::write(tmp.path().join(JOB_FILE), job.to_string()).unwrap();
            run_slot_with(tmp.path(), None, None).unwrap();
            let readings: Readings =
                serde_json::from_slice(&std::fs::read(tmp.path().join(READINGS_FILE)).unwrap())
                    .unwrap();
            assert_eq!(readings.job_id, "b".repeat(64));
            assert_eq!(readings.prompt_version, want);
        }
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

    #[cfg(unix)]
    #[test]
    fn slot_preserves_probe_retry_budget_and_finishes_only_after_fourth_timeout() {
        use crate::boinc::{MAX_PROBE_POSTPONES, PROBE_TIMEOUTS_FILE};

        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path();
        write_qwen_unit(slot);
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>nvidia_vulkan_8b</plan_class><gpu_type>NVIDIA</gpu_type><gpu_device_num>0</gpu_device_num></app_init_data>",
        ).unwrap();
        let server = slot.join("llama-server");
        usnm_test_support::write_executable(&server, "#!/bin/sh\nexec sleep 1\n").unwrap();
        gpu::TEST_PROBE_DEADLINE.with(|deadline| {
            let previous = deadline.replace(Some(Duration::ZERO));
            for attempt in 1..=MAX_PROBE_POSTPONES {
                std::fs::write(slot.join(TEMPORARY_EXIT_FILE), "stale temporary exit\n").unwrap();
                std::fs::write(slot.join(FINISH_FILE), "1\n").unwrap();
                run_slot_with(slot, None, None).unwrap();
                assert_eq!(
                    std::fs::read_to_string(slot.join(PROBE_TIMEOUTS_FILE)).unwrap(),
                    format!("{attempt}\n")
                );
                let request = std::fs::read_to_string(slot.join(TEMPORARY_EXIT_FILE)).unwrap();
                assert!(request.starts_with("120\nGPU driver still loading"));
                assert!(request.contains(&format!("{attempt}/3")));
                assert!(!slot.join(FINISH_FILE).exists());
                assert!(!slot.join(RESULT_FILE).exists());
                assert!(!slot.join(READINGS_FILE).exists());
            }
            run_slot_with(slot, None, None).unwrap();
            deadline.set(previous);
        });
        assert!(
            !slot.join(TEMPORARY_EXIT_FILE).exists(),
            "stale restart request survived final refusal"
        );
        assert_eq!(
            std::fs::read_to_string(slot.join(PROBE_TIMEOUTS_FILE)).unwrap(),
            "3\n"
        );
        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "1\n"
        );
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::NoGpu);
        assert_eq!(result.read, 0);
        result
            .check(
                &std::fs::read(slot.join(READINGS_FILE)).unwrap(),
                &std::fs::read(slot.join(DIAG_FILE)).unwrap(),
            )
            .unwrap();
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

        // Each model file's check is logged by its weights.lock name, never by path. There
        // is no record yet, so both are hashed in full.
        let diag_text = String::from_utf8(diag_bytes.clone()).unwrap();
        assert!(
            diag_text.contains("== weights check\nQwen3-VL-2B-Instruct-Q8_0.gguf: full hash, "),
            "{diag_text}"
        );
        assert!(
            diag_text.contains("\nmmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf: full hash, "),
            "{diag_text}"
        );
        let section = diag_text
            .split("== weights check\n")
            .nth(1)
            .and_then(|rest| rest.split("\n== ").next())
            .unwrap();
        assert!(
            !section.contains(&*tmp.path().to_string_lossy()),
            "{section}"
        );
        assert!(
            !section.contains('/') && !section.contains('\\'),
            "{section}"
        );

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

        // Even with 16000 MB free GPU memory, the 2B classes select the 2B model.
        for plan in ["nvidia_cuda_2b", gpu::NVIDIA_2B_PLAN] {
            let (chosen_2b, _, _) = resolve_model(
                &slot,
                &job.engine.models,
                Some(16000),
                true,
                Some(plan),
                None,
            )
            .unwrap();
            assert_eq!(chosen_2b.name, "qwen3-vl-2b-q8");
        }

        // mt also selects the 2B model even if free_mb is Some(16000).
        let (chosen_mt, _, _) = resolve_model(
            &slot,
            &job.engine.models,
            Some(16000),
            false,
            Some("mt"),
            None,
        )
        .unwrap();
        assert_eq!(chosen_mt.name, "qwen3-vl-2b-q8");

        for plan in [
            "nvidia_cuda_8b",
            "intel_vulkan_8b",
            "nvidia_vulkan_8b",
            "nvidia_vulkan_6gb_8b",
            "amd_vulkan_8b",
        ] {
            let free_mb = if plan == gpu::NVIDIA_6GB_PLAN {
                5600
            } else {
                16000
            };
            let (chosen_8b, _, _) = resolve_model(
                &slot,
                &job.engine.models,
                Some(free_mb),
                true,
                Some(plan),
                None,
            )
            .unwrap();
            assert_eq!(chosen_8b.name, "qwen3-vl-8b-q4km");
        }
    }

    fn write_model_files(slot: &Path, models: &[Model]) {
        for m in models {
            std::fs::write(slot.join(&m.file), FAKE_GGUF).unwrap();
            std::fs::write(slot.join(&m.mmproj), FAKE_MMPROJ).unwrap();
        }
    }

    #[test]
    fn a_pinned_unit_reads_with_its_model_or_not_at_all() {
        use crate::locks::PIN_REFUSED;
        use crate::test_support::{qwen_models, write_xiaomi_unit, xiaomi_model};
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_xiaomi_unit(&slot);
        let pin = job.model.as_deref();
        // Every model a host could hold, Qwen's files on disk and Xiaomi-OCR-0's not.
        let mut all = qwen_models();
        all.push(xiaomi_model());
        write_model_files(&slot, &qwen_models());
        for plan in [Some("nvidia_vulkan_x08"), Some("mt_x08"), None] {
            let err = resolve_model(&slot, &all, Some(16000), true, plan, pin).unwrap_err();
            let err = format!("{err:#}");
            assert!(err.starts_with(PIN_REFUSED), "{err}");
            assert!(err.contains("no other model is read in its place"), "{err}");
        }
        write_model_files(&slot, &[xiaomi_model()]);
        for plan in [
            Some("nvidia_vulkan_x08"),
            Some("amd_vulkan_x08"),
            Some("intel_vulkan_x08"),
            Some("apple_gpu_x08"),
            Some("mt_x08"),
            None,
        ] {
            let (m, model, mmproj) =
                resolve_model(&slot, &all, Some(16000), true, plan, pin).unwrap();
            assert_eq!(m.name, "xiaomi-ocr-0-q8");
            assert!(model.ends_with("xiaomi-ocr-0-q8_0.gguf"));
            assert!(mmproj.ends_with("mmproj-xiaomi-ocr-0-f16.gguf"));
        }
        // A pinned file that doesn't match weights.lock is a pin refusal too, so the
        // failure report flags it.
        let mismatched = tmp.path().join("mismatched");
        write_xiaomi_unit(&mismatched);
        write_model_files(&mismatched, &[xiaomi_model()]);
        std::fs::write(
            mismatched.join(&xiaomi_model().file),
            b"not the pinned weights",
        )
        .unwrap();
        let err =
            resolve_model(&mismatched, &all, Some(16000), true, Some("mt_x08"), pin).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.starts_with(PIN_REFUSED), "{err}");
        assert!(err.contains("sha256 mismatch"), "{err}");
        // A Qwen class never reads a Xiaomi unit, and an _x08 class never reads anything else.
        for plan in ["mt", "nvidia_vulkan_8b", "nvidia_vulkan_2b", "apple_gpu_8b"] {
            let err = resolve_model(&slot, &all, Some(16000), true, Some(plan), pin).unwrap_err();
            assert!(format!("{err:#}").starts_with(PIN_REFUSED), "{plan}");
        }
        for plan in ["nvidia_vulkan_x08", "mt_x08"] {
            let err = resolve_model(&slot, &all, Some(16000), true, Some(plan), None).unwrap_err();
            assert!(format!("{err:#}").starts_with(PIN_REFUSED), "{plan}");
            for qwen in ["qwen3-vl-8b-q4km", "qwen3-vl-2b-q8"] {
                let err = resolve_model(&slot, &all, Some(16000), true, Some(plan), Some(qwen))
                    .unwrap_err();
                assert!(format!("{err:#}").starts_with(PIN_REFUSED), "{plan} {qwen}");
            }
        }
        // Unpinned, no Xiaomi-OCR-0 model is ever chosen, even listed first with room for it.
        let xiaomi_first = vec![xiaomi_model(), qwen_models()[1].clone()];
        let (m, _, _) = resolve_model(&slot, &xiaomi_first, Some(16000), true, None, None).unwrap();
        assert_eq!(m.name, "qwen3-vl-2b-q8");
        // 8B and 2B pins on their own classes (#968), and nowhere else.
        let qwen = qwen_models();
        let pick = |plan, pin| {
            resolve_model(&slot, &qwen, Some(16000), true, Some(plan), Some(pin))
                .map(|(m, _, _)| m.name.clone())
        };
        assert_eq!(
            pick("nvidia_vulkan_8b", "qwen3-vl-8b-q4km").unwrap(),
            "qwen3-vl-8b-q4km"
        );
        assert_eq!(pick("mt", "qwen3-vl-2b-q8").unwrap(), "qwen3-vl-2b-q8");
        assert_eq!(
            pick("nvidia_vulkan_2b", "qwen3-vl-2b-q8").unwrap(),
            "qwen3-vl-2b-q8"
        );
        assert!(pick("nvidia_vulkan_8b", "qwen3-vl-2b-q8").is_err());
        assert!(pick("mt", "qwen3-vl-8b-q4km").is_err());
        assert!(pick("mt", "qwen3-vl-4b").is_err());
        // The pinned files are checked against weights.lock like any other.
        std::fs::write(slot.join("xiaomi-ocr-0-q8_0.gguf"), b"tampered").unwrap();
        let err = resolve_model(&slot, &all, None, false, Some("mt_x08"), pin).unwrap_err();
        assert!(format!("{err:#}").contains("sha256 mismatch"));
    }

    #[test]
    fn a_stale_verified_record_never_lets_a_changed_pinned_file_through() {
        use crate::locks::PIN_REFUSED;
        use crate::test_support::{write_xiaomi_unit, xiaomi_model, FAKE_GGUF};
        use crate::verified::{check, Policy};
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_xiaomi_unit(&slot);
        let pin = job.model.as_deref();
        write_model_files(&slot, &[xiaomi_model()]);
        let model = slot.join(&xiaomi_model().file);
        // A record from a good full hash, with the test policy (any filesystem, no racy
        // margin) so it is written at once.
        let relaxed = Policy {
            require_trusted_fs: false,
            racy_margin: Duration::ZERO,
        };
        let want = sha256(FAKE_GGUF);
        let (got, _) = check(&xiaomi_model().file, &model, &want, false, relaxed).unwrap();
        assert_eq!(got.as_deref(), Some(want.as_str()));
        // The file changes under it: same refusal, same message as without a record.
        std::fs::write(&model, b"not the pinned weights").unwrap();
        let mut log = Vec::new();
        let err = crate::locks::resolve_model_logged(
            &slot,
            &[xiaomi_model()],
            Some(16000),
            true,
            Some("mt_x08"),
            pin,
            &mut log,
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.starts_with(PIN_REFUSED), "{err}");
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert_eq!(log.len(), 1);
        assert!(
            log[0]
                .to_string()
                .starts_with("xiaomi-ocr-0-q8_0.gguf: full hash"),
            "{}",
            log[0]
        );
    }

    #[test]
    fn a_refused_pin_fails_the_unit_with_a_flagged_report() {
        use crate::test_support::{qwen_models, write_xiaomi_unit};
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_xiaomi_unit(&slot);
        // Only Qwen's files reach the slot: the unit fails rather than reading with them.
        write_model_files(&slot, &qwen_models());
        let port = spawn_mock_llama_server(vec!["never asked"]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        assert_eq!(result.model, None);
        assert_eq!(result.read, 0);
        let diag = std::fs::read_to_string(slot.join(DIAG_FILE)).unwrap();
        assert!(diag.contains(crate::locks::PIN_REFUSED), "{diag}");
        let report = failure_report(&result, diag.as_bytes());
        let report: serde_json::Value =
            serde_json::from_str(report.strip_prefix("USNM_QWEN_FAILURE ").unwrap()).unwrap();
        assert_eq!(report["model_pin_refused"], true);
        assert_eq!(job.model.as_deref(), Some("xiaomi-ocr-0-q8"));
    }

    /// A benchmark campaign's unit (`usnm-boinc qwen benchmark-stage`): one whole-image crop with
    /// no LoC text, a `benchmark:` source and a `NOTICE.txt` beside the page. The native worker
    /// reads it at every Qwen request version, the guarded ones (3, 4) included, and reports
    /// the version the job asked for.
    #[test]
    fn a_benchmark_unit_reads_at_every_qwen_request_version() {
        for (version, min_tokens, guard) in [
            (1, "default", "none"),
            (2, "1024", "none"),
            (3, "default", "stop"),
            (4, "1024", "stop"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let slot = tmp.path().join("slot");
            let mut job = write_unit(&slot, qwen_models(), version, None);
            let page = std::fs::read(slot.join(PAGE_FILE)).unwrap();
            let notice = b"Synthetic dataset authors\n";
            std::fs::write(slot.join("NOTICE.txt"), notice).unwrap();
            job.crops = vec![usnm_ocr_contract::qwen::Crop {
                id: "public-001".into(),
                rect: [0, 0, 200, 200],
                loc_text: String::new(),
                stratum: "benchmark".into(),
            }];
            job.input = Input::of(&page, &format!("benchmark:{}", sha256(notice)));
            job.id = job.identity().unwrap();
            job.validate().unwrap();
            std::fs::write(slot.join(JOB_FILE), serde_json::to_vec(&job).unwrap()).unwrap();
            std::fs::write(
                slot.join("init_data.xml"),
                "<app_init_data><plan_class>mt</plan_class></app_init_data>",
            )
            .unwrap();
            write_model_files(&slot, &qwen_models());
            let port = spawn_mock_llama_server(vec!["benchmark reading"]);
            run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();
            let names: Vec<_> = std::fs::read_dir(&slot)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            let result: ResultManifest = serde_json::from_slice(
                &std::fs::read(slot.join(RESULT_FILE)).unwrap_or_else(|e| {
                    panic!(
                        "{e}: {names:?} {}",
                        String::from_utf8_lossy(
                            &std::fs::read(slot.join(DIAG_FILE)).unwrap_or_default()
                        )
                    )
                }),
            )
            .unwrap();
            let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
            assert_eq!(
                result.outcome,
                Outcome::Ok,
                "v{version}: {}",
                String::from_utf8_lossy(&diag_bytes)
            );
            let readings_bytes = std::fs::read(slot.join(READINGS_FILE)).unwrap();
            assert_eq!(result.job_id, job.id);
            let readings = result.check(&readings_bytes, &diag_bytes).unwrap();
            assert_eq!(readings.prompt_version, version);
            assert_eq!(readings.diag["image_min_tokens"], min_tokens, "v{version}");
            assert_eq!(readings.diag["loop_guard"], guard, "v{version}");
            assert_eq!(readings.readings.len(), 1);
            assert_eq!(readings.readings[0].id, "public-001");
            assert_eq!(readings.readings[0].text, "benchmark reading");
        }
    }

    #[test]
    fn a_xiaomi_unit_reads_with_version_5_and_names_its_model() {
        use crate::test_support::{write_xiaomi_unit, xiaomi_model};
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        let job = write_xiaomi_unit(&slot);
        write_model_files(&slot, &[xiaomi_model()]);
        let port = spawn_mock_llama_server(vec!["first reading", "second reading"]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();
        let result_bytes = std::fs::read(slot.join(RESULT_FILE)).unwrap();
        let readings_bytes = std::fs::read(slot.join(READINGS_FILE)).unwrap();
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let result: ResultManifest = serde_json::from_slice(&result_bytes).unwrap();
        assert_eq!(
            result.outcome,
            Outcome::Ok,
            "{}",
            String::from_utf8_lossy(&diag_bytes)
        );
        assert_eq!(result.job_id, job.id);
        assert_eq!(result.model.as_deref(), Some("xiaomi-ocr-0-q8"));
        let readings = result.check(&readings_bytes, &diag_bytes).unwrap();
        assert_eq!(readings.prompt_version, 5);
        assert_eq!(readings.diag["ctx_size"], "16384");
        assert_eq!(readings.diag["loop_guard"], "none");
        assert_eq!(readings.diag["image_min_tokens"], "default");
        assert_eq!(readings.readings.len(), 2);
        // Each crop's request asked for Xiaomi-OCR-0's fixed cap.
        let diag = String::from_utf8_lossy(&diag_bytes);
        assert!(diag.contains("\"max_tokens_requested\":2048"), "{diag}");
    }

    #[test]
    fn qwen_slot_checkpoints_per_crop_and_resumes_multipage_bundle() {
        use usnm_ocr_contract::{
            quality::BundleSlice,
            qwen::{BundlePage, PROMPT_VERSION_IMAGE_MIN_TOKENS},
        };

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
            prompt_version: PROMPT_VERSION_IMAGE_MIN_TOKENS,
            model: None,
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
            generation_metadata: BTreeMap::from([(
                "page_one:c01".into(),
                r#"{"schema":1,"finish_reason":"length","generation_flags":["repetition_loop_at_token_budget"]}"#.into(),
            )]),
            temporary_exits: 0,
            llama_restarts: 0,
            restart_log: Vec::new(),
        };
        let mut legacy = serde_json::to_value(&cp).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("generation_metadata");
        let legacy: QwenCheckpoint = serde_json::from_value(legacy).unwrap();
        assert!(legacy.generation_metadata.is_empty());
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
        assert!(diag_str
            .contains("== generation_v1/page_one:c01\n{\"schema\":1,\"finish_reason\":\"length\""));
        assert_eq!(readings.prompt_version, PROMPT_VERSION_IMAGE_MIN_TOKENS);
        assert_eq!(readings.diag["image_min_tokens"], "1024");
        assert_eq!(readings.diag["repetition_loop_crops"], "page_one:c01");
        assert_eq!(readings.diag["repetition_loop_crop_bits"], "01");
        let fresh: serde_json::Value = serde_json::from_str(
            diag_str
                .split("== generation_v1/page_two:c01\n")
                .nth(1)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert!(fresh["finish_reason"].is_null());
        assert_eq!(fresh["schema"], 2);
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
        let err =
            resolve_model(&slot, &job.engine.models, None, false, Some("mt"), None).unwrap_err();
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

    /// How a scripted `llama-server` answers one request.
    #[derive(Clone, Copy)]
    enum Step {
        Reply(&'static str),
        /// Reads the request and hangs up without a byte, as the Intel host's server does.
        Drop,
        /// An HTTP error status from a live server.
        Status(u16),
    }

    /// Answers each connection from `steps`, then hangs up on every later one. Returns the port
    /// and the number of requests served.
    fn spawn_scripted_llama_server(
        steps: Vec<Step>,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::Ordering::SeqCst;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = served.clone();
        std::thread::spawn(move || {
            let mut steps = steps.into_iter();
            while let Ok((mut stream, _)) = listener.accept() {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buf = vec![0u8; 65536];
                let mut n = 0;
                let mut want = usize::MAX;
                while n < want {
                    match stream.read(&mut buf[n..]) {
                        Ok(0) | Err(_) => break,
                        Ok(k) => {
                            n += k;
                            if let Some(sep) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..sep]).into_owned();
                                let clen: usize = head
                                    .lines()
                                    .find_map(|l| {
                                        let (k, v) = l.split_once(':')?;
                                        k.eq_ignore_ascii_case("content-length")
                                            .then(|| v.trim().parse().ok())?
                                    })
                                    .unwrap_or(0);
                                want = sep + 4 + clen;
                            }
                        }
                    }
                }
                count.fetch_add(1, SeqCst);
                match steps.next().unwrap_or(Step::Drop) {
                    Step::Drop => {}
                    Step::Status(code) => {
                        let _ = write!(
                            stream,
                            "HTTP/1.1 {code} Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom"
                        );
                    }
                    Step::Reply(text) => {
                        let body = serde_json::to_vec(&serde_json::json!({
                            "choices": [{"message": {"content": text}}],
                            "usage": {"completion_tokens": 5}
                        }))
                        .unwrap();
                        let _ = write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(&body);
                    }
                }
            }
        });
        (port, served)
    }

    /// A slot with a two-crop unit that runs on the CPU against a mock server.
    fn crop_retry_slot(tmp: &Path) -> (std::path::PathBuf, Job) {
        let slot = tmp.join("slot");
        let job = write_qwen_unit(&slot);
        for name in [
            "Qwen3-VL-2B-Instruct-Q8_0.gguf",
            "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
        ] {
            std::fs::write(slot.join(name), FAKE_GGUF).unwrap();
        }
        std::fs::write(
            slot.join("mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf"),
            FAKE_MMPROJ,
        )
        .unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            "<app_init_data><plan_class>mt</plan_class></app_init_data>",
        )
        .unwrap();
        (slot, job)
    }

    fn load_checkpoint(slot: &Path, job: &Job) -> QwenCheckpoint {
        QwenCheckpoint::load_valid(slot, job, "qwen3-vl-2b-q8", "cpu").unwrap()
    }

    #[test]
    fn a_dropped_connection_is_retried_on_the_same_crop() {
        use std::sync::atomic::Ordering::SeqCst;
        let tmp = tempfile::tempdir().unwrap();
        let (slot, job) = crop_retry_slot(tmp.path());
        // Crop one loses its connection twice; crop two answers at once.
        let (port, served) = spawn_scripted_llama_server(vec![
            Step::Drop,
            Step::Drop,
            Step::Reply("first"),
            Step::Reply("second"),
        ]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert_eq!(served.load(SeqCst), 4);
        assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "0\n"
        );
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::Ok);
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let readings = result
            .check(
                &std::fs::read(slot.join(READINGS_FILE)).unwrap(),
                &diag_bytes,
            )
            .unwrap();
        assert_eq!(readings.readings[0].text, "first");
        assert_eq!(readings.readings[1].text, "second");
        assert_eq!(readings.diag["llama_restarts"], "2");
        assert_eq!(readings.diag["temporary_exits"], "0");
        let diag = String::from_utf8_lossy(&diag_bytes);
        assert!(diag.contains("== llama-server restart (c01)"), "{diag}");
        assert!(
            diag.contains("== llama-server restarts (whole task)"),
            "{diag}"
        );
        assert!(!slot.join(CHECKPOINT_FILE).exists());
        let _ = job;
    }

    #[test]
    fn a_crop_that_keeps_failing_asks_the_client_to_restart_and_resumes_from_the_checkpoint() {
        use std::sync::atomic::Ordering::SeqCst;
        let tmp = tempfile::tempdir().unwrap();
        let (slot, job) = crop_retry_slot(tmp.path());
        // Crop one is read; crop two loses its connection on all three tries.
        let (port, served) = spawn_scripted_llama_server(vec![
            Step::Reply("first"),
            Step::Drop,
            Step::Drop,
            Step::Drop,
        ]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert_eq!(served.load(SeqCst), 4);
        let exit = std::fs::read_to_string(slot.join(TEMPORARY_EXIT_FILE)).unwrap();
        assert!(
            exit.starts_with("60\nllama-server connection failed (1/3): c02: "),
            "{exit}"
        );
        // Not finished: the client restarts the task rather than reporting it.
        assert!(!slot.join(FINISH_FILE).exists());
        assert!(!slot.join(RESULT_FILE).exists());
        assert!(!slot.join(READINGS_FILE).exists());
        let cp = load_checkpoint(&slot, &job);
        assert_eq!(cp.readings.len(), 1);
        assert_eq!(cp.readings[0].text, "first");
        assert_eq!((cp.temporary_exits, cp.llama_restarts), (1, 2));

        // The client restarts the task: only the missing crop is asked for.
        let (port, served) = spawn_scripted_llama_server(vec![Step::Reply("second")]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();
        assert_eq!(served.load(SeqCst), 1);
        assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "0\n"
        );
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let readings = result
            .check(
                &std::fs::read(slot.join(READINGS_FILE)).unwrap(),
                &diag_bytes,
            )
            .unwrap();
        assert_eq!(readings.readings[0].text, "first");
        assert_eq!(readings.readings[1].text, "second");
        assert_eq!(readings.diag["temporary_exits"], "1");
        assert_eq!(readings.diag["llama_restarts"], "2");
        let diag = String::from_utf8_lossy(&diag_bytes);
        assert!(diag.contains("resumed from checkpoint: 1 of 2 crops already complete"));
        assert!(diag.contains("c02: temporary exit 1"), "{diag}");
    }

    #[test]
    fn a_task_that_used_all_its_temporary_exits_fails_as_before() {
        let tmp = tempfile::tempdir().unwrap();
        let (slot, job) = crop_retry_slot(tmp.path());
        let cp = QwenCheckpoint {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            model: "qwen3-vl-2b-q8".into(),
            device: "cpu".into(),
            readings: vec![],
            generation_metadata: BTreeMap::new(),
            temporary_exits: MAX_TEMPORARY_EXITS,
            llama_restarts: 0,
            restart_log: Vec::new(),
        };
        cp.save(&slot).unwrap();
        let (port, _) = spawn_scripted_llama_server(vec![]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "1\n"
        );
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        let diag =
            String::from_utf8_lossy(&std::fs::read(slot.join(DIAG_FILE)).unwrap()).into_owned();
        assert!(
            diag.contains("all 3 temporary exits already used"),
            "{diag}"
        );
        assert_eq!(
            load_checkpoint(&slot, &job).temporary_exits,
            MAX_TEMPORARY_EXITS
        );
    }

    #[test]
    fn a_real_http_error_status_fails_at_once() {
        use std::sync::atomic::Ordering::SeqCst;
        let tmp = tempfile::tempdir().unwrap();
        let (slot, _) = crop_retry_slot(tmp.path());
        let (port, served) = spawn_scripted_llama_server(vec![Step::Status(500)]);
        run_slot_with(&slot, Some(port), Some(Duration::from_secs(30))).unwrap();

        assert_eq!(served.load(SeqCst), 1, "no retry");
        assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "1\n"
        );
        let result: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
    }
}
