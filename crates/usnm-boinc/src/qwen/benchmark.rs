//! Held-out transcription experiments on public gold crops. References never enter a workunit.
use super::{doc_id::DocId, outputs, units, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use image::GenericImageView as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};
use usnm_core::page_id::{PageId, Source};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{Input, MAX_SCAN},
    qwen::{
        Crop, Engine, Job, Outcome, MAX_BUNDLE_CROPS, MAX_BUNDLE_PAGES, MAX_JOB_JSON, MAX_READINGS,
        VERSION,
    },
};
use usnm_ocr_quality::align::{align, Op};
use usnm_ocr_survey::score;

const MANIFEST: &str = "benchmark.json";
pub const NOTICE_FILE: &str = "NOTICE.txt";
pub const MAX_NOTICE: usize = 64 * 1024;

fn notice(provenance: &Value) -> anyhow::Result<Vec<u8>> {
    let field = |name: &str| {
        provenance[name]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .with_context(|| format!("missing source {name}"))
    };
    let license = field("source_license")?;
    let upper = license.to_uppercase().replace(' ', "-");
    let url = if upper.contains("CC-BY-NC-SA-4.0") {
        "https://creativecommons.org/licenses/by-nc-sa/4.0/"
    } else if upper.contains("CC-BY-NC-4.0") {
        "https://creativecommons.org/licenses/by-nc/4.0/"
    } else if upper.contains("CC-BY-SA-4.0") {
        "https://creativecommons.org/licenses/by-sa/4.0/"
    } else if upper.contains("CC-BY-4.0") {
        "https://creativecommons.org/licenses/by/4.0/"
    } else if upper.contains("CC0-1.0") {
        "https://creativecommons.org/publicdomain/zero/1.0/"
    } else if upper.contains("APACHE-2.0") {
        // American Stories' transcriptions (#968's en-as); its page images are public domain.
        "https://www.apache.org/licenses/LICENSE-2.0"
    } else {
        anyhow::bail!("unsupported source licence");
    };
    let bytes = format!(
        "Source: {}\nAttribution: {}\nLicence: {license}\nLicence terms: {url}\n\
         Modifications: cropped/re-encoded by the public dataset converter where applicable; \
         this campaign redistributes those crop bytes unchanged. No reference text is included.\n",
        field("source_url")?,
        field("source_attribution")?,
    )
    .into_bytes();
    ensure!(
        bytes.len() <= MAX_NOTICE,
        "source notices exceed size limit"
    );
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub crops: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    pub max_crops: usize,
    pub timeout_secs: u32,
    pub doc_ids: DocIds,
    /// The request version every staged job carries (1 stages as before #1776).
    pub prompt_version: u32,
    /// Pin every staged job to this model of `engine`, by name; `None` leaves the choice to the
    /// host's plan class, as before #1776.
    pub model: Option<String>,
    /// Crops a unit carries: 1 stages one unit per crop, as before; more packs up to that many
    /// consecutive crops into one bundle unit (`Job.pages`, at most [`MAX_BUNDLE_PAGES`]), each
    /// crop a bundle page with its own doc id and its JPEG bytes unchanged in the bundle's
    /// `page.jpg`.
    pub bundle_size: usize,
}

/// How a campaign spells each crop's `doc_id`. Both pass every released worker's check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum DocIds {
    /// `benchmark-<sha256 of the crop id>`, as every campaign staged before #983.
    #[default]
    Legacy,
    /// `sample-<source_dataset>_<crop id>`, the crop's sample id (ADR-0026) in wire form.
    Sample,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Entry {
    job: Job,
    reference: Value,
    provenance: Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    v: u32,
    source_sha256: String,
    source_manifest_sha256: String,
    /// One per crop, in `crops.jsonl` order. Without `bundles` each entry's job is a unit.
    entries: Vec<Entry>,
    /// With `--bundle-size` over 1, the units: each a bundle of consecutive entries' jobs.
    /// The entries' own jobs are then never sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    bundles: Vec<Bundle>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Bundle {
    job: Job,
    /// The crop ids of its pages, in page order.
    crops: Vec<String>,
}

/// A unit as submit sends it, with its `page.jpg` and `NOTICE.txt`.
struct Unit<'a> {
    job: &'a Job,
    image: Vec<u8>,
    notice: Vec<u8>,
}

/// The notice of a bundle: each distinct crop notice once, in page order.
fn bundle_notice(notices: &[&[u8]]) -> anyhow::Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for n in notices {
        if seen.insert(*n) {
            if !out.is_empty() {
                out.push(b'\n');
            }
            out.extend_from_slice(n);
        }
    }
    ensure!(out.len() <= MAX_NOTICE, "source notices exceed size limit");
    Ok(out)
}

/// A bundle with its `page.jpg` and `NOTICE.txt` bytes.
struct Packed {
    bundle: Bundle,
    image: Vec<u8>,
    notice: Vec<u8>,
}

/// Packs `entries` (with their crop bytes and notices) into bundles of up to `size` consecutive
/// crops, splitting early where a bundle's `qwen.json` or longest `readings.json` would pass
/// the workers' limits.
fn pack(
    entries: &[Entry],
    images: &[Vec<u8>],
    notices: &[Vec<u8>],
    size: usize,
) -> anyhow::Result<Vec<Packed>> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < entries.len() {
        let mut end = start + 1;
        while end < entries.len() && end - start < size {
            let jobs: Vec<Job> = entries[start..=end].iter().map(|e| e.job.clone()).collect();
            let crops: usize = jobs.iter().map(|j| j.crops.len()).sum();
            let bytes: usize = images[start..=end].iter().map(Vec::len).sum();
            if crops > MAX_BUNDLE_CROPS
                || bytes > usnm_ocr_contract::quality::MAX_BUNDLE_SCAN
                || super::stage::bundle_json_len(&jobs)? > MAX_JOB_JSON
                || super::stage::longest_readings_len(&jobs)? > MAX_READINGS
            {
                break;
            }
            end += 1;
        }
        let jobs: Vec<Job> = entries[start..end].iter().map(|e| e.job.clone()).collect();
        let image = images[start..end].concat();
        let parts: Vec<&[u8]> = notices[start..end].iter().map(Vec::as_slice).collect();
        let notice = bundle_notice(&parts)?;
        let blob = Input::of(&image, &format!("benchmark:{}", sha256(&notice)));
        let job = super::stage::bundle_job(&jobs, blob)?;
        job.validate()?;
        let crops = entries[start..end]
            .iter()
            .map(|e| e.job.crops[0].id.clone())
            .collect();
        out.push(Packed {
            bundle: Bundle { job, crops },
            image,
            notice,
        });
        start = end;
    }
    Ok(out)
}

/// One model per campaign prevents a hardware-dependent fallback from confounding a comparison.
pub fn stage(opts: &Options) -> anyhow::Result<usize> {
    super::check_no_hold(&opts.root)?;
    ensure!(opts.max_crops > 0, "--max-crops must be positive");
    ensure!(
        (1..=MAX_BUNDLE_PAGES).contains(&opts.bundle_size),
        "--bundle-size must be 1 to {MAX_BUNDLE_PAGES}"
    );
    ensure!(
        opts.engine.models.len() == 1,
        "benchmark requires exactly one model"
    );
    if let Some(pin) = &opts.model {
        ensure!(
            *pin == opts.engine.models[0].name,
            "--model {pin} is not the engine's one model"
        );
    }
    let source = std::fs::read(&opts.crops)?;
    let rows = score::read_jsonl(&opts.crops)?;
    ensure!(!rows.is_empty(), "no benchmark crops");
    ensure!(
        score::invalid_crops(&rows, true).is_empty(),
        "benchmark requires valid, reviewed human transcriptions"
    );
    let base = opts
        .crops
        .parent()
        .context("crops has no parent")?
        .canonicalize()?;
    ensure!(
        base.join("LICENSES").is_dir(),
        "dataset has no LICENSES provenance directory"
    );
    let source_manifest = std::fs::read(base.join("manifest.jsonl"))?;
    let mut metadata = BTreeMap::new();
    for row in score::read_jsonl(&base.join("manifest.jsonl"))? {
        let cid = row["crop_id"]
            .as_str()
            .context("manifest has no crop_id")?
            .to_string();
        ensure!(
            metadata.insert(cid, row).is_none(),
            "duplicate manifest crop"
        );
    }
    let mut entries = Vec::new();
    let mut images = Vec::new();
    let mut notices = Vec::new();
    for row in rows.into_iter().take(opts.max_crops) {
        ensure!(
            row["for_scoring"] == true,
            "comparison-only crop is not ground truth"
        );
        let cid = row["crop_id"].as_str().context("missing crop_id")?;
        let provenance = metadata
            .get(cid)
            .context("crop missing from source manifest")?;
        let license = provenance["source_license"]
            .as_str()
            .context("missing source licence")?;
        ensure!(
            !license
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part.eq_ignore_ascii_case("ND")
                    || part.eq_ignore_ascii_case("NoDerivatives")),
            "NoDerivatives source requires original-file transport, not crop workunits"
        );
        let relative = row["image"].as_str().context("missing crop image")?;
        ensure!(
            !Path::new(relative).is_absolute(),
            "image must be relative to crops.jsonl"
        );
        let image = base.join(relative).canonicalize()?;
        ensure!(image.starts_with(&base), "image escapes dataset directory");
        let bytes = read(&image, MAX_SCAN)?;
        let notice = notice(provenance)?;
        ensure!(
            provenance["crop_sha256"].as_str() == Some(&sha256(&bytes)),
            "crop checksum differs from source manifest"
        );
        let (w, h) = image::load_from_memory(&bytes)
            .with_context(|| format!("{cid}: decoding crop"))?
            .dimensions();
        ensure!(
            w > 0 && h > 0 && w <= 16384 && h <= 16384,
            "invalid crop dimensions"
        );
        let doc_id = match opts.doc_ids {
            DocIds::Legacy => DocId::legacy_benchmark(cid),
            DocIds::Sample => {
                let dataset = provenance["source_dataset"]
                    .as_str()
                    .with_context(|| format!("{cid}: sample doc ids need a source_dataset"))?;
                DocId::Page(PageId::new(Source::Sample, &format!("{dataset}/{cid}"))?)
            }
        };
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: opts.timeout_secs,
            gpu_timeout_secs: opts.timeout_secs,
            crops: vec![Crop {
                id: cid.into(),
                rect: [0, 0, w, h],
                loc_text: String::new(),
                stratum: "benchmark".into(),
            }],
            doc_id: doc_id.wire()?,
            engine: opts.engine.clone(),
            group: row["group"].as_str().context("missing group")?.into(),
            input: Input::of(&bytes, &format!("benchmark:{}", sha256(&notice))),
            language: row["lang"].as_str().context("missing lang")?.into(),
            max_tokens_per_1000px2: 4,
            pages: vec![],
            prompt_version: opts.prompt_version,
            model: opts.model.clone(),
        };
        job.id = job.identity()?;
        job.validate()?;
        entries.push(Entry {
            job,
            reference: row,
            provenance: provenance.clone(),
        });
        images.push(bytes);
        notices.push(notice);
    }
    let packed = if opts.bundle_size > 1 {
        pack(&entries, &images, &notices, opts.bundle_size)?
    } else {
        Vec::new()
    };
    let mut bundles = Vec::with_capacity(packed.len());
    let mut bundle_files = Vec::with_capacity(packed.len());
    for p in packed {
        bundles.push(p.bundle);
        bundle_files.push((p.image, p.notice));
    }
    let manifest = Manifest {
        v: 1,
        source_sha256: sha256(&source),
        source_manifest_sha256: sha256(&source_manifest),
        entries,
        bundles,
    };
    std::fs::create_dir_all(&opts.root)?;
    let path = opts.root.join(MANIFEST);
    if path.exists() {
        let old: Manifest = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(old == manifest, "campaign is immutable; use a new --root");
    } else {
        ensure!(
            !units(&opts.root).exists(),
            "root already contains non-benchmark units"
        );
        atomic(&path, &serde_json::to_vec_pretty(&manifest)?)?;
    }
    let staged: Vec<Unit<'_>> = if manifest.bundles.is_empty() {
        manifest
            .entries
            .iter()
            .zip(images)
            .zip(notices)
            .map(|((e, image), notice)| Unit {
                job: &e.job,
                image,
                notice,
            })
            .collect()
    } else {
        manifest
            .bundles
            .iter()
            .zip(bundle_files)
            .map(|(b, (image, notice))| Unit {
                job: &b.job,
                image,
                notice,
            })
            .collect()
    };
    let count = staged.len();
    for Unit {
        job,
        image: bytes,
        notice,
    } in staged
    {
        let dir = units(&opts.root).join(&job.id);
        std::fs::create_dir_all(&dir)?;
        if dir.join(JOB_FILE).exists() {
            let old: Job = serde_json::from_slice(&read(&dir.join(JOB_FILE), MAX_JOB_JSON)?)?;
            ensure!(old == *job, "staged job changed");
            let max_scan = if job.pages.is_empty() {
                MAX_SCAN
            } else {
                usnm_ocr_contract::quality::MAX_BUNDLE_SCAN
            };
            ensure!(
                job.input.matches(&read(&dir.join(PAGE_FILE), max_scan)?),
                "staged image changed"
            );
            ensure!(
                read(&dir.join(NOTICE_FILE), MAX_NOTICE)? == notice,
                "staged notices changed"
            );
        } else {
            atomic(&dir.join(PAGE_FILE), &bytes)?;
            atomic(&dir.join(NOTICE_FILE), &notice)?;
            atomic(&dir.join(JOB_FILE), &serde_json::to_vec_pretty(job)?)?;
        }
    }
    Ok(count)
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub expected: usize,
    pub returned: usize,
    pub pending: usize,
    pub failed: usize,
    pub incomplete: usize,
}

/// Only the assimilator's canonical outputs count; validator copies are not canonical results.
/// Empty hypotheses penalize missing output instead of silently shrinking the denominator.
pub fn collect(root: &Path, spool: &Path, out: &Path) -> anyhow::Result<Summary> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(root.join(MANIFEST))?)?;
    ensure!(
        manifest.v == 1 && !manifest.entries.is_empty(),
        "invalid benchmark manifest"
    );
    let mut summary = Summary {
        expected: manifest.entries.len(),
        ..Default::default()
    };
    // Each crop's unit and the id its reading carries there: the crop id in a one-crop unit,
    // `<doc_id>:<crop id>` in a bundle.
    let mut unit_of: HashMap<String, (&Job, String)> = HashMap::new();
    if !manifest.bundles.is_empty() {
        for b in &manifest.bundles {
            b.job.validate()?;
            ensure!(
                b.job.pages.len() == b.crops.len(),
                "{}: bundle pages and crops differ",
                b.job.id
            );
            for (page, cid) in b.job.pages.iter().zip(&b.crops) {
                ensure!(
                    page.crops.len() == 1 && page.crops[0].id == *cid,
                    "{}: bundle page {} is not crop {cid}",
                    b.job.id,
                    page.doc_id
                );
                let reading = format!("{}:{cid}", page.doc_id);
                ensure!(
                    unit_of.insert(cid.clone(), (&b.job, reading)).is_none(),
                    "{cid} is in two bundles"
                );
            }
        }
        ensure!(
            unit_of.len() == manifest.entries.len(),
            "bundles do not hold every crop once"
        );
    }
    let mut loaded_units: HashMap<String, super::validate::Loaded> = HashMap::new();
    let mut hypotheses = HashMap::new();
    let mut references = Vec::new();
    let mut statuses = Vec::new();
    let mut model = None;
    for entry in &manifest.entries {
        let job = &entry.job;
        job.validate()?;
        ensure!(
            job.engine.models.len() == 1,
            "benchmark requires exactly one model"
        );
        let expected_model = &job.engine.models[0].name;
        if let Some(name) = &model {
            ensure!(name == expected_model, "mixed models in benchmark");
        } else {
            model = Some(expected_model.clone());
        }
        let cid = entry.reference["crop_id"]
            .as_str()
            .context("reference has no crop_id")?;
        ensure!(
            job.crops.len() == 1 && job.crops[0].id == cid && job.crops[0].loc_text.is_empty(),
            "invalid benchmark crop"
        );
        ensure!(
            DocId::parse(&job.doc_id)?.names_crop(entry.provenance["source_dataset"].as_str(), cid),
            "{}: doc id {:?} does not name crop {cid}",
            job.id,
            job.doc_id
        );
        ensure!(!hypotheses.contains_key(cid), "duplicate reference");
        let (unit, reading_id) = match unit_of.get(cid) {
            Some((unit, reading)) => {
                let page = unit
                    .pages
                    .iter()
                    .find(|p| p.crops[0].id == cid)
                    .context("crop missing from its bundle")?;
                ensure!(
                    page.doc_id == job.doc_id
                        && page.crops == job.crops
                        && page.input.sha256 == job.input.sha256
                        && page.input.bytes == job.input.bytes
                        && unit.engine == job.engine
                        && unit.model == job.model
                        && unit.prompt_version == job.prompt_version,
                    "{cid}: its bundle page differs from its staged job"
                );
                (*unit, reading.clone())
            }
            None => (job, cid.to_string()),
        };
        let bundled = !unit.pages.is_empty();
        let files: Vec<PathBuf> = outputs()
            .iter()
            .map(|n| spool.join(format!("{}__file_{n}", unit.workunit())))
            .collect();
        let count = files.iter().filter(|f| f.exists()).count();
        let mut text = String::new();
        let mut detail = serde_json::json!({"crop_id": cid, "job_id": job.id});
        if bundled {
            detail["unit_job_id"] = unit.id.clone().into();
        }
        if count == 0 {
            let failed = ["error", "no_output_files"]
                .iter()
                .any(|suffix| spool.join(format!("{}_{suffix}", unit.workunit())).exists());
            if failed {
                summary.failed += 1;
            } else {
                summary.pending += 1;
            }
            detail["status"] = if failed { "failed" } else { "pending" }.into();
        } else {
            ensure!(
                count == files.len(),
                "{}: partial spool; retry after assimilation finishes",
                unit.workunit()
            );
            if !loaded_units.contains_key(&unit.id) {
                let loaded = super::validate::load(&files)?;
                let result = &loaded.result;
                ensure!(
                    result.job_id == unit.id && result.engine == unit.engine,
                    "result belongs to another job or engine"
                );
                ensure!(
                    loaded.readings.prompt_version == unit.prompt_version,
                    "reading was made at another request version"
                );
                ensure!(
                    usize::try_from(result.crops).ok() == Some(unit.crops.len()),
                    "unexpected crop count"
                );
                ensure!(
                    result.model.as_ref().is_none_or(|m| m == expected_model),
                    "unexpected model"
                );
                let mut ids: Vec<&str> = loaded
                    .readings
                    .readings
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect();
                ids.sort_unstable();
                ids.dedup();
                ensure!(
                    ids.len() == loaded.readings.readings.len()
                        && ids.iter().all(|id| unit.crops.iter().any(|c| c.id == *id)),
                    "unexpected readings"
                );
                if result.outcome == Outcome::Ok {
                    ensure!(
                        result.model.as_ref() == Some(expected_model),
                        "successful result has no expected model"
                    );
                }
                loaded_units.insert(unit.id.clone(), loaded);
            }
            let loaded = &loaded_units[&unit.id];
            let result = &loaded.result;
            let reading = loaded.readings.readings.iter().find(|r| r.id == reading_id);
            if result.outcome == Outcome::Ok {
                ensure!(reading.is_some(), "successful result has no reading");
                summary.returned += 1;
            } else {
                summary.incomplete += 1;
            }
            if let Some(reading) = reading {
                text.clone_from(&reading.text);
            }
            detail["status"] = serde_json::to_value(result.outcome)?;
            detail["device"] = result.device.clone().into();
            detail["model"] = serde_json::to_value(&result.model)?;
            detail["load_ms"] = result.load_ms.into();
            detail["engine_ms"] = result.engine_ms.into();
            detail["reading_ms"] = serde_json::to_value(reading.map(|r| r.ms))?;
            detail["peak_rss_kib"] = serde_json::to_value(result.peak_rss_kib)?;
            detail["gpu_mem_peak_mib"] = serde_json::to_value(result.gpu_mem_peak_mib)?;
            detail["unit_error"] = serde_json::to_value(&result.unit_error)?;
        }
        let truth = entry.reference["transcription"]
            .as_str()
            .context("missing reference text")?;
        let ops = align(
            &usnm_ocr_survey::text::normalize(truth, true)
                .chars()
                .collect::<Vec<_>>(),
            &usnm_ocr_survey::text::normalize(&text, false)
                .chars()
                .collect::<Vec<_>>(),
        );
        detail["char_insertions"] = ops.iter().filter(|op| **op == Op::Ins).count().into();
        detail["char_deletions"] = ops.iter().filter(|op| **op == Op::Del).count().into();
        detail["char_substitutions"] = ops.iter().filter(|op| **op == Op::Sub).count().into();
        detail["counts"] = serde_json::to_value(score::cer_wer(truth, &text))?;
        hypotheses.insert(cid.to_string(), text);
        statuses.push(detail);
        references.push(entry.reference.clone());
    }
    ensure!(
        score::invalid_crops(&references, true).is_empty(),
        "invalid benchmark references"
    );
    let hyps: BTreeMap<_, _> = [(model.context("benchmark has no model")?, hypotheses)].into();
    let scored = score::score(&references, &hyps, true)?;
    std::fs::create_dir_all(out)?;
    score::write_csvs(out, &scored)?;
    atomic(
        &out.join("coverage.json"),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    let mut lines = Vec::new();
    for status in statuses {
        serde_json::to_writer(&mut lines, &status)?;
        lines.push(b'\n');
    }
    atomic(&out.join("by-crop.jsonl"), &lines)?;
    let mut lines = Vec::new();
    let mut predictions: Vec<_> = hyps.values().flat_map(|h| h.iter()).collect();
    predictions.sort_by_key(|(id, _)| *id);
    for (cid, text) in predictions {
        serde_json::to_writer(
            &mut lines,
            &serde_json::json!({"crop_id": cid, "text": text}),
        )?;
        lines.push(b'\n');
    }
    atomic(&out.join("hypotheses.jsonl"), &lines)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> Options {
        let (image, _) = crate::qwen::stage::synthetic_page();
        std::fs::write(dir.join("image.jpg"), image).unwrap();
        let row = serde_json::json!({
            "crop_id": "public-001", "image": "image.jpg", "group": "public",
            "lang": "eng", "for_scoring": true, "transcription": "HELLO NEWSPAPER",
            "transcriber": "public dataset", "reviewer": "published review"
        });
        std::fs::write(dir.join("crops.jsonl"), row.to_string()).unwrap();
        std::fs::create_dir(dir.join("LICENSES")).unwrap();
        let provenance = serde_json::json!({
            "crop_id": "public-001", "source_license": "CC-BY-4.0",
            "source_url": "https://example.org/public",
            "source_attribution": "Synthetic dataset authors",
            "source_dataset": "synthetic-public",
            "crop_sha256": sha256(&std::fs::read(dir.join("image.jpg")).unwrap()),
        });
        std::fs::write(dir.join("manifest.jsonl"), provenance.to_string()).unwrap();
        let mut engine = crate::qwen::stage::tests::engine();
        engine.models = vec![super::super::default_models().pop().unwrap()];
        Options {
            crops: dir.join("crops.jsonl"),
            root: dir.join("campaign"),
            engine,
            max_crops: 20,
            timeout_secs: 600,
            doc_ids: DocIds::Legacy,
            prompt_version: usnm_ocr_contract::qwen::PROMPT_VERSION,
            model: None,
            bundle_size: 1,
        }
    }

    /// `fixture` with `n` crops, each its own small JPEG, as sample doc ids.
    fn fixture_n(dir: &Path, n: usize) -> Options {
        let mut opts = fixture(dir);
        let (mut rows, mut provenance) = (String::new(), String::new());
        for i in 0..n {
            let img = image::RgbImage::from_fn(40 + i as u32, 24, |x, y| {
                image::Rgb([(x * 5) as u8, (y * 9) as u8, (i * 31) as u8])
            });
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
                .encode_image(&img)
                .unwrap();
            let name = format!("crop-{i:03}.jpg");
            std::fs::write(dir.join(&name), &bytes).unwrap();
            let cid = format!("public-{i:03}");
            rows += &serde_json::json!({
                "crop_id": cid, "image": name, "group": "public",
                "lang": "eng", "for_scoring": true, "transcription": format!("TEXT {i}"),
                "transcriber": "public dataset", "reviewer": "published review"
            })
            .to_string();
            rows.push('\n');
            provenance += &serde_json::json!({
                "crop_id": cid, "source_license": "CC-BY-4.0",
                "source_url": "https://example.org/public",
                "source_attribution": "Synthetic dataset authors",
                "source_dataset": "synthetic-public",
                "crop_sha256": sha256(&bytes),
            })
            .to_string();
            provenance.push('\n');
        }
        std::fs::write(dir.join("crops.jsonl"), rows).unwrap();
        std::fs::write(dir.join("manifest.jsonl"), provenance).unwrap();
        opts.max_crops = n;
        opts.doc_ids = DocIds::Sample;
        opts
    }

    /// Writes a unit's three outputs to `spool` with `readings` as the host read them.
    fn spool_result(spool: &Path, job: &Job, outcome: Outcome, texts: &[(String, String)]) {
        use usnm_ocr_contract::qwen::{Reading, Readings, ResultManifest};
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: "vulkan".into(),
            diag: BTreeMap::new(),
            model: Some(job.engine.models[0].name.clone()),
            outcome,
            prompt_version: job.prompt_version,
            readings: texts
                .iter()
                .map(|(id, text)| Reading {
                    id: id.clone(),
                    ms: 3,
                    text: text.clone(),
                    tokens: 4,
                })
                .collect(),
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let result = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-11-12T00:00:00Z".into(),
            crops: job.crops.len() as u32,
            device: "vulkan".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 3,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: readings.model.clone(),
            outcome,
            peak_rss_kib: None,
            read: texts.len() as u32,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        for (name, bytes) in
            outputs()
                .iter()
                .zip([rb, serde_json::to_vec(&result).unwrap(), b"diag".to_vec()])
        {
            std::fs::write(
                spool.join(format!("{}__file_{name}", job.workunit())),
                bytes,
            )
            .unwrap();
        }
    }

    #[test]
    fn bundles_carry_each_crops_bytes_unchanged_and_collect_maps_readings_back() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture_n(tmp.path(), 5);
        opts.bundle_size = 2;
        assert_eq!(stage(&opts).unwrap(), 3);
        // Staging again is a no-op; another packing needs a new root.
        assert_eq!(stage(&opts).unwrap(), 3);
        let manifest = staged(&opts);
        assert_eq!(manifest.entries.len(), 5);
        let sizes: Vec<usize> = manifest.bundles.iter().map(|b| b.crops.len()).collect();
        assert_eq!(sizes, [2, 2, 1]);
        // Only the bundles are units.
        assert_eq!(std::fs::read_dir(units(&opts.root)).unwrap().count(), 3);
        for b in &manifest.bundles {
            let dir = units(&opts.root).join(&b.job.id);
            let blob = std::fs::read(dir.join(PAGE_FILE)).unwrap();
            assert!(b.job.input.matches(&blob));
            let notice = std::fs::read(dir.join(NOTICE_FILE)).unwrap();
            assert_eq!(b.job.input.source, format!("benchmark:{}", sha256(&notice)));
            assert_eq!(
                String::from_utf8_lossy(&notice)
                    .matches("Synthetic dataset authors")
                    .count(),
                1
            );
            for (page, cid) in b.job.pages.iter().zip(&b.crops) {
                // The page's slice is the crop file, byte for byte, so it decodes to the same
                // pixels as the crop a local run reads.
                let i: usize = cid.trim_start_matches("public-").parse().unwrap();
                let original = std::fs::read(tmp.path().join(format!("crop-{i:03}.jpg"))).unwrap();
                let (o, n) = (page.input.offset as usize, page.input.bytes as usize);
                assert_eq!(&blob[o..o + n], original.as_slice());
                assert_eq!(
                    image::load_from_memory(&blob[o..o + n])
                        .unwrap()
                        .to_rgb8()
                        .into_raw(),
                    image::load_from_memory(&original)
                        .unwrap()
                        .to_rgb8()
                        .into_raw()
                );
                assert_eq!(page.doc_id, format!("sample-synthetic-public_{cid}"));
            }
            assert!(
                !String::from_utf8(std::fs::read(dir.join(JOB_FILE)).unwrap())
                    .unwrap()
                    .contains("TEXT")
            );
        }
        let mut other = opts.clone();
        other.bundle_size = 3;
        assert!(stage(&other).unwrap_err().to_string().contains("immutable"));
        other.bundle_size = MAX_BUNDLE_PAGES + 1;
        other.root = tmp.path().join("too-big");
        assert!(stage(&other).is_err());

        // First bundle read in full, the second timed out after one crop, the third failed.
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let ids = |b: &Bundle| -> Vec<String> {
            b.job
                .pages
                .iter()
                .map(|p| format!("{}:{}", p.doc_id, p.crops[0].id))
                .collect()
        };
        let first = ids(&manifest.bundles[0]);
        spool_result(
            &spool,
            &manifest.bundles[0].job,
            Outcome::Ok,
            &[
                (first[0].clone(), "TEXT 0".into()),
                (first[1].clone(), "TEXT 1".into()),
            ],
        );
        let second = ids(&manifest.bundles[1]);
        spool_result(
            &spool,
            &manifest.bundles[1].job,
            Outcome::Timeout,
            &[(second[1].clone(), "TEXT 3".into())],
        );
        std::fs::write(
            spool.join(format!("{}_error", manifest.bundles[2].job.workunit())),
            "",
        )
        .unwrap();
        let out = tmp.path().join("out");
        let s = collect(&opts.root, &spool, &out).unwrap();
        assert_eq!(
            (s.expected, s.returned, s.incomplete, s.failed, s.pending),
            (5, 2, 2, 1, 0)
        );
        let hyps = score::read_jsonl(&out.join("hypotheses.jsonl")).unwrap();
        let text = |cid: &str| {
            hyps.iter().find(|h| h["crop_id"] == cid).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(text("public-000"), "TEXT 0");
        assert_eq!(text("public-001"), "TEXT 1");
        assert_eq!(text("public-002"), "");
        assert_eq!(text("public-003"), "TEXT 3");
        assert_eq!(text("public-004"), "");
        let details = score::read_jsonl(&out.join("by-crop.jsonl")).unwrap();
        assert_eq!(details[2]["status"], "timeout");
        assert_eq!(details[3]["status"], "timeout");
        assert_eq!(details[0]["unit_job_id"], manifest.bundles[0].job.id);

        // A reading of a crop the bundle doesn't hold is refused.
        spool_result(
            &spool,
            &manifest.bundles[1].job,
            Outcome::Timeout,
            &[(first[0].clone(), "TEXT 0".into())],
        );
        assert!(collect(&opts.root, &spool, &out)
            .unwrap_err()
            .to_string()
            .contains("unexpected readings"));
    }

    fn staged(opts: &Options) -> Manifest {
        stage(opts).unwrap();
        serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap()
    }

    #[test]
    fn request_version_and_model_pin_are_staged_and_frozen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture(tmp.path());
        let name = opts.engine.models[0].name.clone();
        let plain = staged(&opts).entries.remove(0).job;
        assert_eq!((plain.prompt_version, plain.model.clone()), (1, None));
        opts.root = tmp.path().join("guarded");
        opts.prompt_version = usnm_ocr_contract::qwen::PROMPT_VERSION_LOOP_GUARD;
        opts.model = Some(name.clone());
        let job = staged(&opts).entries.remove(0).job;
        assert_eq!(job.prompt_version, 3);
        assert_eq!(job.model.as_deref(), Some(name.as_str()));
        assert_ne!(job.id, plain.id);
        opts.prompt_version = 1;
        assert!(stage(&opts).unwrap_err().to_string().contains("immutable"));
        opts.root = tmp.path().join("foreign-pin");
        opts.model = Some("qwen3-vl-8b-q4km-other".into());
        assert!(stage(&opts)
            .unwrap_err()
            .to_string()
            .contains("not the engine's one model"));
    }

    #[test]
    fn legacy_doc_ids_are_unchanged_and_sample_ids_need_a_new_root() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture(tmp.path());
        let legacy = staged(&opts);
        assert_eq!(
            legacy.entries[0].job.doc_id,
            "benchmark-6f892088f1a6f86391beba14f4f5b382bc3621592e906db75762a7a1db726c61"
        );
        opts.doc_ids = DocIds::Sample;
        let err = stage(&opts).unwrap_err().to_string();
        assert!(err.contains("immutable"), "{err}");
        opts.root = tmp.path().join("sample-campaign");
        let job = staged(&opts).entries.remove(0).job;
        assert_eq!(job.doc_id, "sample-synthetic-public_public-001");
        assert_ne!(job.id, legacy.entries[0].job.id);
        assert_eq!(
            DocId::parse(&job.doc_id).unwrap(),
            DocId::Page("sample:synthetic-public/public-001".parse().unwrap())
        );
        let path = tmp.path().join("manifest.jsonl");
        let row: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        opts.root = tmp.path().join("bad-dataset");
        for dataset in [Value::Null, "Synthetic_Public".into(), "../x".into()] {
            let mut bad = row.clone();
            bad["source_dataset"] = dataset;
            std::fs::write(&path, bad.to_string()).unwrap();
            assert!(stage(&opts).is_err(), "{bad}");
            assert!(!opts.root.exists());
        }
    }

    #[test]
    fn legacy_and_sample_campaigns_collect_and_a_foreign_doc_id_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let out = tmp.path().join("out");
        let mut opts = fixture(tmp.path());
        let forged = [
            DocId::legacy_benchmark("public-002").wire().unwrap(),
            "sample-nzz_public-001".into(),
            "sample-synthetic-public_public-002".into(),
            "sn85055101_1917-04-03_ed-1_seq-3".into(),
        ];
        for doc_ids in [DocIds::Legacy, DocIds::Sample] {
            opts.doc_ids = doc_ids;
            opts.root = tmp.path().join(format!("{doc_ids:?}"));
            stage(&opts).unwrap();
            let path = opts.root.join(MANIFEST);
            let original = std::fs::read(&path).unwrap();
            for doc_id in &forged {
                let mut manifest: Manifest = serde_json::from_slice(&original).unwrap();
                let job = &mut manifest.entries[0].job;
                job.doc_id.clone_from(doc_id);
                job.id = job.identity().unwrap();
                std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
                let err = collect(&opts.root, &spool, &out).unwrap_err().to_string();
                assert!(err.contains("does not name crop"), "{doc_id}: {err}");
            }
            std::fs::write(&path, &original).unwrap();
            let s = collect(&opts.root, &spool, &out).unwrap();
            assert_eq!((s.expected, s.pending), (1, 1), "{doc_ids:?}");
        }
    }

    #[test]
    fn immutable_units_contain_no_truth_and_missing_results_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture(tmp.path());
        assert_eq!(stage(&opts).unwrap(), 1);
        assert_eq!(stage(&opts).unwrap(), 1);
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let bytes = std::fs::read(units(&opts.root).join(&job.id).join(JOB_FILE)).unwrap();
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("HELLO NEWSPAPER"));
        assert_eq!(job.crops[0].loc_text, "");
        let notice = std::fs::read(units(&opts.root).join(&job.id).join(NOTICE_FILE)).unwrap();
        assert!(String::from_utf8_lossy(&notice).contains("Synthetic dataset authors"));
        assert!(!String::from_utf8_lossy(&notice).contains("HELLO NEWSPAPER"));
        assert_eq!(job.input.source, format!("benchmark:{}", sha256(&notice)));
        let out = tmp.path().join("out");
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let s = collect(&opts.root, &spool, &out).unwrap();
        assert_eq!((s.expected, s.pending, s.returned), (1, 1, 0));
        let csv = std::fs::read_to_string(out.join("cer-summary.csv")).unwrap();
        assert!(csv.contains("1.0"), "{csv}");
        let details = score::read_jsonl(&out.join("by-crop.jsonl")).unwrap();
        assert_eq!(details[0]["char_deletions"], "HELLO NEWSPAPER".len());
        assert_eq!(details[0]["char_insertions"], 0);
        std::fs::write(spool.join(format!("{}_error", job.workunit())), "").unwrap();
        assert_eq!(collect(&opts.root, &spool, &out).unwrap().failed, 1);
        std::fs::write(
            spool.join(format!("{}__file_readings.json", job.workunit())),
            "{}",
        )
        .unwrap();
        assert!(collect(&opts.root, &spool, &out).is_err());
        opts.engine.image = format!("sha256:{}", "f".repeat(64));
        assert!(stage(&opts).is_err());
    }

    #[test]
    fn an_apache_licensed_source_gets_its_licence_terms_in_the_notice() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        let path = tmp.path().join("manifest.jsonl");
        let mut row: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        row["source_license"] =
            "Apache-2.0 (repository metadata). The page images are public domain".into();
        std::fs::write(&path, row.to_string()).unwrap();
        stage(&opts).unwrap();
        let manifest = staged(&opts);
        let dir = units(&opts.root).join(&manifest.entries[0].job.id);
        let notice = std::fs::read_to_string(dir.join(NOTICE_FILE)).unwrap();
        assert!(
            notice.contains("https://www.apache.org/licenses/LICENSE-2.0"),
            "{notice}"
        );
        row["source_license"] = "All rights reserved".into();
        std::fs::write(&path, row.to_string()).unwrap();
        let mut other = opts.clone();
        other.root = tmp.path().join("reserved");
        assert!(stage(&other)
            .unwrap_err()
            .to_string()
            .contains("unsupported source licence"));
    }

    #[test]
    fn refuses_weak_truth_unreviewed_rows_and_escaping_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        let original: Value = serde_json::from_slice(&std::fs::read(&opts.crops).unwrap()).unwrap();
        for (key, value) in [
            ("for_scoring", Value::Bool(false)),
            ("reviewer", Value::Null),
            ("image", Value::String("../outside.jpg".into())),
        ] {
            let mut row = original.clone();
            row[key] = value;
            std::fs::write(&opts.crops, row.to_string()).unwrap();
            assert!(stage(&opts).is_err(), "{key}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn submission_pins_notices_and_downloads_only_three_public_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        stage(&opts).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let unit = units(&opts.root).join(&job.id);
        let path = unit.join(NOTICE_FILE);
        let original = std::fs::read(&path).unwrap();
        let project = tmp.path().join("project");
        for subdir in ["bin", "templates", "download"] {
            std::fs::create_dir_all(project.join(subdir)).unwrap();
        }
        for (name, script) in [
            (
                "dir_hier_path",
                "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n",
            ),
            ("create_work", "#!/bin/sh\nprintf '%s\\n' \"$@\" > args\n"),
        ] {
            let path = project.join("bin").join(name);
            usnm_test_support::write_executable(&path, script).unwrap();
        }
        let submit = super::super::submit::Options {
            root: opts.root.clone(),
            project: project.clone(),
            max_units: 1,
            target: super::super::submit::Target::Container,
            model_dir: None,
            models: super::super::submit::Models::Verify,
        };
        std::fs::write(&path, "changed notices").unwrap();
        assert!(super::super::submit::run(&submit).await.is_err());
        assert!(!unit.join("submitting").exists());
        assert!(!project.join("args").exists());
        std::fs::write(&path, &original).unwrap();
        assert_eq!(
            super::super::submit::run(&submit).await.unwrap().submitted,
            1
        );
        assert!(unit.join("submitted").exists());
        assert_eq!(
            super::super::submit::run(&submit).await.unwrap().submitted,
            0
        );
        assert!(std::fs::read_to_string(project.join("args"))
            .unwrap()
            .contains("templates/usnm_qwen_benchmark_in"));
        let files: Vec<_> = std::fs::read_dir(project.join("download"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 3);
        for file in files {
            assert!(
                !String::from_utf8_lossy(&std::fs::read(file).unwrap()).contains("HELLO NEWSPAPER")
            );
        }
        assert_eq!(
            std::fs::read(
                project
                    .join("download")
                    .join(format!("{}_notice_txt", job.id))
            )
            .unwrap(),
            original,
        );
    }

    #[test]
    fn canonical_readings_score_and_foreign_engines_are_rejected() {
        use usnm_ocr_contract::qwen::{Reading, Readings, ResultManifest};
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        stage(&opts).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: "cpu".into(),
            diag: BTreeMap::new(),
            model: Some(job.engine.models[0].name.clone()),
            outcome: Outcome::Ok,
            prompt_version: usnm_ocr_contract::qwen::PROMPT_VERSION,
            readings: vec![Reading {
                id: "public-001".into(),
                ms: 3,
                text: "HELLO NEWSPAPER".into(),
                tokens: 4,
            }],
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let mut result = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-10-15T00:00:00Z".into(),
            crops: 1,
            device: "cpu".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 3,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: readings.model.clone(),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: 1,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        for (name, bytes) in
            outputs()
                .iter()
                .zip([rb, serde_json::to_vec(&result).unwrap(), b"diag".to_vec()])
        {
            std::fs::write(
                spool.join(format!("{}__file_{name}", job.workunit())),
                bytes,
            )
            .unwrap();
        }
        let out = tmp.path().join("out");
        let s = collect(&opts.root, &spool, &out).unwrap();
        assert_eq!(
            (s.returned, s.pending, s.failed, s.incomplete),
            (1, 0, 0, 0)
        );
        let details = score::read_jsonl(&out.join("by-crop.jsonl")).unwrap();
        assert_eq!(details[0]["counts"]["char_edits"], 0);
        result.engine.weights = format!("sha256:{}", "e".repeat(64));
        std::fs::write(
            spool.join(format!("{}__file_result.json", job.workunit())),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        assert!(collect(&opts.root, &spool, &out).is_err());
    }

    #[test]
    fn a_reading_at_another_request_version_is_refused() {
        use usnm_ocr_contract::qwen::{Reading, Readings, ResultManifest};
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture(tmp.path());
        opts.prompt_version = usnm_ocr_contract::qwen::PROMPT_VERSION_LOOP_GUARD;
        opts.model = Some(opts.engine.models[0].name.clone());
        stage(&opts).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let out = tmp.path().join("out");
        for (version, accepted) in [(1, false), (job.prompt_version, true)] {
            let readings = Readings {
                v: VERSION,
                job_id: job.id.clone(),
                device: "cpu".into(),
                diag: BTreeMap::new(),
                model: Some(job.engine.models[0].name.clone()),
                outcome: Outcome::Ok,
                prompt_version: version,
                readings: vec![Reading {
                    id: "public-001".into(),
                    ms: 3,
                    text: "HELLO NEWSPAPER".into(),
                    tokens: 4,
                }],
            };
            let rb = serde_json::to_vec(&readings).unwrap();
            let result = ResultManifest {
                v: VERSION,
                job_id: job.id.clone(),
                completed_at: "2026-10-15T00:00:00Z".into(),
                crops: 1,
                device: "cpu".into(),
                diag_bytes: 4,
                diag_sha256: sha256(b"diag"),
                engine: job.engine.clone(),
                engine_ms: 3,
                gpu_mem_peak_mib: None,
                host: Default::default(),
                load_ms: 1,
                model: readings.model.clone(),
                outcome: Outcome::Ok,
                peak_rss_kib: None,
                read: 1,
                readings_bytes: rb.len() as u64,
                readings_sha256: sha256(&rb),
                unit_error: None,
            };
            for (name, bytes) in
                outputs()
                    .iter()
                    .zip([rb, serde_json::to_vec(&result).unwrap(), b"diag".to_vec()])
            {
                std::fs::write(
                    spool.join(format!("{}__file_{name}", job.workunit())),
                    bytes,
                )
                .unwrap();
            }
            let got = collect(&opts.root, &spool, &out);
            if accepted {
                assert_eq!(got.unwrap().returned, 1);
            } else {
                let err = got.unwrap_err().to_string();
                assert!(err.contains("another request version"), "{err}");
            }
        }
    }
}
