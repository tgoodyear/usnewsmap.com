//! The client-side runner (`usnm-ocr-run`): reads a workunit's manifest, runs the static
//! Tesseract on each page and writes `result.json`. BOINC's generic wrapper starts it.

use crate::result::{sha256_hex, Engine, PageResult, ResultFile};
use crate::workgen::WorkunitManifest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// BOINC puts a small link file in the slot directory for each input that lives in the
/// project directory (and for remote files). The link is XML:
/// `<soft_link>../../projects/URL/jf_MD5</soft_link>`. Apps that do not use the BOINC API
/// must resolve it themselves. A file without that wrapper is the file itself.
pub fn resolve_link(slot_dir: &Path, name: &str, content_head: &str) -> PathBuf {
    let t = content_head.trim();
    if let Some(rest) = t.strip_prefix("<soft_link>") {
        if let Some(target) = rest.split("</soft_link>").next() {
            return slot_dir.join(target.trim());
        }
    }
    slot_dir.join(name)
}

/// Tesseract language string for a page. The pilot profile names the traineddata set;
/// `default` falls back to the language. UNVERIFIED mapping: the exact `-l` strings of the
/// pinned engine (for example `deu_latf+deu`) must be read from the engine contract.
pub fn tesseract_lang(language: &str, profile: &str) -> String {
    if profile == "default" {
        language.to_string()
    } else {
        profile.to_string()
    }
}

/// Command line for one page: text to stdout, one thread, fixed layout mode.
pub fn tesseract_args(image: &Path, tessdata_dir: &Path, lang: &str) -> Vec<String> {
    vec![
        image.display().to_string(),
        "stdout".into(),
        "--tessdata-dir".into(),
        tessdata_dir.display().to_string(),
        "-l".into(),
        lang.into(),
        "--psm".into(),
        "1".into(),
    ]
}

pub fn page_result(page_id: &str, input_md5: &str, text: String) -> PageResult {
    PageResult {
        alto: None,
        blank: text.trim().is_empty(),
        input_md5: input_md5.into(),
        page_id: page_id.into(),
        source: None,
        text_sha256: sha256_hex(text.as_bytes()),
        text,
    }
}

/// Longest time, in seconds, a page may run before the client checks for a stop request.
/// Documented limit, not enforced here: one page takes about 60 to 250 s on one thread, and an
/// Azure Spot VM gets a 30 s eviction notice, so a stop can lose at most the page in progress.
pub const CHECKPOINT_UNIT: &str = "page";

fn checkpoint_file(dir: &Path, index: usize) -> PathBuf {
    dir.join(format!("page{index}.json"))
}

/// Save one finished page. The file is written beside its name, flushed, then renamed, so a
/// kill at any point leaves either the whole page or nothing under the final name.
pub fn save_checkpoint(dir: &Path, index: usize, page: &PageResult) -> Result<(), String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!("page{index}.json.tmp"));
    let json = serde_json::to_vec(page).map_err(|e| e.to_string())?;
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(&json).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, checkpoint_file(dir, index)).map_err(|e| e.to_string())
}

/// The pages already finished, in order. Reading stops at the first page that is missing,
/// unreadable or for a different page id than the manifest lists (a stale checkpoint from
/// another workunit), so the run redoes that page and the ones after it.
pub fn load_checkpoints(dir: &Path, page_ids: &[&str]) -> Vec<PageResult> {
    let mut done = Vec::new();
    for (i, id) in page_ids.iter().enumerate() {
        let Ok(bytes) = std::fs::read(checkpoint_file(dir, i)) else {
            break;
        };
        match serde_json::from_slice::<PageResult>(&bytes) {
            Ok(p) if p.page_id == *id && p.text_sha256 == sha256_hex(p.text.as_bytes()) => {
                done.push(p)
            }
            _ => break,
        }
    }
    done
}

pub fn build_result(
    m: &WorkunitManifest,
    engine_version: &str,
    traineddata: BTreeMap<String, String>,
    pages: Vec<PageResult>,
) -> ResultFile {
    ResultFile {
        engine: Engine {
            name: "tesseract".into(),
            traineddata,
            version: engine_version.into(),
        },
        pages,
        schema: 1,
        wu: m.wu.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::{check, Config};

    #[test]
    fn soft_link_files_are_resolved_and_plain_files_are_not() {
        let slot = Path::new("/slot/0");
        let p = resolve_link(
            slot,
            "page0.jp2",
            "<soft_link>../../projects/x/jf_abc</soft_link>\n",
        );
        assert_eq!(p, Path::new("/slot/0/../../projects/x/jf_abc"));
        let p = resolve_link(slot, "page0.jp2", "\u{0}JP2 binary");
        assert_eq!(p, Path::new("/slot/0/page0.jp2"));
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("usnm-ckpt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn checkpoints_resume_in_order_and_stop_at_a_gap() {
        let d = temp("order");
        let ids = ["a", "b", "c"];
        assert!(load_checkpoints(&d, &ids).is_empty());
        save_checkpoint(&d, 0, &page_result("a", "m", "eins".into())).unwrap();
        save_checkpoint(&d, 1, &page_result("b", "m", "zwei".into())).unwrap();
        save_checkpoint(&d, 3, &page_result("c", "m", "drei".into())).unwrap();
        let done = load_checkpoints(&d, &ids);
        assert_eq!(done.len(), 2);
        assert_eq!(done[1].text, "zwei");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_stale_torn_or_edited_checkpoint_is_redone() {
        let d = temp("bad");
        let ids = ["a", "b"];
        save_checkpoint(&d, 0, &page_result("other-wu", "m", "x".into())).unwrap();
        assert!(load_checkpoints(&d, &ids).is_empty(), "wrong page id");
        save_checkpoint(&d, 0, &page_result("a", "m", "eins".into())).unwrap();
        std::fs::write(d.join("page1.json"), b"{\"alto\":").unwrap();
        assert_eq!(load_checkpoints(&d, &ids).len(), 1, "torn file");
        let mut p = page_result("b", "m", "zwei".into());
        p.text = "changed".into();
        std::fs::write(d.join("page1.json"), serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(load_checkpoints(&d, &ids).len(), 1, "hash mismatch");
        // A leftover temp file is never read.
        std::fs::write(d.join("page1.json.tmp"), b"junk").unwrap();
        assert_eq!(load_checkpoints(&d, &ids).len(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn lang_uses_profile_unless_default() {
        assert_eq!(tesseract_lang("deu", "frak2021"), "frak2021");
        assert_eq!(tesseract_lang("eng", "default"), "eng");
    }

    #[test]
    fn args_pin_threads_free_command_line() {
        let a = tesseract_args(
            Path::new("/s/page0.jp2"),
            Path::new("/s/tessdata"),
            "frak2021",
        );
        assert_eq!(a[1], "stdout");
        assert!(a.windows(2).any(|w| w == ["-l", "frak2021"]));
    }

    #[test]
    fn built_results_pass_the_validator_checks() {
        let cfg = Config::pilot();
        let m = WorkunitManifest {
            pages: vec![],
            schema: 1,
            wu: "usnm_ocr_00".into(),
        };
        let pages = vec![page_result(
            "p0",
            "0123456789abcdef0123456789abcdef",
            "Der Kaiser ist da".into(),
        )];
        let r = build_result(&m, &cfg.engine_version, cfg.traineddata.clone(), pages);
        assert_eq!(check(&r, &cfg), Ok(()));
    }

    #[test]
    fn empty_text_is_marked_blank() {
        assert!(page_result("p", "0123456789abcdef0123456789abcdef", " \n".into()).blank);
    }
}
