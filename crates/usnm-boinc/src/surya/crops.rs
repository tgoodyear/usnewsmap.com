//! Crop units (#968): one `usnm_surya` unit per crop of a page, cut on the
//! server from the staged `usnm_quality` page with the campaign's recipe
//! (`crop_seed`, `k_crops`), so Surya reads exactly the boxes `usnm_qwen`
//! reads and `usnm_quality` scores. A crop is read in `whole` mode only and
//! takes seconds, so CPU hosts can run it and a wave can be thousands deep.
//!
//! The pages come from the `usnm_quality` staging root, as `qwen stage`
//! takes them: each holds `page.jpg`, `loc.xml` and `quality.json`, so LoC
//! is not asked again. A page's unit directories are
//! `units/loc/<doc_id>.<crop_id>` (`units/<doc_id>.<crop_id>` in a campaign
//! staged before ADR-0026); the quality unit's `sample.json`, when there, is
//! copied into each for a collector.
use super::{check_no_hold, Units, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use image::GenericImageView as _;
use std::path::PathBuf;
use usnm_core::page_id::{PageId, Source};
use usnm_ocr_contract::{
    quality::{Job as QualityJob, MAX_ALTO, MAX_JOB_JSON as MAX_QUALITY_JOB, MAX_SCAN},
    surya::{CropRef, Engine, Input, InputMode, Job, VERSION},
};

/// JPEG quality of a cut crop: close to the page's pixels, well under the
/// unit size limit.
pub const JPEG_QUALITY: u8 = 95;

#[derive(Debug, Clone)]
pub struct Options {
    /// The `usnm_quality` staging root whose pages to cut crops from.
    pub quality_root: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    /// Deadline for the one `whole` run, on a GPU and on a CPU.
    pub timeout_secs: u32,
    /// Pages to take new units from, at most.
    pub max_pages: usize,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Pages whose crops were staged.
    pub pages: usize,
    /// Crop units written.
    pub staged: usize,
    /// Pages whose ALTO has no line the recipe can cut.
    pub no_crops: usize,
    /// Pages whose units were already there.
    pub already: usize,
}

/// Writes the crop units of every staged `usnm_quality` page (those with a
/// `quality.json`), in doc id order, up to `max_pages` pages with a crop
/// still to stage. Every crop is cut before any unit of the page is
/// written; a crop whose unit is already there (its `surya.json` exists)
/// is left as it is, so a run that stopped part way through a page
/// finishes it next time without touching units that may be submitted.
pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    crate::qwen::check_digest(&opts.engine.image, "--engine-image")?;
    crate::qwen::check_digest(&opts.engine.llama, "--engine-llama")?;
    crate::qwen::check_digest(&opts.engine.weights, "--engine-weights")?;
    check_no_hold(&opts.root)?;
    let dir = Units::open(&opts.root)?;
    let pages_dir = opts.quality_root.join("pages");
    let src = if pages_dir.exists() {
        pages_dir
    } else {
        crate::quality::units(&opts.quality_root)
    };
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&src)
        .with_context(|| format!("reading {}", src.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(crate::quality::JOB_FILE).exists())
        .collect();
    pages.sort();
    let mut summary = Summary::default();
    for page in pages {
        if summary.pages + summary.no_crops >= opts.max_pages {
            break;
        }
        let name = page
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let page_id = PageId::new(Source::Loc, &name)?;
        let marker = dir.path(&page_id, Some("no_crops"))?;
        if marker.exists() {
            summary.already += 1;
            continue;
        }
        let quality: QualityJob = serde_json::from_slice(&read(
            &page.join(crate::quality::JOB_FILE),
            MAX_QUALITY_JOB,
        )?)
        .with_context(|| format!("{name}: quality.json"))?;
        quality.validate()?;
        let bytes = read(&page.join(crate::quality::PAGE_FILE), MAX_SCAN)?;
        ensure!(
            quality.inputs.page.matches(&bytes),
            "{name}: staged page changed"
        );
        let alto = read(&page.join(crate::quality::ALTO_FILE), MAX_ALTO)?;
        ensure!(
            quality.inputs.alto.matches(&alto),
            "{name}: staged ALTO changed"
        );
        let crops = crate::qwen::stage::cut(&bytes, &alto, quality.crop_seed, quality.k_crops)
            .with_context(|| format!("{name}: cutting crops"))?;
        if crops.is_empty() {
            atomic(&marker, b"the ALTO has no line the recipe can cut\n")?;
            summary.no_crops += 1;
            continue;
        }
        let image = image::load_from_memory(&bytes)
            .with_context(|| format!("{name}: decoding page.jpg"))?;
        let sample = page.join(crate::quality::stage::SAMPLE_FILE);
        let sample = sample
            .exists()
            .then(|| std::fs::read(&sample))
            .transpose()?;
        let mut staged = Vec::with_capacity(crops.len());
        for c in &crops {
            if dir.path(&page_id, Some(&c.id))?.join(JOB_FILE).exists() {
                continue;
            }
            let crop = CropRef {
                id: c.id.clone(),
                rect: c.rect,
                stratum: c.stratum.clone(),
            };
            let jpeg = cut_jpeg(&image, c.rect)
                .with_context(|| format!("{name}: cutting crop {}", c.id))?;
            let job = crop_job(&quality, &crop, &jpeg, &opts.engine, opts.timeout_secs)
                .with_context(|| format!("{name}: crop {} job", c.id))?;
            staged.push((crop, jpeg, job));
        }
        if staged.is_empty() {
            summary.already += 1;
            continue;
        }
        for (crop, jpeg, job) in staged {
            let unit = dir.path(&page_id, Some(&crop.id))?;
            std::fs::create_dir_all(&unit)?;
            atomic(&unit.join(PAGE_FILE), &jpeg)?;
            if let Some(sample) = &sample {
                atomic(&unit.join(crate::quality::stage::SAMPLE_FILE), sample)?;
            }
            atomic(&unit.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
            summary.staged += 1;
        }
        summary.pages += 1;
    }
    Ok(summary)
}

/// `<doc_id>.<crop_id>`: the page's quality unit name and the crop's id.
pub fn unit_name(page: &str, crop_id: &str) -> String {
    format!("{page}.{crop_id}")
}

/// The box's pixels, clamped to the page, as a JPEG.
pub fn cut_jpeg(page: &image::DynamicImage, rect: [u32; 4]) -> anyhow::Result<Vec<u8>> {
    let (w, h) = page.dimensions();
    let [x0, y0, x1, y1] = rect;
    let (x1, y1) = (x1.min(w), y1.min(h));
    ensure!(
        x0 < x1 && y0 < y1,
        "box {rect:?} is outside the {w}x{h} page"
    );
    let crop = page.crop_imm(x0, y0, x1 - x0, y1 - y0).to_rgb8();
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&crop)
        .context("encoding the crop")?;
    let bytes = out.into_inner();
    ensure!(bytes.len() <= MAX_SCAN, "crop JPEG over size limit");
    Ok(bytes)
}

/// The crop unit's job: the page's ids and language, the crop's JPEG as the
/// input with the page's source and the box as its fragment, `whole` only.
pub fn crop_job(
    quality: &QualityJob,
    crop: &CropRef,
    jpeg: &[u8],
    engine: &Engine,
    timeout_secs: u32,
) -> anyhow::Result<Job> {
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        doc_id: quality.doc_id.clone(),
        group: quality.group.clone(),
        language: quality.language.clone(),
        input: Input {
            bytes: jpeg.len() as u64,
            sha256: crate::boinc::sha256(jpeg),
            url: format!("{}#{}", quality.inputs.page.source, crop.xywh()),
        },
        engine: engine.clone(),
        gpu_modes: vec![InputMode::Whole],
        cpu_modes: vec![InputMode::Whole],
        gpu_timeout_secs: timeout_secs,
        cpu_timeout_secs: timeout_secs,
        crop: Some(crop.clone()),
    };
    job.id = job.identity()?;
    job.validate().context("staged crop job")?;
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::super::units;
    use super::*;

    fn engine() -> Engine {
        Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", "b".repeat(64)),
            surya: "0.22.1".into(),
            weights: format!("sha256:{}", "c".repeat(64)),
        }
    }

    #[tokio::test]
    async fn stages_one_unit_per_crop_of_every_quality_page() {
        let tmp = tempfile::tempdir().unwrap();
        let qroot = tmp.path().join("quality");
        let page = "sn83045462_1890-05-03_ed-1_seq-1";
        let unit = crate::quality::units(&qroot).join(page);
        let qjob = crate::quality::work::tests::unit(&unit);
        std::fs::write(unit.join("sample.json"), b"{}").unwrap();
        let opts = Options {
            quality_root: qroot.clone(),
            root: tmp.path().join("surya"),
            engine: engine(),
            timeout_secs: 600,
            max_pages: 20,
        };
        let s = run(&opts).await.unwrap();
        let expected = crate::qwen::stage::cut(
            &std::fs::read(unit.join("page.jpg")).unwrap(),
            &std::fs::read(unit.join("loc.xml")).unwrap(),
            qjob.crop_seed,
            qjob.k_crops,
        )
        .unwrap();
        assert!(!expected.is_empty());
        assert_eq!(
            s,
            Summary {
                pages: 1,
                staged: expected.len(),
                no_crops: 0,
                already: 0
            }
        );
        let full = image::load_from_memory(&std::fs::read(unit.join("page.jpg")).unwrap()).unwrap();
        for c in &expected {
            let dir = units(&opts.root).join("loc").join(unit_name(page, &c.id));
            let job: Job =
                serde_json::from_slice(&std::fs::read(dir.join(JOB_FILE)).unwrap()).unwrap();
            job.validate().unwrap();
            assert_eq!(job.doc_id, qjob.doc_id);
            assert_eq!(job.language, qjob.language);
            let crop = job.crop.as_ref().unwrap();
            assert_eq!((crop.id.as_str(), crop.rect), (c.id.as_str(), c.rect));
            assert_eq!(job.gpu_modes, vec![InputMode::Whole]);
            assert_eq!(job.cpu_modes, vec![InputMode::Whole]);
            assert!(job.input.url.ends_with(&format!("#{}", crop.xywh())));
            // The JPEG is the box's pixels, and the job names it.
            let jpeg = std::fs::read(dir.join(PAGE_FILE)).unwrap();
            assert_eq!(jpeg.len() as u64, job.input.bytes);
            assert_eq!(crate::boinc::sha256(&jpeg), job.input.sha256);
            let (w, h) = image::load_from_memory(&jpeg).unwrap().dimensions();
            let [x0, y0, x1, y1] = c.rect;
            assert_eq!(
                (w, h),
                (x1.min(full.width()) - x0, y1.min(full.height()) - y0)
            );
            assert!(dir.join("sample.json").exists());
        }
        // Ids differ between crops of one page, and from the page unit's.
        let ids: std::collections::BTreeSet<String> = expected
            .iter()
            .map(|c| {
                let dir = units(&opts.root).join("loc").join(unit_name(page, &c.id));
                serde_json::from_slice::<Job>(&std::fs::read(dir.join(JOB_FILE)).unwrap())
                    .unwrap()
                    .id
            })
            .collect();
        assert_eq!(ids.len(), expected.len());
        // A second run stages nothing new.
        let s = run(&opts).await.unwrap();
        assert_eq!((s.pages, s.staged, s.already), (0, 0, 1));
        // A run that stopped after the first crop's unit was written
        // finishes the page next time and leaves that unit alone.
        let first = units(&opts.root)
            .join("loc")
            .join(unit_name(page, &expected[0].id));
        let kept = std::fs::read(first.join(JOB_FILE)).unwrap();
        for c in &expected[1..] {
            std::fs::remove_dir_all(units(&opts.root).join("loc").join(unit_name(page, &c.id)))
                .unwrap();
        }
        let s = run(&opts).await.unwrap();
        assert_eq!(
            (s.pages, s.staged, s.already),
            (1, expected.len() - 1, 0),
            "{s:?}"
        );
        assert_eq!(std::fs::read(first.join(JOB_FILE)).unwrap(), kept);
        for c in &expected {
            assert!(units(&opts.root)
                .join("loc")
                .join(unit_name(page, &c.id))
                .join(JOB_FILE)
                .exists());
        }
        // A quality group with a digit, as the campaign names them, is kept.
        let mut qjob = qjob;
        qjob.group = "campaign-1".into();
        let crop = CropRef {
            id: "c01".into(),
            rect: [10, 20, 500, 60],
            stratum: "wc-low/top".into(),
        };
        let job = crop_job(&qjob, &crop, b"jpeg", &engine(), 600).unwrap();
        assert_eq!(job.group, "campaign-1");
    }

    #[test]
    fn cut_jpeg_clamps_to_the_page() {
        let img = image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(
            100,
            50,
            image::Luma([200]),
        ));
        let (w, h) = image::load_from_memory(&cut_jpeg(&img, [90, 40, 200, 100]).unwrap())
            .unwrap()
            .dimensions();
        assert_eq!((w, h), (10, 10));
        assert!(cut_jpeg(&img, [100, 0, 120, 10]).is_err());
    }
}
