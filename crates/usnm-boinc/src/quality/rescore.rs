//! Rescore a campaign's single-page units on the server from corrected
//! LoC texts, with no new volunteer work (#1079).
//!
//! A unit's result has two parts. The scan's features (image and layout)
//! come from the page image and the ALTO; everything else (each reader's
//! record, the hat and the legibility) comes from the texts alone, through
//! `slot::text_analysis` with no image and no ALTO. When the staged texts
//! were wrong, as v1-wave6's `loc-hist` text was (ocr-local's `loc_text`
//! label `bulk` in place of LoC's text), the text part can be computed
//! again here and the returned features kept.
//!
//! Reads `<root>/units/*/{quality.json, texts.json}`, the canonical results
//! in `--spool`, and `--loc-texts` (JSON lines `{"doc_id", "text"}`: LoC's
//! curated text of each page, as the staging scripts normalize it). For
//! each unit with a result and a LoC text it writes the re-staged unit
//! (`<out-dir>/units/<doc_id>/{quality.json, texts.json}`, the `loc-hist`
//! text replaced; `quality rehat` reads that root), and the page's rows
//! of `pages.parquet` as `collect` writes them, hat version 2 included.
//! Bundles, units with no result, and pages with no LoC text are counted
//! and skipped. Logs counts, never text.
use super::{
    collect::{self, PageRow},
    stage, units, validate, JOB_FILE, TEXTS_FILE,
};
use crate::{atomic, read};
use anyhow::Context as _;
use std::{
    collections::BTreeMap,
    io::BufRead as _,
    path::{Path, PathBuf},
    sync::Mutex,
};
use usnm_ocr_contract::quality::{Job, Texts, MAX_JOB_JSON, MAX_TEXT, MAX_TEXTS};
use usnm_ocr_quality::{findability::Places, slot};

/// The reader whose text `--loc-texts` replaces: LoC's curated text.
pub const LOC_HIST: &str = "loc-hist";

#[derive(Debug, Clone)]
pub struct Options {
    pub campaign: String,
    pub root: PathBuf,
    pub spool: PathBuf,
    pub loc_texts: PathBuf,
    pub places: PathBuf,
    pub out_dir: PathBuf,
    pub threads: usize,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Summary {
    pub units: usize,
    pub rescored: usize,
    /// Multi-page bundles: not handled.
    pub bundles: usize,
    /// Units with no canonical result, or that BOINC gave up on.
    pub no_result: usize,
    /// Pages with no `loc-hist` reader, or no LoC text in `--loc-texts`.
    pub no_loc_text: usize,
    pub rows: usize,
}

/// `--loc-texts`, by doc id; a doc id given twice keeps the last line.
pub fn read_loc_texts(path: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    #[derive(serde::Deserialize)]
    struct Line {
        doc_id: String,
        text: String,
    }
    let f = std::fs::File::open(path).with_context(|| format!("{}", path.display()))?;
    let mut out = BTreeMap::new();
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let l: Line =
            serde_json::from_str(&line).with_context(|| format!("{}:{}", path.display(), i + 1))?;
        usnm_ocr_contract::quality::check_doc_id(&l.doc_id)?;
        anyhow::ensure!(
            l.text.len() <= MAX_TEXT,
            "{}: LoC text over {MAX_TEXT} bytes",
            l.doc_id
        );
        out.insert(l.doc_id, l.text);
    }
    Ok(out)
}

/// The page's texts with `loc-hist` replaced by LoC's text; none when the
/// page has no `loc-hist` reader or `loc_text` is empty.
pub fn restage(mut texts: Texts, loc_text: &str) -> Option<Texts> {
    if loc_text.trim().is_empty() {
        return None;
    }
    let r = texts.readers.iter_mut().find(|r| r.name == LOC_HIST)?;
    r.text = loc_text.to_owned();
    Some(texts)
}

/// One page's rows from its unit's result and the corrected texts: the
/// result's scan features, the text part computed again.
pub fn page_rows(
    job: &Job,
    mut result: usnm_ocr_contract::quality::ResultManifest,
    texts: &Texts,
    places: &Places,
) -> anyhow::Result<Vec<PageRow>> {
    stage::check_loc_texts(texts)?;
    let a = slot::text_analysis(&job.readers, &job.function_words, texts, places, || None)
        .context("a LoC text is empty and no ALTO is staged")?;
    result.readers = a.report.readers;
    result.hat = Some(a.report.hat);
    result.legibility = Some(a.report.legibility);
    let mut rows = collect::page_rows(job, &result, false);
    collect::enrich_rows_with_damage(&mut rows, texts, None);
    anyhow::ensure!(
        collect::apply_hat_v2(&mut rows, &job.readers, texts, || None),
        "hat version 2: a LoC text is empty"
    );
    Ok(rows)
}

enum Outcome {
    Rows(Vec<PageRow>),
    Bundle,
    NoResult,
    NoLocText,
}

fn unit(
    opts: &Options,
    unit: &Path,
    loc: &BTreeMap<String, String>,
    places_txt: &str,
    places: &Places,
) -> anyhow::Result<Outcome> {
    let job_bytes = read(&unit.join(JOB_FILE), MAX_JOB_JSON)?;
    let job: Job =
        serde_json::from_slice(&job_bytes).with_context(|| format!("{}", unit.display()))?;
    if !job.pages.is_empty() {
        return Ok(Outcome::Bundle);
    }
    let wu = job.workunit();
    let files = collect::spool_files(&opts.spool, &wu, &super::outputs());
    if collect::gave_up(&opts.spool, &wu) || !files.iter().all(|f| f.exists()) {
        return Ok(Outcome::NoResult);
    }
    let got = validate::load(&files).with_context(|| format!("canonical result {wu}"))?;
    anyhow::ensure!(
        got.result.job_id == job.id && got.result.engine == job.engine,
        "{wu}: result does not belong to its job"
    );
    // The texts and places the unit read, so only LoC's text changes.
    anyhow::ensure!(
        job.inputs.places.matches(places_txt.as_bytes()),
        "{}: --places is not the unit's places.txt",
        job.doc_id
    );
    let texts_bytes = read(&unit.join(TEXTS_FILE), MAX_TEXTS)?;
    anyhow::ensure!(
        job.inputs.texts.matches(&texts_bytes),
        "{}: texts.json is not the one the unit read",
        job.doc_id
    );
    let texts: Texts = serde_json::from_slice(&texts_bytes)
        .with_context(|| format!("{}", unit.join(TEXTS_FILE).display()))?;
    let Some(texts) = loc.get(&job.doc_id).and_then(|t| restage(texts, t)) else {
        return Ok(Outcome::NoLocText);
    };
    let rows = page_rows(&job, got.result, &texts, places)
        .with_context(|| format!("rescoring {}", job.doc_id))?;
    let out = units(&opts.out_dir).join(&job.doc_id);
    std::fs::create_dir_all(&out)?;
    atomic(&out.join(JOB_FILE), &job_bytes)?;
    atomic(&out.join(TEXTS_FILE), &serde_json::to_vec(&texts)?)?;
    Ok(Outcome::Rows(rows))
}

/// Rescores every unit under `opts.root`; writes the re-staged units and
/// `pages.parquet` and `summary.md` under `opts.out_dir`.
pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    anyhow::ensure!(
        usnm_store::is_safe_segment(&opts.campaign),
        "invalid campaign name {:?}",
        opts.campaign
    );
    // A new output root: re-staged units must not land on the staging root
    // (their checksums would no longer match quality.json), nor mix with a
    // previous run's units that this run skips.
    let out_units = units(&opts.out_dir);
    anyhow::ensure!(
        !out_units.exists() && std::fs::symlink_metadata(&out_units).is_err(),
        "{} exists: give a new --out-dir",
        out_units.display()
    );
    let root =
        std::fs::canonicalize(&opts.root).with_context(|| format!("{}", opts.root.display()))?;
    std::fs::create_dir_all(&opts.out_dir)?;
    let out = std::fs::canonicalize(&opts.out_dir)?;
    anyhow::ensure!(
        !out.starts_with(&root) && !root.starts_with(&out),
        "--out-dir must be outside --root"
    );
    let loc = read_loc_texts(&opts.loc_texts)?;
    let places_txt = stage::read_places(&opts.places)?;
    let places = Places::parse(&places_txt);
    let dir = units(&opts.root);
    let mut unit_dirs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("{}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(JOB_FILE).exists() && p.join("submitted").exists())
        .collect();
    unit_dirs.sort();
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<(usize, Outcome)>> = Mutex::new(Vec::new());
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..opts.threads.max(1) {
            scope.spawn(|| loop {
                let k = {
                    let mut n = next.lock().unwrap();
                    let k = *n;
                    *n += 1;
                    k
                };
                let Some(u) = unit_dirs.get(k) else {
                    break;
                };
                if failed.lock().unwrap().is_some() {
                    break;
                }
                match unit(opts, u, &loc, &places_txt, &places) {
                    Ok(o) => results.lock().unwrap().push((k, o)),
                    Err(e) => {
                        failed.lock().unwrap().get_or_insert(e);
                        break;
                    }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(k, _)| *k);
    let mut s = Summary::default();
    let mut pages = Vec::new();
    for (_, o) in results {
        s.units += 1;
        match o {
            Outcome::Rows(rows) => {
                s.rescored += 1;
                s.rows += rows.len();
                pages.extend(rows);
            }
            Outcome::Bundle => s.bundles += 1,
            Outcome::NoResult => s.no_result += 1,
            Outcome::NoLocText => s.no_loc_text += 1,
        }
    }
    std::fs::create_dir_all(&opts.out_dir)?;
    atomic(
        &opts.out_dir.join("pages.parquet"),
        &collect::to_parquet(&pages)?,
    )?;
    let cs = collect::Summary {
        pages: s.rescored,
        page_rows: s.rows,
        ..collect::Summary::default()
    };
    atomic(
        &opts.out_dir.join("summary.md"),
        collect::summary_md(&opts.campaign, &cs, &pages, &[]).as_bytes(),
    )?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::{work, ALTO_FILE, PAGE_FILE};
    use usnm_ocr_contract::quality::{ResultManifest, TextReader};

    const DOC: &str = "sn83045462_1890-05-03_ed-1_seq-1";

    /// The work fixture's page staged as v1-wave6 staged it (readers
    /// `loc-hist`, `as`, `ours-tesseract`), once with LoC's text and once
    /// with `loc-hist` as `bulk`. Each runs as a volunteer's unit would.
    async fn staged(dir: &Path, texts: &Texts) -> (Job, ResultManifest) {
        let fixture = dir.with_extension("fixture");
        let j = work::tests::unit(&fixture);
        let job = stage::write_unit(&stage::WriteUnit {
            dir,
            doc_id: DOC,
            group: "wave-six-three-reader",
            language: "eng",
            page: &std::fs::read(fixture.join(PAGE_FILE)).unwrap(),
            page_source: "handoff:scans/p.jpg",
            alto: &std::fs::read(fixture.join(ALTO_FILE)).unwrap(),
            alto_source: "handoff:scans/p.xml",
            texts,
            texts_source: "handoff:texts/p.json",
            places: "Albany\nTroy\n",
            places_source: "campaign:places.txt",
            engine: j.engine.clone(),
            function_words: j.function_words.clone(),
            crop_seed: j.crop_seed,
            k_crops: j.k_crops,
        })
        .unwrap();
        work::run(dir).await.unwrap();
        let files: Vec<PathBuf> = super::super::outputs()
            .iter()
            .map(|n| dir.join(n))
            .collect();
        (job, validate::load(&files).unwrap().result)
    }

    fn wave6_texts(loc: &str) -> Texts {
        let mut t: Texts = Texts {
            page: None,
            readers: vec![],
        };
        for (name, text) in [
            (LOC_HIST, loc),
            ("as", "The mayor of Albany spoke with the counci1 about bridges and roads across the rivor"),
            ("ours-tesseract", "The rnayor of Albany spoke with the council about bridges and roads"),
        ] {
            t.readers.push(TextReader {
                alto: false,
                name: name.into(),
                regions: vec![],
                source: name.into(),
                text: text.into(),
            });
        }
        t
    }

    /// Rescored with LoC's text, the `bulk` unit's rows equal the rows of
    /// the unit staged right in every text number, and keep the scan's
    /// features from the volunteer's result.
    #[tokio::test]
    async fn rescoring_a_bulk_unit_matches_the_unit_staged_right() {
        let tmp = tempfile::tempdir().unwrap();
        let loc = work::tests::TRUTH;
        let (right_job, right) = staged(&tmp.path().join("right"), &wave6_texts(loc)).await;
        let root = tmp.path().join("root");
        let bulk_dir = units(&root).join(DOC);
        let (bulk_job, bulk) = staged(&bulk_dir, &wave6_texts("bulk")).await;
        std::fs::write(bulk_dir.join("submitted"), b"x").unwrap();
        assert_ne!(bulk.readers, right.readers);
        // The canonical result in the spool, as the assimilator names it.
        let spool = tmp.path().join("spool");
        std::fs::create_dir_all(&spool).unwrap();
        let wu = bulk_job.workunit();
        for n in super::super::outputs() {
            std::fs::copy(bulk_dir.join(n), spool.join(format!("{wu}__file_{n}"))).unwrap();
        }
        let loc_texts = tmp.path().join("loc.jsonl");
        std::fs::write(
            &loc_texts,
            format!("{}\n", serde_json::json!({"doc_id": DOC, "text": loc})),
        )
        .unwrap();
        let places = tmp.path().join("places.txt");
        std::fs::write(&places, "Albany\nTroy\n").unwrap();
        let opts = Options {
            campaign: "v1-wave6".into(),
            root,
            spool,
            loc_texts,
            places,
            out_dir: tmp.path().join("out"),
            threads: 2,
        };
        let s = run(&opts).unwrap();
        assert_eq!(
            (s.units, s.rescored, s.no_result, s.no_loc_text, s.rows),
            (1, 1, 0, 0, 3)
        );
        let restaged = restage(wave6_texts("bulk"), loc).unwrap();
        let got = page_rows(
            &bulk_job,
            bulk.clone(),
            &restaged,
            &Places::parse("Albany\nTroy\n"),
        )
        .unwrap();
        let mut want = collect::page_rows(&right_job, &right, false);
        collect::enrich_rows_with_damage(&mut want, &wave6_texts(loc), None);
        assert!(collect::apply_hat_v2(
            &mut want,
            &right_job.readers,
            &wave6_texts(loc),
            || None
        ));
        assert_eq!(got.len(), 3);
        for (g, w) in got.iter().zip(&want) {
            // Everything but the job id and the run time.
            let mut g = g.clone();
            g.job_id.clone_from(&w.job_id);
            g.total_ms = w.total_ms;
            assert_eq!(&g, w);
        }
        assert_eq!(got[0].reader, LOC_HIST);
        assert!(got[0].chars > 40);
        assert_eq!(got[0].hat_version, 2);
        // The re-staged root holds LoC's text, for quality rehat.
        let t: Texts = serde_json::from_slice(
            &std::fs::read(units(&opts.out_dir).join(DOC).join(TEXTS_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(t, restaged);
        assert!(units(&opts.out_dir).join(DOC).join(JOB_FILE).exists());
        assert!(opts.out_dir.join("pages.parquet").exists());
        // The bulk texts themselves are refused, not scored.
        let e = page_rows(&bulk_job, bulk, &wave6_texts("bulk"), &Places::parse(""))
            .unwrap_err()
            .to_string();
        assert!(e.contains("loc_text label"), "{e}");
        // A used --out-dir, or one inside --root: refused.
        let e = run(&opts).unwrap_err().to_string();
        assert!(e.contains("new --out-dir"), "{e}");
        let e = run(&Options {
            out_dir: opts.root.join("out"),
            ..opts.clone()
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("outside --root"), "{e}");
        // Other places than the unit's: refused.
        let other = tmp.path().join("other-places.txt");
        std::fs::write(&other, "Albany\n").unwrap();
        let e = run(&Options {
            places: other,
            out_dir: tmp.path().join("out-other"),
            ..opts.clone()
        })
        .unwrap_err();
        assert!(format!("{e:#}").contains("--places"), "{e:#}");
        // No LoC text for the page: counted, no rows.
        std::fs::write(&opts.loc_texts, "").unwrap();
        let s = run(&Options {
            out_dir: tmp.path().join("out2"),
            ..opts.clone()
        })
        .unwrap();
        assert_eq!((s.rescored, s.no_loc_text, s.rows), (0, 1, 0));
    }

    #[test]
    fn restage_replaces_only_loc_hist() {
        let t = wave6_texts("bulk");
        let r = restage(t.clone(), "the real text").unwrap();
        assert_eq!(r.readers[0].text, "the real text");
        assert_eq!(r.readers[1..], t.readers[1..]);
        assert!(restage(t.clone(), "  ").is_none());
        let mut no_loc = t;
        no_loc.readers.remove(0);
        assert!(restage(no_loc, "x").is_none());
    }
}
