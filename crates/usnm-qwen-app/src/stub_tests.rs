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
