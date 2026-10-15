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
/// corrupt fixture fails as Leptonica does, a large page is slow, the
/// blank page has no text, and any other page reads as `text` (a shell
/// command's output).
fn stand_in(dir: &Path, text: &str) -> PathBuf {
    let blank = blank_png(dir);
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
case "$1" in *.png) ;; *) echo "not a PNG: $1" >&2; exit 1 ;; esac
if {large}; then sleep 30; fi
if cmp -s "$1" '{blank}'; then exit 0; fi
{text}
"#,
            tessdata.display(),
            large = is_large("1"),
            blank = blank.display(),
        ),
    )
}

/// The suite's blank page as it sends it, so a stand-in can tell it by
/// its bytes: its pages are PNGs (13 §13.4.1), with no raw pixels to read.
fn blank_png(dir: &Path) -> PathBuf {
    let (w, h) = usnm_ocr_worker::testimg::lines_size(&[conformance::LINE]);
    let path = dir.join("blank.png");
    let png = usnm_ocr_worker::testimg::png(&usnm_ocr_worker::testimg::blank_image(w, h));
    std::fs::write(&path, png).unwrap();
    path
}

/// The shell for "the PNG `$1` (a variable) is the suite's large page":
/// its `IHDR` width is over 3,000 px.
fn is_large(var: &str) -> String {
    format!(
        r#"[ "$(od -An -j16 -N4 -tu1 "${var}" | awk 'NF{{print $1*16777216+$2*65536+$3*256+$4}}')" -gt 3000 ]"#
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
        "protocol",
        "stdin_eof",
    ] {
        assert_eq!(r.outcome(case), Some(Outcome::Pass), "{}", outcomes(&r));
    }
    assert_eq!(r.outcome("deadline_cgroup"), Some(Outcome::Skipped));
    assert_eq!(r.engine, "tesseract-5.5 5.5.9");
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

/// A stand-in adapter (`cancel = adapter`) that answers with a
/// page-granularity `boxes.json`: `unsupported` for any profile but `eng`,
/// `bad_input` for the corrupt fixture. For the deadline case's large page
/// it runs `big` (shell), which answers `timeout` by default; one that sets
/// `$big` instead reads the page as blank. For the other pages `page`
/// (shell) sets `$text`, the block's text, and `$sidecar`, the text file's
/// (`-` for none), from `$blank` (1 for the suite's blank page); it may
/// set `$blocks` to replace the blocks.
fn boxes_adapter(dir: &Path, page: &str, big: Option<&str>) -> PathBuf {
    let big = big.unwrap_or("fail timeout timeout; continue");
    let blank = blank_png(dir);
    script(
        dir,
        "adapter",
        &format!(
            r#"#!/bin/sh
echo '{hello}'
while IFS= read -r l; do
  field() {{ printf '%s\n' "$l" | sed "s/.*\"$1\":\"\([^\"]*\)\".*/\1/"; }}
  num() {{ printf '%s\n' "$l" | sed "s/.*\"$1\":\([0-9]*\).*/\1/"; }}
  run=$(field run_id); out=$(field out); img=$(field path); profile=$(field profile)
  w=$(num width); h=$(num height)
  fail() {{ printf '{{"engine_stats":{{}},"error":{{"kind":"%s","message":"%s"}},"markup":"none","run_id":"%s","status":"%s","type":"result","warnings":[]}}\n' "$2" "$2" "$run" "$1"; }}
  if [ "$profile" != eng ]; then fail unsupported profile; continue; fi
  if grep -q badcrc "$img"; then fail bad_input image; continue; fi
  big=
  case "$l" in *'"width":3500'*) {big} ;; esac
  case "$img" in *.png) ;; *) fail failed not_png; continue ;; esac
  if [ -n "$big" ] || cmp -s "$img" '{blank_png}'; then blank=1; else blank=0; fi
  blocks=
  {page}
  [ -n "$blocks" ] || blocks=$(printf '{{"bbox":[0,0,%s,%s],"kind":"text","text":"%s"}}' "$w" "$h" "$text")
  printf '{{"v":1,"units":"px","frame":{{"width":%s,"height":%s}},"granularity":"page","reading_order":"engine","blocks":[%s]}}' "$w" "$h" "$blocks" > "${{out}}boxes.json"
  if [ "$sidecar" = - ]; then
    printf '{{"engine_stats":{{}},"layout":{{"kind":"boxes","path":"%sboxes.json"}},"markup":"none","run_id":"%s","status":"ok","type":"result","warnings":[]}}\n' "$out" "$run"
  else
    printf '%s\n' "$sidecar" > "${{out}}page.txt"
    printf '{{"engine_stats":{{}},"layout":{{"kind":"boxes","path":"%sboxes.json"}},"markup":"none","run_id":"%s","status":"ok","text":{{"path":"%spage.txt"}},"type":"result","warnings":[]}}\n' "$out" "$run" "$out"
  fi
done
"#,
            hello = HELLO.replace("CANCEL", "adapter"),
            blank_png = blank.display(),
        ),
    )
}

async fn boxes_report(page: &str) -> Report {
    boxes_report_with(page, None).await
}

async fn boxes_report_with(page: &str, big: Option<&str>) -> Report {
    let dir = tempfile::tempdir().unwrap();
    let adapter = boxes_adapter(dir.path(), page, big);
    conformance::run(
        options(adapter, Vec::new(), dir.path()),
        Path::new(FIXTURES),
    )
    .await
    .unwrap()
}

/// The cases check the text the worker stores, from the layout: an
/// adapter that writes no text file conforms.
#[tokio::test]
async fn a_layout_only_adapter_conforms() {
    let r = boxes_report(&format!(
        r#"if [ $blank = 1 ]; then text=; else text='{}'; fi; sidecar=-"#,
        conformance::LINE
    ))
    .await;
    assert!(r.passed(), "{}", outcomes(&r));
    assert_eq!(r.outcome("text_line"), Some(Outcome::Pass));
    assert_eq!(r.outcome("blank_page"), Some(Outcome::Pass));
    assert_eq!(r.outcome("deadline"), Some(Outcome::Pass));
}

/// An engine that reads the deadline case's page within its deadline
/// leaves nothing to cancel: the case is skipped, not failed. One that
/// answers after the deadline fails.
#[tokio::test]
async fn a_fast_read_skips_the_deadline_and_a_late_one_fails() {
    let page = format!(
        r#"if [ $blank = 1 ]; then text=; else text='{}'; fi; sidecar=-"#,
        conformance::LINE
    );
    let r = boxes_report_with(&page, Some("big=1")).await;
    assert_eq!(
        r.outcome("deadline"),
        Some(Outcome::Skipped),
        "{}",
        outcomes(&r)
    );
    assert!(r.passed(), "{}", outcomes(&r));
    let r = boxes_report_with(&page, Some("big=1; sleep 2")).await;
    assert_eq!(
        r.outcome("deadline"),
        Some(Outcome::Fail),
        "{}",
        outcomes(&r)
    );
    assert!(
        r.cases
            .iter()
            .any(|c| c.case == "deadline" && c.detail.contains("read after")),
        "{}",
        outcomes(&r)
    );
}

/// A right text file doesn't hide a wrong layout, nor an empty one text in
/// a blank page's layout.
#[tokio::test]
async fn a_layout_contradicting_its_text_file_fails() {
    let r = boxes_report(&format!(
        r#"if [ $blank = 1 ]; then text=NOISE; sidecar=; else text='HELLO WORLO'; sidecar='{}'; fi"#,
        conformance::LINE
    ))
    .await;
    for case in ["text_line", "blank_page", "deadline"] {
        assert_eq!(
            r.outcome(case),
            Some(Outcome::Fail),
            "{case}\n{}",
            outcomes(&r)
        );
    }
    assert_eq!(
        r.outcome("protocol"),
        Some(Outcome::Pass),
        "{}",
        outcomes(&r)
    );
}

/// A layout over the boxes' limits (20,001 blocks, in about 1 MiB: far
/// under the 64 MiB file cap) is `engine_protocol`: the case fails, and
/// the instance is replaced for the next page.
#[tokio::test]
async fn a_layout_over_its_limits_is_caught() {
    let r = boxes_report(&format!(
        r#"text='{}'; sidecar=-
  if [ $blank = 1 ]; then blocks=$(awk 'BEGIN{{for(i=0;i<20001;i++)printf "%s{{\"bbox\":[0,0,1,1],\"kind\":\"text\",\"text\":\"\"}}", (i?",":"")}}'); fi"#,
        conformance::LINE
    ))
    .await;
    assert_eq!(
        r.outcome("oversized_output"),
        Some(Outcome::Fail),
        "{}",
        outcomes(&r)
    );
    assert!(
        r.cases
            .iter()
            .any(|c| c.case == "blank_page" && c.detail.contains("instance replaced")),
        "{}",
        outcomes(&r)
    );
    // Neither noise nor another protocol failure, and the pages after it
    // were read by a new instance.
    for case in [
        "stdout_noise",
        "protocol",
        "text_line",
        "corrupt_image",
        "deadline",
    ] {
        assert_eq!(
            r.outcome(case),
            Some(Outcome::Pass),
            "{case}\n{}",
            outcomes(&r)
        );
    }
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
