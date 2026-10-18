//! Manually acquired Library of Congress debate PDFs (#229, phase 3).
//!
//! Congress.gov's robots policy bars automated downloading, so this command
//! reads only PDFs an operator has downloaded in a browser and placed in the
//! raw archive. It never makes requests to Congress.gov or loc.gov.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

use super::curated::{self, CuratedPage};
use super::{globe, parse};
use crate::source::hex;

pub const SOURCE: &str = "lcg";
pub const RAW_PREFIX: &str = "congress/lcg/manual";
pub const LOCK: &str = "congress-lcg-manual";
pub const MAX_PAGE_OCR: Duration = Duration::from_secs(300);

/// The two series for which manually acquired PDFs are expected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[value(rename_all = "lowercase")]
pub enum Series {
    Annals,
    Register,
}

impl Series {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Annals => "annals",
            Self::Register => "register",
        }
    }
}

/// One downloaded, bound volume. The checksum and byte count are computed
/// after the browser download; they bind the manifest to the archived PDF.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    pub unit: String,
    pub volume: u16,
    pub congress: u16,
    pub session: u16,
    pub title: String,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub pages: u32,
    pub pdf_sha256: String,
    pub pdf_bytes: u64,
    pub source_url: String,
    pub pdf_url: String,
    pub accessed: NaiveDate,
    /// Transcribed from the individual item's Rights & Access statement.
    pub rights: String,
    pub rights_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub series: String,
    pub volumes: Vec<Volume>,
}

impl Manifest {
    pub fn parse(bytes: &[u8], expected: Series) -> anyhow::Result<Self> {
        let manifest: Self = serde_json::from_slice(bytes).context("manual debate manifest")?;
        ensure!(
            manifest.schema == 1,
            "unsupported manual debate manifest schema {}",
            manifest.schema
        );
        ensure!(
            manifest.series == expected.as_str(),
            "manifest series `{}` does not match `{}`",
            manifest.series,
            expected.as_str()
        );
        ensure!(
            !manifest.volumes.is_empty(),
            "manual debate manifest has no volumes"
        );
        let year_range = match expected {
            Series::Annals => 1789..=1824,
            Series::Register => 1824..=1837,
        };
        let mut units = BTreeSet::new();
        let mut volume_numbers = BTreeSet::new();
        for v in &manifest.volumes {
            ensure!(
                usnm_store::is_safe_segment(&v.unit),
                "unsafe volume id `{}`",
                v.unit
            );
            ensure!(
                v.unit.starts_with(&format!("{}-", expected.as_str())),
                "volume id `{}` does not identify the expected series",
                v.unit
            );
            ensure!(units.insert(v.unit.as_str()), "duplicate unit `{}`", v.unit);
            ensure!(
                v.volume > 0 && v.congress > 0 && v.session > 0,
                "volume, Congress and session must be positive for `{}`",
                v.unit
            );
            ensure!(
                volume_numbers.insert(v.volume),
                "duplicate volume number {}",
                v.volume
            );
            ensure!(
                year_range.contains(&v.first_day.year()) && year_range.contains(&v.last_day.year()),
                "dates for `{}` fall outside the {} series",
                v.unit,
                expected.as_str()
            );
            ensure!(
                v.first_day <= v.last_day,
                "invalid date range for `{}`",
                v.unit
            );
            ensure!(v.pages > 0, "page count is zero for `{}`", v.unit);
            ensure!(v.pdf_bytes > 0, "PDF byte count is zero for `{}`", v.unit);
            ensure!(
                v.pdf_sha256.len() == 64 && v.pdf_sha256.bytes().all(|b| b.is_ascii_hexdigit()),
                "invalid PDF SHA-256 for `{}`",
                v.unit
            );
            ensure!(
                v.source_url.starts_with("https://www.congress.gov/"),
                "source_url for `{}` must be the browser-viewed Congress.gov item",
                v.unit
            );
            ensure!(
                v.pdf_url.starts_with("https://www.congress.gov/"),
                "pdf_url for `{}` must be the manually downloaded Congress.gov PDF",
                v.unit
            );
            ensure!(!v.title.trim().is_empty(), "missing title for `{}`", v.unit);
            ensure!(
                !v.rights.trim().is_empty() && v.rights_url.starts_with("https://"),
                "record the item's Rights & Access statement and link for `{}`",
                v.unit
            );
        }
        Ok(manifest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageText {
    pub text: String,
    /// `None` for an existing text layer, otherwise the exact OCR engine.
    pub ocr_engine: Option<String>,
}

#[async_trait]
pub trait PdfPages: Send + Sync {
    async fn read(
        &self,
        pdf: &Path,
        scratch: &Path,
        expected_pages: u32,
    ) -> anyhow::Result<Vec<PageText>>;
}

/// Poppler text extraction with Tesseract fallback for pages that have no
/// usable text layer. The tools and OCR language data are installed in the
/// ingest image; OCR is strictly local and never uploads the PDF.
pub struct TesseractPdf {
    pub pdftotext: PathBuf,
    pub pdfinfo: PathBuf,
    pub pdftoppm: PathBuf,
    pub tesseract: PathBuf,
    pub engine: String,
}

impl TesseractPdf {
    pub async fn new(
        pdftotext: PathBuf,
        pdfinfo: PathBuf,
        pdftoppm: PathBuf,
        tesseract: PathBuf,
    ) -> anyhow::Result<Self> {
        let out = tokio::process::Command::new(&tesseract)
            .arg("--version")
            .output()
            .await
            .with_context(|| format!("running `{}`", tesseract.display()))?;
        ensure!(
            out.status.success(),
            "`{}` --version failed",
            tesseract.display()
        );
        let version_output = String::from_utf8_lossy(&out.stdout);
        let first = version_output.lines().next().unwrap_or_default().trim();
        let version = first.strip_prefix("tesseract ").unwrap_or(first);
        ensure!(!version.is_empty(), "could not determine Tesseract version");
        Ok(Self {
            pdftotext,
            pdfinfo,
            pdftoppm,
            tesseract,
            engine: format!("tesseract/{version} lang=eng psm=3 dpi=300"),
        })
    }

    async fn output(
        bin: &Path,
        args: &[&std::ffi::OsStr],
        timeout: Duration,
    ) -> anyhow::Result<std::process::Output> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = tokio::time::timeout(timeout, cmd.output())
            .await
            .with_context(|| format!("`{}` exceeded {timeout:?}", bin.display()))?
            .with_context(|| format!("running `{}`", bin.display()))?;
        if !out.status.success() {
            bail!(
                "`{}` failed ({}): {}",
                bin.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .chars()
                    .take(500)
                    .collect::<String>()
            );
        }
        Ok(out)
    }

    async fn page_count(&self, pdf: &Path) -> anyhow::Result<u32> {
        let out = Self::output(&self.pdfinfo, &[pdf.as_os_str()], Duration::from_secs(60)).await?;
        let text = String::from_utf8_lossy(&out.stdout);
        let pages = text
            .lines()
            .find_map(|line| line.strip_prefix("Pages:")?.trim().parse::<u32>().ok())
            .context("pdfinfo did not report a page count")?;
        ensure!(pages > 0, "PDF has no pages");
        Ok(pages)
    }

    async fn ocr_page(&self, pdf: &Path, scratch: &Path, page: u32) -> anyhow::Result<String> {
        tokio::fs::create_dir_all(scratch).await?;
        let stem = scratch.join(format!("page-{page:05}"));
        let page_arg = page.to_string();
        let stem_arg = stem.as_os_str();
        Self::output(
            &self.pdftoppm,
            &[
                std::ffi::OsStr::new("-f"),
                std::ffi::OsStr::new(page_arg.as_str()),
                std::ffi::OsStr::new("-l"),
                std::ffi::OsStr::new(page_arg.as_str()),
                std::ffi::OsStr::new("-singlefile"),
                std::ffi::OsStr::new("-png"),
                std::ffi::OsStr::new("-r"),
                std::ffi::OsStr::new("300"),
                pdf.as_os_str(),
                stem_arg,
            ],
            Duration::from_secs(300),
        )
        .await?;
        let image = scratch.join(format!("page-{page:05}.png"));
        let out = Self::output(
            &self.tesseract,
            &[
                image.as_os_str(),
                std::ffi::OsStr::new("stdout"),
                std::ffi::OsStr::new("-l"),
                std::ffi::OsStr::new("eng"),
                std::ffi::OsStr::new("--psm"),
                std::ffi::OsStr::new("3"),
            ],
            MAX_PAGE_OCR,
        )
        .await;
        let _ = tokio::fs::remove_file(image).await;
        let out = out?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

#[async_trait]
impl PdfPages for TesseractPdf {
    async fn read(
        &self,
        pdf: &Path,
        scratch: &Path,
        expected_pages: u32,
    ) -> anyhow::Result<Vec<PageText>> {
        let actual = self.page_count(pdf).await?;
        ensure!(
            actual == expected_pages,
            "PDF has {actual} pages; manifest says {expected_pages}"
        );
        let out = Self::output(
            &self.pdftotext,
            &[
                std::ffi::OsStr::new("-enc"),
                std::ffi::OsStr::new("UTF-8"),
                std::ffi::OsStr::new("-layout"),
                pdf.as_os_str(),
                std::ffi::OsStr::new("-"),
            ],
            Duration::from_secs(900),
        )
        .await?;
        let mut pages = super::serialset::split_pages(&String::from_utf8_lossy(&out.stdout));
        ensure!(
            pages.len() <= actual as usize,
            "pdftotext returned {} pages for a {actual}-page PDF",
            pages.len()
        );
        pages.resize(actual as usize, String::new());
        let mut extracted = Vec::with_capacity(actual as usize);
        for (index, text) in pages.into_iter().enumerate() {
            let normalized = super::serialset::page_text(&text);
            if usnm_core::text::text_status(&normalized) == usnm_core::text::TextStatus::Ok {
                extracted.push(PageText {
                    text: normalized,
                    ocr_engine: None,
                });
            } else {
                let page = u32::try_from(index + 1)?;
                let ocr = self.ocr_page(pdf, scratch, page).await?;
                extracted.push(PageText {
                    text: super::serialset::page_text(&ocr),
                    ocr_engine: Some(self.engine.clone()),
                });
            }
        }
        Ok(extracted)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub series: Series,
    pub units: Vec<String>,
    pub refetch: bool,
    pub max_units: Option<usize>,
    pub deadline: Option<tokio::time::Instant>,
    pub work_dir: PathBuf,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub series: String,
    pub listed: usize,
    pub written: Vec<(String, u32, u64)>,
    pub pages: u64,
    pub text_layer_pages: u64,
    pub ocr_pages: u64,
    pub already: usize,
    pub left: usize,
}

fn unit_ordinal(volume: &Volume) -> u32 {
    (u32::from(volume.congress) << 16)
        | (u32::from(volume.session) << 8)
        | u32::from(volume.volume.min(255))
}

fn ordinal(n: u16) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, x) if x != 11 => "st",
        (2, x) if x != 12 => "nd",
        (3, x) if x != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn page_row(
    volume: &Volume,
    series: Series,
    seq: u32,
    page: PageText,
    facts: &globe::PageFacts,
    at: DateTime<Utc>,
) -> CuratedPage {
    let lines: Vec<String> = page.text.lines().map(str::to_owned).collect();
    let canvas = globe::Canvas { seq, label: None };
    let item = globe::Item {
        id: volume.unit.clone(),
        title: volume.title.clone(),
        congress: Some(volume.congress),
        session: Some(volume.session.to_string()),
        coverage: Some((volume.first_day, volume.last_day)),
        extent: volume.pages.to_string(),
        appendix: false,
        part: Some(volume.volume),
        ..globe::Item::default()
    };
    let mut row = globe::page_row(&item, &canvas, &lines, facts, at);
    row.doc_id = format!("lcg-{}_p{seq}", volume.unit);
    row.source = SOURCE.to_owned();
    row.series = series.as_str().to_owned();
    row.unit_id = volume.unit.clone();
    row.unit_ordinal = unit_ordinal(volume);
    row.package_id = Some(volume.unit.clone());
    row.citation = Some(format!(
        "{}, {} Cong., {} Sess.{}",
        match series {
            Series::Annals => "Annals",
            Series::Register => "Register of Debates",
        },
        ordinal(volume.congress),
        ordinal(volume.session),
        parse::printed_page(None, &lines)
            .map(|p| format!(" {p}"))
            .unwrap_or_default()
    ));
    row.ocr_source = if page.ocr_engine.is_some() {
        "usnm-tesseract"
    } else {
        SOURCE
    }
    .to_owned();
    row.ocr_engine = page.ocr_engine;
    row.link = Some(format!("{}#page={seq}", volume.pdf_url));
    row
}

fn fingerprint(volume: &Volume) -> anyhow::Result<String> {
    Ok(hex(&Sha256::digest(serde_json::to_vec(volume)?)))
}

/// Read one source manifest and curate its selected, verified PDFs. This
/// path has no HTTP client; the only input is an operator-populated raw store.
pub async fn run(
    raw: &dyn ObjectStore,
    curated_store: &dyn ObjectStore,
    pdf: &dyn PdfPages,
    opts: &Options,
    lost: &AtomicBool,
) -> anyhow::Result<Report> {
    let series = opts.series.as_str();
    let manifest_path = format!("{RAW_PREFIX}/{series}/manifest.json");
    let bytes = raw.get(&manifest_path).await?.with_context(|| {
        format!("missing browser-download manifest `{manifest_path}` in the raw store")
    })?;
    let manifest = Manifest::parse(&bytes, opts.series)?;
    let requested: BTreeSet<&str> = opts.units.iter().map(String::as_str).collect();
    let selected: Vec<&Volume> = manifest
        .volumes
        .iter()
        .filter(|v| requested.is_empty() || requested.contains(v.unit.as_str()))
        .collect();
    ensure!(
        requested.is_empty() || selected.len() == requested.len(),
        "one or more --units are absent from the manifest"
    );
    let mut report = Report {
        series: series.to_owned(),
        listed: selected.len(),
        ..Report::default()
    };
    let finished: BTreeMap<String, curated::Finished> = curated::finished_in(curated_store, series)
        .await?
        .into_iter()
        .map(|f| (f.unit.clone(), f))
        .collect();
    let mut processed = 0usize;
    for volume in selected {
        if lost.load(Ordering::SeqCst)
            || opts
                .deadline
                .is_some_and(|d| tokio::time::Instant::now() >= d)
            || opts.max_units.is_some_and(|n| processed >= n)
        {
            report.left += 1;
            continue;
        }
        let stamp = fingerprint(volume)?;
        if !opts.refetch {
            if let Some(f) = finished.get(&volume.unit) {
                let (counts, _) = curated::read_counts(curated_store, f).await?;
                if counts.source_packages.as_deref() == Some(stamp.as_str()) {
                    report.already += 1;
                    continue;
                }
            }
        }
        let pdf_path = format!("{RAW_PREFIX}/{series}/{}.pdf", volume.unit);
        let bytes = raw
            .get(&pdf_path)
            .await?
            .with_context(|| format!("missing manually downloaded PDF `{pdf_path}`"))?;
        ensure!(
            bytes.len() as u64 == volume.pdf_bytes,
            "`{pdf_path}` has {} bytes, manifest says {}",
            bytes.len(),
            volume.pdf_bytes
        );
        ensure!(
            hex(&Sha256::digest(&bytes)) == volume.pdf_sha256.to_ascii_lowercase(),
            "`{pdf_path}` does not match its manifest SHA-256"
        );
        tokio::fs::create_dir_all(&opts.work_dir).await?;
        let local_pdf = opts.work_dir.join(format!("{}.pdf", volume.unit));
        tokio::fs::write(&local_pdf, &bytes)
            .await
            .with_context(|| local_pdf.display().to_string())?;
        drop(bytes);
        let page_scratch = opts.work_dir.join(&volume.unit);
        let got = pdf.read(&local_pdf, &page_scratch, volume.pages).await;
        let _ = tokio::fs::remove_file(&local_pdf).await;
        let _ = tokio::fs::remove_dir_all(&page_scratch).await;
        let pages = got.with_context(|| format!("reading `{pdf_path}`"))?;
        ensure!(
            pages.len() == volume.pages as usize,
            "PDF reader returned {} pages; expected {}",
            pages.len(),
            volume.pages
        );
        let item = globe::Item {
            id: volume.unit.clone(),
            title: volume.title.clone(),
            congress: Some(volume.congress),
            session: Some(volume.session.to_string()),
            coverage: Some((volume.first_day, volume.last_day)),
            extent: volume.pages.to_string(),
            ..globe::Item::default()
        };
        let mut dating = globe::Dating::new(&item)?;
        let at = Utc::now();
        let rows: Vec<CuratedPage> = pages
            .into_iter()
            .enumerate()
            .map(|(i, page)| {
                let lines: Vec<String> = page.text.lines().map(str::to_owned).collect();
                let facts = dating.page(&lines);
                page_row(
                    volume,
                    opts.series,
                    u32::try_from(i + 1).unwrap_or(u32::MAX),
                    page,
                    &facts,
                    at,
                )
            })
            .collect();
        report.pages += rows.len() as u64;
        report.text_layer_pages += rows.iter().filter(|r| r.ocr_source == SOURCE).count() as u64;
        report.ocr_pages += rows
            .iter()
            .filter(|r| r.ocr_source == "usnm-tesseract")
            .count() as u64;
        let counts = curated::write_attempt_from(
            curated_store,
            series,
            SOURCE,
            &volume.unit,
            &rows,
            at,
            Some(volume.accessed.to_string()),
            Some(stamp),
        )
        .await?;
        processed += 1;
        report
            .written
            .push((volume.unit.clone(), counts.attempt, counts.pages));
    }
    Ok(report)
}

pub async fn locked<T>(
    state: &crate::state::State,
    owner: &str,
    body: impl std::future::Future<Output = anyhow::Result<T>>,
    lost: Arc<AtomicBool>,
) -> anyhow::Result<T> {
    super::fetch::locked(state, LOCK, owner, body, lost).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn fixture() -> Vec<u8> {
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/congress/lcg/manual/annals/manifest.json"
        ))
        .to_vec()
    }

    #[test]
    fn manifest_checks_series_ids_coverage_and_pdf_provenance() {
        let m = Manifest::parse(&fixture(), Series::Annals).unwrap();
        assert_eq!(m.volumes.len(), 1);
        assert_eq!(m.volumes[0].unit, "annals-c01-s01-v01");
        let mut bad = m.clone();
        bad.volumes[0].unit = "../outside".into();
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap(), Series::Annals).is_err());
        assert!(Manifest::parse(&fixture(), Series::Register).is_err());
        let mut bad = m;
        bad.volumes[0].pdf_sha256 = "no-digest".into();
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap(), Series::Annals).is_err());
    }

    struct FixturePdf;

    #[async_trait]
    impl PdfPages for FixturePdf {
        async fn read(
            &self,
            pdf: &Path,
            _scratch: &Path,
            expected_pages: u32,
        ) -> anyhow::Result<Vec<PageText>> {
            ensure!(
                tokio::fs::metadata(pdf).await?.len() > 0,
                "fixture PDF is empty"
            );
            ensure!(expected_pages == 1, "unexpected fixture page count");
            Ok(vec![PageText {
                text: "MONDAY, November 2, 1789\nMr. CLAY of Alabama introduced the bill".into(),
                ocr_engine: Some("tesseract/5.3.0 lang=eng psm=3 dpi=300".into()),
            }])
        }
    }

    #[tokio::test]
    async fn manual_pdf_is_hash_checked_curated_and_repeatable() {
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.test-work/congress-manual");
        let _ = std::fs::remove_dir_all(&root);
        let raw = usnm_store::LocalStore::new(root.join("raw"));
        let curated_store = usnm_store::LocalStore::new(root.join("curated"));
        let manifest = fixture();
        let parsed = Manifest::parse(&manifest, Series::Annals).unwrap();
        let volume = &parsed.volumes[0];
        let pdf_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/congress/lcg/manual/annals/annals-c01-s01-v01.pdf"
        ));
        raw.put_new(
            &format!("{RAW_PREFIX}/annals/manifest.json"),
            manifest,
            "application/json",
        )
        .await
        .unwrap();
        raw.put_new(
            &format!("{RAW_PREFIX}/annals/{}.pdf", volume.unit),
            pdf_bytes.to_vec(),
            "application/pdf",
        )
        .await
        .unwrap();
        let opts = Options {
            series: Series::Annals,
            units: vec![],
            refetch: false,
            max_units: None,
            deadline: None,
            work_dir: root.join("work"),
        };
        let first = run(
            &raw,
            &curated_store,
            &FixturePdf,
            &opts,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(
            (first.pages, first.ocr_pages, first.text_layer_pages),
            (1, 1, 0)
        );
        assert_eq!(first.written[0].0, volume.unit);
        let finished = curated::finished_in(&curated_store, "annals")
            .await
            .unwrap();
        let hidden = super::super::release::Listing::load_enabled(&curated_store, false)
            .await
            .unwrap();
        assert!(hidden.units.is_empty());
        assert_eq!(hidden.left_out, ["annals/annals-c01-s01-v01"]);
        let enabled = super::super::release::Listing::load_enabled(&curated_store, true)
            .await
            .unwrap();
        assert_eq!(enabled.units.len(), 1);
        let counts = curated::read_counts(&curated_store, &finished[0])
            .await
            .unwrap()
            .0;
        let rows = curated::read_checked(&curated_store, &counts.parts[0])
            .await
            .unwrap();
        assert_eq!(rows[0].source, "lcg");
        assert_eq!(rows[0].doc_id, "lcg-annals-c01-s01-v01_p1");
        assert_eq!(rows[0].date.to_string(), "1789-11-02");
        assert_eq!(
            rows[0].ocr_engine.as_deref(),
            Some("tesseract/5.3.0 lang=eng psm=3 dpi=300")
        );
        assert!(rows[0].link.as_deref().unwrap().ends_with("#page=1"));
        let second = run(
            &raw,
            &curated_store,
            &FixturePdf,
            &opts,
            &AtomicBool::new(false),
        )
        .await
        .unwrap();
        assert_eq!(second.already, 1);
        assert_eq!(second.written.len(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
