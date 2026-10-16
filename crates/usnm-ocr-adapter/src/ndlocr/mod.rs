//! CPU NDLOCR-Lite behind `usnm-ocr-engine/1`. The unmodified upstream CLI
//! loads the models per page, just as `ja-ocr/jaocr.py` does.
mod alto;
pub use alto::{ndlocr_to_alto, Geometry};

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{sha256_file, ReadError, Reader, Reading};
use anyhow::{ensure, Context};
use usnm_alto::Granularity;
use usnm_ocr_contract::engine::{
    Adapter, Cancel, EngineInfo, Hello, Layout, Model, ModelsLoaded, PageRequest, Profile, CONTRACT,
};

pub const COMMIT: &str = "636d1cfeb1331f89f4048f416e49e23a09a714b5";
pub const MODELS_LOCK: &str = include_str!("../../../../ocr/engines/ndlocr-lite/models.lock");
pub const PROFILE: &str = "default";

/// Aggregate identity of the model files (not the lock's comments or URLs).
pub fn models_digest() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (name, sum) in locked_models() {
        h.update(format!("{name} {sum}\n").as_bytes());
    }
    format!(
        "sha256:{}",
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

fn locked_models() -> impl Iterator<Item = (&'static str, &'static str)> {
    MODELS_LOCK
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let fields: Vec<_> = l.split_whitespace().collect();
            (fields.len() == 3).then(|| (fields[0], fields[2]))
        })
}

/// Hashes installed model bytes, and refuses a changed upstream checkout.
pub fn hello(root: &Path) -> anyhow::Result<Hello> {
    ensure!(
        std::fs::read_to_string(root.join("COMMIT"))?.trim() == COMMIT,
        "NDLOCR commit differs from lock"
    );
    let mut models = Vec::new();
    for (name, sum) in locked_models() {
        let file = root.join(name);
        ensure!(
            std::fs::symlink_metadata(&file)?.file_type().is_file(),
            "model is not a regular file"
        );
        let actual = sha256_file(&file).with_context(|| format!("hashing {name}"))?;
        ensure!(actual == sum, "NDLOCR model checksum mismatch: {name}");
        models.push(Model {
            name: name.into(),
            sha256: actual,
        });
    }
    Ok(Hello {
        adapter: Adapter {
            lang: "rust".into(),
            name: "usnm-ocr-adapter-ndlocr-lite".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        cancel: Cancel::Adapter,
        contract: CONTRACT,
        // ONNX floating point can vary across CPU architectures. No cross-host
        // determinism claim until the pilot measures it.
        deterministic: false,
        engine: EngineInfo {
            components: None,
            family: "ndlocr-lite".into(),
            id: "ndlocr-lite".into(),
            version: COMMIT.into(),
        },
        granularity: Granularity::Line,
        layout: Layout::Alto,
        profiles: vec![Profile {
            id: PROFILE.into(),
            languages: vec!["jpn".into()],
            models: models.iter().map(|m| m.name.clone()).collect(),
            tunable: Vec::new(),
        }],
        models,
        models_loaded: ModelsLoaded::PerPage,
    })
}

/// The same normalization as `jaocr.normalize`, without the Latin ligature
/// expansion and dehyphenation of the general `normalize_ocr` path.
pub fn normalize(raw: &str) -> String {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    use unicode_normalization::UnicodeNormalization;
    let text: String = raw
        .nfc()
        .filter(|c| {
            matches!(c, '\n' | '\t' | ' ')
                || !matches!(
                    get_general_category(*c),
                    G::Control | G::Format | G::PrivateUse | G::Surrogate | G::Unassigned
                )
        })
        .collect();
    text.split('\n')
        .map(|line| {
            let mut out = String::new();
            let mut space = false;
            for c in line.chars() {
                if matches!(c, ' ' | '\t' | '\u{3000}') {
                    if !space {
                        out.push(' ');
                    }
                    space = true;
                } else {
                    out.push(c);
                    space = false;
                }
            }
            out.trim().to_owned()
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug)]
pub struct Output {
    pub xml: Vec<u8>,
    pub text: String,
}

pub struct Ndlocr {
    pub root: PathBuf,
    pub python: PathBuf,
    pub scratch: PathBuf,
}

fn read_bounded(path: &Path, max: u64) -> anyhow::Result<Vec<u8>> {
    ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_file(),
        "not a regular output file"
    );
    let f = File::open(path)?;
    ensure!(f.metadata()?.len() <= max, "NDLOCR output over limit");
    let mut bytes = Vec::new();
    f.take(max + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= max, "NDLOCR output over limit");
    Ok(bytes)
}

impl Ndlocr {
    /// Runs one image with a deadline; kills/reaps its entire process group.
    /// Scratch is always under the caller's writable job directory, not /in.
    pub fn ocr(&self, image: &Path, deadline: Instant) -> Result<Output, ReadError> {
        self.run(image, deadline).map_err(|e| {
            e.downcast_ref::<ReadError>()
                .cloned()
                .unwrap_or_else(|| ReadError::Failed {
                    kind: "engine".into(),
                    message: format!("{e:#}"),
                    restart: false,
                })
        })
    }
    fn run(&self, image: &Path, deadline: Instant) -> anyhow::Result<Output> {
        ensure!(Instant::now() < deadline, ReadError::Timeout);
        let scratch = self.scratch.canonicalize()?;
        let dir = tempfile::Builder::new()
            .prefix("ndlocr-")
            .tempdir_in(scratch)?;
        let image = image.canonicalize()?;
        // BOINC's logical filename is extensionless; upstream filters by suffix.
        let mut head = [0; 16];
        let n = File::open(&image)?.read(&mut head)?;
        let ext = if head[..n].starts_with(b"\xff\xd8\xff") {
            "jpg"
        } else if head[..n].starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if head[..n].starts_with(b"\x00\x00\x00\x0cjP  \r\n\x87\n")
            || head[..n].starts_with(b"\xff\x4f\xff\x51")
        {
            "jp2"
        } else {
            return Err(ReadError::BadInput(
                "unsupported/corrupt scan (JPEG, PNG or JP2 required)".into(),
            )
            .into());
        };
        let source = dir.path().join(format!("page.{ext}"));
        std::fs::copy(image, &source)?;
        let output = dir.path().join("output");
        std::fs::create_dir(&output)?;
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.path().join("engine.log"))?;
        let mut cmd = Command::new(&self.python);
        cmd.current_dir(self.root.join("src"))
            .arg("ocr.py")
            .arg("--sourceimg")
            .arg(&source)
            .arg("--output")
            .arg(&output)
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("starting NDLOCR-Lite")?;
        let stop = |child: &mut std::process::Child| {
            #[cfg(unix)]
            if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        };
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() >= deadline => {
                    stop(&mut child);
                    return Err(ReadError::Timeout.into());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    stop(&mut child);
                    return Err(e.into());
                }
            }
        };
        if !status.success() {
            // A bounded tail, never page text, is enough to diagnose a broken image.
            use std::io::{Seek, SeekFrom};
            let mut f = File::open(dir.path().join("engine.log"))?;
            f.seek(SeekFrom::End(-(f.metadata()?.len().min(2048) as i64)))?;
            let mut tail = String::new();
            f.read_to_string(&mut tail)?;
            if [
                "PIL.UnidentifiedImageError",
                "DecompressionBombError",
                "image file is truncated",
                "broken data stream when reading image file",
            ]
            .iter()
            .any(|message| tail.contains(message))
            {
                return Err(
                    ReadError::BadInput("NDLOCR-Lite could not decode the scan".into()).into(),
                );
            }
            anyhow::bail!("NDLOCR-Lite exit {status}: {tail}");
        }
        Ok(Output {
            xml: read_bounded(&output.join("page.xml"), 64 << 20)
                .context("NDLOCR-Lite produced no XML")?,
            text: normalize(std::str::from_utf8(
                &read_bounded(&output.join("page.txt"), 8 << 20)
                    .context("NDLOCR-Lite produced no text")?,
            )?),
        })
    }
}

/// Contract scratch belongs inside the request's writable output directory.
/// The on-prem host deliberately grants search-only access to its /out parent.
pub struct ContractReader {
    pub engine_root: PathBuf,
    pub python: PathBuf,
    pub root: crate::Root,
}

impl Reader for ContractReader {
    fn read(
        &mut self,
        req: &PageRequest,
        image: &Path,
        deadline: Instant,
    ) -> Result<Reading, ReadError> {
        let engine = Ndlocr {
            root: self.engine_root.clone(),
            python: self.python.clone(),
            scratch: self.root.map(&req.out),
        };
        let result = engine.ocr(image, deadline)?;
        let alto = ndlocr_to_alto(
            &result.xml,
            Geometry {
                dpi: req.image.dpi,
                ..Geometry::default()
            },
            &req.image.path,
            COMMIT,
            1,
            &processing_time(chrono::Utc::now()),
        )
        .map_err(|e| ReadError::Failed {
            kind: "layout".into(),
            message: e.to_string(),
            restart: false,
        })?;
        Ok(Reading::Alto {
            alto,
            text: result.text,
            dpi: req.image.dpi,
        })
    }
}

fn processing_time(at: chrono::DateTime<chrono::Utc>) -> String {
    // Python datetime cannot round-trip Linux's nanosecond precision, and
    // spells UTC as +00:00 rather than Z in the reference ALTO converter.
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn processing_time_round_trips_through_python_datetime_spelling() {
        for stamp in [
            "2026-10-16T00:00:00.123456789+00:00",
            "2026-10-16T00:00:00+00:00",
        ] {
            let at = chrono::DateTime::parse_from_rfc3339(stamp)
                .unwrap()
                .with_timezone(&chrono::Utc);
            assert_eq!(super::processing_time(at), "2026-10-16T00:00:00+00:00");
        }
    }
}
