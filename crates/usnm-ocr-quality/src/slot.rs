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
    collections::{BTreeMap, BTreeSet},
    io::{Read as _, Write as _},
    ops::ControlFlow,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use usnm_alto::{Page, ParseError, ParseOptions};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{
        ConfusionPair, Engine, Host, Job, Outcome, PageResult, ReaderSpec, ResultManifest, Texts,
        Timings, MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS, MAX_ERROR,
        MAX_JOB_JSON, MAX_PLACES, MAX_SCAN, MAX_TEXTS,
    },
};

mod control;
mod diag;
mod gpu_cache;

#[cfg(windows)]
pub use control::WinShmem;
pub use control::{
    parse_comm_obj_name, write_mmap_channel, BoincControl, BoincRuntime, ChildAction, ChildHook,
    APP_STATUS_OFFSET, HEARTBEAT_OFFSET, HEARTBEAT_TIMEOUT, MSG_CHANNEL_SIZE,
    PROCESS_CONTROL_REPLY_OFFSET, PROCESS_CONTROL_REQUEST_OFFSET, SHARED_MEM_SIZE,
};
pub use diag::{boinc_info, runtime_info, tail, Diag};
pub use gpu_cache::{
    apply_gpu_cache_env, gpu_threads, persistent_gpu_cache_dir, DEFAULT_CUDA_CACHE_MAXSIZE,
    DEFAULT_MESA_SHADER_CACHE_MAX_SIZE,
};

#[cfg(test)]
use gpu_cache::ensure_writable_cache_pair;

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
pub const CHECKPOINT_FILE: &str = "quality_checkpoint.json";

/// Embedded BOINC `API_VERSION_` marker (`lib/boinc_api.cpp` convention) so
/// `update_versions` (`strings | grep 'API_VERSION_[0-9]+\.[0-9]+\.[0-9]+$'`)
/// and the BOINC client recognize the 6.0+ file-backed `boinc_mmap_file` protocol.
pub const BOINC_API_VERSION: &str = "8.3.0";
#[used]
pub static BOINC_API_VERSION_MARKER: [u8; 18] = *b"API_VERSION_8.3.0\0";

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

/// Normalizes early NDNP ALTO 1.x files (`http://schema.ccs-gmbh.com/ALTO`)
/// to the LoC ALTO v2 namespace (`http://www.loc.gov/standards/alto/ns-v2#`)
/// so `usnm_alto::check` and `usnm_alto::parse` accept 2008-era NDNP batches.
pub fn normalize_alto_ns(xml: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    const CCS_NS: &[u8] = b"http://schema.ccs-gmbh.com/ALTO";
    const V2_NS: &[u8] = b"http://www.loc.gov/standards/alto/ns-v2#";
    let head_len = xml.len().min(4096);
    if let Some(pos) = xml[..head_len]
        .windows(CCS_NS.len())
        .position(|w| w == CCS_NS)
    {
        let mut out = Vec::with_capacity(xml.len() + V2_NS.len() - CCS_NS.len());
        out.extend_from_slice(&xml[..pos]);
        out.extend_from_slice(V2_NS);
        out.extend_from_slice(&xml[pos + CCS_NS.len()..]);
        std::borrow::Cow::Owned(out)
    } else {
        std::borrow::Cow::Borrowed(xml)
    }
}

/// Reads an ALTO into a page in the image's pixels. LoC's ALTO is in
/// 1/1200 inch; it is read at 1200 dpi (its own units) and then scaled to
/// the image by the page's width and height, as the gold set's `crops.py`
/// scales it, so the units never matter.
pub fn parse_alto(xml: &[u8], width: f64, height: f64) -> anyhow::Result<Page> {
    ensure!(width > 0.0 && height > 0.0, "page has no size");
    let xml = normalize_alto_ns(xml);
    let opts = ParseOptions::default();
    let mut page = match usnm_alto::parse(&xml, &opts) {
        Ok(p) => p,
        Err(ParseError::NeedsDpi(_)) => usnm_alto::parse(
            &xml,
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

/// Per-page checkpoint state persisted atomically in `quality_checkpoint.json`
/// so multi-page workunits resume from the last completed page after BOINC
/// preemption or client restart.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityCheckpoint {
    pub v: u32,
    pub job_id: String,
    pub engine: Engine,
    pub pages: Vec<PageResult>,
    pub confusions: BTreeMap<String, Vec<ConfusionPair>>,
}

impl QualityCheckpoint {
    pub fn load_valid(work: &Path, job: &Job) -> Option<Self> {
        let bytes = std::fs::read(work.join(CHECKPOINT_FILE)).ok()?;
        let cp: Self = serde_json::from_slice(&bytes).ok()?;
        if cp.v != usnm_ocr_contract::quality::VERSION
            || cp.job_id != job.id
            || cp.engine != job.engine
            || cp.pages.len() > job.pages.len()
        {
            return None;
        }
        for (got, want) in cp.pages.iter().zip(&job.pages) {
            if got.doc_id != want.doc_id {
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

/// Each reader's text as the unit compares it: from `texts.json`, with
/// LoC's ALTO text (`alto_text`, called at most once) standing in for an
/// empty or `none` LoC text. `collect` calls this too, so the server's hat
/// sees the texts the unit saw. `alto_text` returns none when the ALTO is
/// not available (a staged `loc.xml` truncated after submission, say);
/// then, if a LoC reader needs it, the texts the unit read can't be
/// rebuilt and this returns none. An ALTO that parses to no words is
/// `Some("")`, the text the unit used.
pub fn reader_inputs(
    readers_spec: &[ReaderSpec],
    texts: &Texts,
    alto_text: impl FnOnce() -> Option<String>,
) -> Option<Vec<ReaderInput>> {
    let mut alto_text = Some(alto_text);
    let mut alto: Option<Option<String>> = None;
    readers_spec
        .iter()
        .map(|spec| {
            let mut text = texts
                .readers
                .iter()
                .find(|r| r.name == spec.name)
                .map(|r| r.text.clone())
                .unwrap_or_default();
            if (spec.name == "loc" || spec.name == "loc-hist")
                && (text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none"))
            {
                let got = alto.get_or_insert_with(|| alto_text.take().and_then(|f| f()));
                text = got.clone()?;
            }
            Some(ReaderInput {
                name: spec.name.clone(),
                source: spec.source.clone(),
                text,
            })
        })
        .collect()
}

/// The text part of a page's record, as the unit computes it: the readers'
/// texts by [`reader_inputs`] compared with no image and no ALTO. It holds
/// the readers' records, the hat and the legibility; the scan's features
/// are not in it. `quality rescore` calls this too, so a page rescored on
/// the server from corrected texts gets the numbers a unit would give.
/// None when a LoC reader needs the ALTO text and `alto_text` has none.
pub fn text_analysis(
    readers_spec: &[ReaderSpec],
    function_words_list: &[String],
    texts: &Texts,
    places: &Places,
    alto_text: impl FnOnce() -> Option<String>,
) -> Option<report::Analysis> {
    let readers = reader_inputs(readers_spec, texts, alto_text)?;
    let function_words: BTreeSet<String> = function_words_list
        .iter()
        .map(|w| crate::consensus::fold(w))
        .collect();
    Some(report::analyse(&PageInputs {
        image: None,
        alto: None,
        readers,
        function_words,
        places,
    }))
}

fn compute_page_record(
    doc_id: &str,
    readers_spec: &[ReaderSpec],
    function_words_list: &[String],
    page_bytes: &[u8],
    alto_bytes: &[u8],
    texts: &Texts,
    places: &Places,
) -> anyhow::Result<(PageResult, BTreeMap<String, Vec<ConfusionPair>>)> {
    let started = Instant::now();
    texts.validate_readers(readers_spec)?;
    let t = Instant::now();
    let image = image::load_from_memory(page_bytes)
        .with_context(|| format!("decoding page.jpg for {doc_id}"))?
        .to_luma8();
    let (w, h) = image.dimensions();
    let alto = parse_alto(alto_bytes, f64::from(w), f64::from(h))
        .with_context(|| format!("parsing ALTO for {doc_id}"))?;
    let decode_ms = ms(t);
    let t = Instant::now();
    let page = PageFeatures {
        image: Some(report::round_image(features::image_features(&image))),
        layout: Some(report::round_layout(features::layout_features(&alto))),
    };
    let features_ms = ms(t);
    let t = Instant::now();
    let analysis = text_analysis(readers_spec, function_words_list, texts, places, || {
        Some(usnm_alto::text(&alto))
    })
    .context("the ALTO text is always available here")?;
    let text_ms = ms(t);
    let r = analysis.report;
    Ok((
        PageResult {
            doc_id: doc_id.to_owned(),
            hat: Some(r.hat),
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
        analysis.confusions,
    ))
}

/// The computation for one unit: deterministic, single-threaded.
pub fn compute(inputs: &Inputs) -> anyhow::Result<Computed> {
    let started = Instant::now();
    let job = inputs.job;
    job.validate()?;
    let places = Places::parse(inputs.places);
    if job.pages.is_empty() {
        let (pr, conf_map) = compute_page_record(
            &job.doc_id,
            &job.readers,
            &job.function_words,
            inputs.page,
            inputs.alto,
            inputs.texts,
            &places,
        )?;
        let confusions = serde_json::to_vec_pretty(&conf_map)?;
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
                hat: pr.hat,
                host: Host::default(),
                legibility: pr.legibility,
                outcome: Outcome::Ok,
                page: pr.page,
                pages: vec![],
                readers: pr.readers,
                timings: Timings {
                    total_ms: ms(started),
                    ..pr.timings
                },
                unit_error: None,
            },
            confusions,
        })
    } else {
        let mut page_results = Vec::with_capacity(job.pages.len());
        let mut all_confusions: BTreeMap<String, Vec<ConfusionPair>> = BTreeMap::new();
        let mut timings = Timings::default();
        let raw_texts = serde_json::to_vec(inputs.texts).unwrap_or_default();
        for p in &job.pages {
            let page_bytes = p.inputs.page.extract(inputs.page, "page")?;
            let alto_bytes = p.inputs.alto.extract(inputs.alto, "alto")?;
            let texts_bytes = p.inputs.texts.extract(&raw_texts, "texts");
            let page_texts: Texts = match texts_bytes {
                Ok(b) => serde_json::from_slice(b).context("bundle page texts.json")?,
                Err(_) => inputs.texts.clone(),
            };
            let fw = if p.function_words.is_empty() {
                &job.function_words
            } else {
                &p.function_words
            };
            let (pr, conf_map) = compute_page_record(
                &p.doc_id,
                &p.readers,
                fw,
                page_bytes,
                alto_bytes,
                &page_texts,
                &places,
            )?;
            timings.decode_ms += pr.timings.decode_ms;
            timings.features_ms += pr.timings.features_ms;
            timings.text_ms += pr.timings.text_ms;
            for (k, v) in conf_map {
                all_confusions.insert(format!("{}:{k}", p.doc_id), v);
            }
            page_results.push(pr);
        }
        timings.total_ms = ms(started);
        let confusions = serde_json::to_vec_pretty(&all_confusions)?;
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
                hat: None,
                host: Host::default(),
                legibility: None,
                outcome: Outcome::Ok,
                page: None,
                pages: page_results,
                readers: vec![],
                timings,
                unit_error: None,
            },
            confusions,
        })
    }
}

/// The unit's files, read and checked against the job.
pub struct Loaded {
    pub job: Job,
    pub page: Vec<u8>,
    pub alto: Vec<u8>,
    pub texts: Texts,
    pub bundle_texts: Vec<Texts>,
    pub places: String,
}

/// Checks the unit before anything runs: the job and every input's checksum.
pub fn prepare(work: &Path) -> anyhow::Result<Loaded> {
    let job_path = resolve_slot_file(work, JOB_FILE);
    let job: Job = serde_json::from_slice(&read_bounded(&job_path, MAX_JOB_JSON)?)?;
    job.validate()?;
    let (max_scan, max_alto, max_texts) = if job.pages.is_empty() {
        (MAX_SCAN, MAX_ALTO, MAX_TEXTS)
    } else {
        (MAX_BUNDLE_SCAN, MAX_BUNDLE_ALTO, MAX_BUNDLE_TEXTS)
    };
    let page = read_bounded(&resolve_slot_file(work, PAGE_FILE), max_scan)?;
    ensure!(
        job.inputs.page.matches(&page),
        "input page checksum mismatch"
    );
    let alto = read_bounded(&resolve_slot_file(work, ALTO_FILE), max_alto)?;
    ensure!(
        job.inputs.alto.matches(&alto),
        "input ALTO checksum mismatch"
    );
    let texts_bytes = read_bounded(&resolve_slot_file(work, TEXTS_FILE), max_texts)?;
    ensure!(
        job.inputs.texts.matches(&texts_bytes),
        "input texts checksum mismatch"
    );
    let (texts, bundle_texts) = if job.pages.is_empty() {
        let t: Texts = serde_json::from_slice(&texts_bytes).context("texts.json")?;
        t.validate(&job)?;
        (t, vec![])
    } else {
        let mut b_texts = Vec::with_capacity(job.pages.len());
        for p in &job.pages {
            p.inputs.page.extract(&page, "page")?;
            p.inputs.alto.extract(&alto, "alto")?;
            let tb = p.inputs.texts.extract(&texts_bytes, "texts")?;
            let t: Texts = serde_json::from_slice(tb)
                .with_context(|| format!("texts.json for {}", p.doc_id))?;
            t.validate_readers(&p.readers)?;
            b_texts.push(t);
        }
        let first = b_texts.first().cloned().unwrap_or(Texts {
            page: None,
            readers: vec![],
        });
        (first, b_texts)
    };
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
        bundle_texts,
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
        pages: vec![],
        readers: vec![],
        timings: Timings::default(),
        unit_error: error,
    }
}

fn run_on_worker_thread<T, F>(
    job: &Job,
    deadline: Duration,
    control: &BoincControl,
    f: F,
) -> Result<T, Box<(Job, anyhow::Error)>>
where
    T: Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
        return Err(Box::new((
            job.clone(),
            anyhow::anyhow!("boinc control requested exit ({reason})"),
        )));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("quality-compute".into())
        .stack_size(64 << 20)
        .spawn(move || {
            let _ = tx.send(f());
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
                job.clone(),
                anyhow::anyhow!("boinc control requested exit ({reason})"),
            )));
        }
        let remaining = deadline.saturating_sub(active_elapsed);
        if remaining.is_zero() {
            return Err(Box::new((
                job.clone(),
                anyhow::anyhow!("computation exceeded {} s", deadline.as_secs()),
            )));
        }
        let step_start = Instant::now();
        match rx.recv_timeout(poll_step.min(remaining)) {
            Ok(Ok(c)) => {
                if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
                    return Err(Box::new((
                        job.clone(),
                        anyhow::anyhow!("boinc control requested exit ({reason})"),
                    )));
                }
                return Ok(c);
            }
            Ok(Err(e)) => return Err(Box::new((job.clone(), e))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !control.is_suspended() {
                    active_elapsed += step_start.elapsed();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Box::new((
                    job.clone(),
                    anyhow::anyhow!("computation thread ended without a result"),
                )));
            }
        }
    }
}

fn compute_with_deadline(
    work: &Path,
    l: Loaded,
    deadline: Duration,
    control: &BoincControl,
    diag: &mut Diag,
) -> Result<Computed, Box<(Job, anyhow::Error)>> {
    if l.job.pages.is_empty() {
        let job = l.job.clone();
        return run_on_worker_thread(&job, deadline, control, move || {
            compute(&Inputs {
                job: &l.job,
                page: &l.page,
                alto: &l.alto,
                texts: &l.texts,
                places: &l.places,
            })
        });
    }

    let started = Instant::now();
    let job = l.job.clone();
    let total_pages = job.pages.len();
    let places = Places::parse(&l.places);
    let mut cp = match QualityCheckpoint::load_valid(work, &job) {
        Some(existing) => {
            if !existing.pages.is_empty() {
                diag.section(
                    "checkpoint",
                    &format!(
                        "resumed from checkpoint: {} of {} pages already complete",
                        existing.pages.len(),
                        total_pages
                    ),
                    256,
                );
                let frac = existing.pages.len() as f64 / total_pages.max(1) as f64;
                let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.6}\n").as_bytes());
            }
            existing
        }
        None => QualityCheckpoint {
            v: usnm_ocr_contract::quality::VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            pages: Vec::with_capacity(total_pages),
            confusions: BTreeMap::new(),
        },
    };

    for idx in cp.pages.len()..total_pages {
        if let ControlFlow::Break(reason) = control.wait_if_suspended_or_should_exit() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("boinc control requested exit ({reason})"),
            )));
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(Box::new((
                job,
                anyhow::anyhow!("computation exceeded {} s", deadline.as_secs()),
            )));
        }
        let bp = job.pages[idx].clone();
        let page_bytes = match bp.inputs.page.extract(&l.page, "page") {
            Ok(s) => s.to_vec(),
            Err(e) => return Err(Box::new((job, e))),
        };
        let alto_bytes = match bp.inputs.alto.extract(&l.alto, "alto") {
            Ok(s) => s.to_vec(),
            Err(e) => return Err(Box::new((job, e))),
        };
        let page_texts = l.bundle_texts[idx].clone();
        let fw = if bp.function_words.is_empty() {
            job.function_words.clone()
        } else {
            bp.function_words.clone()
        };
        let places_clone = places.clone();
        let doc_id = bp.doc_id.clone();
        let readers_spec = bp.readers.clone();
        let (pr, conf_map) = run_on_worker_thread(&job, remaining, control, move || {
            compute_page_record(
                &doc_id,
                &readers_spec,
                &fw,
                &page_bytes,
                &alto_bytes,
                &page_texts,
                &places_clone,
            )
        })?;
        for (k, v) in conf_map {
            cp.confusions.insert(format!("{}:{k}", bp.doc_id), v);
        }
        cp.pages.push(pr);
        let _ = cp.save(work);
        let frac = cp.pages.len() as f64 / total_pages.max(1) as f64;
        let _ = write_atomic(&work.join(FRACTION_FILE), format!("{frac:.6}\n").as_bytes());
    }

    let _ = std::fs::remove_file(work.join(CHECKPOINT_FILE));
    let mut timings = Timings::default();
    for p in &cp.pages {
        timings.decode_ms += p.timings.decode_ms;
        timings.features_ms += p.timings.features_ms;
        timings.text_ms += p.timings.text_ms;
        timings.total_ms += p.timings.total_ms;
    }
    let confusions = serde_json::to_vec_pretty(&cp.confusions)
        .map_err(|e| Box::new((job.clone(), anyhow::Error::from(e))))?;
    Ok(Computed {
        result: ResultManifest {
            v: usnm_ocr_contract::quality::VERSION,
            job_id: job.id,
            completed_at: String::new(),
            confusions_bytes: confusions.len() as u64,
            confusions_sha256: sha256(&confusions),
            diag_bytes: 0,
            diag_sha256: sha256(b""),
            engine: job.engine,
            hat: None,
            host: Host::default(),
            legibility: None,
            outcome: Outcome::Ok,
            page: None,
            pages: cp.pages,
            readers: vec![],
            timings,
            unit_error: None,
        },
        confusions,
    })
}

/// Runs a complete native `usnm_quality` unit in `work`, monitoring BOINC's
/// shared-memory lifecycle channels and writing `fraction_done`,
/// `confusions.json`, `diag.txt`, `result.json`, and `boinc_finish_called`.
pub fn run_slot(work: &Path) -> anyhow::Result<()> {
    run_slot_with_mode(work, true)
}

/// Runs a complete container `usnm_quality` unit in `work`, writing
/// `fraction_done`, `confusions.json`, `diag.txt`, and `result.json` while
/// leaving `boinc_mmap_file` and `boinc_finish_called` exclusively to
/// `docker_wrapper`.
pub fn run_container_slot(work: &Path) -> anyhow::Result<()> {
    run_slot_with_mode(work, false)
}

fn run_slot_with_mode(work: &Path, native_lifecycle: bool) -> anyhow::Result<()> {
    let confusions_out = resolve_slot_file(work, CONFUSIONS_FILE);
    let diag_out = resolve_slot_file(work, DIAG_FILE);
    let result_out = resolve_slot_file(work, RESULT_FILE);
    let fraction_out = work.join(FRACTION_FILE);
    let finish_out = work.join(FINISH_FILE);

    for p in [&confusions_out, &diag_out, &result_out, &fraction_out] {
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    if native_lifecycle && finish_out.exists() {
        std::fs::remove_file(&finish_out)?;
    }
    write_atomic(&fraction_out, b"0.0\n")?;

    let control = if native_lifecycle {
        BoincControl::start(work)
    } else {
        BoincControl::noop()
    };
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
            match compute_with_deadline(work, l, DEADLINE, &control, &mut diag) {
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
    if native_lifecycle {
        write_atomic(&finish_out, b"0\n")?;
    }
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

    #[test]
    fn boinc_startup_with_init_data_creates_mmap_channel_and_advertises_api_version() {
        assert_eq!(
            std::str::from_utf8(&BOINC_API_VERSION_MARKER[..17]).unwrap(),
            format!("API_VERSION_{BOINC_API_VERSION}")
        );
        let tmp = TestDir::new("init-creates-mmap");
        let slot = tmp.path();
        std::fs::write(slot.join("init_data.xml"), "<app_init_data/>\n").unwrap();
        let mmap = slot.join(MMAP_FILE);
        assert!(!mmap.exists());

        let control = BoincControl::start(slot);
        assert!(mmap.is_file());
        assert_eq!(mmap.metadata().unwrap().len(), SHARED_MEM_SIZE as u64);

        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
    }

    #[test]
    fn parse_comm_obj_name_extracts_segment_name() {
        assert_eq!(
            parse_comm_obj_name(
                "<app_init_data>\n<comm_obj_name>boinc_usnm_0</comm_obj_name>\n</app_init_data>"
            ),
            Some("boinc_usnm_0".to_string())
        );
        assert_eq!(
            parse_comm_obj_name("<app_init_data><comm_obj_name>  </comm_obj_name></app_init_data>"),
            None
        );
        assert_eq!(parse_comm_obj_name("<app_init_data/>"), None);
    }

    #[test]
    fn container_slot_leaves_mmap_and_finish_file_to_docker_wrapper() {
        let tmp = TestDir::new("container-slot");
        let slot = tmp.path();
        let mmap = slot.join(MMAP_FILE);
        write_mmap_channel(&mmap, PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>").unwrap();
        write_mmap_channel(&mmap, HEARTBEAT_OFFSET, "<heartbeat/>").unwrap();

        run_container_slot(slot).unwrap();
        // Container slot must not consume docker_wrapper's mmap messages or write boinc_finish_called.
        let bytes = std::fs::read(&mmap).unwrap();
        assert_eq!(bytes[PROCESS_CONTROL_REQUEST_OFFSET], 1);
        assert_eq!(bytes[HEARTBEAT_OFFSET], 1);
        assert!(!slot.join(FINISH_FILE).exists());
        assert!(slot.join(RESULT_FILE).is_file());
    }

    #[test]
    fn multipage_bundle_checkpoints_per_page_and_resumes_cleanly() {
        use usnm_ocr_contract::quality::{
            BundlePage, BundlePageInputs, BundleSlice, Input, Inputs, TextReader, VERSION,
        };

        let mut jpg1 = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(32, 32, image::Luma([220])))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg1),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut jpg2 = Vec::new();
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(32, 32, image::Luma([200])))
            .write_to(
                &mut std::io::Cursor::new(&mut jpg2),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        let alto1 = br#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
  <Layout><Page ID="P1" WIDTH="32" HEIGHT="32" PHYSICAL_IMG_NR="1">
    <PrintSpace HPOS="0" VPOS="0" WIDTH="32" HEIGHT="32">
      <TextBlock ID="B1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
        <TextLine ID="L1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
          <String ID="S1" CONTENT="washington" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12" WC="0.95"/>
        </TextLine>
      </TextBlock>
    </PrintSpace>
  </Page></Layout>
</alto>"#;
        let alto2 = br#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
  <Layout><Page ID="P2" WIDTH="32" HEIGHT="32" PHYSICAL_IMG_NR="2">
    <PrintSpace HPOS="0" VPOS="0" WIDTH="32" HEIGHT="32">
      <TextBlock ID="B1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
        <TextLine ID="L1" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12">
          <String ID="S1" CONTENT="baltimore" HPOS="2" VPOS="2" WIDTH="28" HEIGHT="12" WC="0.92"/>
        </TextLine>
      </TextBlock>
    </PrintSpace>
  </Page></Layout>
</alto>"#;

        let readers_spec = vec![
            ReaderSpec {
                name: "loc".into(),
                source: "loc".into(),
            },
            ReaderSpec {
                name: "surya".into(),
                source: "surya".into(),
            },
        ];
        let t1 = serde_json::to_vec(&Texts {
            page: None,
            readers: vec![
                TextReader {
                    name: "loc".into(),
                    source: "loc".into(),
                    text: "the washington news".into(),
                    alto: false,
                    regions: vec![],
                },
                TextReader {
                    name: "surya".into(),
                    source: "surya".into(),
                    text: "the washington news".into(),
                    alto: false,
                    regions: vec![],
                },
            ],
        })
        .unwrap();
        let t2 = serde_json::to_vec(&Texts {
            page: None,
            readers: vec![
                TextReader {
                    name: "loc".into(),
                    source: "loc".into(),
                    text: "the baltimore sun".into(),
                    alto: false,
                    regions: vec![],
                },
                TextReader {
                    name: "surya".into(),
                    source: "surya".into(),
                    text: "the baltimore sun".into(),
                    alto: false,
                    regions: vec![],
                },
            ],
        })
        .unwrap();

        let mut page_blob = Vec::new();
        let p_slice1 = BundleSlice::of(0, &jpg1, "s3://b/p1.jpg");
        page_blob.extend_from_slice(&jpg1);
        let p_slice2 = BundleSlice::of(page_blob.len() as u64, &jpg2, "s3://b/p2.jpg");
        page_blob.extend_from_slice(&jpg2);

        let mut alto_blob = Vec::new();
        let a_slice1 = BundleSlice::of(0, alto1, "s3://b/p1.xml");
        alto_blob.extend_from_slice(alto1);
        let a_slice2 = BundleSlice::of(alto_blob.len() as u64, alto2, "s3://b/p2.xml");
        alto_blob.extend_from_slice(alto2);

        let mut texts_blob = Vec::new();
        let t_slice1 = BundleSlice::of(0, &t1, "s3://b/t1.json");
        texts_blob.extend_from_slice(&t1);
        let t_slice2 = BundleSlice::of(texts_blob.len() as u64, &t2, "s3://b/t2.json");
        texts_blob.extend_from_slice(&t2);

        let places_bytes = b"washington\nbaltimore\n";
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            crop_seed: 7,
            doc_id: "bundle_p1".into(),
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
            },
            function_words: vec!["the".into()],
            group: "pilot".into(),
            inputs: Inputs {
                alto: Input::of(&alto_blob, "bundle:loc.xml"),
                page: Input::of(&page_blob, "bundle:page.jpg"),
                places: Input::of(places_bytes, "curated:places"),
                texts: Input::of(&texts_blob, "bundle:texts.json"),
            },
            k_crops: 6,
            language: "en".into(),
            pages: vec![
                BundlePage {
                    crop_seed: 7,
                    doc_id: "bundle_p1".into(),
                    function_words: vec![],
                    group: "pilot".into(),
                    inputs: BundlePageInputs {
                        alto: a_slice1,
                        page: p_slice1,
                        texts: t_slice1,
                    },
                    k_crops: 6,
                    language: "en".into(),
                    readers: readers_spec.clone(),
                },
                BundlePage {
                    crop_seed: 7,
                    doc_id: "bundle_p2".into(),
                    function_words: vec![],
                    group: "pilot".into(),
                    inputs: BundlePageInputs {
                        alto: a_slice2,
                        page: p_slice2,
                        texts: t_slice2,
                    },
                    k_crops: 6,
                    language: "en".into(),
                    readers: readers_spec,
                },
            ],
            readers: vec![],
        };
        job.id = job.identity().unwrap();

        let write_slot_inputs = |dir: &Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(PAGE_FILE), &page_blob).unwrap();
            std::fs::write(dir.join(ALTO_FILE), &alto_blob).unwrap();
            std::fs::write(dir.join(TEXTS_FILE), &texts_blob).unwrap();
            std::fs::write(dir.join(PLACES_FILE), places_bytes).unwrap();
            std::fs::write(dir.join(JOB_FILE), serde_json::to_vec_pretty(&job).unwrap()).unwrap();
        };

        let tmp = TestDir::new("multipage-bundle");
        let slot_full = tmp.path().join("full");
        write_slot_inputs(&slot_full);
        run_slot(&slot_full).unwrap();
        assert!(slot_full.join(FINISH_FILE).is_file());
        assert!(!slot_full.join(CHECKPOINT_FILE).exists());
        let res_full: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot_full.join(RESULT_FILE)).unwrap()).unwrap();
        let conf_full_bytes = std::fs::read(slot_full.join(CONFUSIONS_FILE)).unwrap();
        let diag_full_bytes = std::fs::read(slot_full.join(DIAG_FILE)).unwrap();
        assert_eq!(res_full.pages.len(), 2);
        res_full.check(&conf_full_bytes, &diag_full_bytes).unwrap();

        // Simulate resuming from a 1-page checkpoint on a second slot.
        let slot_resume = tmp.path().join("resume");
        write_slot_inputs(&slot_resume);
        let full_conf: BTreeMap<String, Vec<ConfusionPair>> =
            serde_json::from_slice(&conf_full_bytes).unwrap();
        let page1_conf: BTreeMap<String, Vec<ConfusionPair>> = full_conf
            .into_iter()
            .filter(|(k, _)| k.starts_with("bundle_p1:"))
            .collect();
        let cp = QualityCheckpoint {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            pages: vec![res_full.pages[0].clone()],
            confusions: page1_conf,
        };
        cp.save(&slot_resume).unwrap();
        assert!(slot_resume.join(CHECKPOINT_FILE).is_file());

        run_slot(&slot_resume).unwrap();
        assert!(!slot_resume.join(CHECKPOINT_FILE).exists());
        let res_resume: ResultManifest =
            serde_json::from_slice(&std::fs::read(slot_resume.join(RESULT_FILE)).unwrap()).unwrap();
        let conf_resume_bytes = std::fs::read(slot_resume.join(CONFUSIONS_FILE)).unwrap();
        let diag_resume_bytes = std::fs::read(slot_resume.join(DIAG_FILE)).unwrap();
        res_resume
            .check(&conf_resume_bytes, &diag_resume_bytes)
            .unwrap();
        usnm_ocr_contract::quality::agree(&res_full, &res_resume).unwrap();
        let diag_str = std::fs::read_to_string(slot_resume.join(DIAG_FILE)).unwrap();
        assert!(diag_str.contains("resumed from checkpoint: 1 of 2 pages already complete"));
    }

    #[test]
    fn gpu_cache_dir_and_gpu_threads_scale_outside_ephemeral_home() {
        let tmp = TestDir::new("gpu-cache");
        let dir = persistent_gpu_cache_dir(tmp.path());
        assert!(dir.join("cuda").is_dir());
        assert!(dir.join("mesa").is_dir());

        let mut cmd = std::process::Command::new("sh");
        apply_gpu_cache_env(&mut cmd, tmp.path());
        let envs: BTreeMap<String, String> = cmd
            .get_envs()
            .filter_map(|(k, v)| Some((k.to_str()?.to_owned(), v?.to_str()?.to_owned())))
            .collect();
        if std::env::var_os("CUDA_CACHE_PATH").is_none() {
            assert_eq!(
                envs.get("CUDA_CACHE_PATH").map(String::as_str),
                Some(dir.join("cuda").to_string_lossy().as_ref())
            );
        }
        if std::env::var_os("CUDA_CACHE_MAXSIZE").is_none() {
            assert_eq!(
                envs.get("CUDA_CACHE_MAXSIZE").map(String::as_str),
                Some(DEFAULT_CUDA_CACHE_MAXSIZE)
            );
        }
        if std::env::var_os("MESA_SHADER_CACHE_DIR").is_none() {
            assert_eq!(
                envs.get("MESA_SHADER_CACHE_DIR").map(String::as_str),
                Some(dir.join("mesa").to_string_lossy().as_ref())
            );
        }

        // Default 1-CPU GPU plan class on a 4-core host scales up to 4 threads.
        assert_eq!(gpu_threads(Some(1.0), Some(4)), 4);
        // 2-core host stays at 2 threads.
        assert_eq!(gpu_threads(Some(1.0), Some(2)), 2);
        // 16-core host with default 1-CPU GPU slot caps opportunistic threads at 4,
        // while an explicit <ncpus>6.0</ncpus> allocation scales to 6 (clamped at 8).
        assert_eq!(gpu_threads(Some(1.0), Some(16)), 4);
        assert_eq!(gpu_threads(Some(6.0), Some(16)), 6);
        assert_eq!(gpu_threads(Some(12.0), Some(16)), 8);
        assert_eq!(gpu_threads(None, None), 2);
    }

    #[cfg(unix)]
    #[test]
    fn gpu_cache_dir_passes_over_shared_and_linked_directories() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = TestDir::new("gpu-cache-private");

        // Created private.
        let own = tmp.path().join("own");
        assert!(ensure_writable_cache_pair(&own));
        let mode = std::fs::metadata(&own).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");

        // Open to group or others: passed over.
        let open = tmp.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!ensure_writable_cache_pair(&open));

        // A symlink, even to this user's own private directory: passed over.
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&own, &link).unwrap();
        assert!(!ensure_writable_cache_pair(&link));

        // A probe-named symlink is removed, never written through.
        let victim = tmp.path().join("victim");
        std::fs::write(&victim, b"keep").unwrap();
        let probe = own
            .join("cuda")
            .join(format!(".writable.{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &probe).unwrap();
        assert!(ensure_writable_cache_pair(&own));
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
        assert!(std::fs::symlink_metadata(&probe).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_comm_obj_name_shared_memory_controls_lifecycle() {
        let tmp = TestDir::new("win-comm-obj");
        let slot = tmp.path();
        let seg_name = format!("boinc_usnm_test_{}", std::process::id());
        let client_shm = WinShmem::attach_or_create(&seg_name, true).unwrap();
        std::fs::write(
            slot.join("init_data.xml"),
            format!("<app_init_data><comm_obj_name>{seg_name}</comm_obj_name></app_init_data>\n"),
        )
        .unwrap();

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<suspend/>")
            .unwrap();
        let control = BoincControl::start(slot);
        assert!(control.is_suspended());

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<resume/>")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.is_suspended() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!control.is_suspended());

        client_shm
            .write_channel(PROCESS_CONTROL_REQUEST_OFFSET, "<quit/>")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while control.should_exit().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(control.should_exit(), Some("quit"));
    }
}
