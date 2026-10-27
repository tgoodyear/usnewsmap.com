//! A text-only engine adapter: the Tesseract CLI's plain text, one call per
//! page, as a page-sized block in `boxes.json` (layout `boxes`,
//! granularity `page`). It exists to run the worker's path for engines
//! that return text without ALTO (vision-language models, for one) with
//! an engine every node already has. `--alto` returns Tesseract's word-level
//! ALTO instead, with text derived from the same parsed layout.
//!
//! `usnm-ocr-adapter-tesseract-text --contract 1 --threads 1`

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::Parser;
use usnm_alto::{Granularity, Markup};
use usnm_ocr_adapter::{serve, sha256_file, ReadError, Reader, Reading, Root};
use usnm_ocr_contract::engine::{
    Adapter, Cancel, EngineInfo, Hello, Layout, Model, ModelsLoaded, PageRequest, Profile, CONTRACT,
};

#[derive(Parser)]
#[command(about = "Tesseract adapter for the engine contract (usnm-ocr-engine/1)")]
struct Args {
    /// The contract version the worker speaks.
    #[arg(long)]
    contract: u32,
    /// OpenMP threads per page.
    #[arg(long, default_value_t = 1)]
    threads: u32,
    /// The Tesseract binary.
    #[arg(long, default_value = "tesseract")]
    tesseract_bin: PathBuf,
    /// Where the requests' `/in` and `/out` are: `/` in a container.
    #[arg(long, default_value = "/")]
    root: PathBuf,
    /// Return word-level ALTO rather than page-sized text boxes.
    #[arg(long)]
    alto: bool,
}

struct Tesseract {
    bin: PathBuf,
    threads: u32,
    alto: bool,
}

/// `tesseract 5.5.1` from `tesseract --version`.
fn version(bin: &Path) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .output()?;
    let all = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    all.lines()
        .find_map(|l| l.trim().strip_prefix("tesseract "))
        .map(|v| v.trim().trim_start_matches('v').to_owned())
        .filter(|v| !v.is_empty())
        .context("no version from tesseract --version")
}

/// The tessdata directory and the models in it, from `--list-langs`.
fn models(bin: &Path) -> anyhow::Result<(PathBuf, Vec<String>)> {
    let out = Command::new(bin)
        .arg("--list-langs")
        .stdin(Stdio::null())
        .output()?;
    let all = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines = all
        .lines()
        .skip_while(|l| !l.starts_with("List of available languages"));
    let head = lines
        .next()
        .context("no model list from tesseract --list-langs")?;
    let dir = head
        .split('"')
        .nth(1)
        .context("no tessdata directory in the model list")?;
    let names = lines
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains(' ') && *l != "osd")
        .map(str::to_owned)
        .collect();
    Ok((PathBuf::from(dir), names))
}

fn hello(bin: &Path, alto: bool) -> anyhow::Result<Hello> {
    let version = version(bin)?;
    let (dir, names) = models(bin)?;
    let mut models = Vec::new();
    let mut profiles = Vec::new();
    for name in names {
        let file = format!("{name}.traineddata");
        let sha256 = sha256_file(&dir.join(&file)).with_context(|| format!("hashing {file}"))?;
        let model = format!("tessdata/{file}");
        models.push(Model {
            name: model.clone(),
            sha256,
        });
        // A model named for its language (`deu`, `deu_latf`) reads it.
        let lang: String = name.chars().take_while(char::is_ascii_lowercase).collect();
        if lang.len() == 3 {
            profiles.push(Profile {
                id: name,
                languages: vec![lang],
                models: vec![model],
                tunable: vec!["psm".into()],
            });
        }
    }
    Ok(Hello {
        adapter: Adapter {
            lang: "rust".into(),
            name: "usnm-ocr-adapter-tesseract-text".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        cancel: Cancel::Adapter,
        contract: CONTRACT,
        deterministic: true,
        engine: EngineInfo {
            components: None,
            family: "tesseract".into(),
            id: format!(
                "tesseract-{}",
                version.split('.').take(2).collect::<Vec<_>>().join(".")
            ),
            version,
        },
        granularity: if alto {
            Granularity::Word
        } else {
            Granularity::Page
        },
        layout: if alto { Layout::Alto } else { Layout::Boxes },
        models,
        models_loaded: ModelsLoaded::PerPage,
        profiles,
    })
}

impl Reader for Tesseract {
    fn read(
        &mut self,
        req: &PageRequest,
        image: &Path,
        deadline: Instant,
    ) -> Result<Reading, ReadError> {
        let psm = req.options.get("psm").and_then(|v| v.as_u64()).unwrap_or(3);
        usnm_ocr_route::check_tesseract_psm(psm)
            .map_err(|e| ReadError::Unsupported(e.to_string()))?;
        let mut cmd = Command::new(&self.bin);
        cmd.arg(image)
            .arg("stdout")
            .args(["-l", &req.profile, "--psm", &psm.to_string()])
            .args(["--dpi", &req.image.dpi.to_string()])
            .env("OMP_THREAD_LIMIT", self.threads.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if self.alto {
            cmd.arg("alto");
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own process group, so a timeout stops everything it started.
            cmd.process_group(0);
        }
        eprintln!("run {}: {:?}", req.run_id, cmd);
        let failed = |message: String| ReadError::Failed {
            kind: "engine".into(),
            message,
            restart: false,
        };
        let mut child = cmd
            .spawn()
            .map_err(|e| failed(format!("starting tesseract: {e}")))?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let out = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stdout.read_to_end(&mut b);
            b
        });
        let err = std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = stderr.read_to_end(&mut b);
            b
        });
        // Stops the whole process group, so nothing it started keeps
        // working into the next page (13 §13.6.5), then reaps the child.
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
                // Descendants may hold the pipes after the parent exits.
                Ok(Some(s)) if out.is_finished() && err.is_finished() => break s,
                Ok(_) if Instant::now() >= deadline => {
                    stop(&mut child);
                    return Err(ReadError::Timeout);
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    stop(&mut child);
                    return Err(failed(format!("waiting for tesseract: {e}")));
                }
            }
        };
        let text = out.join().unwrap_or_default();
        let log = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
        if !status.success() {
            // Leptonica couldn't decode the image: the page's input, not
            // the engine (13 §13.4.6).
            if log.contains("Leptonica Error in pixRead") {
                let why = log
                    .lines()
                    .find(|l| l.contains("Leptonica Error in pixRead"))
                    .unwrap_or_default();
                return Err(ReadError::BadInput(format!(
                    "tesseract can't read the image: {why}"
                )));
            }
            let tail: String = log
                .chars()
                .rev()
                .take(400)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(failed(format!("tesseract exited with {status}: {tail}")));
        }
        if self.alto {
            let page = usnm_alto::parse(
                &text,
                &usnm_alto::ParseOptions {
                    dpi: Some(f64::from(req.image.dpi)),
                    ..Default::default()
                },
            )
            .map_err(|e| failed(format!("tesseract's ALTO is invalid: {e}")))?;
            return Ok(Reading::Alto {
                text: usnm_alto::text(&page),
                alto: text,
                dpi: req.image.dpi,
            });
        }
        let text =
            String::from_utf8(text).map_err(|_| failed("tesseract's text isn't UTF-8".into()))?;
        Ok(Reading::Text {
            text,
            markup: Markup::None,
        })
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if args.contract != CONTRACT {
        bail!(
            "this adapter speaks contract {CONTRACT}, not {}",
            args.contract
        );
    }
    let hello = hello(&args.tesseract_bin, args.alto)?;
    let mut reader = Tesseract {
        bin: args.tesseract_bin,
        threads: args.threads.max(1),
        alto: args.alto,
    };
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    serve(&hello, &mut reader, &Root(args.root), stdin, stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> PageRequest {
        PageRequest {
            deadline_ms: 1000,
            image: usnm_ocr_contract::engine::Image {
                dpi: 300,
                dpi_source: usnm_ocr_contract::engine::DpiSource::Assumed,
                height: 100,
                width: 100,
                path: "/in/test.jpg".into(),
                sha256: "ab".repeat(32),
            },
            language: "eng".into(),
            options: Default::default(),
            out: "/out/test/".into(),
            profile: "eng".into(),
            run_id: "test".into(),
            script: None,
        }
    }

    #[cfg(unix)]
    #[test]
    fn alto_mode_derives_text_from_word_layout_and_rejects_bad_xml() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tesseract");
        let alto = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><MeasurementUnit>pixel</MeasurementUnit></Description><Layout><Page WIDTH="100" HEIGHT="100"><PrintSpace HPOS="0" VPOS="0" WIDTH="100" HEIGHT="100"><TextBlock HPOS="1" VPOS="2" WIDTH="80" HEIGHT="20"><TextLine HPOS="1" VPOS="2" WIDTH="80" HEIGHT="20"><String CONTENT="HELLO" HPOS="1" VPOS="2" WIDTH="30" HEIGHT="20" WC="0.9"/><String CONTENT="LOWER" HPOS="40" VPOS="2" WIDTH="40" HEIGHT="20" WC="0.8"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;
        let mut engine = Tesseract {
            bin: bin.clone(),
            threads: 2,
            alto: true,
        };
        for output in [alto, "not XML"] {
            std::fs::write(
                &bin,
                format!(
                    "#!/bin/sh\n[ \"$OMP_THREAD_LIMIT\" = 2 ] || exit 2\ncase \"$*\" in *'--dpi 300 alto') ;; *) exit 3;; esac\nprintf '%s' '{output}'\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let result = engine.read(
                &request(),
                Path::new("fixture.png"),
                Instant::now() + Duration::from_secs(5),
            );
            if output == alto {
                let Reading::Alto { text, alto, dpi } = result.unwrap() else {
                    panic!("expected word-level ALTO");
                };
                assert_eq!(text.trim(), "HELLO LOWER");
                assert_eq!(alto, output.as_bytes());
                assert_eq!(dpi, 300);
            } else {
                assert!(
                    matches!(result, Err(ReadError::Failed { message, .. }) if message.contains("ALTO is invalid"))
                );
            }
        }
    }

    #[test]
    fn osd_modes_fail_before_spawning_tesseract() {
        let mut engine = Tesseract {
            bin: "must-not-run-tesseract".into(),
            threads: 1,
            alto: false,
        };
        for psm in [0, 1, 12] {
            let mut req = request();
            req.options.insert("psm".into(), serde_json::json!(psm));
            assert!(
                matches!(engine.read(&req, Path::new("must-not-read-image"), Instant::now()),
                Err(ReadError::Unsupported(message)) if message.contains("uncataloged osd.traineddata"))
            );
        }
    }
}
