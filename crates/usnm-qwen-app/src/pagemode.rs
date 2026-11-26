//! Page mode (#1998, see docs/boinc/apps/quality.md): a unit whose crops are the
//! regions of one page (or a few small pages), read with several requests in flight on one
//! `llama-server`.
//!
//! * [`slots_for`] picks `--parallel` from free GPU memory, the weights and the KV cache one
//!   slot needs ([`Gguf`] reads the shape from the model file).
//! * [`read_regions`] reads the regions longest first on one thread per slot. The crash rules of
//!   the sequential path hold per region: a failed request restarts `llama-server` and tries
//!   again, and a blank 0-token reply from a server that died, or was restarted while the
//!   request was in flight, is a failed attempt and never a reading (#1964, #1967).
//! * [`split_ms`] shares a unit's GPU time between its pages by completion tokens.
use crate::{
    checkpoint::QwenCheckpoint,
    report::redacted_tail,
    retry::{run_crop, CropEnd},
    server::{chat_completion_for, Launch, Llama, LOAD_DEADLINE},
};
use anyhow::{bail, ensure, Context};
use std::{
    collections::{BTreeSet, VecDeque},
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};
use usnm_ocr_contract::qwen::{image_min_tokens, loop_guard, Crop, Job, Reading, MAX_PAGE_SLOTS};
use usnm_ocr_quality::slot::{write_atomic, BoincControl, Diag, FRACTION_FILE};

/// Share of free GPU memory left unused.
pub(crate) const MARGIN: f64 = 0.15;
/// llama.cpp's compute buffers and the projector's image encoding, beyond the weights. Measured
/// per class on the first units (`gpu_mem_peak_mib` at one slot) and kept here, not guessed;
/// `USNM_QWEN_COMPUTE_RESERVE_MIB` replaces it for a calibration run.
pub(crate) const COMPUTE_RESERVE_MIB: u64 = 1536;
pub(crate) const ENV_COMPUTE_RESERVE: &str = "USNM_QWEN_COMPUTE_RESERVE_MIB";
/// What one region needs: image tokens, the prompt and 2,048 output tokens.
pub(crate) const CTX_PER_SLOT: u32 = 8192;
const MIB: u64 = 1 << 20;
/// What readings.json may hold: the contract's limit less room for the header and diag.
const READINGS_BUDGET: usize = usnm_ocr_contract::qwen::MAX_READINGS - 64 * 1024;

/// The memory model in the slot rule: the slot count that fits.
///
/// `fixed_mib` is the weights, the projector and the compute reserve. `None` for free memory
/// (CPU, or a probe that did not answer) is one slot.
pub(crate) fn slots_for(free_mib: Option<u64>, fixed_mib: u64, kv_per_slot_mib: u64) -> u32 {
    let Some(free) = free_mib else {
        return 1;
    };
    if kv_per_slot_mib == 0 {
        return 1;
    }
    let usable = free as f64 * (1.0 - MARGIN) - fixed_mib as f64;
    if usable <= 0.0 {
        return 1;
    }
    ((usable / kv_per_slot_mib as f64).floor() as u32).clamp(1, MAX_PAGE_SLOTS)
}

/// One step of halving after an out-of-memory start.
pub(crate) fn halve(slots: u32) -> u32 {
    (slots / 2).max(1)
}

/// Whether text from `llama-server` (its log, or the error that reported its exit) says the
/// device ran out of memory.
pub(crate) fn is_oom(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "out of memory",
        "outofdevicememory",
        "out_of_device_memory",
        "failed to allocate",
        "cudamalloc failed",
        "unable to allocate",
        "insufficient memory",
        "oom",
    ]
    .iter()
    .any(|m| match *m {
        // "oom" alone matches room, zoom, ...
        "oom" => t
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == "oom"),
        m => t.contains(m),
    })
}

/// Shares `total_ms` of GPU time between pages by their completion tokens. The shares add up to
/// `total_ms` exactly (largest remainder); with no tokens at all the pages share it evenly.
pub(crate) fn split_ms(total_ms: u64, tokens: &[u64]) -> Vec<u64> {
    if tokens.is_empty() {
        return vec![];
    }
    let sum: u128 = tokens.iter().map(|&t| u128::from(t)).sum();
    let weights: Vec<u128> = if sum == 0 {
        vec![1; tokens.len()]
    } else {
        tokens.iter().map(|&t| u128::from(t)).collect()
    };
    let den: u128 = weights.iter().sum();
    let mut shares: Vec<u64> = weights
        .iter()
        .map(|&w| (u128::from(total_ms) * w / den) as u64)
        .collect();
    let mut left = total_ms - shares.iter().sum::<u64>();
    let mut order: Vec<usize> = (0..tokens.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(u128::from(total_ms) * weights[i] % den));
    for i in order {
        if left == 0 {
            break;
        }
        shares[i] += 1;
        left -= 1;
    }
    shares
}

/// The shape of the language model in a GGUF file: what its KV cache costs per token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Gguf {
    /// KV heads summed over the layers.
    pub(crate) kv_heads_total: u64,
    pub(crate) key_len: u64,
    pub(crate) value_len: u64,
}

impl Gguf {
    /// Bytes of f16 KV cache one token costs.
    pub(crate) fn kv_bytes_per_token(&self) -> u64 {
        self.kv_heads_total * (self.key_len + self.value_len) * 2
    }

    /// MiB of KV cache one slot of `ctx` tokens costs, rounded up.
    pub(crate) fn kv_per_slot_mib(&self, ctx: u32) -> u64 {
        (self.kv_bytes_per_token() * u64::from(ctx)).div_ceil(MIB)
    }

    /// Reads the metadata header only.
    pub(crate) fn read(path: &Path) -> anyhow::Result<Self> {
        let mut r = BufReader::new(std::fs::File::open(path)?);
        Self::parse(&mut r)
    }

    pub(crate) fn parse<R: Read + Seek>(r: &mut R) -> anyhow::Result<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        ensure!(&magic == b"GGUF", "not a GGUF file");
        let version = u32_le(r)?;
        ensure!((2..=3).contains(&version), "unsupported GGUF version");
        let _tensors = u64_le(r)?;
        let kvs = u64_le(r)?;
        ensure!(kvs <= 1 << 20, "implausible GGUF metadata count");
        let mut arch = None::<String>;
        // Every integer, or array of integers, by key; the architecture's keys are picked out
        // once it is known, since a file need not list `general.architecture` first.
        let mut ints = std::collections::BTreeMap::<String, Vec<u64>>::new();
        for _ in 0..kvs {
            let key = string(r, 4096)?.context("GGUF key too long")?;
            let ty = u32_le(r)?;
            if key == "general.architecture" && ty == 8 {
                arch = string(r, 256)?;
                continue;
            }
            let wanted = [
                ".block_count",
                ".attention.head_count",
                ".attention.head_count_kv",
                ".embedding_length",
                ".attention.key_length",
                ".attention.value_length",
            ]
            .iter()
            .any(|s| key.ends_with(s));
            if wanted && ty != 8 {
                if ty == 9 {
                    let elem = u32_le(r)?;
                    let n = u64_le(r)?;
                    ensure!(n <= 1 << 16, "implausible GGUF array");
                    let mut v = Vec::new();
                    for _ in 0..n {
                        v.push(scalar(r, elem)?);
                    }
                    ints.insert(key, v);
                } else {
                    ints.insert(key, vec![scalar(r, ty)?]);
                }
            } else {
                skip(r, ty)?;
            }
        }
        let arch = arch.context("GGUF has no general.architecture")?;
        let get = |name: &str| ints.get(&format!("{arch}.{name}"));
        let layers = *get("block_count")
            .and_then(|v| v.first())
            .context("GGUF has no block_count")?;
        let heads = get("attention.head_count");
        let kv = get("attention.head_count_kv").or(heads);
        let kv_total = match kv {
            Some(v) if v.len() == 1 => v[0] * layers,
            // One entry per layer.
            Some(v) => v.iter().sum(),
            None => bail!("GGUF has no head_count_kv"),
        };
        let heads0 = heads.and_then(|v| v.first().copied());
        let embed = get("embedding_length").and_then(|v| v.first().copied());
        let default_dim = match (embed, heads0) {
            (Some(e), Some(h)) if h > 0 => e / h,
            _ => 0,
        };
        let dim = |name: &str| {
            get(name)
                .and_then(|v| v.first().copied())
                .unwrap_or(default_dim)
        };
        let (key_len, value_len) = (dim("attention.key_length"), dim("attention.value_length"));
        ensure!(
            kv_total > 0 && key_len > 0 && value_len > 0,
            "GGUF shape is incomplete"
        );
        Ok(Self {
            kv_heads_total: kv_total,
            key_len,
            value_len,
        })
    }
}

fn u32_le<R: Read>(r: &mut R) -> anyhow::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn u64_le<R: Read>(r: &mut R) -> anyhow::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

/// A string of at most `max` bytes; a longer one is skipped and reads as `None`.
fn string<R: Read + Seek>(r: &mut R, max: u64) -> anyhow::Result<Option<String>> {
    let n = u64_le(r)?;
    if n > max {
        r.seek(SeekFrom::Current(i64::try_from(n)?))?;
        return Ok(None);
    }
    let mut b = vec![0u8; n as usize];
    r.read_exact(&mut b)?;
    Ok(Some(String::from_utf8_lossy(&b).into_owned()))
}

fn width(ty: u32) -> anyhow::Result<i64> {
    Ok(match ty {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => bail!("unknown GGUF value type {ty}"),
    })
}

fn scalar<R: Read>(r: &mut R, ty: u32) -> anyhow::Result<u64> {
    let mut b = [0u8; 8];
    let w = width(ty)? as usize;
    r.read_exact(&mut b[..w])?;
    Ok(u64::from_le_bytes(b))
}

fn skip<R: Read + Seek>(r: &mut R, ty: u32) -> anyhow::Result<()> {
    match ty {
        8 => {
            let n = u64_le(r)?;
            r.seek(SeekFrom::Current(i64::try_from(n)?))?;
        }
        9 => {
            let elem = u32_le(r)?;
            let n = u64_le(r)?;
            if elem == 8 || elem == 9 {
                for _ in 0..n {
                    skip(r, elem)?;
                }
            } else {
                r.seek(SeekFrom::Current(i64::try_from(n)? * width(elem)?))?;
            }
        }
        t => {
            r.seek(SeekFrom::Current(width(t)?))?;
        }
    }
    Ok(())
}

/// The slot count and what it was worked out from, for the diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlotPlan {
    pub(crate) slots: u32,
    pub(crate) forced: bool,
    pub(crate) free_mib: Option<u64>,
    pub(crate) fixed_mib: u64,
    pub(crate) kv_per_slot_mib: u64,
    pub(crate) note: String,
}

/// Picks the slot count for a page-mode unit: the job's forced count (calibration units), else
/// the memory rule on a GPU, else one.
pub(crate) fn plan_slots(
    forced: Option<u32>,
    use_gpu: bool,
    free_mib: Option<u64>,
    model_path: &Path,
    mmproj_path: &Path,
) -> SlotPlan {
    let file_mib = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len().div_ceil(MIB));
    let reserve = std::env::var(ENV_COMPUTE_RESERVE)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(COMPUTE_RESERVE_MIB);
    let fixed_mib = file_mib(model_path) + file_mib(mmproj_path) + reserve;
    let (kv_per_slot_mib, note) = match Gguf::read(model_path) {
        Ok(g) => (g.kv_per_slot_mib(CTX_PER_SLOT), String::new()),
        Err(e) => (0, format!("GGUF shape unreadable: {e:#}")),
    };
    let (slots, note) = match forced {
        Some(n) => (n.clamp(1, MAX_PAGE_SLOTS), note),
        None if !use_gpu => (1, "no GPU: one slot".into()),
        None if kv_per_slot_mib == 0 => (1, format!("one slot; {note}")),
        None => (slots_for(free_mib, fixed_mib, kv_per_slot_mib), note),
    };
    SlotPlan {
        slots,
        forced: forced.is_some(),
        free_mib,
        fixed_mib,
        kv_per_slot_mib,
        note,
    }
}

/// How a page-mode read ended early.
#[derive(Debug)]
pub(crate) enum Ending {
    /// BOINC asked the task to stop.
    Exit,
    Timeout(String),
    Failed(#[allow(dead_code)] String),
    /// `llama-server` kept failing on this region: restart the task and resume.
    GiveUp(String, anyhow::Error),
}

pub(crate) struct PageEnd {
    pub(crate) ending: Option<Ending>,
    pub(crate) gpu_peak_mib: Option<u64>,
    /// Slots when the last server started, after any halving.
    pub(crate) slots: u32,
    pub(crate) halvings: u32,
}

/// What the readers share.
struct Shared<'a, 'b> {
    job: &'a Job,
    work: &'a Path,
    deadline: Duration,
    started: Instant,
    control: &'a BoincControl,
    gpu: &'a crate::gpu::GpuState,
    per_slot_ctx: u32,
    feed: Mutex<Feed<'a>>,
    llama: Mutex<&'b mut Llama>,
    launch: Mutex<&'b mut Launch<'a>>,
    /// Counts server starts; a request that began under an older count was in flight when the
    /// server was lost.
    generation: AtomicU64,
    cp: Mutex<&'b mut QwenCheckpoint>,
    diag: Mutex<&'b mut Diag>,
    stop: AtomicBool,
    ending: Mutex<Option<Ending>>,
    peak: Mutex<Option<u64>>,
    halvings: AtomicU64,
    total: usize,
    load_cost_ms: u64,
    /// Bytes the readings so far take in readings.json; changed under the checkpoint lock.
    readings_bytes: AtomicUsize,
}

struct Feed<'a> {
    pending: VecDeque<usize>,
    pages: &'a mut usnm_ocr_quality::qwen_pages::QwenPages,
    page_bytes: &'a [u8],
}

/// Region indexes in the order to read them: longest LoC text first, so the last ones to finish
/// are short and the slots do not sit idle at the end. Ties keep the unit's order. In a bundle
/// the pages go one after the other (in the unit's order) and each is longest first, since only
/// one decoded page is kept at a time.
pub(crate) fn longest_first(crops: &[Crop], done: &BTreeSet<&str>) -> Vec<usize> {
    let mut v: Vec<usize> = (0..crops.len())
        .filter(|&i| !done.contains(crops[i].id.as_str()))
        .collect();
    let mut pages: Vec<String> = Vec::new();
    let mut rank = |id: &str| {
        let doc = id.split_once(':').map_or("", |(d, _)| d);
        pages.iter().position(|p| p == doc).unwrap_or_else(|| {
            pages.push(doc.to_owned());
            pages.len() - 1
        })
    };
    let ranks: Vec<usize> = crops.iter().map(|c| rank(&c.id)).collect();
    v.sort_by_key(|&i| {
        (
            ranks[i],
            std::cmp::Reverse(crops[i].loc_text.chars().count()),
        )
    });
    v
}

impl Shared<'_, '_> {
    fn end(&self, e: Ending) {
        self.stop.store(true, Ordering::SeqCst);
        let mut slot = self.ending.lock().unwrap();
        // A task that must restart outranks a plain failure that raced it.
        if slot.is_none()
            || (matches!(e, Ending::GiveUp(..)) && !matches!(*slot, Some(Ending::GiveUp(..))))
        {
            *slot = Some(e);
        }
    }

    fn next(&self) -> Option<(usize, anyhow::Result<Vec<u8>>)> {
        let mut f = self.feed.lock().unwrap();
        let idx = f.pending.pop_front()?;
        let crop = &self.job.crops[idx];
        let Feed {
            pages, page_bytes, ..
        } = &mut *f;
        let png = pages
            .page_for(self.job, page_bytes, crop)
            .and_then(|page| crate::crop_png(page, crop));
        Some((idx, png))
    }

    /// Starts the server again after it was lost. If another reader has done so since this
    /// one's request began, there is nothing to do. An out-of-memory exit halves the slots
    /// first, and so does a start that runs out of memory.
    fn restart(&self, seen: &mut u64) -> anyhow::Result<()> {
        let mut llama = self.llama.lock().unwrap();
        if self.generation.load(Ordering::SeqCst) != *seen {
            *seen = self.generation.load(Ordering::SeqCst);
            return Ok(());
        }
        let mut launch = self.launch.lock().unwrap();
        let log = std::fs::read_to_string(self.work.join("llama-server.log")).unwrap_or_default();
        if is_oom(&log) && halve_launch(&mut launch, self.per_slot_ctx) {
            self.note_halved(&launch);
        }
        let left = LOAD_DEADLINE.min(self.deadline.saturating_sub(self.started.elapsed()));
        let r = start_halving(
            &mut llama,
            &mut launch,
            self.per_slot_ctx,
            left,
            true,
            &mut |l| self.note_halved(l),
        );
        if r.is_ok() {
            *seen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        }
        r.map(drop)
    }

    fn note_halved(&self, launch: &Launch<'_>) {
        self.halvings.fetch_add(1, Ordering::SeqCst);
        self.diag.lock().unwrap().section(
            "slots halved",
            &format!(
                "out of memory: now {} slots, ctx-size {}",
                launch.parallel, launch.ctx_size
            ),
            256,
        );
    }
}

/// Halves the launch's slots and its context to match. False when it is at one already.
pub(crate) fn halve_launch(launch: &mut Launch<'_>, per_slot_ctx: u32) -> bool {
    if launch.parallel <= 1 {
        return false;
    }
    launch.parallel = halve(launch.parallel);
    launch.ctx_size = launch.parallel * per_slot_ctx;
    true
}

/// Starts the server (`restart`: starts it again, reporting a failure as a broken connection); a
/// start that fails out of memory is tried again at half the slots, down to one. `on_halve`
/// hears about each halving. Returns the load time (0 for a restart).
pub(crate) fn start_halving(
    llama: &mut Llama,
    launch: &mut Launch<'_>,
    per_slot_ctx: u32,
    load_deadline: Duration,
    restart: bool,
    on_halve: &mut dyn FnMut(&Launch<'_>),
) -> anyhow::Result<u64> {
    loop {
        let r = if restart {
            llama.restart(launch, load_deadline).map(|()| 0)
        } else {
            llama.start(launch, load_deadline)
        };
        match r {
            Ok(ms) => return Ok(ms),
            Err(e) => {
                let log = std::fs::read_to_string(launch.work.join("llama-server.log"))
                    .unwrap_or_default();
                if launch.parallel > 1 && (is_oom(&format!("{e:#}")) || is_oom(&log)) {
                    halve_launch(launch, per_slot_ctx);
                    on_halve(launch);
                    continue;
                }
                return Err(e);
            }
        }
    }
}

struct RegionState {
    seen: u64,
}

fn worker(sh: &Shared<'_, '_>) {
    let min_tokens = image_min_tokens(sh.job.prompt_version);
    let guard = loop_guard(sh.job.prompt_version);
    loop {
        if sh.stop.load(Ordering::SeqCst) {
            return;
        }
        if sh.control.wait_if_suspended_or_should_exit().is_break() {
            sh.end(Ending::Exit);
            return;
        }
        if sh.started.elapsed() >= sh.deadline {
            sh.end(Ending::Timeout(format!(
                "deadline {}s reached",
                sh.deadline.as_secs()
            )));
            return;
        }
        let Some((idx, png)) = sh.next() else {
            return;
        };
        let crop = &sh.job.crops[idx];
        let png = match png {
            Ok(p) => p,
            Err(e) => {
                sh.diag
                    .lock()
                    .unwrap()
                    .section("crop error", &format!("{}: {e:#}", crop.id), 1024);
                sh.end(Ending::Failed(format!("{}: {e:#}", crop.id)));
                return;
            }
        };
        let t_crop = Instant::now();
        let max_tokens = sh.job.max_tokens(crop);
        // Once the server has been lost in this task, however long ago and whichever region
        // lost it, a blank 0-token reply may be the damage of the crash (a reset GPU answers
        // nothing), so none is believed. The counts are in the checkpoint, so this holds after
        // the task is restarted too; it does not depend on any one region's notes.
        let distrust = { server_was_lost(&sh.cp.lock().unwrap()) };
        let mut st = RegionState { seen: 0 };
        let end = run_crop(
            &mut st,
            |st| {
                let llama = sh.llama.lock().unwrap();
                st.seen = sh.generation.load(Ordering::SeqCst);
                let port = llama.port;
                drop(llama);
                chat_completion_for(
                    port,
                    &png,
                    sh.job.prompt_version,
                    max_tokens,
                    min_tokens,
                    guard,
                    sh.deadline.saturating_sub(sh.started.elapsed()),
                )
            },
            |(text, tokens, _)| text.trim().is_empty() && *tokens == 0,
            distrust,
            |st| {
                // Lost, or replaced since this request began, or lost earlier in this task:
                // asked again when a blank reply arrives, since another reader may have
                // restarted the server after `distrust` was read.
                let gone = sh.llama.lock().unwrap().has_exited();
                gone || sh.generation.load(Ordering::SeqCst) != st.seen
                    || server_was_lost(&sh.cp.lock().unwrap())
            },
            |st| sh.restart(&mut st.seen),
            || sh.control.should_exit().is_some() || sh.started.elapsed() >= sh.deadline,
            |note| {
                let mut cp = sh.cp.lock().unwrap();
                cp.note_restart(&format!("{}: {note}", crop.id));
                let mut diag = sh.diag.lock().unwrap();
                diag.section(&format!("llama-server restart ({})", crop.id), note, 1024);
                if let Ok(log_text) = std::fs::read_to_string(sh.work.join("llama-server.log")) {
                    diag.section(
                        "llama-server.log before restart",
                        &redacted_tail(&log_text, 1024),
                        1024,
                    );
                }
            },
        );
        match end {
            CropEnd::Done((text, tokens, metadata)) => {
                let mut cp = sh.cp.lock().unwrap();
                // The readings file is bounded; a page that would not fit is not a success.
                let reading = Reading {
                    id: crop.id.clone(),
                    ms: crate::ms(t_crop),
                    text,
                    tokens,
                };
                let used = sh.readings_bytes.load(Ordering::SeqCst);
                if !fits_readings(used, &reading) {
                    drop(cp);
                    sh.diag.lock().unwrap().section(
                        "readings over the size limit",
                        &format!("{}: no room for another reading", crop.id),
                        256,
                    );
                    sh.end(Ending::Failed(format!(
                        "{}: readings over the size limit",
                        crop.id
                    )));
                    return;
                }
                sh.readings_bytes
                    .store(used + reading_json_len(&reading), Ordering::SeqCst);
                cp.generation_metadata.insert(crop.id.clone(), metadata);
                cp.readings.push(reading);
                cp.run_ms = sh.load_cost_ms + crate::ms(sh.started);
                let _ = cp.save(sh.work);
                // The checkpoint stays locked until the progress is written, so two readers
                // finishing together do not write the same file at once or out of order.
                if let Some(dev) = sh.gpu.cuda_device {
                    if let Some(used) = crate::gpu::gpu_used_mib(dev) {
                        let delta = used.saturating_sub(sh.gpu.used_before_mb.unwrap_or(0));
                        let mut p = sh.peak.lock().unwrap();
                        *p = Some(p.map_or(delta, |p| p.max(delta)));
                    }
                }
                let frac = cp.readings.len() as f64 / sh.total.max(1) as f64;
                let _ = write_atomic(
                    &sh.work.join(FRACTION_FILE),
                    format!("{frac:.4}\n").as_bytes(),
                );
            }
            CropEnd::GiveUp(e)
                if sh.control.should_exit().is_none() && sh.started.elapsed() < sh.deadline =>
            {
                sh.end(Ending::GiveUp(crop.id.clone(), e));
                return;
            }
            CropEnd::Failed(e) | CropEnd::GiveUp(e) => {
                if sh.control.should_exit().is_some() {
                    sh.end(Ending::Exit);
                } else {
                    let msg = format!("{}: {e:#}", crop.id);
                    sh.diag
                        .lock()
                        .unwrap()
                        .section("chat completion failed", &msg, 1024);
                    if sh.started.elapsed() >= sh.deadline {
                        sh.end(Ending::Timeout(msg));
                    } else {
                        sh.end(Ending::Failed(msg));
                    }
                }
                return;
            }
        }
    }
}

/// What a page-mode read needs from the task.
pub(crate) struct PageRun<'a> {
    pub(crate) work: &'a Path,
    pub(crate) job: &'a Job,
    pub(crate) deadline: Duration,
    pub(crate) started: Instant,
    pub(crate) control: &'a BoincControl,
    pub(crate) gpu: &'a crate::gpu::GpuState,
    pub(crate) per_slot_ctx: u32,
    /// GPU time spent starting the server, failed starts included.
    pub(crate) load_cost_ms: u64,
}

/// Reads the regions the checkpoint does not have, `launch.parallel` at a time, longest first.
/// Readings are added to the checkpoint as they finish, in completion order.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_regions<'a>(
    run: &PageRun<'a>,
    pages: &'a mut usnm_ocr_quality::qwen_pages::QwenPages,
    page_bytes: &'a [u8],
    llama: &mut Llama,
    launch: &mut Launch<'a>,
    cp: &mut QwenCheckpoint,
    diag: &mut Diag,
) -> PageEnd {
    let done: BTreeSet<&str> = cp.readings.iter().map(|r| r.id.as_str()).collect();
    let pending: VecDeque<usize> = longest_first(&run.job.crops, &done).into();
    let have_bytes: usize = cp.readings.iter().map(reading_json_len).sum();
    let sh = Shared {
        job: run.job,
        work: run.work,
        deadline: run.deadline,
        started: run.started,
        control: run.control,
        gpu: run.gpu,
        per_slot_ctx: run.per_slot_ctx,
        feed: Mutex::new(Feed {
            pending,
            pages,
            page_bytes,
        }),
        llama: Mutex::new(llama),
        launch: Mutex::new(launch),
        generation: AtomicU64::new(0),
        cp: Mutex::new(cp),
        diag: Mutex::new(diag),
        stop: AtomicBool::new(false),
        ending: Mutex::new(None),
        peak: Mutex::new(None),
        halvings: AtomicU64::new(0),
        total: run.job.crops.len(),
        load_cost_ms: run.load_cost_ms,
        readings_bytes: AtomicUsize::new(have_bytes),
    };
    let readers = sh.launch.lock().unwrap().parallel.max(1);
    std::thread::scope(|s| {
        for _ in 0..readers {
            s.spawn(|| worker(&sh));
        }
    });
    let slots = sh.launch.lock().unwrap().parallel;
    PageEnd {
        ending: sh.ending.into_inner().unwrap(),
        gpu_peak_mib: sh.peak.into_inner().unwrap(),
        slots,
        halvings: sh.halvings.load(Ordering::SeqCst) as u32,
    }
}

/// Whether the task has lost its `llama-server` before, in this run or an earlier one.
pub(crate) fn server_was_lost(cp: &QwenCheckpoint) -> bool {
    cp.llama_restarts > 0 || cp.temporary_exits > 0
}

/// Bytes the reading takes in the pretty-printed readings.json, escaping included.
pub(crate) fn reading_json_len(r: &Reading) -> usize {
    let one = serde_json::to_vec_pretty(r).map_or(usize::MAX / 4, |v| v.len());
    let lines =
        serde_json::to_vec_pretty(r).map_or(0, |v| v.iter().filter(|&&b| b == b'\n').count() + 1);
    // Nested two levels deeper than on its own, and followed by a comma.
    one.saturating_add(4 * lines).saturating_add(2)
}

/// Whether one more reading still fits in readings.json, given the bytes the others take.
pub(crate) fn fits_readings(used: usize, next: &Reading) -> bool {
    used.saturating_add(reading_json_len(next)) <= READINGS_BUDGET
}

/// `cp.readings` in the job's crop order.
pub(crate) fn in_crop_order(job: &Job, readings: Vec<Reading>) -> Vec<Reading> {
    let order: std::collections::BTreeMap<&str, usize> = job
        .crops
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    let mut v = readings;
    v.sort_by_key(|r| order.get(r.id.as_str()).copied().unwrap_or(usize::MAX));
    v
}

/// Completion tokens per page of the unit, in `job.pages` order (one page for a single-page
/// unit).
pub(crate) fn tokens_by_page(job: &Job, readings: &[Reading]) -> Vec<u64> {
    if job.pages.is_empty() {
        return vec![readings.iter().map(|r| u64::from(r.tokens)).sum()];
    }
    job.pages
        .iter()
        .map(|p| {
            let prefix = format!("{}:", p.doc_id);
            readings
                .iter()
                .filter(|r| r.id.starts_with(&prefix))
                .map(|r| u64::from(r.tokens))
                .sum()
        })
        .collect()
}

/// `0=ms,1=ms,...` in chunks short enough for a readings `diag` value.
pub(crate) fn gpu_ms_entries(shares: &[u64]) -> Vec<(String, String)> {
    shares
        .chunks(16)
        .enumerate()
        .map(|(n, chunk)| {
            let key = if n == 0 {
                "gpu_ms_by_page".to_owned()
            } else {
                format!("gpu_ms_by_page_{}", n + 1)
            };
            let first = n * 16;
            let value = chunk
                .iter()
                .enumerate()
                .map(|(i, ms)| format!("{}={ms}", first + i))
                .collect::<Vec<_>>()
                .join(",");
            (key, value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn the_slot_rule_follows_free_memory_and_clamps() {
        // 24 GB free, 10 GB fixed, 1 GB per slot: (24000 * 0.85 - 10000) / 1000 = 10.4 -> 8.
        assert_eq!(slots_for(Some(24_000), 10_000, 1000), 8);
        // 16 GB free: (13600 - 10000) / 1000 = 3.6 -> 3.
        assert_eq!(slots_for(Some(16_000), 10_000, 1000), 3);
        // The margin counts: 10,000 + 2 * 1000 needs 14,118 MiB free, not 12,000.
        assert_eq!(slots_for(Some(12_000), 10_000, 1000), 1);
        assert_eq!(slots_for(Some(14_117), 10_000, 1000), 1);
        assert_eq!(slots_for(Some(14_118), 10_000, 1000), 2);
        assert_eq!(slots_for(Some(16_470), 10_000, 1000), 3);
        assert_eq!(slots_for(Some(16_471), 10_000, 1000), 4);
        // A card that fits the model, but not a slot, still runs one.
        assert_eq!(slots_for(Some(10_500), 10_000, 1000), 1);
        assert_eq!(slots_for(Some(4_000), 10_000, 1000), 1);
        // Unknown memory is one slot; the ceiling is 8.
        assert_eq!(slots_for(None, 10_000, 1000), 1);
        assert_eq!(slots_for(Some(1 << 20), 10_000, 1), 8);
    }

    #[test]
    fn halving_stops_at_one() {
        let mut n = 8;
        let mut seen = vec![];
        while n > 1 {
            n = halve(n);
            seen.push(n);
        }
        assert_eq!(seen, [4, 2, 1]);
        assert_eq!(halve(1), 1);
        assert_eq!(halve(3), 1);
    }

    #[test]
    fn out_of_memory_is_recognised_in_the_wording_of_each_backend() {
        for t in [
            "CUDA error: out of memory",
            "ggml_vulkan: Device memory allocation failed: ErrorOutOfDeviceMemory",
            "cudaMalloc failed: out of memory",
            "failed to allocate buffer of size 123",
            "llama_init: OOM",
        ] {
            assert!(is_oom(t), "{t}");
        }
        for t in [
            "",
            "model loaded",
            "room for more",
            "listening on 127.0.0.1",
        ] {
            assert!(!is_oom(t), "{t}");
        }
    }

    #[test]
    fn gpu_time_splits_by_token_share_and_adds_up() {
        assert_eq!(split_ms(1000, &[100, 300]), [250, 750]);
        let s = split_ms(1001, &[1, 1, 1]);
        assert_eq!(s.iter().sum::<u64>(), 1001);
        assert!(s.iter().all(|&x| (333..=334).contains(&x)));
        // No tokens at all: even shares. One page gets everything.
        assert_eq!(split_ms(10, &[0, 0]), [5, 5]);
        assert_eq!(split_ms(77, &[5]), [77]);
        assert!(split_ms(5, &[]).is_empty());
        let s = split_ms(u64::MAX, &[1, 2, 3]);
        assert_eq!(
            s.iter().map(|&x| u128::from(x)).sum::<u128>(),
            u128::from(u64::MAX)
        );
    }

    #[test]
    fn page_shares_are_listed_in_short_chunks() {
        let e = gpu_ms_entries(&[1, 2, 3]);
        assert_eq!(e, [("gpu_ms_by_page".into(), "0=1,1=2,2=3".into())]);
        let big = vec![123_456_789u64; 20];
        let e = gpu_ms_entries(&big);
        assert_eq!(e.len(), 2);
        assert!(e.iter().all(|(_, v)| v.len() <= 256));
        assert!(e[1].1.starts_with("16="));
    }

    #[test]
    fn regions_are_read_longest_first_and_the_checkpoint_ones_skipped() {
        let c = |id: &str, t: &str| Crop {
            rect: [0, 0, 1, 1],
            id: id.into(),
            loc_text: t.into(),
            stratum: "region".into(),
        };
        let crops = [c("a", "xx"), c("b", "xxxxxx"), c("c", "xx"), c("d", "xxxx")];
        assert_eq!(longest_first(&crops, &BTreeSet::new()), [1, 3, 0, 2]);
        let done: BTreeSet<&str> = ["b"].into();
        assert_eq!(longest_first(&crops, &done), [3, 0, 2]);
        // A bundle reads its pages one after the other.
        let bundle = [
            c("p1:a", "xx"),
            c("p2:a", "xxxxxx"),
            c("p1:b", "xxxx"),
            c("p2:b", "x"),
        ];
        assert_eq!(longest_first(&bundle, &BTreeSet::new()), [2, 0, 1, 3]);
    }

    /// A GGUF header with the keys the shape needs, and a string array like a tokenizer.
    fn gguf(kv_heads: &[u32], with_dims: bool) -> Vec<u8> {
        fn s(b: &mut Vec<u8>, v: &str) {
            b.extend((v.len() as u64).to_le_bytes());
            b.extend(v.as_bytes());
        }
        let mut b = b"GGUF".to_vec();
        b.extend(3u32.to_le_bytes());
        b.extend(0u64.to_le_bytes());
        let n_kv: u64 = if with_dims { 7 } else { 6 };
        b.extend(n_kv.to_le_bytes());
        // A tokenizer-like string array first, to be skipped.
        s(&mut b, "tokenizer.ggml.tokens");
        b.extend(9u32.to_le_bytes());
        b.extend(8u32.to_le_bytes());
        b.extend(3u64.to_le_bytes());
        for t in ["a", "bb", "ccc"] {
            s(&mut b, t);
        }
        s(&mut b, "general.architecture");
        b.extend(8u32.to_le_bytes());
        s(&mut b, "qwen3vl");
        for (k, v) in [
            ("qwen3vl.block_count", 28u32),
            ("qwen3vl.attention.head_count", 32),
            ("qwen3vl.embedding_length", 4096),
        ] {
            s(&mut b, k);
            b.extend(4u32.to_le_bytes());
            b.extend(v.to_le_bytes());
        }
        s(&mut b, "qwen3vl.attention.head_count_kv");
        if let [one] = kv_heads {
            b.extend(4u32.to_le_bytes());
            b.extend(one.to_le_bytes());
        } else {
            b.extend(9u32.to_le_bytes());
            b.extend(4u32.to_le_bytes());
            b.extend((kv_heads.len() as u64).to_le_bytes());
            for h in kv_heads {
                b.extend(h.to_le_bytes());
            }
        }
        if with_dims {
            s(&mut b, "qwen3vl.attention.key_length");
            b.extend(4u32.to_le_bytes());
            b.extend(64u32.to_le_bytes());
        }
        b
    }

    #[test]
    fn the_kv_cost_comes_from_the_gguf_header() {
        // 28 layers x 8 KV heads, head_dim 4096 / 32 = 128 for both K and V.
        let g = Gguf::parse(&mut Cursor::new(gguf(&[8], false))).unwrap();
        assert_eq!(g.kv_heads_total, 224);
        assert_eq!((g.key_len, g.value_len), (128, 128));
        // 2 (K and V) x 224 x 128 x 2 bytes per token; 8192 tokens = 896 MiB.
        assert_eq!(g.kv_bytes_per_token(), 2 * 224 * 128 * 2);
        assert_eq!(g.kv_per_slot_mib(8192), 896);
        // A per-layer list sums; an explicit key_length replaces the default for K only.
        let per_layer = vec![4u32; 28];
        let g = Gguf::parse(&mut Cursor::new(gguf(&per_layer, true))).unwrap();
        assert_eq!(g.kv_heads_total, 112);
        assert_eq!((g.key_len, g.value_len), (64, 128));
    }

    #[test]
    fn a_file_that_is_not_gguf_has_no_shape() {
        assert!(Gguf::parse(&mut Cursor::new(b"fake-gguf-weights".to_vec())).is_err());
        let mut cut = gguf(&[8], false);
        cut.truncate(60);
        assert!(Gguf::parse(&mut Cursor::new(cut)).is_err());
    }

    #[test]
    fn the_plan_forces_clamps_or_computes_the_slots() {
        let tmp = tempfile::tempdir().unwrap();
        let model = tmp.path().join("m.gguf");
        let mmproj = tmp.path().join("p.gguf");
        std::fs::write(&model, gguf(&[8], false)).unwrap();
        std::fs::write(&mmproj, b"x").unwrap();
        // 896 MiB per slot; the files are tiny, so fixed is the 1536 MiB reserve.
        let p = plan_slots(None, true, Some(10_000), &model, &mmproj);
        assert_eq!((p.kv_per_slot_mib, p.fixed_mib), (896, 1538));
        assert_eq!(p.slots, ((10_000.0 * 0.85 - 1538.0) / 896.0) as u32);
        assert!(!p.forced);
        assert_eq!(plan_slots(Some(4), false, None, &model, &mmproj).slots, 4);
        assert_eq!(plan_slots(Some(99), true, None, &model, &mmproj).slots, 8);
        assert_eq!(
            plan_slots(None, false, Some(99_999), &model, &mmproj).slots,
            1
        );
        // No readable shape: one slot unless forced.
        std::fs::write(&model, b"fake").unwrap();
        let p = plan_slots(None, true, Some(99_999), &model, &mmproj);
        assert_eq!(p.slots, 1);
        assert!(p.note.contains("unreadable"));
    }

    #[test]
    fn a_page_whose_readings_would_not_fit_the_file_is_not_a_success() {
        // Tabs and quotes double in JSON, and every reading carries its id and the pretty-printing.
        let r = |i: usize| Reading {
            id: format!("r{i:04}"),
            ms: 123_456,
            text: "\t\"x\n".repeat(2 * 1024),
            tokens: 2048,
        };
        let mut have = Vec::new();
        let mut used = 0;
        while fits_readings(used, &r(have.len())) {
            used += reading_json_len(&r(have.len()));
            have.push(r(have.len()));
        }
        assert!(have.len() > 100 && have.len() < 1024, "{}", have.len());
        // What is accepted really does fit: the whole file, header and all.
        let file = usnm_ocr_contract::qwen::Readings {
            v: 1,
            job_id: "a".repeat(64),
            device: "vulkan".into(),
            diag: Default::default(),
            model: Some("xiaomi-ocr-0-q8".into()),
            outcome: usnm_ocr_contract::qwen::Outcome::Ok,
            prompt_version: 5,
            readings: have,
        };
        let size = serde_json::to_vec_pretty(&file).unwrap().len();
        assert!(size <= usnm_ocr_contract::qwen::MAX_READINGS, "{size}");
        assert!(
            size <= used + 4096,
            "the estimate never undercounts: {size} > {used}"
        );
        assert!(fits_readings(0, &r(0)));
    }

    #[test]
    fn a_blank_reply_is_distrusted_once_the_server_was_lost() {
        let job = crate::test_support::write_xiaomi_unit(tempfile::tempdir().unwrap().path());
        let mut cp = QwenCheckpoint {
            v: 1,
            job_id: job.id,
            engine: job.engine,
            model: "xiaomi-ocr-0-q8".into(),
            device: "cpu".into(),
            readings: vec![],
            generation_metadata: Default::default(),
            temporary_exits: 0,
            llama_restarts: 0,
            gpu_ms: 0,
            run_ms: 0,
            restart_log: vec![],
            crop_errors: vec![],
        };
        assert!(!server_was_lost(&cp));
        cp.llama_restarts = 1;
        assert!(server_was_lost(&cp));
        cp.llama_restarts = 0;
        cp.temporary_exits = 1;
        assert!(server_was_lost(&cp));
    }

    #[test]
    fn readings_are_put_back_in_crop_order_and_tokens_by_page() {
        let c = |id: &str| Crop {
            rect: [0, 0, 1, 1],
            id: id.into(),
            loc_text: String::new(),
            stratum: "region".into(),
        };
        let mut job = crate::test_support::write_xiaomi_unit(tempfile::tempdir().unwrap().path());
        job.crops = vec![c("r1"), c("r2"), c("r3")];
        let r = |id: &str, tokens| Reading {
            id: id.into(),
            ms: 1,
            text: "t".into(),
            tokens,
        };
        let out = in_crop_order(&job, vec![r("r3", 3), r("r1", 1), r("r2", 2)]);
        assert_eq!(
            out.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["r1", "r2", "r3"]
        );
        assert_eq!(tokens_by_page(&job, &out), [6]);
    }
}
