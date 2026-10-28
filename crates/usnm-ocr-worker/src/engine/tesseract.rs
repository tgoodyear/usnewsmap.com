//! Tesseract discovery, profile tags and per-page OCR execution.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use usnm_core::ocr_engine::{Digest, Fields, OcrEngineTag, TagError};

use super::{
    check_image_ref, image_digest, invalid_output, models_digest, models_from_lock, read_output,
    route_with_catalog, tail, Engine, EngineError, EngineOutput, Exited, ReadFailed, ReadInfo,
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
        )
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
