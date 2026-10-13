//! The staging manifest: one JSON object per line, one line per staged page.
//!
//! The staging step (not part of this crate; see docs/design.md section 4.1) copies
//! public-domain page images into the staging blob account and writes this file. The work
//! generator reads it and slices it into workunits.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// One staged page. Fields are in alphabetical order so the serialised keys are sorted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedPage {
    /// Size of the image in bytes.
    pub bytes: u64,
    /// Language set as Tesseract names it, for example `deu`.
    pub language: String,
    /// MD5 of the image, 32 lowercase hex characters (BOINC wants MD5 for remote files).
    pub md5: String,
    /// Public page identifier (the LoC path), the same one `ocr-sync` uses.
    pub page_id: String,
    /// OCR profile name, for example `frak2021`.
    pub profile: String,
    /// HTTPS URL of the image, with a read-only SAS if the container is private.
    pub url: String,
}

/// Largest image the work generator accepts, in bytes. A normal page is about 4.5 MB.
pub const MAX_PAGE_BYTES: u64 = 40 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct ManifestError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "manifest line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ManifestError {}

fn check(p: &StagedPage) -> Result<(), String> {
    if p.page_id.is_empty() || p.page_id.len() > 200 || p.page_id.chars().any(char::is_control) {
        return Err("page_id is empty, too long or has control characters".into());
    }
    if p.md5.len() != 32
        || !p
            .md5
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("md5 must be 32 lowercase hex characters".into());
    }
    if p.bytes == 0 || p.bytes > MAX_PAGE_BYTES {
        return Err(format!("bytes must be 1..={MAX_PAGE_BYTES}"));
    }
    if !p.url.starts_with("https://") || p.url.contains(char::is_whitespace) {
        return Err("url must be https:// with no whitespace".into());
    }
    if p.language.is_empty() || p.profile.is_empty() {
        return Err("language and profile are required".into());
    }
    Ok(())
}

/// Parse and validate a JSON-lines manifest. Blank lines are skipped. A page id that appears
/// twice is an error, so one page cannot be queued twice by accident.
pub fn parse(text: &str) -> Result<Vec<StagedPage>, ManifestError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let page: StagedPage = serde_json::from_str(line).map_err(|e| ManifestError {
            line: n,
            message: e.to_string(),
        })?;
        check(&page).map_err(|message| ManifestError { line: n, message })?;
        if !seen.insert(page.page_id.clone()) {
            return Err(ManifestError {
                line: n,
                message: format!("duplicate page_id {:?}", page.page_id),
            });
        }
        out.push(page);
    }
    Ok(out)
}

/// A workunit's worth of pages. All pages share one language and profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slice {
    /// Deterministic workunit name, derived from the page ids, so a re-run never creates a
    /// second workunit for the same pages.
    pub name: String,
    pub pages: Vec<StagedPage>,
}

/// Name of the workunit for these pages: `usnm_ocr_` plus 16 hex characters of the SHA-256 of
/// the page ids joined with newlines, in order.
pub fn slice_name(pages: &[StagedPage]) -> String {
    let mut h = Sha256::new();
    for p in pages {
        h.update(p.page_id.as_bytes());
        h.update(b"\n");
    }
    let d = h.finalize();
    let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("{}_{hex}", crate::APP_NAME)
}

/// Cut the pages into workunits of `pages_per_wu`, never mixing a language or profile, in
/// manifest order. A short final group per profile is kept only if `allow_partial` is set;
/// otherwise its pages wait for the next manifest.
pub fn slice(pages: &[StagedPage], pages_per_wu: usize, allow_partial: bool) -> Vec<Slice> {
    assert!(pages_per_wu > 0, "pages_per_wu must be positive");
    // Group by (language, profile) while keeping first-seen group order.
    let mut keys: Vec<(&str, &str)> = Vec::new();
    for p in pages {
        let k = (p.language.as_str(), p.profile.as_str());
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    let mut out = Vec::new();
    for (lang, prof) in keys {
        let group: Vec<&StagedPage> = pages
            .iter()
            .filter(|p| p.language == lang && p.profile == prof)
            .collect();
        for chunk in group.chunks(pages_per_wu) {
            if chunk.len() < pages_per_wu && !allow_partial {
                continue;
            }
            let pages: Vec<StagedPage> = chunk.iter().map(|p| (*p).clone()).collect();
            out.push(Slice {
                name: slice_name(&pages),
                pages,
            });
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn page(id: &str, lang: &str, prof: &str) -> StagedPage {
        StagedPage {
            bytes: 4_500_000,
            language: lang.into(),
            md5: "0123456789abcdef0123456789abcdef".into(),
            page_id: id.into(),
            profile: prof.into(),
            url: format!("https://staging.example.invalid/pages/{id}.jp2"),
        }
    }

    fn line(p: &StagedPage) -> String {
        serde_json::to_string(p).unwrap()
    }

    #[test]
    fn keys_serialise_in_alphabetical_order() {
        let s = line(&page("a", "deu", "frak2021"));
        let keys: Vec<&str> = ["bytes", "language", "md5", "page_id", "profile", "url"].to_vec();
        let mut last = 0;
        for k in keys {
            let at = s.find(&format!("\"{k}\"")).unwrap();
            assert!(at >= last, "{k} out of order in {s}");
            last = at;
        }
    }

    #[test]
    fn parse_accepts_good_lines_and_skips_blanks() {
        let text = format!(
            "{}\n\n{}\n",
            line(&page("a", "deu", "frak2021")),
            line(&page("b", "deu", "frak2021"))
        );
        assert_eq!(parse(&text).unwrap().len(), 2);
    }

    #[test]
    fn parse_rejects_bad_fields_with_line_numbers() {
        let mut bad = page("a", "deu", "frak2021");
        bad.md5 = "XYZ".into();
        let err = parse(&format!(
            "{}\n{}",
            line(&page("ok", "deu", "p")),
            line(&bad)
        ))
        .unwrap_err();
        assert_eq!(err.line, 2);

        let mut http = page("a", "deu", "p");
        http.url = "http://x.example.invalid/a".into();
        assert!(parse(&line(&http)).is_err());

        let mut big = page("a", "deu", "p");
        big.bytes = MAX_PAGE_BYTES + 1;
        assert!(parse(&line(&big)).is_err());

        assert!(parse("{not json").is_err());
    }

    #[test]
    fn parse_rejects_duplicate_page_ids() {
        let l = line(&page("a", "deu", "p"));
        let err = parse(&format!("{l}\n{l}")).unwrap_err();
        assert_eq!(err.line, 2);
    }

    #[test]
    fn slices_by_size_and_drops_short_tail_by_default() {
        let pages: Vec<_> = (0..10)
            .map(|i| page(&format!("p{i}"), "deu", "frak2021"))
            .collect();
        let s = slice(&pages, 4, false);
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|x| x.pages.len() == 4));
        assert_eq!(s[0].pages[0].page_id, "p0");
        assert_eq!(s[1].pages[3].page_id, "p7");
        let s = slice(&pages, 4, true);
        assert_eq!(s.len(), 3);
        assert_eq!(s[2].pages.len(), 2);
    }

    #[test]
    fn slices_never_mix_profiles() {
        let mut pages = Vec::new();
        for i in 0..4 {
            pages.push(page(&format!("a{i}"), "deu", "frak2021"));
            pages.push(page(&format!("b{i}"), "eng", "default"));
        }
        let s = slice(&pages, 4, false);
        assert_eq!(s.len(), 2);
        for sl in &s {
            let first = (&sl.pages[0].language, &sl.pages[0].profile);
            assert!(sl.pages.iter().all(|p| (&p.language, &p.profile) == first));
        }
    }

    #[test]
    fn names_are_deterministic_and_depend_on_pages() {
        let pages: Vec<_> = (0..4).map(|i| page(&format!("p{i}"), "deu", "x")).collect();
        let a = slice(&pages, 4, false);
        let b = slice(&pages, 4, false);
        assert_eq!(a[0].name, b[0].name);
        assert!(a[0].name.starts_with("usnm_ocr_"));
        assert_eq!(a[0].name.len(), "usnm_ocr_".len() + 16);
        let other: Vec<_> = (1..5).map(|i| page(&format!("p{i}"), "deu", "x")).collect();
        assert_ne!(a[0].name, slice(&other, 4, false)[0].name);
    }

    #[test]
    fn empty_input_gives_no_slices() {
        assert!(slice(&[], 4, true).is_empty());
    }
}
