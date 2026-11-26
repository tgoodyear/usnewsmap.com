//! The whole task loop against a stand-in `llama-server`: a small Python HTTP server the client
//! launches itself, found as `llama-server` in the slot, as it finds the real one. The stub reads
//! the first crop normally, then dies in the middle of the second crop's request. What the
//! restarted copies answer is the case under test (#1919).
use super::*;
use crate::test_support::{qwen_models, write_qwen_unit};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use usnm_ocr_contract::qwen::Readings;

/// `__STATE__` is a directory counting launches; `__RESTARTED__` is the stub's reply to a crop
/// once it has been restarted: `blank` (HTTP 200, empty text, 0 tokens) or `text`.
const STUB: &str = r#"#!/usr/bin/env python3
import http.server, json, os, sys, time
state, restarted = "__STATE__", "__RESTARTED__"
port = int(sys.argv[sys.argv.index("--port") + 1])
launch_file = os.path.join(state, "launches")
n = int(open(launch_file).read()) + 1 if os.path.exists(launch_file) else 1
open(launch_file, "w").write(str(n))
served = 0

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Length", "2"); self.end_headers()
        self.wfile.write(b"ok")
    def do_POST(self):
        global served
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        served += 1
        if n == 1 and served == 2:
            time.sleep(0.2)
            os._exit(1)  # the GPU reset: gone before any reply
        text, tokens = ("first reading", 3) if n == 1 else (("" if restarted == "blank" else "real text"), 0 if restarted == "blank" else 4)
        if b'"stream":true' in body:
            ev = {"choices": [{"delta": {"content": text}, "finish_reason": "stop"}], "usage": {"completion_tokens": tokens}}
            out = ("data: %s\n\ndata: [DONE]\n\n" % json.dumps(ev)).encode()
            ctype = "text/event-stream"
        else:
            out = json.dumps({"choices": [{"message": {"content": text}, "finish_reason": "stop"}], "usage": {"completion_tokens": tokens}}).encode()
            ctype = "application/json"
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(out))); self.send_header("Connection", "close"); self.end_headers()
        self.wfile.write(out)

http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
"#;

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A staged CPU unit of two crops whose `llama-server` is the stub.
fn staged(restarted: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let slot = tmp.path().join("slot");
    let state = tmp.path().join("stub-state");
    std::fs::create_dir_all(&state).unwrap();
    write_qwen_unit(&slot);
    for m in qwen_models() {
        std::fs::write(slot.join(&m.file), crate::test_support::FAKE_GGUF).unwrap();
        std::fs::write(slot.join(&m.mmproj), crate::test_support::FAKE_MMPROJ).unwrap();
    }
    std::fs::write(
        slot.join("init_data.xml"),
        "<app_init_data><plan_class>mt</plan_class></app_init_data>",
    )
    .unwrap();
    let script = STUB
        .replace("__STATE__", &state.to_string_lossy())
        .replace("__RESTARTED__", restarted);
    let bin = slot.join("llama-server");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (tmp, slot, state)
}

/// A stand-in that refuses the first crop with `__STATUS__` (llama-server's reply to a request
/// past its context is a 400 `exceed_context_size_error`) and reads the second normally.
const REFUSE_STUB: &str = r#"#!/usr/bin/env python3
import http.server, json, os, sys
state, status = "__STATE__", int("__STATUS__")
port = int(sys.argv[sys.argv.index("--port") + 1])
launch_file = os.path.join(state, "launches")
n = int(open(launch_file).read()) + 1 if os.path.exists(launch_file) else 1
open(launch_file, "w").write(str(n))
served = 0

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Length", "2"); self.end_headers()
        self.wfile.write(b"ok")
    def do_POST(self):
        global served
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        served += 1
        if served == 1:
            out = json.dumps({"error": {"code": status, "message": "request (4099 tokens) exceeds the available context size (4096 tokens)", "type": "exceed_context_size_error"}}).encode()
            self.send_response(status); self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(out))); self.send_header("Connection", "close"); self.end_headers()
            self.wfile.write(out)
            return
        text, tokens = "second reading", 3
        if b'"stream":true' in body:
            ev = {"choices": [{"delta": {"content": text}, "finish_reason": "stop"}], "usage": {"completion_tokens": tokens}}
            out = ("data: %s\n\ndata: [DONE]\n\n" % json.dumps(ev)).encode()
            ctype = "text/event-stream"
        else:
            out = json.dumps({"choices": [{"message": {"content": text}, "finish_reason": "stop"}], "usage": {"completion_tokens": tokens}}).encode()
            ctype = "application/json"
        self.send_response(200); self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(out))); self.send_header("Connection", "close"); self.end_headers()
        self.wfile.write(out)

http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
"#;

/// [`staged`], with [`REFUSE_STUB`] answering `status` to the first crop.
fn staged_refusing(status: u16) -> (tempfile::TempDir, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let (tmp, slot, state) = staged("text");
    let script = REFUSE_STUB
        .replace("__STATE__", &state.to_string_lossy())
        .replace("__STATUS__", &status.to_string());
    let bin = slot.join("llama-server");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (tmp, slot, state)
}

#[test]
fn a_crop_the_server_refuses_fails_alone_and_the_unit_goes_on() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged_refusing(400);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    let result: ResultManifest =
        serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
    assert_eq!(result.outcome, Outcome::Ok);
    let readings: Readings =
        serde_json::from_slice(&std::fs::read(slot.join(READINGS_FILE)).unwrap()).unwrap();
    // The refused crop has no reading (so it can't count as read, let alone agree); the
    // next crop is read, on the same server, and the refusal is in the diagnostics.
    let texts: Vec<_> = readings.readings.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["second reading"]);
    assert_eq!(readings.diag["crop_errors"], "1");
    assert!(!readings.diag["crop_error_ids"].is_empty());
    assert!(!readings
        .readings
        .iter()
        .any(|r| readings.diag["crop_error_ids"]
            .split(',')
            .any(|id| id == r.id)));
    assert_eq!(launches(&state), 1);
    assert!(!slot.join(CHECKPOINT_FILE).exists());
}

#[test]
fn a_server_error_still_fails_the_unit() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, _state) = staged_refusing(500);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    let result: ResultManifest =
        serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
}

fn launches(state: &Path) -> u32 {
    std::fs::read_to_string(state.join("launches"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn a_restarted_server_that_answers_blank_fails_the_unit_and_uploads_no_result() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged("blank");
    // Each run reads c01 from the checkpoint, loses the server in c02, and restarts it twice;
    // both restarts answer blank 200s. The client asks to be restarted until its budget is spent.
    for exit in 1..=MAX_TEMPORARY_EXITS {
        run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
        let t = std::fs::read_to_string(slot.join(TEMPORARY_EXIT_FILE)).unwrap();
        assert!(
            t.contains(&format!("({exit}/{MAX_TEMPORARY_EXITS})")),
            "{t}"
        );
        assert!(!slot.join(RESULT_FILE).exists(), "no result on exit {exit}");
        assert!(!slot.join(READINGS_FILE).exists());
        assert!(!slot.join(FINISH_FILE).exists());
    }
    // Budget spent: the unit fails, so BOINC reports an error and the server reissues it.
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    assert_eq!(
        std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
        "1\n"
    );
    let result: ResultManifest =
        serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
    assert_eq!(result.outcome, Outcome::Failed);
    let readings: Readings =
        serde_json::from_slice(&std::fs::read(slot.join(READINGS_FILE)).unwrap()).unwrap();
    // Only the crop read before the crash; the blank one was never recorded.
    assert_eq!(readings.readings.len(), 1);
    assert_eq!(readings.readings[0].text, "first reading");
    assert!(readings
        .readings
        .iter()
        .all(|r| !r.text.is_empty() && r.tokens > 0));
    assert!(launches(&state) >= 5, "launches: {}", launches(&state));
}

#[test]
fn a_restarted_server_that_answers_with_text_finishes_the_unit() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged("text");
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
    assert_eq!(
        std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
        "0\n"
    );
    let result: ResultManifest =
        serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
    assert_eq!(result.outcome, Outcome::Ok);
    let readings: Readings =
        serde_json::from_slice(&std::fs::read(slot.join(READINGS_FILE)).unwrap()).unwrap();
    let texts: Vec<_> = readings.readings.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["first reading", "real text"]);
    assert_eq!(launches(&state), 2);
}

/// A stand-in for page mode. `__MODE__` is `ok` (every request answers after 0.3 s, and the most
/// requests in flight at once is kept), `oom` (a launch with more than 2 slots dies out of
/// memory before it is ready), `crash_blank` or `crash_text` (the first launch dies with
/// requests in flight; the later ones answer blank or with text).
const PAGE_STUB: &str = r#"#!/usr/bin/env python3
import http.server, json, os, sys, threading, time
state, mode = "__STATE__", "__MODE__"
port = int(sys.argv[sys.argv.index("--port") + 1])
parallel = int(sys.argv[sys.argv.index("--parallel") + 1])
launch_file = os.path.join(state, "launches")
n = int(open(launch_file).read()) + 1 if os.path.exists(launch_file) else 1
open(launch_file, "w").write(str(n))
open(os.path.join(state, "args"), "a").write(" ".join(sys.argv[1:]) + "\n")
if mode == "oom" and parallel > 2:
    sys.stderr.write("ggml_vulkan: Device memory allocation failed: out of memory\n")
    sys.exit(1)
lock = threading.Lock()
flight = 0
served = 0

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_GET(self):
        self.send_response(200); self.send_header("Content-Length", "2"); self.end_headers()
        self.wfile.write(b"ok")
    def do_POST(self):
        global flight, served
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        with lock:
            flight += 1
            served += 1
            mine = served
            peak_file = os.path.join(state, "peak")
            peak = int(open(peak_file).read()) if os.path.exists(peak_file) else 0
            if flight > peak:
                open(peak_file, "w").write(str(flight))
        if mode.startswith("crash") and n == 1 and mine == 3:
            time.sleep(0.3)
            os._exit(1)
        time.sleep(0.3)
        blank = mode == "crash_blank" and n > 1
        text, tokens = ("", 0) if blank else ("region text", 4)
        out = json.dumps({"choices": [{"message": {"content": text}, "finish_reason": "stop"}], "usage": {"completion_tokens": tokens}}).encode()
        with lock:
            flight -= 1
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(out))); self.send_header("Connection", "close"); self.end_headers()
        self.wfile.write(out)

http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
"#;

/// A staged page-mode unit of `regions` regions whose `llama-server` is [`PAGE_STUB`].
fn staged_page(mode: &str, regions: usize, slots: u32) -> (tempfile::TempDir, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let slot = tmp.path().join("slot");
    let state = tmp.path().join("stub-state");
    std::fs::create_dir_all(&state).unwrap();
    crate::test_support::write_page_unit(&slot, regions, Some(slots));
    std::fs::write(
        slot.join(crate::test_support::xiaomi_model().file),
        crate::test_support::FAKE_GGUF,
    )
    .unwrap();
    std::fs::write(
        slot.join(crate::test_support::xiaomi_model().mmproj),
        crate::test_support::FAKE_MMPROJ,
    )
    .unwrap();
    std::fs::write(
        slot.join("init_data.xml"),
        "<app_init_data><plan_class>mt_x08</plan_class></app_init_data>",
    )
    .unwrap();
    let script = PAGE_STUB
        .replace("__STATE__", &state.to_string_lossy())
        .replace("__MODE__", mode);
    let bin = slot.join("llama-server");
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    (tmp, slot, state)
}

fn read_state(state: &Path, name: &str) -> String {
    std::fs::read_to_string(state.join(name)).unwrap_or_default()
}

fn page_readings(slot: &Path) -> Readings {
    serde_json::from_slice(&std::fs::read(slot.join(READINGS_FILE)).unwrap()).unwrap()
}

#[test]
fn page_mode_reads_regions_concurrently_on_one_server() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged_page("ok", 9, 3);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    assert_eq!(
        std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
        "0\n"
    );
    // One server, with three slots sharing a context three slots wide.
    assert_eq!(launches(&state), 1);
    let args = read_state(&state, "args");
    assert!(args.contains("--parallel 3 "), "{args}");
    assert!(args.contains("--ctx-size 24576 "), "{args}");
    assert!(args.contains("--cont-batching"), "{args}");
    // Three requests were in flight together, and never more.
    assert_eq!(read_state(&state, "peak"), "3");
    let readings = page_readings(&slot);
    assert_eq!(readings.outcome, Outcome::Ok);
    // All nine regions, reported in the unit's order whatever order they finished in.
    let ids: Vec<_> = readings.readings.iter().map(|r| r.id.clone()).collect();
    let want: Vec<_> = (0..9).map(|i| format!("r{i:03}")).collect();
    assert_eq!(ids, want);
    assert!(readings.readings.iter().all(|r| r.text == "region text"));
    for (k, v) in [
        ("slots", "3"),
        ("ctx_size", "24576"),
        ("page_mode", "1"),
        ("slots_forced", "true"),
    ] {
        assert_eq!(readings.diag[k], v, "{k}");
    }
    // The page's GPU time is the unit's whole load and read time.
    let result: ResultManifest =
        serde_json::from_slice(&std::fs::read(slot.join(RESULT_FILE)).unwrap()).unwrap();
    let ms: u64 = readings.diag["gpu_ms_by_page"]
        .strip_prefix("0=")
        .unwrap()
        .parse()
        .unwrap();
    // Starting the server is timed from before the launch, and the read is timed a moment
    // before the manifest's, so the two differ by a few milliseconds either way.
    let manifest = result.load_ms + result.engine_ms;
    assert!(ms + 100 >= manifest, "{ms} against {manifest}");
    assert!(ms < result.load_ms + result.engine_ms + 3000, "{ms}");
    // Three at a time over nine 0.3 s requests is well under nine in a row.
    assert!(result.engine_ms < 2400, "{}", result.engine_ms);
    let diag = std::fs::read_to_string(slot.join(DIAG_FILE)).unwrap();
    assert!(diag.contains("r008 block=P1_TB00008"), "{diag}");
    // The result passes the server's own checks.
    result
        .check(
            &std::fs::read(slot.join(READINGS_FILE)).unwrap(),
            diag.as_bytes(),
        )
        .unwrap();
}

#[test]
fn page_mode_halves_the_slots_when_the_server_runs_out_of_memory() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged_page("oom", 4, 8);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    // 8 slots fail, 4 fail, 2 start.
    assert_eq!(launches(&state), 3);
    let args: Vec<_> = read_state(&state, "args")
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    let parallel = |l: &str| {
        let w: Vec<_> = l.split(' ').collect();
        w[w.iter().position(|a| *a == "--parallel").unwrap() + 1].to_owned()
    };
    assert_eq!(
        args.iter().map(|l| parallel(l)).collect::<Vec<_>>(),
        ["8", "4", "2"]
    );
    assert!(args[2].contains("--ctx-size 16384"), "{}", args[2]);
    let readings = page_readings(&slot);
    assert_eq!(readings.outcome, Outcome::Ok);
    assert_eq!(readings.readings.len(), 4);
    assert_eq!(readings.diag["slots"], "2");
    assert_eq!(readings.diag["slots_planned"], "8");
    assert_eq!(readings.diag["slot_halvings"], "2");
    assert_eq!(readings.diag["ctx_size"], "16384");
    assert_eq!(read_state(&state, "peak"), "2");
}

#[test]
fn page_mode_counts_a_blank_reply_after_a_crash_as_a_failed_attempt() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged_page("crash_blank", 6, 3);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    // The server died with requests in flight and every replacement answered blank 200s: no
    // blank reading was kept, no result was written, and the client was asked to restart the task.
    let t = std::fs::read_to_string(slot.join(TEMPORARY_EXIT_FILE)).unwrap();
    assert!(t.contains(&format!("(1/{MAX_TEMPORARY_EXITS})")), "{t}");
    assert!(!slot.join(RESULT_FILE).exists());
    assert!(!slot.join(READINGS_FILE).exists());
    assert!(!slot.join(FINISH_FILE).exists());
    assert!(launches(&state) >= 2, "launches: {}", launches(&state));
    let cp: crate::checkpoint::QwenCheckpoint =
        serde_json::from_slice(&std::fs::read(slot.join(crate::CHECKPOINT_FILE)).unwrap()).unwrap();
    assert!(cp
        .readings
        .iter()
        .all(|r| !r.text.is_empty() && r.tokens > 0));
    assert!(cp.readings.len() < 6);
    // The run's GPU time is kept for the cost of the unit across restarts.
    assert!(cp.gpu_ms > 0);
    assert_eq!(cp.run_ms, 0);
}

#[test]
fn page_mode_finishes_after_a_crash_when_the_restarted_server_answers() {
    if !python_available() {
        eprintln!("skipped: no python3");
        return;
    }
    let (_tmp, slot, state) = staged_page("crash_text", 6, 3);
    run_slot_with(&slot, None, Some(Duration::from_secs(120))).unwrap();
    assert!(!slot.join(TEMPORARY_EXIT_FILE).exists());
    assert_eq!(
        std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
        "0\n"
    );
    // The server was started again once, however many requests it dropped.
    assert_eq!(launches(&state), 2);
    let readings = page_readings(&slot);
    assert_eq!(readings.readings.len(), 6);
    assert!(readings.readings.iter().all(|r| r.text == "region text"));
}
