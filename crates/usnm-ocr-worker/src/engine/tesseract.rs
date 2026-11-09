//! Tesseract discovery, profile tags and per-page OCR execution.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag, TagError};

use super::{
    check_image_ref, image_digest, invalid_output, models_digest, models_from_lock, read_output,
    route_with_catalog, tail, RouteFor, Engine, EngineError, EngineOutput, Exited, ReadFailed, ReadInfo,
    Runner, Sandbox, CONTRACT, MAX_ALTO, MAX_TEXT, MODELS_LOCK, ROUTE, ROUTE_VERSION,
};

#[derive(Debug, Clone)]
pub struct Tesseract {
    pub(super) catalog: usnm_ocr_route::Catalog,
    pub(super) runner: Runner,
    /// Under the engine runtime.
    pub(super) sandbox: Option<Sandbox>,
    pub(super) version: String,
    pub(super) installed: BTreeSet<String>,
    pub(super) psm: u8,
    pub(super) dpi: u32,
    pub(super) threads: u32,
    pub(super) timeout: Duration,
}

fn tesseract_id(version: &str) -> String {
    format!(
        "tesseract-{}",
        version.split('.').take(2).collect::<Vec<_>>().join(".")
    )
}

/// `tesseract 5.5.1` → `5.5.1`, from `tesseract --version`'s first line
/// (stdout in 5.x, stderr in older releases).
pub(super) fn parse_version(out: &str) -> Option<String> {
    let line = out
        .lines()
        .find(|l| l.trim_start().starts_with("tesseract "))?;
    let v = line.trim().strip_prefix("tesseract ")?.trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    (!v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
    .then(|| v.to_owned())
}

/// The model names in `tesseract --list-langs` output.
pub(super) fn parse_langs(out: &str) -> BTreeSet<String> {
    out.lines()
        .skip_while(|l| !l.starts_with("List of available languages"))
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(' '))
        .map(str::to_owned)
        .collect()
}

/// What Tesseract read on one line crop.
#[derive(Debug, Clone, PartialEq)]
pub struct LineReading {
    /// The words, joined by single spaces; empty when it found none.
    pub text: String,
    /// The words' mean confidence, 0 to 1.
    pub conf: Option<f64>,
}

/// Each image's words from Tesseract's TSV of a multi-image run: rows of level 5 are words,
/// and `page_num` counts the images from 1.
pub(crate) fn parse_line_tsv(tsv: &str, images: usize) -> anyhow::Result<Vec<LineReading>> {
    let mut words: Vec<Vec<(String, Option<f64>)>> = vec![Vec::new(); images];
    let mut lines = tsv.lines();
    let header = lines.next().unwrap_or_default();
    anyhow::ensure!(
        header.starts_with("level\tpage_num\t") && header.ends_with("\tconf\ttext"),
        "not Tesseract's TSV"
    );
    for row in lines {
        let fields: Vec<&str> = row.split('\t').collect();
        anyhow::ensure!(fields.len() == 12, "TSV row has {} fields", fields.len());
        if fields[0] != "5" {
            continue;
        }
        let page: usize = fields[1].parse().context("TSV page_num")?;
        anyhow::ensure!((1..=images).contains(&page), "TSV page {page} of {images} crops");
        let text = fields[11].trim();
        if text.is_empty() {
            continue;
        }
        let conf: f64 = fields[10].parse().context("TSV conf")?;
        let conf = (conf >= 0.0).then(|| (conf / 100.0).clamp(0.0, 1.0));
        words[page - 1].push((text.to_owned(), conf));
    }
    Ok(words
        .into_iter()
        .map(|w| {
            let confs: Vec<f64> = w.iter().filter_map(|(_, c)| *c).collect();
            LineReading {
                text: w.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" "),
                conf: (!confs.is_empty()).then(|| confs.iter().sum::<f64>() / confs.len() as f64),
            }
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct TesseractOptions {
    pub runner: Runner,
    /// Page segmentation mode (3: automatic, no orientation detection).
    pub psm: u8,
    /// Resolution to assume: IIIF JPEGs carry none, and Tesseract guesses 70.
    pub dpi: u32,
    /// OpenMP threads per page (`OMP_THREAD_LIMIT`): 1 when pages run in parallel.
    pub threads: u32,
    pub timeout: Duration,
}

impl Default for TesseractOptions {
    fn default() -> Self {
        Self {
            runner: Runner::Host("tesseract".into()),
            psm: 3,
            dpi: 300,
            threads: 1,
            timeout: Duration::from_secs(600),
        }
    }
}

async fn run(runner: &Runner, args: &[&str]) -> anyhow::Result<String> {
    let (mut cmd, mut guard) = runner.command(1, None, None);
    let out = cmd
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("running tesseract ({})", runner.describe()))?;
    let oom = match guard.as_mut() {
        Some(g) => g.finish().await,
        None => false,
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!(
            "tesseract {:?} ({}) exited with {}{}: {}",
            args,
            runner.describe(),
            out.status,
            if oom { ", killed for memory" } else { "" },
            tail(&err, 400)
        );
    }
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

impl Tesseract {
    /// Find Tesseract's version and installed models (once, at startup).
    pub async fn detect(opts: TesseractOptions) -> anyhow::Result<Self> {
        Self::detect_with_catalog(opts, usnm_ocr_route::ENGINES.clone()).await
    }

    pub async fn detect_with_catalog(
        opts: TesseractOptions,
        catalog: usnm_ocr_route::Catalog,
    ) -> anyhow::Result<Self> {
        usnm_ocr_route::check_tesseract_psm(u64::from(opts.psm))?;
        let mut sandbox = None;
        if let Runner::Podman {
            image,
            podman,
            remote,
        } = &opts.runner
        {
            check_image_ref(image)?;
            if let Some(r) = remote {
                sandbox = Some(Sandbox::detect(podman, r, image).await?);
            }
        }
        let version = parse_version(&run(&opts.runner, &["--version"]).await?)
            .context("can't read `tesseract --version`")?;
        let installed = parse_langs(&run(&opts.runner, &["--list-langs"]).await?);
        let engine_id = tesseract_id(&version);
        catalog
            .engine(&engine_id)
            .context("Tesseract is not in the engine catalog")?;
        Ok(Self {
            catalog,
            runner: opts.runner,
            sandbox,
            version,
            installed,
            psm: opts.psm,
            dpi: opts.dpi,
            threads: opts.threads.max(1),
            timeout: opts.timeout,
        })
    }

    fn route(&self, language: &str) -> Option<String> {
        route_with_catalog(
            &self.catalog,
            &tesseract_id(&self.version),
            language,
            &self.installed,
            RouteFor::Page,
        )
    }

    /// The profile a line job for `language` reads with (#1621), a lines-only one included.
    pub fn line_profile(&self, language: &str) -> Option<String> {
        route_with_catalog(
            &self.catalog,
            &tesseract_id(&self.version),
            language,
            &self.installed,
            RouteFor::Lines,
        )
    }

    /// Reads `crops`, image files in `work` holding one printed line each, with the line
    /// profile for `language`, in one Tesseract run over a list of them (Tesseract reads a
    /// text file of image paths as a multi-page input). Each crop's reading is its words
    /// joined by spaces, with their mean confidence (0 to 1); a crop with no words reads as
    /// empty. Runs the host binary only: a line job is a BOINC job.
    pub async fn read_lines(
        &self,
        crops: &[std::path::PathBuf],
        language: &str,
        work: &Path,
    ) -> Result<Vec<LineReading>, EngineError> {
        let models = self
            .line_profile(language)
            .ok_or_else(|| EngineError::Unsupported(language.to_owned()))?;
        if self.sandbox.is_some() || !matches!(self.runner, Runner::Host(_)) {
            return Err(anyhow::anyhow!("line reads run the host's Tesseract").into());
        }
        let mut list = String::new();
        for crop in crops {
            let name = match crop.parent() {
                Some(p) if p == work => crop.file_name().and_then(|n| n.to_str()),
                _ => None,
            }
            .context("a line crop must be a UTF-8 file name in the scratch directory")?;
            if name.contains(['\n', '\r']) {
                return Err(anyhow::anyhow!("line crop name holds a line break").into());
            }
            list.push_str(name);
            list.push('\n');
        }
        std::fs::write(work.join("lines.txt"), list).context("writing the line list")?;
        let (mut cmd, _guard) = self.runner.command(self.threads, Some(work), None);
        let child = cmd
            .args(["lines.txt", "lines"])
            .args(["-l", models.as_str(), "--psm", &self.psm.to_string()])
            .args(["--dpi", &self.dpi.to_string()])
            // A variable, not the `tsv` config file: the native build ships `txt` and `alto`
            // only.
            .args(["-c", "tessedit_create_tsv=1"])
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(self.timeout, child)
            .await
            .map_err(|_| {
                ReadFailed::error(
                    "timeout",
                    "engine_timeout",
                    format!("tesseract took over {:?}", self.timeout),
                )
            })?
            .context("running tesseract")?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow::Error::new(Exited {
                status: out.status,
                stderr: tail(&err, 400),
            })
            .into());
        }
        let tsv = read_output(&work.join("lines.tsv"), MAX_TEXT).with_context(|| {
            format!(
                "tesseract wrote no line TSV; it said: {}",
                tail(&String::from_utf8_lossy(&out.stderr), 400)
            )
        })?;
        let tsv = String::from_utf8(tsv)
            .map_err(|_| invalid_output("tesseract's TSV isn't UTF-8".into()))?;
        parse_line_tsv(&tsv, crops.len()).map_err(|e| invalid_output(format!("{e:#}")).into())
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn installed(&self) -> &BTreeSet<String> {
        &self.installed
    }

    pub fn runner(&self) -> &Runner {
        &self.runner
    }

    /// The `ocr_engine` value for a language, if it has a model here.
    pub fn engine_for(&self, language: &str) -> Option<String> {
        let profile = self.route(language)?;
        self.tag(&profile).ok().map(|t| t.to_string())
    }

    /// The `ocr_engine` tag for a read with `profile` (13 §13.8.1); an
    /// error when Tesseract's version or a model name doesn't fit the
    /// tag's character sets.
    pub fn tag(&self, profile: &str) -> Result<OcrEngineTag, TagError> {
        let (image, models) = match &self.runner {
            Runner::Podman { image, .. } => (
                image
                    .rsplit_once("sha256:")
                    .and_then(|(_, h)| Digest::short(h).ok())
                    .unwrap_or(Digest::Local),
                models_digest(MODELS_LOCK, profile).unwrap_or(Digest::Local),
            ),
            // A host binary reads whatever models the host has.
            Runner::Host(_) => (Digest::Local, Digest::Local),
        };
        OcrEngineTag::new(Fields {
            family: "tesseract",
            version: &self.version,
            profile,
            image,
            models,
            route: ROUTE,
            route_version: ROUTE_VERSION,
            contract: CONTRACT,
        })
    }
}

#[async_trait]
impl Engine for Tesseract {
    fn source(&self) -> &str {
        "usnm-tesseract"
    }

    fn can_read(&self, language: &str) -> bool {
        self.route(language).is_some()
    }

    fn routes(&self) -> BTreeMap<String, String> {
        let Ok(engine) = self.catalog.engine(&tesseract_id(&self.version)) else {
            return BTreeMap::new();
        };
        let mut routes = BTreeMap::new();
        for profile in &engine.profile {
            for language in &profile.languages {
                if self.route(language).as_deref() == Some(profile.id.as_str()) {
                    routes.insert(language.clone(), profile.id.clone());
                }
            }
        }
        routes
    }

    fn models(&self) -> Vec<String> {
        self.installed.iter().cloned().collect()
    }

    fn dpi(&self) -> u32 {
        self.dpi
    }

    fn execution_config(&self) -> serde_json::Value {
        let (runtime, limits) = match &self.runner {
            Runner::Host(_) => ("host", None),
            Runner::Podman { remote, .. } => ("podman", remote.as_ref().map(|r| r.limits)),
        };
        serde_json::json!({
            "runtime": runtime,
            "limits": limits,
            "dpi": self.dpi,
            "psm": self.psm,
            "threads": self.threads,
            "page_timeout_seconds": self.timeout.as_secs_f64(),
        })
    }

    fn describe(&self, language: &str) -> Option<(String, ReadInfo)> {
        let profile = self.route(language)?;
        let tag = self.tag(&profile).ok()?.to_string();
        Some((tag, self.read_info(&profile)))
    }

    async fn ocr(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
    ) -> Result<EngineOutput, EngineError> {
        let models = self
            .route(language)
            .ok_or_else(|| EngineError::Unsupported(language.to_owned()))?;
        // Under the engine runtime, the page gets a slot on the shared
        // scratch tmpfs instead of `work`, which the engine account's
        // containers can't reach; the scan is copied in.
        match &self.sandbox {
            Some(sb) => {
                // The worker's own scratch, before Tesseract starts: no
                // engine read to report (13 §13.8.3).
                let (dir, level) = sb.page_dir().await.map_err(|e| {
                    anyhow::Error::new(crate::worker::ScratchError(format!(
                        "the page's scratch slot: {e:#}"
                    )))
                })?;
                let out = self
                    .read_page(image, &models, &dir.path, level.as_deref())
                    .await;
                sb.release(dir).await;
                out
            }
            None => self.read_page(image, &models, work, None).await,
        }
    }
}

impl Tesseract {
    /// Reads `image` with `models` in `work`, labelled `level` under the
    /// engine runtime.
    async fn read_page(
        &self,
        image: &Path,
        models: &str,
        work: &Path,
        level: Option<&str>,
    ) -> Result<EngineOutput, EngineError> {
        // Tesseract runs in `work`, on paths relative to it, so the same
        // arguments serve the host binary and a container with `work` as
        // /work. A scan from elsewhere is copied in first.
        let input = match image.parent() {
            Some(p) if p == work => image.to_owned(),
            _ => {
                let ext = image.extension().and_then(|e| e.to_str()).unwrap_or("img");
                let copy = work.join(format!("input.{ext}"));
                tokio::fs::copy(image, &copy).await.map_err(|e| {
                    anyhow::Error::new(crate::worker::ScratchError(format!(
                        "copying the scan into the scratch directory: {e}"
                    )))
                })?;
                copy
            }
        };
        let input_name = input
            .file_name()
            .and_then(|n| n.to_str())
            .context("scan file name isn't UTF-8")?
            .to_owned();
        let base = work.join("page");
        let (mut cmd, mut guard) = self.runner.command(self.threads, Some(work), level);
        // `txt` and `alto` are Tesseract's config files for the two outputs
        // (page.txt, page.xml), written in one pass.
        let child = cmd
            .args([input_name.as_str(), "page"])
            .args(["-l", models, "--psm", &self.psm.to_string()])
            .args(["--dpi", &self.dpi.to_string()])
            .args(["txt", "alto"])
            .kill_on_drop(true)
            .output();
        let out = tokio::time::timeout(self.timeout, child)
            .await
            .map_err(|_| {
                ReadFailed::error(
                    "timeout",
                    "engine_timeout",
                    format!("tesseract took over {:?}", self.timeout),
                )
            })?
            .context("running tesseract")?;
        let oom = match guard.as_mut() {
            Some(g) => g.finish().await,
            None => false,
        };
        if oom {
            let limit = match &self.runner {
                Runner::Podman {
                    remote: Some(r), ..
                } => format!(" ({} MiB)", r.limits.memory_mib),
                _ => String::new(),
            };
            return Err(ReadFailed::error(
                "failed",
                "engine_oom",
                format!("tesseract ran out of memory{limit}"),
            )
            .into());
        }
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(anyhow::Error::new(Exited {
                status: out.status,
                stderr: tail(&err, 400),
            })
            .into());
        }
        // What Tesseract said, for a run that exits 0 but writes nothing.
        let said = || {
            let files: Vec<String> = std::fs::read_dir(work)
                .map(|d| {
                    d.filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "scratch holds {files:?}; tesseract said: {}",
                tail(&String::from_utf8_lossy(&out.stderr), 400)
            )
        };
        // The engine wrote these, so they're read as untrusted (13 §13.4.7):
        // regular files only, no symlinks, size-capped.
        let text = read_output(&base.with_extension("txt"), MAX_TEXT)
            .with_context(|| format!("tesseract wrote no usable text ({})", said()))?;
        let text = String::from_utf8(text)
            .map_err(|_| invalid_output("tesseract's text isn't UTF-8".into()))?;
        let alto = read_output(&base.with_extension("xml"), MAX_ALTO)
            .with_context(|| format!("tesseract wrote no usable ALTO ({})", said()))?;
        Ok(EngineOutput {
            text,
            alto,
            page: None,
            layout_from: "ALTO",
            engine: self
                .tag(models)
                .with_context(|| {
                    format!(
                        "no ocr_engine tag for tesseract {} with {models}",
                        self.version
                    )
                })?
                .to_string(),
            read: self.read_info(models),
        })
    }

    /// The runs record's engine fields for a read with `profile`: the
    /// worker runs Tesseract itself, a container (or a process) per page.
    pub fn read_info(&self, profile: &str) -> ReadInfo {
        let (image_ref, models) = match &self.runner {
            Runner::Podman { image, .. } => (image.clone(), models_from_lock(MODELS_LOCK, profile)),
            Runner::Host(_) => ("local".to_owned(), None),
        };
        let (models_digest, models_json) =
            models.unwrap_or_else(|| ("local".to_owned(), "{}".to_owned()));
        let minor: Vec<&str> = self.version.split('.').take(2).collect();
        ReadInfo {
            engine_id: format!("tesseract-{}", minor.join(".")),
            image_digest: image_digest(&image_ref),
            image_ref,
            models_digest,
            models_json,
            options_json: serde_json::json!({ "dpi": self.dpi, "psm": self.psm }).to_string(),
            ..ReadInfo::local("tesseract")
        }
    }
}

#[cfg(test)]
mod line_tests {
    use super::*;

    const HEADER: &str =
        "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

    #[test]
    fn line_tsv_gives_each_crop_its_words() {
        let tsv = format!(
            "{HEADER}\n\
             1\t1\t0\t0\t0\t0\t0\t0\t100\t30\t-1\t\n\
             5\t1\t1\t1\t1\t1\t2\t2\t40\t20\t90.0\tᏣᎳᎩ\n\
             5\t1\t1\t1\t1\t2\t50\t2\t40\t20\t70.0\tᎦᏬᏂᎯᏍᏗ\n\
             1\t2\t0\t0\t0\t0\t0\t0\t100\t30\t-1\t\n\
             1\t3\t0\t0\t0\t0\t0\t0\t100\t30\t-1\t\n\
             5\t3\t1\t1\t1\t1\t2\t2\t40\t20\t-1\t \n\
             5\t3\t1\t1\t1\t2\t2\t2\t40\t20\t50\tᎠ\n"
        );
        let got = parse_line_tsv(&tsv, 3).unwrap();
        assert_eq!(got[0].text, "ᏣᎳᎩ ᎦᏬᏂᎯᏍᏗ");
        assert!((got[0].conf.unwrap() - 0.8).abs() < 1e-9);
        assert_eq!(got[1], LineReading { text: String::new(), conf: None });
        assert_eq!(got[2].text, "Ꭰ");
        assert_eq!(got[2].conf, Some(0.5));
        assert!(parse_line_tsv(&tsv, 2).is_err());
        assert!(parse_line_tsv("not a tsv", 1).is_err());
        assert!(parse_line_tsv(&format!("{HEADER}\n5\t1\t1"), 1).is_err());
        assert_eq!(parse_line_tsv(HEADER, 2).unwrap().len(), 2);
    }
}
