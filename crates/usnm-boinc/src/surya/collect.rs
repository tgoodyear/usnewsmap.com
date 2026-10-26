//! Reads a Surya campaign's canonical results back from the assimilator's
//! spool (`<workunit>__file_<open name>`, as `quality collect` reads the
//! quality and Qwen spools), by page and by crop, and exports each page's
//! text as the `ours-surya` reader for `quality stage` (#968).
//!
//! A page unit gives the page's text in each mode that ran; a crop unit
//! (`Job::crop`) gives one crop's text. `run` writes
//! `<texts_dir>/<doc_id>.surya.json`, a `TextReader` named `ours-surya`
//! whose text is the first mode of `--modes` the page has an `ok` run for,
//! and whose `source` records the mode and the job. `quality collect`
//! joins the crop texts into `crops.parquet` through [`results`].
use super::{unit_dirs, validate, JOB_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    quality::TextReader,
    surya::{InputMode, Job, Outcome, MAX_JSON},
};

/// The reader's name in `texts.json`, as `quality stage` already maps our
/// OCR runs of Surya (`ours_reader_name`).
pub const READER: &str = "ours-surya";
/// The crop-level reader's name in `crops.parquet`.
pub const CROP_READER: &str = "surya";
/// Which mode's text stands for the page, first found wins.
pub const DEFAULT_MODES: [InputMode; 3] = [InputMode::Whole, InputMode::Capped, InputMode::Tiled];

/// One mode's run of a page unit, with its text.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeText {
    pub job_id: String,
    pub outcome: Outcome,
    pub device: String,
    pub text: String,
}

/// One crop unit's result.
#[derive(Debug, Clone, PartialEq)]
pub struct CropText {
    pub job_id: String,
    pub crop_id: String,
    pub rect: [u32; 4],
    pub stratum: String,
    pub outcome: Outcome,
    pub device: String,
    /// The `whole` read of the crop; empty when it did not run or failed.
    pub text: String,
    pub plan_class: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Page units with a canonical result.
    pub pages: usize,
    /// Crop units with a canonical result.
    pub crops: usize,
    /// Submitted units with no canonical result yet.
    pub pending: usize,
    /// Units BOINC gave up on.
    pub failed: usize,
    /// `<doc_id>.surya.json` files written.
    pub exported: usize,
    /// Earlier exports removed because no requested mode read the page.
    pub removed: usize,
}

/// A campaign's results: page texts by doc id and mode, crop texts by doc
/// id and crop id.
#[derive(Debug, Default)]
pub struct Results {
    pub pages: BTreeMap<String, BTreeMap<InputMode, ModeText>>,
    pub crops: BTreeMap<String, BTreeMap<String, CropText>>,
    pub summary: Summary,
}

fn spool_files(spool: &Path, workunit: &str) -> Vec<PathBuf> {
    super::outputs()
        .iter()
        .map(|n| spool.join(format!("{workunit}__file_{n}")))
        .collect()
}

fn gave_up(spool: &Path, workunit: &str) -> bool {
    spool.join(format!("{workunit}_error")).exists()
        || spool.join(format!("{workunit}_no_output_files")).exists()
}

/// Every staged unit under `root` with its canonical result in `spool`.
pub fn results(root: &Path, spool: &Path) -> anyhow::Result<Results> {
    let mut out = Results::default();
    for unit in unit_dirs(root)? {
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JSON)?)
            .with_context(|| format!("{}", unit.display()))?;
        let wu = job.workunit();
        if gave_up(spool, &wu) {
            out.summary.failed += 1;
            continue;
        }
        let files = spool_files(spool, &wu);
        if !files.iter().all(|f| f.exists()) {
            out.summary.pending += 1;
            continue;
        }
        let got = validate::load(&files).with_context(|| format!("canonical result {wu}"))?;
        ensure!(
            got.result.job_id == job.id && got.result.engine == job.engine,
            "{wu}: result does not belong to its job"
        );
        let text_of = |m: InputMode| -> String {
            got.texts
                .get(&m)
                .map(|t| String::from_utf8_lossy(t).into_owned())
                .unwrap_or_default()
        };
        match &job.crop {
            Some(crop) => {
                let run = got.result.runs.iter().find(|r| r.mode == InputMode::Whole);
                out.crops.entry(job.doc_id.clone()).or_default().insert(
                    crop.id.clone(),
                    CropText {
                        job_id: job.id.clone(),
                        crop_id: crop.id.clone(),
                        rect: crop.rect,
                        stratum: crop.stratum.clone(),
                        outcome: run.map_or(Outcome::Failed, |r| r.outcome),
                        device: run.map(|r| r.device.clone()).unwrap_or_default(),
                        text: if run.is_some_and(|r| r.outcome == Outcome::Ok) {
                            text_of(InputMode::Whole)
                        } else {
                            String::new()
                        },
                        plan_class: got.result.host.plan_class.clone(),
                    },
                );
                out.summary.crops += 1;
            }
            None => {
                let page = out.pages.entry(job.doc_id.clone()).or_default();
                for r in &got.result.runs {
                    page.insert(
                        r.mode,
                        ModeText {
                            job_id: job.id.clone(),
                            outcome: r.outcome,
                            device: r.device.clone(),
                            text: text_of(r.mode),
                        },
                    );
                }
                out.summary.pages += 1;
            }
        }
    }
    Ok(out)
}

/// The page's `ours-surya` reader: the first mode in `prefer` with an `ok`
/// run and some text. Its `source` is `surya:<mode>:<job id>`.
pub fn page_reader(
    page: &BTreeMap<InputMode, ModeText>,
    prefer: &[InputMode],
) -> Option<TextReader> {
    prefer.iter().find_map(|m| {
        let t = page.get(m)?;
        (t.outcome == Outcome::Ok && !t.text.trim().is_empty()).then(|| TextReader {
            alto: false,
            name: READER.into(),
            regions: vec![],
            source: format!("surya:{}:{}", m.name(), t.job_id),
            text: t.text.clone(),
        })
    })
}

/// The export file for a page, beside the hand-off's `<doc_id>.json`.
pub fn export_name(doc_id: &str) -> String {
    format!("{doc_id}.surya.json")
}

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub spool: PathBuf,
    /// Where `<doc_id>.surya.json` goes, for `quality stage --surya-texts`.
    pub texts_dir: Option<PathBuf>,
    pub modes: Vec<InputMode>,
}

pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(!opts.modes.is_empty(), "--modes names at least one mode");
    let mut r = results(&opts.root, &opts.spool)?;
    if let Some(dir) = &opts.texts_dir {
        std::fs::create_dir_all(dir)?;
        for (doc_id, page) in &r.pages {
            // `quality stage` reads LoC pages, whose ids stay bare
            // (ADR-0026); a qualified id, a benchmark sample, has no export.
            if doc_id.contains(':') {
                continue;
            }
            let path = dir.join(export_name(doc_id));
            match page_reader(page, &opts.modes) {
                Some(reader) => {
                    atomic(&path, &serde_json::to_vec_pretty(&reader)?)?;
                    r.summary.exported += 1;
                }
                // An earlier run with other modes may have exported this
                // page; under these modes it has no reader, so none stays.
                None => match std::fs::remove_file(&path) {
                    Ok(()) => r.summary.removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).context(format!("removing {}", path.display())),
                },
            }
        }
    }
    Ok(r.summary)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::units;
    use super::*;
    use crate::boinc::sha256;
    use usnm_ocr_contract::surya::{CropRef, Host, ResultManifest, Run, VERSION};

    /// Writes a unit and its canonical result into `root` and `spool`:
    /// `texts` gives each mode's outcome and text.
    pub(crate) fn unit_with_result(
        root: &Path,
        spool: &Path,
        job: &Job,
        texts: &[(InputMode, Outcome, &str)],
    ) {
        // A bare LoC id as before ADR-0026; a qualified one at its path.
        let page = if job.doc_id.contains(':') {
            let id: usnm_core::page_id::PageId = job.doc_id.parse().unwrap();
            id.path()
        } else {
            job.doc_id.clone()
        };
        let unit = units(root).join(match &job.crop {
            Some(c) => super::super::crops::unit_name(&page, &c.id),
            None => page,
        });
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(JOB_FILE), serde_json::to_vec(job).unwrap()).unwrap();
        std::fs::create_dir_all(spool).unwrap();
        let wu = job.workunit();
        let diag = b"== boinc\n";
        let mut runs = Vec::new();
        for (m, o, t) in texts {
            let t = if *o == Outcome::Ok { *t } else { "" };
            std::fs::write(spool.join(format!("{wu}__file_{}", m.text())), t).unwrap();
            runs.push(Run {
                code: (*o == Outcome::Ok).then_some(0),
                cpu_secs: None,
                device: "cpu".into(),
                engine_ms: 1,
                gpu_mem_peak_mib: None,
                gpu_layers: None,
                mode: *m,
                outcome: *o,
                peak_rss_kib: None,
                signal: None,
                text_bytes: t.len() as u64,
                text_sha256: sha256(t.as_bytes()),
                tiles: Some(1),
            });
        }
        for m in usnm_ocr_contract::surya::MODES {
            let f = spool.join(format!("{wu}__file_{}", m.text()));
            if !f.exists() {
                std::fs::write(f, b"").unwrap();
            }
        }
        let result = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            host: Host {
                plan_class: Some("docker_mt".into()),
                ..Host::default()
            },
            unit_error: None,
            runs,
            diag_bytes: diag.len() as u64,
            diag_sha256: sha256(diag),
            completed_at: "2026-10-22T00:00:00Z".into(),
        };
        std::fs::write(
            spool.join(format!("{wu}__file_result.json")),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        std::fs::write(spool.join(format!("{wu}__file_diag.txt")), diag).unwrap();
    }

    pub(crate) fn crop_job(page: &Job, id: &str, rect: [u32; 4], stratum: &str) -> Job {
        let mut j = page.clone();
        let crop = CropRef {
            id: id.into(),
            rect,
            stratum: stratum.into(),
        };
        j.input.url = format!("{}#{}", page.input.url, crop.xywh());
        j.gpu_modes = vec![InputMode::Whole];
        j.cpu_modes = vec![InputMode::Whole];
        j.crop = Some(crop);
        j.id = j.identity().unwrap();
        j
    }

    #[test]
    fn reads_pages_and_crops_and_exports_the_page_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, spool) = (tmp.path().join("surya"), tmp.path().join("spool"));
        let page = super::super::work::tests::job();
        // The page: whole timed out, capped read, tiled read.
        unit_with_result(
            &root,
            &spool,
            &page,
            &[
                (InputMode::Whole, Outcome::Timeout, ""),
                (InputMode::Capped, Outcome::Ok, "Capped text of the page\n"),
                (InputMode::Tiled, Outcome::Ok, "Tiled text\n"),
            ],
        );
        // Two crops: one read, one failed.
        let c1 = crop_job(&page, "c01", [10, 20, 500, 60], "wc-low/top");
        let c2 = crop_job(&page, "c02", [10, 80, 500, 120], "wc-low/top");
        unit_with_result(
            &root,
            &spool,
            &c1,
            &[(InputMode::Whole, Outcome::Ok, "first line\n")],
        );
        unit_with_result(
            &root,
            &spool,
            &c2,
            &[(InputMode::Whole, Outcome::Failed, "")],
        );
        // A crop still out, and one BOINC gave up on.
        let c3 = crop_job(&page, "c03", [10, 140, 500, 180], "wc-low/top");
        let unit = units(&root).join(super::super::crops::unit_name(&c3.doc_id, "c03"));
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(JOB_FILE), serde_json::to_vec(&c3).unwrap()).unwrap();
        let c4 = crop_job(&page, "c04", [10, 200, 500, 240], "wc-low/top");
        let unit = units(&root).join(super::super::crops::unit_name(&c4.doc_id, "c04"));
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(JOB_FILE), serde_json::to_vec(&c4).unwrap()).unwrap();
        std::fs::write(spool.join(format!("{}_error", c4.workunit())), b"").unwrap();
        // A benchmark sample's unit, at `units/sample/<dataset>/<id>`.
        let mut sample = page.clone();
        sample.doc_id = "sample:overproof-ca/overproof-1".into();
        sample.id = sample.identity().unwrap();
        unit_with_result(
            &root,
            &spool,
            &sample,
            &[(InputMode::Whole, Outcome::Ok, "sample text\n")],
        );
        assert!(units(&root)
            .join("sample/overproof-ca/overproof-1")
            .join(JOB_FILE)
            .exists());

        let r = results(&root, &spool).unwrap();
        assert_eq!(
            r.summary,
            Summary {
                pages: 2,
                crops: 2,
                pending: 1,
                failed: 1,
                exported: 0,
                removed: 0,
            }
        );
        assert_eq!(
            r.pages[&sample.doc_id][&InputMode::Whole].text,
            "sample text\n"
        );
        let p = &r.pages[&page.doc_id];
        assert_eq!(p[&InputMode::Whole].outcome, Outcome::Timeout);
        assert_eq!(p[&InputMode::Capped].text, "Capped text of the page\n");
        let crops = &r.crops[&page.doc_id];
        assert_eq!(crops["c01"].text, "first line\n");
        assert_eq!(crops["c01"].rect, [10, 20, 500, 60]);
        assert_eq!(crops["c02"].outcome, Outcome::Failed);
        assert_eq!(crops["c02"].text, "");
        assert_eq!(crops["c01"].plan_class.as_deref(), Some("docker_mt"));
        // The page reader takes the first preferred mode that read.
        let reader = page_reader(p, &DEFAULT_MODES).unwrap();
        assert_eq!(reader.name, READER);
        assert_eq!(reader.text, "Capped text of the page\n");
        assert_eq!(reader.source, format!("surya:capped:{}", page.id));
        assert_eq!(
            page_reader(p, &[InputMode::Tiled]).unwrap().text,
            "Tiled text\n"
        );
        assert!(page_reader(p, &[InputMode::Whole]).is_none());

        let texts = tmp.path().join("texts");
        let s = run(&Options {
            root: root.clone(),
            spool: spool.clone(),
            texts_dir: Some(texts.clone()),
            modes: DEFAULT_MODES.to_vec(),
        })
        .unwrap();
        // The LoC page only: the sample has no `quality stage` export.
        assert_eq!(s.exported, 1);
        assert_eq!(std::fs::read_dir(&texts).unwrap().count(), 1);
        let got: TextReader =
            serde_json::from_slice(&std::fs::read(texts.join(export_name(&page.doc_id))).unwrap())
                .unwrap();
        assert_eq!(got.name, READER);
        assert!(!got.alto && got.regions.is_empty());
        // Collected again with only `whole` wanted, which timed out: the
        // earlier capped export goes, so a later stage cannot pick it up.
        let s = run(&Options {
            root,
            spool,
            texts_dir: Some(texts.clone()),
            modes: vec![InputMode::Whole],
        })
        .unwrap();
        assert_eq!((s.exported, s.removed), (0, 1));
        assert!(!texts.join(export_name(&page.doc_id)).exists());
    }
}
