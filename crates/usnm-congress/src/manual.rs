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
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use usnm_store::ObjectStore;

use super::curated::{self, CuratedPage};
use super::{globe, parse};
use usnm_core::digest::hex;

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
    /// Printed volume number, which can be shared by separately bound parts.
    pub volume: u16,
    /// The bound PDF's order among parts of the same printed volume.
    pub bound_part: u16,
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
        let mut bound_parts = BTreeSet::new();
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
                v.volume > 0 && v.bound_part > 0 && v.congress > 0 && v.session > 0,
                "volume, bound part, Congress and session must be positive for `{}`",
                v.unit
            );
            ensure!(
                v.volume <= u8::MAX.into()
                    && v.bound_part <= u8::MAX.into()
                    && v.session <= 15
                    && v.congress <= 4095,
                "ordering values for `{}` exceed the supported ranges",
                v.unit
            );
            ensure!(
                bound_parts.insert((v.congress, v.session, v.volume, v.bound_part)),
                "duplicate bound part {} of volume {} for {} Congress, {} session",
                v.bound_part,
                v.volume,
                v.congress,
                v.session
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
    (u32::from(volume.congress) << 20)
        | (u32::from(volume.session) << 16)
        | (u32::from(volume.volume) << 8)
        | u32::from(volume.bound_part)
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

async fn stage_pdf(
    raw: &dyn ObjectStore,
    path: &str,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    let mut chunks = raw
        .get_stream(path)
        .await?
        .with_context(|| format!("missing manually downloaded PDF `{path}`"))?;
    let mut file = tokio::fs::File::create(destination)
        .await
        .with_context(|| format!("creating {}", destination.display()))?;
    let mut digest = Sha256::new();
    let mut bytes = 0_u64;
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        bytes = bytes
            .checked_add(u64::try_from(chunk.len())?)
            .context("PDF byte count overflow")?;
        ensure!(
            bytes <= expected_bytes,
            "`{path}` is larger than its manifest byte count ({expected_bytes})"
        );
        digest.update(&chunk);
        file.write_all(&chunk)
            .await
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    file.flush().await?;
    drop(file);
    ensure!(
        bytes == expected_bytes,
        "`{path}` has {bytes} bytes, manifest says {expected_bytes}"
    );
    ensure!(
        hex(&digest.finalize()) == expected_sha256.to_ascii_lowercase(),
        "`{path}` does not match its manifest SHA-256"
    );
    Ok(())
}

struct ManualDating {
    dating: globe::Dating,
    chamber: Option<parse::Chamber>,
}

impl ManualDating {
    fn new(item: &globe::Item) -> anyhow::Result<Self> {
        Ok(Self {
            dating: globe::Dating::new(item)?,
            chamber: None,
        })
    }

    fn page(&mut self, lines: &[String]) -> globe::PageFacts {
        let next_chamber = lines
            .iter()
            .filter_map(|line| parse::heading(line))
            .find_map(|heading| heading.chamber);
        if let Some(next) = next_chamber {
            if self.chamber.is_some_and(|current| current != next) {
                self.dating.reset_carried_date();
            }
            self.chamber = Some(next);
        }
        self.dating.page(lines)
    }
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
        part: Some(volume.bound_part),
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
        "{}, {} Cong., {} Sess., Vol. {}, Part {}{}",
        match series {
            Series::Annals => "Annals",
            Series::Register => "Register of Debates",
        },
        ordinal(volume.congress),
        ordinal(volume.session),
        ordinal(volume.volume),
        ordinal(volume.bound_part),
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
        tokio::fs::create_dir_all(&opts.work_dir).await?;
        let local_pdf = opts.work_dir.join(format!("{}.pdf", volume.unit));
        if let Err(error) = stage_pdf(
            raw,
            &pdf_path,
            &local_pdf,
            volume.pdf_bytes,
            &volume.pdf_sha256,
        )
        .await
        {
            let _ = tokio::fs::remove_file(&local_pdf).await;
            return Err(error).with_context(|| format!("staging `{pdf_path}`"));
        }
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
        let mut dating = ManualDating::new(&item)?;
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
        ensure!(
            !lost.load(Ordering::SeqCst),
            "lost the `{LOCK}` lock; not writing `{}`",
            volume.unit
        );
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
    state: &usnm_state::state::State,
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
        assert_eq!(m.volumes[0].unit, "annals-c01-s01-v01-b01");
        let mut two_parts = m.clone();
        let mut second_part = two_parts.volumes[0].clone();
        second_part.unit = "annals-c01-s01-v01-b02".into();
        second_part.bound_part = 2;
        two_parts.volumes.push(second_part);
        assert!(Manifest::parse(&serde_json::to_vec(&two_parts).unwrap(), Series::Annals).is_ok());
        assert_ne!(
            unit_ordinal(&two_parts.volumes[0]),
            unit_ordinal(&two_parts.volumes[1])
        );
        let mut duplicate_part = two_parts;
        duplicate_part.volumes[1].bound_part = 1;
        assert!(Manifest::parse(
            &serde_json::to_vec(&duplicate_part).unwrap(),
            Series::Annals
        )
        .is_err());
        let mut bad = m.clone();
        bad.volumes[0].unit = "../outside".into();
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap(), Series::Annals).is_err());
        assert!(Manifest::parse(&fixture(), Series::Register).is_err());
        let mut bad = m;
        bad.volumes[0].pdf_sha256 = "no-digest".into();
        assert!(Manifest::parse(&serde_json::to_vec(&bad).unwrap(), Series::Annals).is_err());
    }

    #[test]
    fn manual_dating_resets_carried_day_when_chamber_changes() {
        #[derive(Deserialize)]
        struct Transition {
            senate: Vec<String>,
            house: Vec<String>,
        }
        let transition: Transition = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/congress/lcg/manual/annals/chamber-transition.json"
        )))
        .unwrap();
        let item = globe::Item {
            id: "annals-dating".into(),
            congress: Some(17),
            session: Some("1".into()),
            coverage: Some((
                NaiveDate::from_ymd_opt(1823, 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(1823, 4, 30).unwrap(),
            )),
            ..globe::Item::default()
        };
        let mut dating = ManualDating::new(&item).unwrap();
        let senate = dating.page(&transition.senate);
        assert_eq!(senate.date, NaiveDate::from_ymd_opt(1823, 3, 10).unwrap());
        assert_eq!(senate.chamber, Some(parse::Chamber::Senate));
        let house = dating.page(&transition.house);
        assert_eq!(house.date, NaiveDate::from_ymd_opt(1823, 1, 10).unwrap());
        assert_eq!(house.date_from, "heading");
        assert_eq!(house.chamber, Some(parse::Chamber::House));
    }

    #[cfg(unix)]
    fn executable_script(root: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tesseract_pdf_reader_runs_only_needed_page_ocr_and_checks_failures() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.test-work/congress-manual-pdf-reader");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let pdf = root.join("fixture.pdf");
        std::fs::write(&pdf, b"controlled PDF input").unwrap();
        let rendered = root.join("rendered-pages.log");
        let rendered_quoted = format!("'{}'", rendered.display());
        let pdfinfo = executable_script(&root, "pdfinfo", "printf 'Pages: 2\\n'");
        let pdftotext = executable_script(
            &root,
            "pdftotext",
            "printf 'Selectable text layer with enough characters to be usable.\\fshort\\f'",
        );
        let pdftoppm = executable_script(
            &root,
            "pdftoppm",
            &format!(
                "page=\nprevious=\nfor arg do\n  if [ \"$previous\" = -f ]; then page=$arg; fi\n  previous=$arg\n  stem=$arg\ndone\nprintf '%s\\n' \"$page\" >> {rendered_quoted}\nprintf image > \"$stem.png\""
            ),
        );
        let tesseract = executable_script(
            &root,
            "tesseract",
            "if [ \"$1\" = --version ]; then printf 'tesseract 5.4.0\\n'; exit 0; fi\nprintf 'OCR text for the rendered second page is sufficiently long.\\n'",
        );
        let reader = TesseractPdf::new(
            pdftotext.clone(),
            pdfinfo.clone(),
            pdftoppm.clone(),
            tesseract,
        )
        .await
        .unwrap();
        let pages = reader.read(&pdf, &root.join("scratch"), 2).await.unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages[0].text.starts_with("Selectable text layer"));
        assert_eq!(pages[0].ocr_engine, None);
        assert!(pages[1]
            .text
            .starts_with("OCR text for the rendered second page"));
        assert_eq!(
            pages[1].ocr_engine.as_deref(),
            Some("tesseract/5.4.0 lang=eng psm=3 dpi=300")
        );
        assert_eq!(std::fs::read_to_string(&rendered).unwrap(), "2\n");

        let mismatch = reader
            .read(&pdf, &root.join("mismatch"), 3)
            .await
            .unwrap_err();
        assert!(mismatch.to_string().contains("manifest says 3"));
        assert_eq!(std::fs::read_to_string(&rendered).unwrap(), "2\n");

        let failing_tesseract = executable_script(
            &root,
            "tesseract-failing",
            "if [ \"$1\" = --version ]; then printf 'tesseract 5.4.0\\n'; exit 0; fi\nprintf 'controlled OCR failure\\n' >&2\nexit 1",
        );
        let failing_reader = TesseractPdf::new(pdftotext, pdfinfo, pdftoppm, failing_tesseract)
            .await
            .unwrap();
        let failure = failing_reader
            .read(&pdf, &root.join("failure"), 2)
            .await
            .unwrap_err();
        assert!(format!("{failure:#}").contains("controlled OCR failure"));
        assert_eq!(std::fs::read_to_string(&rendered).unwrap(), "2\n2\n");
        std::fs::remove_dir_all(root).unwrap();
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

    struct LostLockPdf(Arc<AtomicBool>);

    #[async_trait]
    impl PdfPages for LostLockPdf {
        async fn read(
            &self,
            _pdf: &Path,
            _scratch: &Path,
            expected_pages: u32,
        ) -> anyhow::Result<Vec<PageText>> {
            self.0.store(true, Ordering::SeqCst);
            ensure!(expected_pages == 1, "unexpected fixture page count");
            Ok(vec![PageText {
                text: "MONDAY, November 2, 1789\nMr. CLAY of Alabama introduced the bill".into(),
                ocr_engine: None,
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
            "/../../fixtures/congress/lcg/manual/annals/annals-c01-s01-v01-b01.pdf"
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
        assert_eq!(hidden.left_out, ["annals/annals-c01-s01-v01-b01"]);
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
        assert_eq!(rows[0].doc_id, "lcg-annals-c01-s01-v01-b01_p1");
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
        let lost = Arc::new(AtomicBool::new(false));
        let refetch = Options {
            refetch: true,
            ..opts.clone()
        };
        assert!(run(
            &raw,
            &curated_store,
            &LostLockPdf(lost.clone()),
            &refetch,
            lost.as_ref(),
        )
        .await
        .is_err());
        assert_eq!(
            curated::next_attempt(&curated_store, "annals", &volume.unit)
                .await
                .unwrap(),
            2,
            "a process that lost its lock must not start a curated attempt"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
