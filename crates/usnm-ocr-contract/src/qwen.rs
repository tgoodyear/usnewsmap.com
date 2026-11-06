//! File-based jobs for the vision-language readings on BOINC (#522), the
//! app `usnm_qwen`: one page per workunit, a stratified sample of crops cut
//! from LoC's ALTO, each transcribed by Qwen3-VL on llama.cpp's server at
//! temperature 0. The model is a reader, never a judge: the unit returns
//! its text per crop, and every number is computed from that text on the
//! server. The app has a GPU and a CPU version; the result records the
//! device and model every run used.
//!
//! Every way a run ends is an outcome, including a unit the host could not
//! run at all (`unit_error`): the result and its diagnostics still upload.
use crate::boinc::{is_sha256, sha256};
use crate::quality::{check_doc_id, check_name, BundleSlice, Input, MAX_BUNDLE_SCAN, MAX_SCAN};
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
/// `qwen.json`: the job holds the crops with LoC's text for each (single-page or multi-page bundle).
pub const MAX_JOB_JSON: usize = 512 * 1024;
/// `readings.json`.
pub const MAX_READINGS: usize = 2 * 1024 * 1024;
/// `result.json`.
pub const MAX_RESULT_JSON: usize = 64 * 1024;
/// `diag.txt`.
pub const MAX_DIAG: usize = 64 * 1024;
pub const MAX_ERROR: usize = 1024;
pub const MAX_CROPS: usize = crate::quality::MAX_CROPS as usize;
/// Maximum pages in a multi-page `usnm_qwen` bundle workunit (e.g. 3–4 pages on CPU, 15–20 on GPU).
pub const MAX_BUNDLE_PAGES: usize = 32;
/// Maximum total crops across a multi-page `usnm_qwen` bundle workunit.
pub const MAX_BUNDLE_CROPS: usize = 256;
pub const MAX_MODELS: usize = 4;
/// The longest deadline a unit, single page or bundle, may carry.
pub const MAX_DEADLINE_SECS: u32 = 14_400;
/// One crop's text, LoC's or the model's.
pub const MAX_CROP_TEXT: usize = 8 * 1024;
/// The transcription request version new jobs are staged with. A job's
/// `prompt_version` fixes the whole request a reading is made with: the
/// prompt text and how the crop is encoded for the model.
pub const PROMPT_VERSION: u32 = 1;
/// Request version 2: the same [`PROMPT`], with the vision encoder's
/// minimum image tokens raised to [`V2_IMAGE_MIN_TOKENS`]. Version 1 leaves
/// llama.cpp's default, which encodes a newspaper crop at its native
/// resolution of one token per 32x32 pixels (about 200-400 tokens), below
/// the 1,024 that llama.cpp says Qwen-VL needs; that is where the
/// unreadable, looping Hebrew-script readings came from (#529).
pub const PROMPT_VERSION_IMAGE_MIN_TOKENS: u32 = 2;
pub const V2_IMAGE_MIN_TOKENS: u32 = 1024;
/// Request version 3: version 1's encoding with the loop guard
/// ([`loop_guard`]). Version 4 is version 2's encoding with the same guard.
pub const PROMPT_VERSION_LOOP_GUARD: u32 = 3;
pub const PROMPT_VERSION_IMAGE_MIN_TOKENS_LOOP_GUARD: u32 = 4;
/// Every request version this contract's apps can read with.
pub const SUPPORTED_PROMPT_VERSIONS: [u32; 4] = [
    PROMPT_VERSION,
    PROMPT_VERSION_IMAGE_MIN_TOKENS,
    PROMPT_VERSION_LOOP_GUARD,
    PROMPT_VERSION_IMAGE_MIN_TOKENS_LOOP_GUARD,
];

/// `--image-min-tokens` for a job's request version; `None` keeps the
/// engine default, as version 1 always has.
pub fn image_min_tokens(prompt_version: u32) -> Option<u32> {
    matches!(
        prompt_version,
        PROMPT_VERSION_IMAGE_MIN_TOKENS | PROMPT_VERSION_IMAGE_MIN_TOKENS_LOOP_GUARD
    )
    .then_some(V2_IMAGE_MIN_TOKENS)
}

/// Whether a job's request version stops a reading that starts to repeat
/// (Study 5's `stop` rule, #1420): generation ends when [`tail_repetition`]
/// fires, and the reading keeps the text before the repeated run plus one
/// period of it ([`loop_cut`]).
pub fn loop_guard(prompt_version: u32) -> bool {
    matches!(
        prompt_version,
        PROMPT_VERSION_LOOP_GUARD | PROMPT_VERSION_IMAGE_MIN_TOKENS_LOOP_GUARD
    )
}
pub const PROMPT: &str = "Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, line breaks and hyphenation. Do not correct or modernise anything, and do not add anything that is not printed. If a word is unreadable, write [?] for it. Output only the transcription.";
/// The repeated run that ends `text`, ignoring trailing whitespace: the
/// shortest period of at most 400 chars that repeats at least three times
/// over at least 30 chars, as `(period, span)` in chars. Printed text can
/// repeat (a shelf of "SHAKESPEARE'S WORKS" spines); a reading that also
/// ran out of tokens on such a run is the model looping.
pub fn tail_repetition(text: &str) -> Option<(usize, usize)> {
    let s: Vec<char> = text.trim_end().chars().collect();
    let n = s.len();
    (1..=400.min(n / 3)).find_map(|p| {
        let mut i = n - 1;
        while i >= p && s[i] == s[i - p] {
            i -= 1;
        }
        let span = n - 1 - i + p;
        (span >= 3 * p && span >= 30).then_some((p, span))
    })
}

/// `text` cut by the `stop` rule: the text before the repeated run that ends
/// it, plus one period of the run. `None` when no such run ends it.
pub fn loop_cut(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    let (period, span) = tail_repetition(trimmed)?;
    let kept = trimmed.chars().count() - span + period;
    Some(trimmed.chars().take(kept).collect())
}

/// The probe's acceptance floor for word-bag Dice between two hosts'
/// readings of one crop. Vision-language output can differ between
/// devices, so the threshold proper is measured on the probe's pairs
/// (every compared pair is logged) and set from them; this floor only
/// rejects unrelated text.
pub const PROBE_MIN_DICE: f64 = 0.5;

/// One crop to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Crop {
    /// `[x0, y0, x1, y1]` in page pixels.
    #[serde(rename = "box")]
    pub rect: [u32; 4],
    pub id: String,
    /// LoC's words in the box, in visual line order: the reference a
    /// reading is compared with when nothing else is at hand.
    pub loc_text: String,
    pub stratum: String,
}
impl Crop {
    pub fn area_px2(&self) -> u64 {
        u64::from(self.rect[2].saturating_sub(self.rect[0]))
            * u64::from(self.rect[3].saturating_sub(self.rect[1]))
    }
}

/// One model in the image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Model {
    /// The GGUF file under the image's model directory.
    pub file: String,
    /// GPU memory the model needs to be chosen on a GPU; 0 for the CPU
    /// fallback.
    pub min_gpu_mb: u32,
    /// The vision projector file.
    pub mmproj: String,
    /// `qwen3-vl-8b-q4km`, ...
    pub name: String,
}

/// The engine build a unit was staged for. The image carries the same
/// values in its environment and the host refuses a unit staged for another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Engine {
    /// The worker image's registry digest (`sha256:...`).
    pub image: String,
    /// The llama.cpp server image the worker is built on, by digest.
    pub llama: String,
    /// The models in the image, largest first: the host takes the first
    /// whose `min_gpu_mb` its GPU has free, or the last on CPU.
    pub models: Vec<Model>,
    /// `sha256:` of the weights lock lines (path, URL, sha256) in the image.
    pub weights: String,
}

/// One page inside a multi-page `usnm_qwen` bundle workunit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePage {
    pub crops: Vec<Crop>,
    pub doc_id: String,
    pub group: String,
    /// Byte slice of this page's JPEG inside the bundled `page.jpg`.
    pub input: BundleSlice,
    pub language: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub v: u32,
    pub id: String,
    pub cpu_timeout_secs: u32,
    pub crops: Vec<Crop>,
    pub doc_id: String,
    pub engine: Engine,
    pub gpu_timeout_secs: u32,
    pub group: String,
    /// The full-size IIIF JPEG (`page.jpg` in the slot, or bundled JPEGs when `pages` is non-empty).
    pub input: Input,
    /// ISO 639-3.
    pub language: String,
    /// Output tokens allowed per 1,000 square pixels of crop.
    pub max_tokens_per_1000px2: u32,
    /// Optional multi-page bundle entries (`3–4 pages/WU` CPU, `15–20 pages/WU` GPU).
    /// Empty for single-page workunits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<BundlePage>,
    pub prompt_version: u32,
}
impl Job {
    pub fn identity(&self) -> anyhow::Result<String> {
        if self.pages.is_empty() {
            Ok(sha256(&serde_json::to_vec(&(
                self.v,
                &self.doc_id,
                &self.group,
                &self.language,
                &self.input,
                &self.crops,
                &self.engine,
                self.prompt_version,
                self.gpu_timeout_secs,
                self.cpu_timeout_secs,
                self.max_tokens_per_1000px2,
            ))?))
        } else {
            Ok(sha256(&serde_json::to_vec(&(
                self.v,
                &self.doc_id,
                &self.group,
                &self.language,
                &self.input,
                &self.crops,
                &self.engine,
                self.prompt_version,
                self.gpu_timeout_secs,
                self.cpu_timeout_secs,
                self.max_tokens_per_1000px2,
                &self.pages,
            ))?))
        }
    }
    pub fn workunit(&self) -> String {
        format!("usnm_qwen_{}", self.id)
    }
    /// Output tokens a crop may use.
    pub fn max_tokens(&self, crop: &Crop) -> u32 {
        let per = u64::from(self.max_tokens_per_1000px2);
        u32::try_from((crop.area_px2() * per).div_ceil(1000).max(16)).unwrap_or(u32::MAX)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        self.validate_source_candidate()?;
        for c in &self.crops {
            ensure!(
                c.loc_text.len() <= MAX_CROP_TEXT,
                "crop text over size limit"
            );
        }
        Ok(())
    }

    /// Validate every invariant except source crop-text size. Staging uses
    /// this before retaining oversized evidence; publishable jobs must still
    /// pass [`Self::validate`].
    pub fn validate_source_candidate(&self) -> anyhow::Result<()> {
        ensure!(self.v == VERSION, "unsupported qwen job version");
        ensure!(
            is_sha256(&self.id) && self.id == self.identity()?,
            "job identity mismatch"
        );
        let max_scan = if self.pages.is_empty() {
            MAX_SCAN
        } else {
            MAX_BUNDLE_SCAN
        };
        self.input.validate(max_scan, "page")?;
        for d in [&self.engine.image, &self.engine.llama, &self.engine.weights] {
            ensure!(
                d.strip_prefix("sha256:").is_some_and(is_sha256),
                "engine digests must be full sha256 digests"
            );
        }
        ensure!(
            !self.engine.models.is_empty() && self.engine.models.len() <= MAX_MODELS,
            "an engine has 1 to {MAX_MODELS} models"
        );
        for m in &self.engine.models {
            check_name(&m.name, "model name")?;
            for f in [&m.file, &m.mmproj] {
                ensure!(
                    !f.is_empty()
                        && f.len() <= 128
                        && f.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                        && !f.starts_with('.'),
                    "invalid model file name"
                );
            }
            ensure!(m.min_gpu_mb <= 1 << 20, "invalid model memory");
        }
        ensure!(
            self.engine
                .models
                .windows(2)
                .all(|w| w[0].min_gpu_mb >= w[1].min_gpu_mb),
            "models must be listed largest first"
        );
        if self.pages.is_empty() {
            ensure!(
                !self.crops.is_empty() && self.crops.len() <= MAX_CROPS,
                "a job has 1 to {MAX_CROPS} crops"
            );
        } else {
            ensure!(
                self.pages.len() <= MAX_BUNDLE_PAGES,
                "a bundle has at most {MAX_BUNDLE_PAGES} pages"
            );
            ensure!(
                !self.crops.is_empty() && self.crops.len() <= MAX_BUNDLE_CROPS,
                "a bundle job has 1 to {MAX_BUNDLE_CROPS} crops"
            );
            let mut seen_docs = std::collections::BTreeSet::new();
            let mut expected_crops = Vec::new();
            for p in &self.pages {
                check_doc_id(&p.doc_id)?;
                ensure!(
                    seen_docs.insert(p.doc_id.as_str()),
                    "duplicate doc_id {} in bundle",
                    p.doc_id
                );
                check_name(&p.group, "group")?;
                check_name(&p.language, "language")?;
                p.input.validate(MAX_SCAN, "page")?;
                ensure!(
                    p.input
                        .offset
                        .checked_add(p.input.bytes)
                        .is_some_and(|end| end <= self.input.bytes),
                    "bundle page slice exceeds page.jpg bytes"
                );
                ensure!(
                    !p.crops.is_empty() && p.crops.len() <= MAX_CROPS,
                    "a bundle page has 1 to {MAX_CROPS} crops"
                );
                for c in &p.crops {
                    check_name(&c.id, "crop id")?;
                    expected_crops.push(Crop {
                        rect: c.rect,
                        id: format!("{}:{}", p.doc_id, c.id),
                        loc_text: c.loc_text.clone(),
                        stratum: c.stratum.clone(),
                    });
                }
            }
            ensure!(
                self.crops == expected_crops,
                "bundle top-level crops must match flattened <doc_id>:<crop_id> list"
            );
        }
        let mut ids: Vec<&str> = self.crops.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ensure!(ids.len() == self.crops.len(), "a crop id is listed twice");
        for c in &self.crops {
            if self.pages.is_empty() {
                check_name(&c.id, "crop id")?;
            } else {
                let (doc, cid) =
                    c.id.split_once(':')
                        .ok_or_else(|| anyhow::anyhow!("bundle crop id missing doc_id prefix"))?;
                check_doc_id(doc)?;
                check_name(cid, "crop id")?;
            }
            check_name(&c.stratum.replace('/', "-"), "stratum")?;
            ensure!(
                c.rect[0] < c.rect[2]
                    && c.rect[1] < c.rect[3]
                    && c.rect[2] <= 1 << 16
                    && c.rect[3] <= 1 << 16,
                "invalid crop box"
            );
        }
        ensure!(
            SUPPORTED_PROMPT_VERSIONS.contains(&self.prompt_version),
            "unsupported prompt version"
        );
        for t in [self.gpu_timeout_secs, self.cpu_timeout_secs] {
            ensure!((60..=MAX_DEADLINE_SECS).contains(&t), "invalid deadline");
        }
        ensure!(
            (1..=4096).contains(&self.max_tokens_per_1000px2),
            "invalid token budget"
        );
        for (s, what) in [(&self.group, "group"), (&self.language, "language")] {
            check_name(s, what)?;
        }
        check_doc_id(&self.doc_id)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Every crop was read.
    Ok,
    /// The deadline passed; the crops read so far are in the readings.
    Timeout,
    /// llama-server did not start or answer, or died.
    Failed,
    /// The GPU version ran, but no usable GPU was visible (no NVIDIA GPU for
    /// a CUDA class; no hardware Vulkan device, or too little free device
    /// memory for the model, for a Vulkan class), so nothing was read.
    NoGpu,
    /// The unit could not run at all (bad input, an engine that differs
    /// from the job).
    UnitError,
}

/// One crop's reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    pub id: String,
    /// Wall time of the request.
    pub ms: u64,
    pub text: String,
    /// Completion tokens the server reported.
    pub tokens: u32,
}

/// `readings.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Readings {
    pub v: u32,
    pub job_id: String,
    /// `cuda`, `cpu` or `none`.
    pub device: String,
    /// Short facts about the run, bounded: the context size, the layers
    /// offloaded, the server's version line.
    pub diag: BTreeMap<String, String>,
    /// The model read with, by name; none when nothing ran.
    pub model: Option<String>,
    pub outcome: Outcome,
    pub prompt_version: u32,
    pub readings: Vec<Reading>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultManifest {
    pub v: u32,
    pub job_id: String,
    pub completed_at: String,
    /// Crops in the job.
    pub crops: u32,
    pub device: String,
    pub diag_bytes: u64,
    pub diag_sha256: String,
    pub engine: Engine,
    /// Wall time from the first request to the last answer.
    pub engine_ms: u64,
    /// Highest GPU memory in use while the unit ran, less what was in use
    /// before it started.
    pub gpu_mem_peak_mib: Option<u64>,
    pub host: crate::surya::Host,
    /// Time to start llama-server and load the model.
    pub load_ms: u64,
    pub model: Option<String>,
    pub outcome: Outcome,
    /// Highest summed RSS of llama-server, sampled while it ran.
    pub peak_rss_kib: Option<u64>,
    /// Crops read.
    pub read: u32,
    pub readings_bytes: u64,
    pub readings_sha256: String,
    pub unit_error: Option<String>,
}
impl ResultManifest {
    /// Checks one host's result on its own: shape, sizes and checksums.
    pub fn check(&self, readings: &[u8], diag: &[u8]) -> anyhow::Result<Readings> {
        ensure!(self.v == VERSION, "unsupported result version");
        ensure!(is_sha256(&self.job_id), "invalid job id");
        chrono::DateTime::parse_from_rfc3339(&self.completed_at)?;
        ensure!(diag.len() <= MAX_DIAG, "diagnostics over size limit");
        ensure!(
            self.diag_sha256 == sha256(diag) && self.diag_bytes == diag.len() as u64,
            "diagnostics checksum mismatch"
        );
        ensure!(readings.len() <= MAX_READINGS, "readings over size limit");
        ensure!(
            self.readings_sha256 == sha256(readings)
                && self.readings_bytes == readings.len() as u64,
            "readings checksum mismatch"
        );
        let r: Readings = serde_json::from_slice(readings)?;
        ensure!(
            r.v == VERSION && r.job_id == self.job_id && r.outcome == self.outcome,
            "readings do not match the result"
        );
        ensure!(
            r.device == self.device && r.model == self.model,
            "readings device or model differs"
        );
        ensure!(
            ["cpu", "cuda", "metal", "vulkan", "none"].contains(&self.device.as_str()),
            "unknown device"
        );
        ensure!(
            self.unit_error
                .as_ref()
                .is_none_or(|e| e.len() <= MAX_ERROR),
            "error text too long"
        );
        ensure!(
            (self.outcome == Outcome::UnitError) == self.unit_error.is_some(),
            "a unit error has a reason and nothing else does"
        );
        ensure!(
            self.read as usize == r.readings.len() && self.read <= self.crops,
            "read count does not match the readings"
        );
        ensure!(
            self.crops as usize <= MAX_BUNDLE_CROPS && r.readings.len() <= MAX_BUNDLE_CROPS,
            "too many crops"
        );
        let mut ids: Vec<&str> = r.readings.iter().map(|x| x.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ensure!(ids.len() == r.readings.len(), "a crop is read twice");
        for x in &r.readings {
            ensure!(x.text.len() <= MAX_CROP_TEXT, "reading over size limit");
            ensure!(
                !x.text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t'),
                "control characters in a reading"
            );
        }
        let consistent = match self.outcome {
            Outcome::Ok => self.read == self.crops && self.model.is_some() && self.device != "none",
            Outcome::NoGpu | Outcome::UnitError => self.read == 0 && self.model.is_none(),
            Outcome::Timeout | Outcome::Failed => self.read < self.crops,
        };
        ensure!(consistent, "outcome does not match the readings");
        ensure!(
            r.diag.len() <= 32 && r.diag.values().all(|v| v.len() <= 256),
            "readings diag too long"
        );
        Ok(r)
    }
}

/// One crop both results read, with how far the readings agree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CropAgreement {
    pub dice: Option<f64>,
    pub id: String,
    pub read: (bool, bool),
}

/// Compares two hosts' results for one unit. They agree when they are for
/// the same job and engine, both read the page or neither did, and for
/// every crop both read or neither did and, where both did, the word-bag
/// Dice of the readings is at least `min_dice`.
pub fn agree(
    a: &ResultManifest,
    a_readings: &Readings,
    b: &ResultManifest,
    b_readings: &Readings,
    min_dice: f64,
) -> (Vec<CropAgreement>, Result<(), String>) {
    let mut out = Vec::new();
    if a.job_id != b.job_id || a.engine != b.engine {
        return (out, Err("different job or engine".into()));
    }
    let mut verdict = Ok(());
    let a_ok = matches!(a.outcome, Outcome::Ok);
    let b_ok = matches!(b.outcome, Outcome::Ok);
    if a_ok != b_ok {
        verdict = Err(format!("outcomes {:?} and {:?}", a.outcome, b.outcome));
    }
    let bt: BTreeMap<&str, &Reading> = b_readings
        .readings
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let at: BTreeMap<&str, &Reading> = a_readings
        .readings
        .iter()
        .map(|r| (r.id.as_str(), r))
        .collect();
    let mut ids: Vec<&str> = at.keys().chain(bt.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let (ra, rb) = (at.get(id), bt.get(id));
        let read = (
            ra.is_some_and(|r| !r.text.trim().is_empty()),
            rb.is_some_and(|r| !r.text.trim().is_empty()),
        );
        let dice = (read.0 && read.1)
            .then(|| crate::boinc::dice(ra.map_or("", |r| &r.text), rb.map_or("", |r| &r.text)));
        if verdict.is_ok() {
            if read.0 != read.1 {
                verdict = Err(format!("{id}: read by one host only"));
            } else if let Some(d) = dice.filter(|d| *d < min_dice) {
                verdict = Err(format!("{id}: reading agreement {d:.4}"));
            }
        }
        out.push(CropAgreement {
            dice,
            id: id.to_owned(),
            read,
        });
    }
    if verdict.is_ok() {
        if a.unit_error.is_some() || b.unit_error.is_some() {
            if a.unit_error.is_none() || b.unit_error.is_none() {
                verdict = Err("one result could not run the unit".into());
            }
        } else if out.is_empty() && (a_ok || b_ok) {
            verdict = Err("no crop in common".into());
        }
    }
    (out, verdict)
}

/// One page of a bundle unit, judged on its own from a pair's per-crop
/// agreement (#529): the unit's own verdict still needs every page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageAgreement {
    pub doc_id: String,
    pub agree: bool,
    pub crops: Vec<CropAgreement>,
    pub reason: Option<String>,
}

/// Splits a bundle pair's `crops` (from [`agree`]) by page, the
/// `<doc_id>:` prefix of each crop id, and judges each page by the rule
/// [`agree`] applies to a whole unit. A page is only judged when both
/// results are for the same job and engine, both ran to the end
/// (`Outcome::Ok`, so every crop of every page was read or left blank),
/// and neither is a unit error; a timed-out or failed result leaves every
/// page unjudged, since which of its pages were complete is not in the
/// result. Single-page units have no pages.
pub fn agree_pages(
    a: &ResultManifest,
    b: &ResultManifest,
    crops: &[CropAgreement],
    min_dice: f64,
) -> Vec<PageAgreement> {
    if a.job_id != b.job_id
        || a.engine != b.engine
        || a.outcome != Outcome::Ok
        || b.outcome != Outcome::Ok
        || a.unit_error.is_some()
        || b.unit_error.is_some()
        || crops.is_empty()
        || !crops.iter().all(|c| c.id.contains(':'))
    {
        return Vec::new();
    }
    let mut pages: BTreeMap<&str, Vec<CropAgreement>> = BTreeMap::new();
    for c in crops {
        let (doc, _) = c.id.split_once(':').expect("checked above");
        pages.entry(doc).or_default().push(c.clone());
    }
    pages
        .into_iter()
        .map(|(doc, crops)| {
            let mut reason = None;
            for c in &crops {
                if c.read.0 != c.read.1 {
                    reason = Some(format!("{}: read by one host only", c.id));
                    break;
                }
                if let Some(d) = c.dice.filter(|d| *d < min_dice) {
                    reason = Some(format!("{}: reading agreement {d:.4}", c.id));
                    break;
                }
            }
            if reason.is_none() && !crops.iter().any(|c| c.read.0) {
                reason = Some("no crop read".into());
            }
            PageAgreement {
                doc_id: doc.to_owned(),
                agree: reason.is_none(),
                crops,
                reason,
            }
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn engine() -> Engine {
        Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", "b".repeat(64)),
            models: vec![
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
            ],
            weights: format!("sha256:{}", "c".repeat(64)),
        }
    }

    pub(crate) fn job() -> Job {
        let mut j = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 5400,
            crops: vec![
                Crop {
                    rect: [10, 10, 800, 200],
                    id: "c01".into(),
                    loc_text: "one two three four".into(),
                    stratum: "wc-low/top".into(),
                },
                Crop {
                    rect: [10, 3000, 800, 3200],
                    id: "c02".into(),
                    loc_text: "five six seven eight".into(),
                    stratum: "wc-high/bottom".into(),
                },
            ],
            doc_id: "sn85055101_1917-04-03_ed-1_seq-3".into(),
            engine: engine(),
            gpu_timeout_secs: 1800,
            group: "serbian".into(),
            input: Input::of(b"jpeg", "https://tile.loc.gov/x/full/full/0/default.jpg"),
            language: "srp".into(),
            max_tokens_per_1000px2: 4,
            pages: vec![],
            prompt_version: PROMPT_VERSION,
        };
        j.id = j.identity().unwrap();
        j
    }

    pub(crate) fn result(
        outcome: Outcome,
        texts: &[(&str, &str)],
    ) -> (ResultManifest, Readings, Vec<u8>) {
        let unit = outcome == Outcome::UnitError;
        let ran = !matches!(outcome, Outcome::UnitError | Outcome::NoGpu);
        let readings = Readings {
            v: VERSION,
            job_id: job().id,
            device: if ran { "cuda".into() } else { "none".into() },
            diag: BTreeMap::new(),
            model: ran.then(|| "qwen3-vl-8b-q4km".into()),
            outcome,
            prompt_version: PROMPT_VERSION,
            readings: texts
                .iter()
                .map(|(id, t)| Reading {
                    id: id.to_string(),
                    ms: 1,
                    text: t.to_string(),
                    tokens: 4,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec_pretty(&readings).unwrap();
        (
            ResultManifest {
                v: VERSION,
                job_id: job().id,
                completed_at: "2026-10-14T00:00:00Z".into(),
                crops: 2,
                device: readings.device.clone(),
                diag_bytes: 0,
                diag_sha256: sha256(b""),
                engine: engine(),
                engine_ms: 1,
                gpu_mem_peak_mib: None,
                host: Default::default(),
                load_ms: 1,
                model: readings.model.clone(),
                outcome,
                peak_rss_kib: None,
                read: texts.len() as u32,
                readings_bytes: bytes.len() as u64,
                readings_sha256: sha256(&bytes),
                unit_error: unit.then(|| "checksum".into()),
            },
            readings,
            bytes,
        )
    }

    #[test]
    fn longer_gpu_budget_is_valid_and_has_a_new_immutable_identity() {
        let original = job();
        let mut longer = original.clone();
        longer.gpu_timeout_secs = 14_400;
        longer.id = longer.identity().unwrap();
        longer.validate().unwrap();
        assert_ne!(longer.id, original.id);
        assert_eq!(longer.crops, original.crops);
        longer.gpu_timeout_secs = 14_401;
        longer.id = longer.identity().unwrap();
        assert!(longer.validate().is_err());
    }

    #[test]
    fn job_identity_and_limits() {
        let j = job();
        j.validate().unwrap();
        assert_eq!(
            j.max_tokens(&j.crops[0]),
            (790 * 190 * 4u64).div_ceil(1000) as u32
        );
        let mut bad = j.clone();
        bad.crops[1].id = "c01".into();
        bad.id = bad.identity().unwrap();
        assert!(bad.validate().is_err());
        let mut order = j.clone();
        order.engine.models.reverse();
        order.id = order.identity().unwrap();
        assert!(order.validate().is_err());
        let mut tampered = j.clone();
        tampered.prompt_version = PROMPT_VERSION_IMAGE_MIN_TOKENS;
        assert!(tampered.validate().is_err());
        tampered.id = tampered.identity().unwrap();
        assert!(tampered.validate().is_ok());
        assert_ne!(tampered.id, j.id);
        let mut unknown = j;
        unknown.prompt_version = 5;
        unknown.id = unknown.identity().unwrap();
        assert!(unknown.validate().is_err());
    }

    #[test]
    fn request_versions_set_the_encoding_and_the_guard_apart() {
        for (version, tokens, guard) in [
            (PROMPT_VERSION, None, false),
            (
                PROMPT_VERSION_IMAGE_MIN_TOKENS,
                Some(V2_IMAGE_MIN_TOKENS),
                false,
            ),
            (PROMPT_VERSION_LOOP_GUARD, None, true),
            (
                PROMPT_VERSION_IMAGE_MIN_TOKENS_LOOP_GUARD,
                Some(V2_IMAGE_MIN_TOKENS),
                true,
            ),
        ] {
            assert!(SUPPORTED_PROMPT_VERSIONS.contains(&version));
            assert_eq!(image_min_tokens(version), tokens);
            assert_eq!(loop_guard(version), guard);
        }
    }

    #[test]
    fn loop_cut_keeps_the_head_and_one_period() {
        let looped = format!("Head line.\n{}", "[?] ".repeat(40));
        assert_eq!(loop_cut(&looped).unwrap(), "Head line.\n[?] ");
        // Cutting twice changes nothing: one period no longer repeats.
        assert_eq!(loop_cut("Head line.\n[?] "), None);
        // Multibyte text is cut on chars, not bytes.
        let hebrew = format!("\u{5d0}\u{5d1} {}", "\u{5d2}\u{5d3} ".repeat(20));
        assert_eq!(loop_cut(&hebrew).unwrap(), "\u{5d0}\u{5d1} \u{5d2}\u{5d3}");
        assert_eq!(loop_cut("a plain reading that stops"), None);
    }

    #[test]
    fn tail_repetition_finds_the_run_that_ends_a_reading() {
        let looped = format!("Head line.\n{}", "[?] ".repeat(40));
        assert_eq!(tail_repetition(&looped), Some((4, 159)));
        assert_eq!(tail_repetition("word\nword\nword"), None);
        assert_eq!(
            tail_repetition("The ship sailed for England, and the enemy sank it."),
            None
        );
        assert_eq!(tail_repetition(""), None);
    }

    #[test]
    fn check_and_agree() {
        let (a, ar, ab) = result(
            Outcome::Ok,
            &[
                ("c01", "one two three four"),
                ("c02", "five six seven eight"),
            ],
        );
        a.check(&ab, b"").unwrap();
        assert!(a.check(&ab, b"diag").is_err());
        let (b, br, bb) = result(
            Outcome::Ok,
            &[
                ("c01", "one two three five"),
                ("c02", "five six seven eight"),
            ],
        );
        b.check(&bb, b"").unwrap();
        let (crops, v) = agree(&a, &ar, &b, &br, 0.7);
        v.unwrap();
        assert_eq!(crops.len(), 2);
        assert_eq!(crops[0].dice, Some(0.75));
        assert!(agree(&a, &ar, &b, &br, 0.8).1.is_err());
        let (t, tr, tb) = result(Outcome::Timeout, &[("c01", "one two three four")]);
        t.check(&tb, b"").unwrap();
        assert!(agree(&a, &ar, &t, &tr, 0.5).1.is_err());
        let (u, ur, ub) = result(Outcome::UnitError, &[]);
        u.check(&ub, b"").unwrap();
        assert!(agree(&u, &ur, &u, &ur, 0.5).1.is_ok());
        assert!(agree(&a, &ar, &u, &ur, 0.5).1.is_err());
        let (n, nr, nb) = result(Outcome::NoGpu, &[]);
        n.check(&nb, b"").unwrap();
        assert!(agree(&n, &nr, &n, &nr, 0.5).1.is_ok());
        // An ok result must have read every crop.
        let (mut short, _, sb) = result(Outcome::Ok, &[("c01", "x")]);
        short.crops = 2;
        assert!(short.check(&sb, b"").is_err());
    }

    #[test]
    fn bundle_pages_are_judged_apart_while_the_unit_stays_inconclusive() {
        let a_texts = [
            ("p1:c01", "one two three four"),
            ("p1:c02", "five six seven eight"),
            ("p2:c01", "the ship sailed"),
            ("p2:c02", "for england"),
            ("p3:c01", ""),
        ];
        let b_texts = [
            ("p1:c01", "one two three five"),
            ("p1:c02", "five six seven eight"),
            ("p2:c01", "the ship sailed"),
            ("p2:c02", "[?] [?] [?] [?] [?] [?]"),
            ("p3:c01", ""),
        ];
        let (a, ar, _) = result(Outcome::Ok, &a_texts);
        let (b, br, _) = result(Outcome::Ok, &b_texts);
        let (crops, unit) = agree(&a, &ar, &b, &br, 0.5);
        assert!(
            unit.is_err(),
            "one disagreeing page keeps the unit inconclusive"
        );
        let pages = agree_pages(&a, &b, &crops, 0.5);
        let verdicts: Vec<(&str, bool)> =
            pages.iter().map(|p| (p.doc_id.as_str(), p.agree)).collect();
        assert_eq!(verdicts, [("p1", true), ("p2", false), ("p3", false)]);
        assert_eq!(pages[0].crops.len(), 2);
        assert!(pages[1].reason.as_deref().unwrap().starts_with("p2:c02"));
        assert_eq!(pages[2].reason.as_deref(), Some("no crop read"));
        // A timed-out result says nothing about which of its pages were whole.
        let (t, tr, _) = result(Outcome::Timeout, &a_texts[..2]);
        let (crops, _) = agree(&a, &ar, &t, &tr, 0.5);
        assert!(agree_pages(&a, &t, &crops, 0.5).is_empty());
        // Single-page units have no pages to judge.
        let (s, sr, _) = result(Outcome::Ok, &[("c01", "x"), ("c02", "y")]);
        let (crops, _) = agree(&s, &sr, &s, &sr, 0.5);
        assert!(agree_pages(&s, &s, &crops, 0.5).is_empty());
    }
}
