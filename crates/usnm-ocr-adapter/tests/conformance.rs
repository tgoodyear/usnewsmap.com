//! The conformance runner (13 §13.4.8) against the text-only adapter and
//! against stand-in adapters that break the contract: each case must
//! catch what it is for, and an instance that breaks the protocol or a
//! size cap is replaced for the next page.

use std::path::{Path, PathBuf};
use std::time::Duration;

use usnm_ocr_worker::contract::conformance::{self, Outcome, Report};
use usnm_ocr_worker::contract::{ContractOptions, Launch};

const ADAPTER: &str = env!("CARGO_BIN_EXE_usnm-ocr-adapter-tesseract-text");
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../ocr/contract/fixtures");

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// A `tesseract` that reads the suite's pages by what they hold: the
/// corrupt fixture fails as Leptonica does, a large page is slow, a page
/// with no black pixel has no text, and any other page reads as `text`
/// (a shell command's output).
fn stand_in(dir: &Path, text: &str) -> PathBuf {
    let tessdata = dir.join("tessdata");
    std::fs::create_dir_all(&tessdata).unwrap();
    std::fs::write(tessdata.join("eng.traineddata"), b"model").unwrap();
    script(
        dir,
        "tesseract",
        &format!(
            r#"#!/bin/sh
case "$1" in
  --version) echo "tesseract 5.5.9"; exit 0 ;;
  --list-langs) echo "List of available languages in \"{}/\" (1):"; echo eng; exit 0 ;;
esac
echo "run: $*" >&2
if grep -q badcrc "$1"; then
  echo "Leptonica Error in pixRead: pix not read: $1" >&2
  echo "Error during processing." >&2
  exit 1
fi
if [ "$(wc -c < "$1")" -gt 1000000 ]; then sleep 30; fi
if ! od -An -v -tu1 "$1" | grep -qw 0; then exit 0; fi
{text}
"#,
            tessdata.display()
        ),
    )
}

fn options(program: PathBuf, args: Vec<String>, scratch: &Path) -> ContractOptions {
    ContractOptions {
        launch: Launch::Local {
            program,
            args,
            scratch: scratch.to_owned(),
        },
        instances: 1,
        threads: 1,
        start_timeout: Duration::from_secs(30),
        page_timeout: Duration::from_secs(30),
        max_jobs: 100,
        dpi: 72,
        node: "test1".into(),
    }
}

async fn text_adapter(text: &str) -> Report {
    let dir = tempfile::tempdir().unwrap();
    let tesseract = stand_in(dir.path(), text);
    let opts = options(
        ADAPTER.into(),
        vec![format!("--tesseract-bin={}", tesseract.display())],
        dir.path(),
    );
    conformance::run(opts, Path::new(FIXTURES)).await.unwrap()
}

fn outcomes(r: &Report) -> String {
    r.cases
        .iter()
        .map(|c| format!("{} {:?}: {}", c.case, c.outcome, c.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_text_adapter_conforms() {
    let r = text_adapter(&format!("echo '{}'", conformance::LINE)).await;
    assert!(r.passed(), "{}", outcomes(&r));
    for case in [
        "hello",
        "text_line",
        "tesseract_dpi",
        "blank_page",
        "corrupt_image",
        "unknown_profile",
        "deadline",
        "stdout_noise",
        "oversized_output",
        "stdin_eof",
    ] {
        assert_eq!(r.outcome(case), Some(Outcome::Pass), "{}", outcomes(&r));
    }
    assert_eq!(r.outcome("deadline_cgroup"), Some(Outcome::Skipped));
    assert_eq!(r.engine, "tesseract-text 5.5.9");
}

#[tokio::test]
async fn a_wrong_reading_fails_the_text_line() {
    let r = text_adapter("echo 'HELLO WORLO'").await;
    assert_eq!(
        r.outcome("text_line"),
        Some(Outcome::Fail),
        "{}",
        outcomes(&r)
    );
    assert!(!r.passed());
}

/// Text over the contract's caps (the block's 512 KiB, the text file's
/// 8 MiB) is refused and the instance replaced: the next page reads.
#[tokio::test]
async fn an_oversized_output_is_caught() {
    let r = text_adapter("head -c 9000000 /dev/zero | tr '\\000' a").await;
    assert_eq!(
        r.outcome("oversized_output"),
        Some(Outcome::Fail),
        "{}",
        outcomes(&r)
    );
    assert_eq!(r.outcome("text_line"), Some(Outcome::Fail));
    assert_eq!(
        r.outcome("stdout_noise"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
    // On a new instance.
    assert_eq!(
        r.outcome("blank_page"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
    assert_eq!(r.outcome("corrupt_image"), Some(Outcome::Pass));
}

const HELLO: &str = r#"{"adapter":{"lang":"sh","name":"stand-in","version":"0"},"cancel":"CANCEL","contract":1,"deterministic":true,"engine":{"family":"stand-in","id":"stand-in","version":"1"},"granularity":"page","layout":"boxes","models":[],"models_loaded":"at_start","profiles":[{"id":"eng","languages":["eng"],"models":[],"tunable":[]}],"type":"hello"}"#;

/// An adapter that prints its engine's chatter on stdout.
#[tokio::test]
async fn noise_on_stdout_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = script(
        dir.path(),
        "adapter",
        &format!(
            "#!/bin/sh\necho '{}'\nwhile read l; do echo 'loading model'; done\n",
            HELLO.replace("CANCEL", "adapter")
        ),
    );
    let r = conformance::run(
        options(adapter, Vec::new(), dir.path()),
        Path::new(FIXTURES),
    )
    .await
    .unwrap();
    assert_eq!(r.outcome("hello"), Some(Outcome::Pass), "{}", outcomes(&r));
    assert_eq!(
        r.outcome("stdout_noise"),
        Some(Outcome::Fail),
        "{}",
        outcomes(&r)
    );
    assert_eq!(r.outcome("oversized_output"), Some(Outcome::Pass));
    assert_eq!(r.outcome("tesseract_dpi"), Some(Outcome::Skipped));
    assert_eq!(
        r.outcome("stdin_eof"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
    assert!(!r.passed());
}

/// `cancel = kill`: an adapter that never answers the large page is killed
/// at the deadline plus the worker's grace, and a new instance reads the
/// next page.
#[tokio::test]
async fn a_kill_engine_is_killed_and_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let adapter = script(
        dir.path(),
        "adapter",
        &format!(
            r#"#!/bin/sh
echo '{}'
while IFS= read -r l; do
  case "$l" in *'"width":3'*) exec sleep 60 ;; esac
  run=$(printf '%s\n' "$l" | sed 's/.*"run_id":"\([^"]*\)".*/\1/')
  out=$(printf '%s\n' "$l" | sed 's/.*"out":"\([^"]*\)".*/\1/')
  w=$(printf '%s\n' "$l" | sed 's/.*"width":\([0-9]*\).*/\1/')
  h=$(printf '%s\n' "$l" | sed 's/.*"height":\([0-9]*\).*/\1/')
  printf '{{"v":1,"units":"px","frame":{{"width":%s,"height":%s}},"granularity":"page","reading_order":"engine","blocks":[{{"bbox":[0,0,%s,%s],"kind":"text","text":"{line}"}}]}}' "$w" "$h" "$w" "$h" > "${{out}}boxes.json"
  printf '{line}\n' > "${{out}}page.txt"
  printf '{{"engine_stats":{{}},"layout":{{"kind":"boxes","path":"%sboxes.json"}},"markup":"none","run_id":"%s","status":"ok","text":{{"path":"%spage.txt"}},"type":"result","warnings":[]}}\n' "$out" "$run" "$out"
done
"#,
            HELLO.replace("CANCEL", "kill"),
            line = conformance::LINE,
        ),
    );
    let started = std::time::Instant::now();
    let r = conformance::run(
        options(adapter, Vec::new(), dir.path()),
        Path::new(FIXTURES),
    )
    .await
    .unwrap();
    assert_eq!(
        r.outcome("deadline"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
    assert!(
        r.cases
            .iter()
            .any(|c| c.case == "deadline" && c.detail.contains("killed")),
        "{}",
        outcomes(&r)
    );
    assert_eq!(
        r.outcome("text_line"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
    // The deadline, then the worker's 15 s grace.
    assert!(started.elapsed() >= Duration::from_secs(16));
}

/// With the real Tesseract, when it is installed with English.
#[tokio::test]
async fn the_real_tesseract_conforms() {
    let has_eng = std::process::Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "eng")
        });
    if !has_eng {
        eprintln!("tesseract with eng isn't installed; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let r = conformance::run(
        options(ADAPTER.into(), Vec::new(), dir.path()),
        Path::new(FIXTURES),
    )
    .await
    .unwrap();
    assert!(r.passed(), "{}", outcomes(&r));
}
