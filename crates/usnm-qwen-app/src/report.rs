//! The failure report a failed task sends through BOINC's stderr: bounded facts, and an
//! excerpt of `diag.txt` with the volunteer's paths redacted.
use std::{io::Write as _, path::Path};
use usnm_ocr_contract::qwen::ResultManifest;
use usnm_ocr_quality::slot::tail;

pub(crate) fn report_failure(work: &Path, report: &str) -> std::io::Result<()> {
    // Windows BOINC starts native apps without inherited stderr handles.
    #[cfg(windows)]
    {
        let mut stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(work.join("stderr.txt"))?;
        writeln!(stderr, "{report}")?;
        stderr.flush()
    }
    #[cfg(not(windows))]
    {
        let _ = work;
        writeln!(std::io::stderr().lock(), "{report}")
    }
}

/// The last `n` bytes of a log, redacted before the cut: a cut can then split only an
/// already-redacted `.../<file>`, never a name, and a long single-line message (an HTTP error
/// body) keeps its end.
pub(crate) fn redacted_tail(s: &str, n: usize) -> String {
    tail(&redact_paths(s), n).to_string()
}

/// Most of a failed task's diag.txt that its stderr carries. BOINC uploads no output files
/// for a failed task, so this is the only way the reason reaches the server.
pub(crate) const DIAG_EXCERPT_BYTES: usize = 6144;
const DIAG_HEAD_BYTES: usize = 2048;

/// Replaces absolute paths (`/home/user/...`, `C:\\Users\\...`, `~/...`, `\\\\server\\...`)
/// with `.../<file>`, so the excerpt names files without exposing a volunteer's name or
/// directory layout. Paths can hold spaces and brackets (`C:\\Users\\Alice Smith\\...`,
/// `Program Files (x86)`), so a path opened by a quote runs to the closing quote, and an
/// unquoted one runs through the last chunk on its line that has a path separator. This errs
/// toward redacting too much.
fn redact_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    out
}

fn path_starts_at(s: &str) -> bool {
    let b = s.as_bytes();
    let sep = |c: u8| c == b'/' || c == b'\\';
    if s.starts_with("~/") || s.starts_with("\\\\") {
        return true;
    }
    if b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && sep(b[2]) {
        return true;
    }
    // `/x/...`: a slash, a name, another slash (not `//` or a lone `/`).
    b.first() == Some(&b'/')
        && b.len() > 2
        && b[1] != b'/'
        && !b[1].is_ascii_whitespace()
        && s[1..]
            .split(char::is_whitespace)
            .next()
            .is_some_and(|t| t.contains('/'))
}

fn redact_line(line: &str) -> String {
    let is_boundary = |c: char| c.is_whitespace() || "\"'`=,([{<:;|".contains(c);
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut prev: Option<char> = None;
    while let Some((i, c)) = rest.char_indices().find(|&(i, c)| {
        let before = if i == 0 {
            prev
        } else {
            rest[..i].chars().next_back()
        };
        before.is_none_or(is_boundary)
            && (c == '/' || c == '~' || c == '\\' || c.is_ascii_alphabetic())
            && path_starts_at(&rest[i..])
    }) {
        let _ = c;
        out.push_str(&rest[..i]);
        let quote = if i == 0 {
            prev
        } else {
            rest[..i].chars().next_back()
        }
        .filter(|q| "\"'`".contains(*q));
        let tail = &rest[i..];
        let body_end = tail.find('\n').unwrap_or(tail.len());
        // Through the last whitespace-separated chunk that holds a separator.
        let mut sep_end = 0;
        let mut pos = 0;
        for chunk in tail[..body_end].split_inclusive(char::is_whitespace) {
            let word = chunk.trim_end();
            if word.contains(['/', '\\']) {
                sep_end = pos + word.len();
            }
            pos += chunk.len();
        }
        // A quoted path ends at the last matching quote on its line, not the first: names
        // like O'Connor hold the quote character. Whichever reaches further wins.
        let end = match quote.and_then(|q| tail[..body_end].rfind(q)) {
            Some(close) => close.max(sep_end),
            None => sep_end,
        };
        let path = &tail[..end];
        let name = path
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("");
        out.push_str(".../");
        out.push_str(name);
        prev = path.chars().next_back();
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The diag excerpt for a failed task's stderr: paths redacted, at most
/// `DIAG_EXCERPT_BYTES`, keeping the head (host, GPU and unit sections) and the tail (the
/// llama-server log and the reason the unit stopped).
pub(crate) fn diag_block(diag: &[u8]) -> String {
    let text = redact_paths(&String::from_utf8_lossy(diag));
    let body = if text.len() <= DIAG_EXCERPT_BYTES {
        text
    } else {
        let mut head = DIAG_HEAD_BYTES;
        while !text.is_char_boundary(head) {
            head -= 1;
        }
        let mut tail = text.len() - (DIAG_EXCERPT_BYTES - DIAG_HEAD_BYTES);
        while !text.is_char_boundary(tail) {
            tail += 1;
        }
        format!(
            "{}\n[... {} bytes omitted ...]\n{}",
            &text[..head],
            tail - head,
            &text[tail..]
        )
    };
    format!(
        "USNM_QWEN_DIAG_BEGIN\n{}\nUSNM_QWEN_DIAG_END",
        body.trim_end()
    )
}

pub(crate) fn failure_report(result: &ResultManifest, diag: &[u8]) -> String {
    let evidence = String::from_utf8_lossy(diag).to_ascii_lowercase();
    let short = |s: Option<&str>| {
        s.map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || "._- ".contains(*c))
                .take(64)
                .collect::<String>()
        })
    };
    let digest = |s: &str| {
        s.strip_prefix("sha256:")
            .filter(|s| usnm_ocr_contract::boinc::is_sha256(s))
            .map(|s| format!("sha256:{s}"))
    };
    let report = serde_json::json!({
        "v": 1, "exit_status": 1, "outcome": result.outcome,
        "device": short(Some(&result.device)), "model": short(result.model.as_deref()),
        "llama": digest(&result.engine.llama), "weights": digest(&result.engine.weights),
        "runtime": short(result.host.runtime.as_deref()),
        "plan_class": short(result.host.plan_class.as_deref()),
        "gpu_driver": short(result.host.gpu_driver.as_deref()),
        "gpu_memory_mib": result.host.gpu_mem_total_mib,
        "gpu_peak_mib": result.gpu_mem_peak_mib, "peak_rss_kib": result.peak_rss_kib,
        "load_ms": result.load_ms, "engine_ms": result.engine_ms,
        "runtime_missing": evidence.contains("no such file") || evidence.contains("os error 2"),
        "access_denied": evidence.contains("permission denied") || evidence.contains("os error 5"),
        "cuda_error": evidence.contains("cuda error"),
        "out_of_memory": evidence.contains("out of memory"),
        "runtime_exited": evidence.contains("exited"),
        "diag_sha256": result.diag_sha256,
    });
    // Failed BOINC tasks need not upload declared outputs. Only this fixed-schema
    // summary travels via stderr; never include prompts, readings or raw paths.
    format!("USNM_QWEN_FAILURE {report}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_redaction_covers_spaces_brackets_and_quotes() {
        let cases = [
            (
                "model C:\\Users\\Alice Smith\\AppData\\BOINC\\llama-server.exe failed",
                "model .../llama-server.exe failed",
            ),
            (
                "open \"C:\\Users\\Alice Smith\\x y\\Qwen3VL-8B.gguf\": access denied",
                "open \".../Qwen3VL-8B.gguf\": access denied",
            ),
            (
                "C:\\Program Files (x86)\\BOINC\\boinc.exe started",
                ".../boinc.exe started",
            ),
            (
                "spawning /home/alice smith/boinc/llama-server failed: os error 2",
                "spawning .../llama-server failed: os error 2",
            ),
            ("cwd='/Users/bob/Library/BOINC Data/slots/3'", "cwd='.../3'"),
            (
                "GET http://127.0.0.1:8080/health ok; a/b stays",
                "GET http://127.0.0.1:8080/health ok; a/b stays",
            ),
            ("\\\\nas\\share\\Alice\\f.txt", ".../f.txt"),
            (
                "failed to load 'C:\\Users\\O'Connor\\BOINC\\model.gguf' now",
                "failed to load '.../model.gguf' now",
            ),
            (
                "open \"/home/d'arcy/BOINC data/x.gguf\": denied",
                "open \".../x.gguf\": denied",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact_paths(input), want, "{input}");
        }
        let two = redact_paths("a /home/alice/x\nb C:\\Users\\Bob Jones\\y.gguf\n");
        assert_eq!(two, "a .../x\nb .../y.gguf\n");
        assert!(!two.contains("alice") && !two.contains("Bob"));
    }

    #[test]
    fn log_tails_are_redacted_before_the_cut() {
        let log = "load C:\\Users\\Alice Smith\\AppData\\BOINC\\model.gguf\nready\n";
        // Any cut through the path line leaves no part of the name.
        for n in 1..log.len() {
            let t = redacted_tail(log, n);
            assert!(
                !t.contains("Alice") && !t.contains("Smith") && !t.contains("AppData"),
                "{n}: {t}"
            );
        }
        // A long single-line error body keeps its end.
        let body = format!(
            "{{\"error\":{{\"message\":\"{}model too large for /home/bob/x/llama.gguf\"}}}}",
            "x".repeat(400)
        );
        let t = redacted_tail(&body, 256);
        assert!(t.ends_with("model too large for .../llama.gguf\"}}"), "{t}");
        assert!(!t.contains("bob"));
    }

    #[test]
    fn diag_excerpt_redacts_paths_and_keeps_head_and_tail() {
        let text = "spawning /home/alice/boinc/slots/3/llama-server failed: os error 2\n\
                    model C:\\Users\\bob\\AppData\\BOINC\\Qwen3VL-8B.gguf (\"~/x/y.txt\")\n\
                    GET http://127.0.0.1:8080/health ok; relative a/b stays";
        let r = redact_paths(text);
        assert!(r.contains("spawning .../llama-server failed"), "{r}");
        // Two paths on one line collapse to the last file name: redacting more is the safe side.
        assert!(r.contains("model .../y.txt\")"), "{r}");
        assert!(
            r.contains("http://127.0.0.1:8080/health") && r.contains("a/b stays"),
            "{r}"
        );
        assert!(!r.contains("alice") && !r.contains("bob"));

        let short = diag_block(b"== gpu\nno hardware Vulkan device\n");
        assert_eq!(
            short,
            "USNM_QWEN_DIAG_BEGIN\n== gpu\nno hardware Vulkan device\nUSNM_QWEN_DIAG_END"
        );

        let long = format!("HEAD-MARK{}TAIL-MARK", "x".repeat(20_000));
        let block = diag_block(long.as_bytes());
        assert!(block.len() <= DIAG_EXCERPT_BYTES + 128);
        assert!(block.contains("HEAD-MARK") && block.contains("TAIL-MARK"));
        assert!(block.contains("bytes omitted"));
        // Multi-byte text is cut on character boundaries.
        let wide = "é".repeat(10_000);
        assert!(diag_block(wide.as_bytes()).len() <= DIAG_EXCERPT_BYTES + 128);
    }
}
