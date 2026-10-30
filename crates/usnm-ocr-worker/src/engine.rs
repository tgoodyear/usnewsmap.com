//! OCR engines. Tesseract to start; the engine survey (#296) may add others
//! behind the same trait.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use async_trait::async_trait;
use usnm_core::ocr_engine::Digest;
pub use usnm_ocr_contract::routing::{EngineKind, EngineRoute};

mod sandbox;
mod tesseract;

pub use sandbox::{check_image_ref, Limits, Remote, Runner, ENGINE_USER};
pub use tesseract::{Tesseract, TesseractOptions};

pub(crate) use sandbox::{checked, podman_command, ContainerGuard, PageDir, Sandbox};

/// What an engine read on a page.
#[derive(Debug)]
pub struct EngineOutput {
    /// Plain text, as the engine wrote it (not normalized).
    pub text: String,
    /// ALTO XML, when the engine wrote ALTO.
    pub alto: Vec<u8>,
    /// The page, when the engine wrote boxes (`boxes.json`) rather than
    /// ALTO: already in pixels of the image read, in reading order.
    pub page: Option<usnm_alto::Page>,
    /// What the engine wrote, for the stored ALTO's settings: `ALTO` or
    /// `boxes.json`.
    pub layout_from: &'static str,
    /// The `ocr_engine` tag (13 §13.8.1): engine, version, models, image
    /// and route, e.g. `tesseract/5.5.0 profile=frak2021 image=sha256:…
    /// models=sha256:… route=minimal@r1 contract=0`.
    pub engine: String,
    /// What the runs record and the stored ALTO say about the read that
    /// only the engine knows (13 §13.8.2, §13.8.3).
    pub read: ReadInfo,
}

/// The engine's side of a read's provenance (13 §13.8.3's Engine,
/// Artifacts, Configuration and Resources groups). The worker adds what it
/// knows itself: the page, the times and the layout counts.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReadInfo {
    /// The engine's id (`engines.toml`, or `hello`'s `engine.id`).
    pub engine_id: String,
    /// `<name> <version> (<lang>)`: the contract adapter, or the worker
    /// itself when it runs the engine directly.
    pub adapter: String,
    /// `<registry>/usnm-ocr/<family>@sha256:<digest>`, `sha256:<id>`, or
    /// `local` (a host binary or a local adapter).
    pub image_ref: String,
    /// `sha256:<64 hex>`, or `local`.
    pub image_digest: String,
    /// `sha256:<64 hex>` of the profile's models as sorted
    /// `<name> <sha256>` lines, or `local`.
    pub models_digest: String,
    /// The profile's model files and their sha256s, as a JSON object.
    pub models_json: String,
    /// The options the engine read with, as a JSON object, keys sorted.
    pub options_json: String,
    /// `per_page` or `at_start`.
    pub models_loaded: &'static str,
    /// The read started the engine (a container per page, or a new
    /// instance).
    pub cold_start: bool,
    /// Start to `hello`, for a cold start of a long-lived instance.
    pub model_load_seconds: Option<f64>,
    /// `none` or `html` (13 §13.4.4).
    pub markup: &'static str,
    /// The adapter's `engine_stats`, as a JSON object, keys sorted.
    pub engine_stats_json: String,
}

impl ReadInfo {
    /// An engine with nothing to say (tests, `ocr-file`).
    pub fn local(engine_id: &str) -> Self {
        Self {
            engine_id: engine_id.to_owned(),
            adapter: format!("usnm-ocr-worker {} (rust)", crate::stored::worker_version()),
            image_ref: "local".into(),
            image_digest: "local".into(),
            models_digest: "local".into(),
            models_json: "{}".into(),
            options_json: "{}".into(),
            models_loaded: "per_page",
            cold_start: true,
            model_load_seconds: None,
            markup: "none",
            engine_stats_json: "{}".into(),
        }
    }
}

/// `sha256:<64 hex>` from an image reference pinned by digest
/// (`<name>@sha256:<digest>` or `sha256:<id>`), else `local`.
pub fn image_digest(image: &str) -> String {
    match image.rsplit_once("sha256:") {
        Some((_, h)) if h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()) => {
            format!("sha256:{}", h.to_ascii_lowercase())
        }
        _ => "local".into(),
    }
}

/// A profile's models as 13 §13.8.1 digests them: their sorted
/// `<name> <sha256>` lines' full sha256 (`sha256:<64 hex>`) and a JSON
/// object of name to sha256. None when a model has no known sha256.
pub fn models_full<'a>(
    models: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
) -> Option<(String, String)> {
    let mut map = std::collections::BTreeMap::new();
    for (name, sum) in models {
        map.insert(name, sum?);
    }
    let lines: String = map.iter().map(|(n, s)| format!("{n} {s}\n")).collect();
    let digest = format!("sha256:{}", crate::prefetch::sha256(lines.as_bytes()));
    Some((digest, serde_json::to_string(&map).ok()?))
}

/// Tesseract exited unsuccessfully on a page (not a timeout or a kill for
/// memory). Carried in [`EngineError::Failed`], so the worker can tell a
/// crash from other failures.
#[derive(Debug, thiserror::Error)]
#[error("tesseract exited with {status}: {stderr}")]
pub struct Exited {
    pub status: std::process::ExitStatus,
    /// The end of what Tesseract wrote to stderr.
    pub stderr: String,
}

impl Exited {
    /// Exit status 136 (128 + SIGFPE, as a container reports it), or the
    /// signal itself from a host process: the crash some JP2s cause in
    /// the engine's JPEG 2000 path (#454). The IIIF JPEG of such a page
    /// reads normally.
    pub fn is_status_136(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            if self.status.signal() == Some(8) {
                return true;
            }
        }
        self.status.code() == Some(136)
    }
}

/// Whether `e` is Tesseract exiting with status 136 ([`Exited::is_status_136`]).
pub fn exited_136(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Exited>()
        .is_some_and(Exited::is_status_136)
}

/// A failed read's status and error kind as 13 §13.4.6 names them, for
/// the `ocr_engine_run` event (§13.8.3). Its message is the error's whole
/// text, so logs read as before.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ReadFailed {
    /// `failed`, `timeout` or `bad_input`.
    pub status: &'static str,
    /// `engine`, `engine_timeout`, `engine_oom`, `engine_crash`,
    /// `engine_protocol` or `convert`.
    pub kind: &'static str,
    pub message: String,
}

impl ReadFailed {
    pub fn error(status: &'static str, kind: &'static str, message: String) -> anyhow::Error {
        anyhow::Error::new(Self {
            status,
            kind,
            message,
        })
    }
}

/// The status and error kind of a read that failed with `e`: what the
/// engine said ([`ReadFailed`]), a crash ([`Exited`]), or else `engine`.
pub fn read_failure(e: &anyhow::Error) -> (&'static str, &'static str) {
    if let Some(r) = e.downcast_ref::<ReadFailed>() {
        (r.status, r.kind)
    } else if e.is::<Exited>() {
        ("failed", "engine_crash")
    } else {
        ("failed", "engine")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// No model for the language on this node: retrying here won't help.
    #[error("no model for language `{0}` on this node")]
    Unsupported(String),
    #[error(transparent)]
    Failed(#[from] anyhow::Error),
}

#[async_trait]
pub trait Engine: Send + Sync + std::fmt::Debug {
    /// `ocr_source` for this engine's rows, e.g. `usnm-tesseract`.
    fn source(&self) -> &str;
    /// Whether this node can read pages in `language` (ISO 639-3). The
    /// worker asks before it fetches a scan, so a page with no model here
    /// costs no image request (12 Step 7). The engine contract (ADR-0016,
    /// #307) answers it from routing.
    fn can_read(&self, language: &str) -> bool;
    /// The languages and profiles this engine can read before its first page.
    fn routes(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
    /// The models available to this engine before its first page.
    fn models(&self) -> Vec<String> {
        Vec::new()
    }
    /// The resolution the engine reads scans at: IIIF JPEGs carry none,
    /// so it is assumed (13 §13.4.3), and the stored ALTO records it.
    fn dpi(&self) -> u32 {
        300
    }
    /// Safe execution configuration for opt-in provenance. Never include
    /// command arguments, environment variables, credentials or service URLs.
    fn execution_config(&self) -> serde_json::Value {
        serde_json::Value::Null
    }
    /// Stops what the engine keeps running between pages, after the last
    /// page (long-lived instances); nothing for an engine that starts
    /// one process per page.
    async fn shutdown(&self) {}
    /// The `ocr_engine` tag and engine fields a read in `language` would
    /// have, known before the read: for the `ocr_engine_run` event of a
    /// read that writes nothing (13 §13.8.3). None when nothing is known.
    fn describe(&self, language: &str) -> Option<(String, ReadInfo)> {
        let _ = language;
        None
    }
    /// Read `image` (a file) for a page in `language` (ISO 639-3), using
    /// `work` for scratch files.
    async fn ocr(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
    ) -> Result<EngineOutput, EngineError>;
    /// [`Engine::ocr`] for the worker's page `run_id` (13 §13.4.3), minted
    /// before the engine call: an engine behind the contract sends it to
    /// its adapter, so the protocol, the logs and the outputs share it.
    async fn read(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
        run_id: &str,
    ) -> Result<EngineOutput, EngineError> {
        let _ = run_id;
        self.ocr(image, language, work).await
    }
    /// [`Engine::read`], calling `on_instance` with the read's engine
    /// fields once it has the instance it reads on (its `cold_start`),
    /// before the engine answers: a failed read reports them.
    async fn read_traced(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
        run_id: &str,
        on_instance: &(dyn Fn(ReadInfo) + Send + Sync),
    ) -> Result<EngineOutput, EngineError> {
        let _ = on_instance;
        self.read(image, language, work, run_id).await
    }
}

/// The minimal route in every `ocr_engine` tag (13 §13.8.1).
/// Profile preferences and the production licence gate come from engines.toml.
pub const ROUTE: &str = "minimal";
pub const ROUTE_VERSION: u32 = 1;
/// The engine contract's version in the tag: 0, the worker runs Tesseract
/// directly (ADR-0016's contract isn't built).
pub const CONTRACT: u32 = 0;

/// The Tesseract image's `models.lock`, built into the worker so the tag's
/// `models` digest comes from the same file the image checked its models
/// against.
pub const MODELS_LOCK: &str = include_str!("../../../ocr/engines/tesseract/models.lock");

/// Engine/profile choices for a language, in catalog preference order.
pub fn candidates(language: &str) -> Vec<EngineRoute> {
    if EngineKind::for_language(language) == EngineKind::NdlocrLite {
        return vec![EngineRoute {
            kind: EngineKind::NdlocrLite,
            profile: "default".into(),
        }];
    }
    usnm_ocr_route::ENGINES
        .engine("tesseract-5.5")
        .expect("built-in Tesseract")
        .profile
        .iter()
        .filter(|p| p.languages.iter().any(|l| l == language))
        .map(|p| EngineRoute {
            kind: EngineKind::Tesseract,
            profile: p.id.clone(),
        })
        .collect()
}

/// The first cleared route available on a Tesseract-only node.
/// Other engines use `EngineKind::for_language` and their contract hello
/// profiles, subject to the same catalog production gate.
pub fn route(language: &str, installed: &BTreeSet<String>) -> Option<EngineRoute> {
    route_with_catalog(
        &usnm_ocr_route::ENGINES,
        "tesseract-5.5",
        language,
        installed,
    )
    .map(|profile| EngineRoute {
        kind: EngineKind::Tesseract,
        profile,
    })
}

fn route_with_catalog(
    catalog: &usnm_ocr_route::Catalog,
    engine_id: &str,
    language: &str,
    installed: &BTreeSet<String>,
) -> Option<String> {
    if EngineKind::for_language(language) != EngineKind::Tesseract {
        return None;
    }
    catalog
        .engine(engine_id)
        .ok()?
        .profile
        .iter()
        .find(|p| {
            p.languages.iter().any(|l| l == language)
            && catalog.check_production(engine_id, &p.models).is_ok()
            && p.models.iter().all(|name| {
                name.strip_prefix("tessdata/")
                    .and_then(|n| n.strip_suffix(".traineddata"))
                    .is_some_and(|n| installed.contains(n))
            })
            // The CLI reads the model names in the profile id. Do not allow
            // a catalog edit to gate one set of models and execute another.
            && p.id.split('+').map(|m| format!("tessdata/{m}.traineddata"))
                .collect::<BTreeSet<_>>() == p.models.iter().cloned().collect()
        })
        .map(|p| p.id.clone())
}

/// The last `n` characters of `s`.
pub(crate) fn tail(s: &str, n: usize) -> String {
    let skip = s.chars().count().saturating_sub(n);
    s.chars().skip(skip).collect()
}

/// The sha256 of a profile's model files (`deu_latf+deu`), as sorted lines
/// `<name> <sha256>` from `models.lock` (13 §13.8.1); none when a model
/// isn't in the lock.
pub fn models_digest(lock: &str, profile: &str) -> Option<Digest> {
    let entries: std::collections::BTreeMap<&str, &str> = lock
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (name, _url, sum) = (f.next()?, f.next()?, f.next()?);
            Some((name, sum))
        })
        .collect();
    let mut lines = Vec::new();
    for m in profile.split('+') {
        let name = format!("tessdata/{m}.traineddata");
        let sum = entries.get(name.as_str())?;
        lines.push(format!("{name} {sum}\n"));
    }
    lines.sort();
    let hex = crate::prefetch::sha256(lines.concat().as_bytes());
    Digest::short(&hex).ok()
}

/// A Tesseract profile's model files and their sha256s from `models.lock`,
/// as [`models_full`] gives them; none when a model isn't in the lock.
pub fn models_from_lock(lock: &str, profile: &str) -> Option<(String, String)> {
    let entries: std::collections::BTreeMap<&str, &str> = lock
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (name, _url, sum) = (f.next()?, f.next()?, f.next()?);
            Some((name, sum))
        })
        .collect();
    let names: Vec<String> = profile
        .split('+')
        .map(|m| format!("tessdata/{m}.traineddata"))
        .collect();
    models_full(
        names
            .iter()
            .map(|n| (n.as_str(), entries.get(n.as_str()).copied())),
    )
}

/// Size caps on an engine's files (13 §13.4.7).
const MAX_TEXT: u64 = 8 << 20;
const MAX_ALTO: u64 = 64 << 20;

/// An engine's file that breaks the rules on its outputs (13 §13.4.7):
/// `engine_protocol`, unlike a file it didn't write or an I/O error.
fn invalid_output(message: String) -> anyhow::Error {
    ReadFailed::error("failed", "engine_protocol", message)
}

/// Reads a file an engine wrote: it must be a regular file, not a symlink
/// (checked before opening, and refused by `O_NOFOLLOW` when opening), and
/// at most `max` bytes. The container has exited by then, so nothing
/// changes the file meanwhile. A file that breaks these rules is
/// `engine_protocol` ([`ReadFailed`]).
pub(crate) fn read_output(path: &Path, max: u64) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        return Err(invalid_output(format!(
            "{} isn't a regular file",
            path.display()
        )));
    }
    if meta.len() > max {
        return Err(invalid_output(format!(
            "{} is {} bytes, over {max}",
            path.display(),
            meta.len()
        )));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    let file = match opts.open(path) {
        // A symlink put there after the check.
        #[cfg(unix)]
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Err(invalid_output(format!("{} is a symlink", path.display())))
        }
        r => r?,
    };
    if !file.metadata()?.file_type().is_file() {
        return Err(invalid_output(format!(
            "{} isn't a regular file",
            path.display()
        )));
    }
    let mut buf = Vec::new();
    file.take(max + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        return Err(invalid_output(format!(
            "{} is over {max} bytes",
            path.display()
        )));
    }
    Ok(buf)
}

/// Point ALTO's `sourceImageInformation/fileName` at the scan's URL rather
/// than the scratch file it was read from.
pub fn set_alto_file_name(alto: &str, name: &str) -> String {
    let escaped = name
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    match (alto.find("<fileName>"), alto.find("</fileName>")) {
        (Some(a), Some(b)) if a < b => {
            format!("{}<fileName>{escaped}{}", &alto[..a], &alto[b..])
        }
        _ => alto.to_owned(),
    }
}

/// Point ALTO's `sourceImageInformation/fileName` at the scan's URL, as
/// [`set_alto_file_name`], and record the sha256 of the bytes read in a
/// `fileIdentifier` after it (ADR-0015 decision 9), so a result can be
/// traced to the exact scan, fetched or cached. A JP2 from its batch also
/// gets a `fileIdentifier` with the md5 LoC's bag manifest lists for it,
/// which the bytes matched.
pub fn set_alto_source(alto: &str, name: &str, sha256: &str, md5: Option<&str>) -> String {
    let named = set_alto_file_name(alto, name);
    let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex(sha256) {
        return named;
    }
    let mut ids =
        format!("<fileIdentifier fileIdentifierLocation=\"sha256\">{sha256}</fileIdentifier>");
    if let Some(m) = md5.filter(|m| hex(m)) {
        ids.push_str(&format!(
            "<fileIdentifier fileIdentifierLocation=\"md5\">{m}</fileIdentifier>"
        ));
    }
    match named.find("</fileName>") {
        Some(b) if named.contains("<fileName>") => {
            let end = b + "</fileName>".len();
            format!("{}{ids}{}", &named[..end], &named[end..])
        }
        _ => named,
    }
}

#[cfg(test)]
mod tests {
    use super::sandbox::{claim_slot, owner, parse_info, remove_stale_slots, RuntimeInfo, SLOTS};
    use super::tesseract::{parse_langs, parse_version};
    use super::*;
    use anyhow::Context;
    use std::time::Duration;

    #[test]
    fn run_metadata_execution_config_excludes_launch_credentials() {
        let mut tess = Tesseract {
            catalog: usnm_ocr_route::ENGINES.clone(),
            runner: Runner::Host("private-binary-path".into()),
            sandbox: None,
            version: "5.5.0".into(),
            installed: BTreeSet::new(),
            psm: 3,
            dpi: 300,
            threads: 2,
            timeout: Duration::from_secs(120),
        };
        assert_eq!(
            tess.execution_config(),
            serde_json::json!({
                "runtime": "host", "limits": null, "dpi": 300,
                "psm": 3, "threads": 2, "page_timeout_seconds": 120.0,
            })
        );
        tess.runner = Runner::Podman {
            podman: "private-podman-path".into(),
            remote: Some(Remote {
                url: "private-connection-url".into(),
                scratch: "private-scratch-path".into(),
                limits: Limits::default(),
            }),
            image: format!("registry/engine@sha256:{}", "a".repeat(64)),
        };
        let config = tess.execution_config();
        assert_eq!(config["limits"]["memory_mib"], Limits::default().memory_mib);
        assert_eq!(config["runtime"], "podman");
        assert!(!config.to_string().contains("private-"));
    }

    #[test]
    fn japanese_selects_ndlocr_but_is_unavailable_to_tesseract() {
        assert_eq!(
            candidates("jpn"),
            vec![EngineRoute {
                kind: EngineKind::NdlocrLite,
                profile: "default".into(),
            }]
        );
        let installed = BTreeSet::from(["jpn".into(), "eng".into()]);
        assert!(route("jpn", &installed).is_none());
        let tess = Tesseract {
            catalog: usnm_ocr_route::ENGINES.clone(),
            runner: Runner::Host("tesseract".into()),
            sandbox: None,
            version: "5.5.0".into(),
            installed,
            psm: 3,
            dpi: 300,
            threads: 1,
            timeout: Duration::from_secs(1200),
        };
        assert!(!tess.can_read("jpn"));
        assert!(tess.engine_for("jpn").is_none());
    }

    #[test]
    fn all_other_languages_keep_the_catalogs_tesseract_preferences() {
        let tess = usnm_ocr_route::ENGINES.engine("tesseract-5.5").unwrap();
        for language in tess.profile.iter().flat_map(|p| &p.languages) {
            if language == "jpn" {
                continue;
            }
            let expected: Vec<_> = tess
                .profile
                .iter()
                .filter(|p| p.languages.contains(language))
                .map(|p| EngineRoute {
                    kind: EngineKind::Tesseract,
                    profile: p.id.clone(),
                })
                .collect();
            assert_eq!(candidates(language), expected, "{language}");
        }
        assert!(candidates("unknown").is_empty());
    }

    #[test]
    fn routes_german_to_fraktur_when_installed() {
        let set = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
        assert_eq!(
            route("deu", &set(&["deu", "deu_latf", "eng"]))
                .map(|r| r.profile)
                .as_deref(),
            Some("deu_latf+deu")
        );
        assert_eq!(
            route("deu", &set(&["deu", "deu_latf", "frak2021"]))
                .map(|r| r.profile)
                .as_deref(),
            Some("frak2021")
        );
        assert_eq!(
            route("deu", &set(&["deu"])).map(|r| r.profile).as_deref(),
            Some("deu")
        );
        assert_eq!(route("pol", &set(&["deu", "eng"])), None);
        assert_eq!(route("deu", &set(&["eng"])), None);
        assert_eq!(
            route("srp", &set(&["srp"])).map(|r| r.profile).as_deref(),
            Some("srp")
        );
        assert_eq!(
            route("ces", &set(&["ces"])).map(|r| r.profile).as_deref(),
            Some("ces")
        );
        assert_eq!(
            route("eng", &set(&["eng", "osd"]))
                .map(|r| r.profile)
                .as_deref(),
            Some("eng")
        );
    }

    #[test]
    fn startup_status_includes_installed_models_and_resolved_routes() {
        let installed = ["deu", "eng", "frak2021"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let tesseract = Tesseract {
            catalog: usnm_ocr_route::ENGINES.clone(),
            runner: Runner::Host("tesseract".into()),
            sandbox: None,
            version: "5.5.0".into(),
            installed,
            psm: 3,
            dpi: 300,
            threads: 1,
            timeout: Duration::from_secs(600),
        };
        assert_eq!(tesseract.models(), ["deu", "eng", "frak2021"]);
        let routes = tesseract.routes();
        assert_eq!(routes.get("deu").map(String::as_str), Some("frak2021"));
        assert_eq!(routes.get("eng").map(String::as_str), Some("eng"));
    }

    #[test]
    fn the_lock_pins_every_model_a_route_names() {
        let lock: BTreeSet<String> = MODELS_LOCK
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| l.split_whitespace().next())
            .filter_map(|n| n.strip_prefix("tessdata/")?.strip_suffix(".traineddata"))
            .map(str::to_owned)
            .collect();
        for l in ["ces", "deu", "eng", "pol", "rus", "srp"] {
            let selected = route(l, &lock).unwrap_or_else(|| panic!("no route for {l}"));
            let profile = selected.profile;
            assert!(models_digest(MODELS_LOCK, &profile).is_some(), "{profile}");
        }
        assert_eq!(
            route("deu", &lock).map(|r| r.profile).as_deref(),
            Some("frak2021")
        );
        assert_eq!(models_digest(MODELS_LOCK, "frk"), None);
    }

    #[tokio::test]
    async fn osd_modes_fail_before_tesseract_can_start() {
        for psm in [0, 1, 12] {
            let result = Tesseract::detect(TesseractOptions {
                runner: Runner::Host("must-not-run-tesseract".into()),
                psm,
                ..Default::default()
            })
            .await;
            assert!(format!("{:#}", result.unwrap_err()).contains("uncataloged osd.traineddata"));
        }
    }

    #[test]
    fn worker_and_routes_check_apply_the_same_gate_to_code_and_models() {
        let set = |names: &[&str]| {
            names
                .iter()
                .map(|n| (*n).to_owned())
                .collect::<BTreeSet<_>>()
        };
        let routes = r#"version = 1
[[route]]
id = "test"
candidates = [{engine="tesseract-5.5", profile="eng"}]
"#;
        let installed = set(&["eng"]);
        for code in [true, false] {
            for tier in ["Apache-2.0", "none"] {
                for missing in [
                    "",
                    "author",
                    "source",
                    "credit",
                    "fetched",
                    "note",
                    "restricted",
                    "undecided",
                    "license",
                    "production",
                ] {
                    let mut catalog = usnm_ocr_route::ENGINES.clone();
                    let a = if code {
                        &mut catalog.engine[0].attribution
                    } else {
                        &mut catalog.engine[0].model[0].attribution
                    };
                    a.license = tier.into();
                    a.note = Some("No explicit licence, source checked".into());
                    match missing {
                        "author" => a.author = None,
                        "source" => a.source = None,
                        "credit" => a.credit = None,
                        "fetched" => a.fetched = None,
                        "note" => a.note = None,
                        "restricted" => a.restricted = Some("clause".into()),
                        "undecided" => a.undecided = Some("decision pending".into()),
                        "license" => a.license = "unknown".into(),
                        "production" => catalog.engine[0].production = false,
                        "" => {}
                        _ => unreachable!(),
                    }
                    let allowed = missing.is_empty()
                        || (tier != "none" && matches!(missing, "fetched" | "note"));
                    assert_eq!(
                        catalog.check_routes(routes).is_ok(),
                        allowed,
                        "{code} {tier} {missing}"
                    );
                    assert_eq!(
                        route_with_catalog(&catalog, "tesseract-5.5", "eng", &installed).is_some(),
                        allowed,
                        "{code} {tier} {missing}"
                    );
                }
            }
        }
        let mut catalog = usnm_ocr_route::ENGINES.clone();
        catalog.engine[0]
            .profile
            .iter_mut()
            .find(|p| p.id == "eng")
            .unwrap()
            .models = vec!["tessdata/deu.traineddata".into()];
        assert!(
            route_with_catalog(&catalog, "tesseract-5.5", "eng", &set(&["eng", "deu"])).is_none()
        );
    }

    #[test]
    fn tags_name_the_image_and_models() {
        let t = Tesseract {
            catalog: usnm_ocr_route::ENGINES.clone(),
            runner: Runner::Podman {
                podman: "podman".into(),
                remote: None,
                image: format!("sha256:{}", "ab".repeat(32)),
            },
            sandbox: None,
            version: "5.5.0".into(),
            installed: BTreeSet::new(),
            psm: 3,
            dpi: 300,
            threads: 1,
            timeout: Duration::from_secs(1),
        };
        let tag = t.tag("frak2021").unwrap().to_string();
        let want = models_digest(MODELS_LOCK, "frak2021").unwrap();
        assert_eq!(
            tag,
            format!("tesseract/5.5.0 profile=frak2021 image=sha256:abababababab models={want} route=minimal@r1 contract=0")
        );
        // The order of a profile's models doesn't change the digest.
        assert_eq!(
            models_digest(MODELS_LOCK, "deu_latf+deu"),
            models_digest(MODELS_LOCK, "deu+deu_latf")
        );
        let host = Tesseract {
            runner: Runner::Host("tesseract".into()),
            ..t
        };
        assert!(!host.tag("eng").unwrap().pinned());
    }

    /// Direct Tesseract's scratch (its slot, the scan's copy) is the
    /// worker's: a failure there is a `ScratchError`, so no engine read is
    /// reported, and Tesseract isn't run.
    #[tokio::test]
    async fn scratch_failures_before_tesseract_are_the_workers() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("page.png");
        std::fs::write(&image, b"png").unwrap();
        let t = Tesseract {
            catalog: usnm_ocr_route::ENGINES.clone(),
            runner: Runner::Host(dir.path().join("no-tesseract")),
            sandbox: None,
            version: "5.5.0".into(),
            installed: ["eng".to_owned()].into(),
            psm: 3,
            dpi: 300,
            threads: 1,
            timeout: Duration::from_secs(1),
        };
        let scratch_error = |r: Result<EngineOutput, EngineError>| match r {
            Err(EngineError::Failed(e)) => {
                assert!(e.is::<crate::worker::ScratchError>(), "{e:#}");
            }
            other => panic!("{:?}", other.map(|o| o.engine)),
        };
        // The scan can't be copied into a work directory that isn't there.
        scratch_error(t.ocr(&image, "eng", &dir.path().join("gone")).await);
        // No slot on a scratch that isn't there.
        let t = Tesseract {
            sandbox: Some(Sandbox {
                podman: "podman".into(),
                url: String::new(),
                image: String::new(),
                scratch: dir.path().join("no-scratch"),
                info: RuntimeInfo {
                    engine_uid: 1,
                    runtime_uid: None,
                    selinux: false,
                },
            }),
            ..t
        };
        scratch_error(t.ocr(&image, "eng", dir.path()).await);
        // Tesseract that isn't there is the engine's failure, not scratch.
        let t = Tesseract { sandbox: None, ..t };
        match t.ocr(&image, "eng", dir.path()).await {
            Err(EngineError::Failed(e)) => {
                assert!(!e.is::<crate::worker::ScratchError>(), "{e:#}")
            }
            other => panic!("{:?}", other.map(|o| o.engine)),
        }
    }

    #[test]
    fn reads_version_and_languages() {
        assert_eq!(
            parse_version("tesseract 5.5.1\n leptonica-1.87.0\n").as_deref(),
            Some("5.5.1")
        );
        assert_eq!(
            parse_version("tesseract v5.3.0.20221214\n").as_deref(),
            Some("5.3.0.20221214")
        );
        assert_eq!(parse_version("nope"), None);
        let langs = parse_langs(
            "List of available languages in \"/usr/share/tesseract/tessdata/\" (3):\neng\nosd\ndeu_latf\n",
        );
        assert_eq!(
            langs.into_iter().collect::<Vec<_>>(),
            ["deu_latf", "eng", "osd"]
        );
    }

    #[test]
    fn image_must_be_pinned() {
        let id = "a".repeat(64);
        assert!(check_image_ref(&format!("sha256:{id}")).is_ok());
        assert!(check_image_ref(&format!("ghcr.io/x/tesseract@sha256:{id}")).is_ok());
        assert!(check_image_ref(&format!("localhost/usnm-ocr/tesseract:5.5@sha256:{id}")).is_ok());
        assert!(check_image_ref("usnm-ocr/tesseract:latest").is_err());
        assert!(check_image_ref("sha256:abc").is_err());
        assert!(check_image_ref(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(check_image_ref(&format!("@sha256:{id}")).is_err());
        assert!(check_image_ref(&format!("x y@sha256:{id}")).is_err());
    }

    fn args_of(cmd: &tokio::process::Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn podman_runs_the_image_without_network_or_privileges() {
        let image = format!("sha256:{}", "b".repeat(64));
        let runner = Runner::Podman {
            podman: "podman".into(),
            remote: Some(Remote {
                url: "unix:///run/usnm-ocr/podman.sock".into(),
                scratch: "/run/usnm-ocr/scratch".into(),
                limits: Limits::default(),
            }),
            image: image.clone(),
        };
        let (cmd, guard) = runner.command(
            2,
            Some(Path::new("/run/usnm-ocr/scratch/page-x")),
            Some("s0:c4,c5"),
        );
        let mut guard = guard.expect("a container has a guard");
        guard.armed = false;
        let args = args_of(&cmd);
        let has = |a: &str| args.iter().any(|x| x == a);
        let pair = |a: &str, b: &str| args.windows(2).any(|w| w[0] == a && w[1] == b);
        assert_eq!(
            args[..3],
            ["--remote", "--url", "unix:///run/usnm-ocr/podman.sock"]
        );
        for a in [
            "run",
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=all",
            "--security-opt=no-new-privileges",
            "--http-proxy=false",
            "--no-hosts",
            "--log-driver=none",
            "OMP_THREAD_LIMIT=2",
            "HOME=/tmp",
            "/run/usnm-ocr/scratch/page-x:/work:rw",
        ] {
            assert!(has(a), "{a} missing from {args:?}");
        }
        // No --rm: OOMKilled is read before the container is removed.
        assert!(!has("--rm"), "{args:?}");
        assert!(pair("--user", "10001:10001"), "{args:?}");
        assert!(pair("--memory", "1536m") && pair("--memory-swap", "1536m"));
        assert!(pair("--cpus", "1") && pair("--pids-limit", "64"));
        assert!(pair("--security-opt", "label=level:s0:c4,c5"));
        assert!(!args
            .iter()
            .any(|a| a.ends_with(":Z") || a == "--userns=keep-id"));
        assert_eq!(args.last(), Some(&image));
        // The client gets none of the worker's environment.
        let envs: Vec<_> = cmd.as_std().get_envs().collect();
        assert!(envs.iter().all(|(k, _)| [
            "PATH",
            "LANG",
            "HOME",
            "XDG_RUNTIME_DIR",
            "XDG_CONFIG_HOME",
            "CONTAINERS_CONF"
        ]
        .contains(&k.to_str().unwrap())));

        // Podman run by the worker's own user (a developer machine): the
        // engine runs as the scratch directory's owner, which keep-id maps.
        let local = Runner::Podman {
            podman: "podman".into(),
            remote: None,
            image: image.clone(),
        };
        let dir = tempfile::tempdir().unwrap();
        let (cmd, guard) = local.command(1, Some(dir.path()), None);
        guard.unwrap().armed = false;
        let args = args_of(&cmd);
        assert_eq!(args[0], "run");
        assert!(args.iter().any(|a| a == "--userns=keep-id"));
        assert!(args.iter().any(|a| a.ends_with("/work:Z")));
        #[cfg(unix)]
        {
            let (uid, gid) = owner(dir.path()).unwrap();
            let want = format!("{uid}:{gid}");
            assert!(args.contains(&want));
        }
        let (cmd, guard) = Runner::Host("tesseract".into()).command(1, None, None);
        assert!(guard.is_none());
        assert_eq!(cmd.as_std().get_program(), "tesseract");
    }

    #[test]
    fn reads_the_engine_runtime_from_podman_info() {
        let info = |controllers: &str, uidmap: &str| {
            format!(
                r#"{{"host": {{"cgroupControllers": {controllers},
                  "idMappings": {{"uidmap": {uidmap}}},
                  "security": {{"selinuxEnabled": true}}}}}}"#
            )
        };
        let rootless = r#"[{"container_id": 0, "host_id": 991, "size": 1},
                           {"container_id": 1, "host_id": 231072, "size": 65536}]"#;
        assert_eq!(
            parse_info(&info(r#"["cpu", "io", "memory", "pids"]"#, rootless)).unwrap(),
            RuntimeInfo {
                engine_uid: 231072 + 10000,
                runtime_uid: Some(991),
                selinux: true
            }
        );
        // Rootful: the user is itself on the host.
        assert_eq!(
            parse_info(&info(r#"["cpu", "memory", "pids"]"#, "[]")).unwrap(),
            RuntimeInfo {
                engine_uid: 10001,
                runtime_uid: None,
                selinux: true
            }
        );
        // EL9's default delegation (memory and pids only) can't apply --cpus.
        let err = parse_info(&info(r#"["memory", "pids"]"#, rootless)).unwrap_err();
        assert!(err.to_string().contains("no cpu controller"), "{err}");
        // Subordinate ids too few for the image's user.
        let small = r#"[{"container_id": 0, "host_id": 991, "size": 1},
                        {"container_id": 1, "host_id": 231072, "size": 1000}]"#;
        assert!(parse_info(&info(r#"["cpu", "memory", "pids"]"#, small)).is_err());
        assert!(parse_info("nope").is_err());
    }

    #[test]
    fn engine_files_are_read_as_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("page.txt");
        std::fs::write(&f, b"hello").unwrap();
        assert_eq!(read_output(&f, 5).unwrap(), b"hello");
        // Files that break the rules are the engine's protocol failures;
        // a file it didn't write is an ordinary failure.
        let kind = |r: anyhow::Result<Vec<u8>>| read_failure(&r.unwrap_err());
        let protocol = ("failed", "engine_protocol");
        assert_eq!(kind(read_output(&f, 4)), protocol);
        assert_eq!(kind(read_output(dir.path(), 100)), protocol);
        assert_eq!(
            kind(read_output(&dir.path().join("none"), 100)),
            ("failed", "engine")
        );
        // With the caller's context, as the Tesseract engine adds it.
        let e = read_output(&f, 4)
            .context("tesseract wrote no usable text")
            .unwrap_err();
        assert_eq!(read_failure(&e), protocol);
        assert!(format!("{e:#}").contains("over 4"), "{e:#}");
        #[cfg(unix)]
        {
            let link = dir.path().join("page.xml");
            std::os::unix::fs::symlink(&f, &link).unwrap();
            assert_eq!(kind(read_output(&link, 100)), protocol);
        }
    }

    #[test]
    fn slots_are_claimed_once_and_freed() {
        let scratch = tempfile::tempdir().unwrap();
        let a = claim_slot(scratch.path()).unwrap();
        let b = claim_slot(scratch.path()).unwrap();
        assert_ne!(a.slot, b.slot);
        assert_ne!(a.level(), b.level());
        assert!(a.level().starts_with("s0:c"), "{}", a.level());
        // Another process's slot is skipped.
        let taken = (a.slot + 1) % SLOTS;
        std::fs::create_dir_all(scratch.path().join(format!("slot-{taken}"))).unwrap();
        for _ in 0..4 {
            let c = claim_slot(scratch.path()).unwrap();
            assert!(c.slot != taken && c.slot != a.slot && c.slot != b.slot);
        }
        let path = a.path.clone();
        drop(a);
        assert!(!path.exists());
        // Old slots go; new ones stay.
        remove_stale_slots(scratch.path(), Duration::from_secs(3600));
        assert!(b.path.exists());
        remove_stale_slots(scratch.path(), Duration::ZERO);
        assert!(!scratch.path().join(format!("slot-{taken}")).exists());
    }

    #[test]
    fn alto_names_the_scan() {
        let alto =
            "<sourceImageInformation><fileName>/tmp/x/page.jpg</fileName></sourceImageInformation>";
        assert_eq!(
            set_alto_file_name(alto, "https://tile.loc.gov/a?b&c"),
            "<sourceImageInformation><fileName>https://tile.loc.gov/a?b&amp;c</fileName></sourceImageInformation>"
        );
        assert_eq!(set_alto_file_name("<alto/>", "x"), "<alto/>");
        assert_eq!(
            set_alto_source(alto, "https://tile.loc.gov/a", "ab12", None),
            "<sourceImageInformation><fileName>https://tile.loc.gov/a</fileName><fileIdentifier fileIdentifierLocation=\"sha256\">ab12</fileIdentifier></sourceImageInformation>"
        );
        assert_eq!(set_alto_source("<alto/>", "x", "ab", None), "<alto/>");
        assert!(!set_alto_source(alto, "x", "<x>", None).contains("fileIdentifier"));
        // A JP2 from its batch: the storage URL, the sha256 and the bag's md5.
        assert_eq!(
            set_alto_source(alto, "https://tile.loc.gov/storage-services/service/ndnp/x/0812.jp2", "ab12", Some("cd34")),
            "<sourceImageInformation><fileName>https://tile.loc.gov/storage-services/service/ndnp/x/0812.jp2</fileName><fileIdentifier fileIdentifierLocation=\"sha256\">ab12</fileIdentifier><fileIdentifier fileIdentifierLocation=\"md5\">cd34</fileIdentifier></sourceImageInformation>"
        );
        assert!(!set_alto_source(alto, "x", "ab", Some("<x>")).contains("md5"));
    }

    /// Reads a rendered line with the real Tesseract, when it is installed
    /// with English (skipped otherwise).
    #[tokio::test]
    async fn real_tesseract_reads_text_and_alto() {
        let Ok(t) = Tesseract::detect(TesseractOptions::default()).await else {
            eprintln!("tesseract not installed; skipping");
            return;
        };
        if t.engine_for("eng").is_none() {
            eprintln!("tesseract has no eng model; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("line.pgm");
        std::fs::write(&img, crate::testimg::text_image("HELLO WORLD")).unwrap();
        let out = t.ocr(&img, "eng", dir.path()).await.unwrap();
        assert!(out.engine.starts_with("tesseract/"), "{}", out.engine);
        assert!(out.engine.contains(" profile=eng "), "{}", out.engine);
        assert!(out.text.contains("HELLO"), "read {:?}", out.text);
        assert!(String::from_utf8_lossy(&out.alto).contains("<alto"));
        assert!(matches!(
            t.ocr(&img, "zzz", dir.path()).await,
            Err(EngineError::Unsupported(_))
        ));
    }
}
