//! Reading a batch's bulk OCR archive (04 §4.1).
//!
//! An archive is a tar file (optionally gzip, bzip2 or zstd compressed; the
//! format is detected from its first bytes) holding one `ocr.txt` per page.
//! Page identity comes from the path, in either of these layouts:
//!
//! - `…/{lccn}/{yyyy}/{mm}/{dd}/ed-{n}/seq-{n}/ocr.txt` (the historical
//!   Chronicling America OCR bundles)
//! - `…/{lccn}/{yyyy-mm-dd}/ed-{n}/seq-{n}/ocr.txt` (the page key itself)
//!
//! Everything else (ALTO XML, images, manifests) is skipped. Spike S-1
//! confirms the Datasets portal's current layout against these.

use std::collections::hash_map::{Entry, HashMap};
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use anyhow::{bail, Context};
use chrono::NaiveDate;
use sha2::{Digest, Sha256};
use usnm_core::ids::PageKey;

/// Larger `ocr.txt` files are treated as corrupt (real pages run to tens of
/// KB). It also keeps every page's document under the index ingest limit.
pub const MAX_PAGE_BYTES: u64 = 2 * 1024 * 1024;

/// One page's raw OCR text.
#[derive(Debug)]
pub struct RawPage {
    pub key: PageKey,
    pub text: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ArchiveStats {
    pub pages: u64,
    /// Entries that aren't page OCR text (XML, images, …).
    pub skipped: u64,
    /// `ocr.txt` files that weren't valid UTF-8 (decoded lossily).
    pub lossy: u64,
    /// Repeats of a page already read, with byte-identical text (skipped).
    pub duplicates: u64,
}

/// Parse a page key from an archive path, or `None` if it isn't page OCR text.
pub fn page_key_from_path(path: &str) -> anyhow::Result<Option<PageKey>> {
    let parts: Vec<&str> = path.trim_start_matches("./").split('/').collect();
    if parts.last() != Some(&"ocr.txt") || parts.len() < 5 {
        return Ok(None);
    }
    let n = parts.len();
    let (seq, ed) = (parts[n - 2], parts[n - 3]);
    let num = |s: &str, prefix: &str| -> anyhow::Result<u16> {
        s.strip_prefix(prefix)
            .and_then(|v| v.parse().ok())
            .with_context(|| format!("{path}: expected `{prefix}N`, found `{s}`"))
    };
    let (seq, ed) = (num(seq, "seq-")?, num(ed, "ed-")?);
    // `{yyyy-mm-dd}` or `{yyyy}/{mm}/{dd}`.
    let (date, lccn) = if let Ok(d) = NaiveDate::parse_from_str(parts[n - 4], "%Y-%m-%d") {
        (d, parts[n - 5])
    } else if n >= 7 {
        let ymd = format!("{}-{}-{}", parts[n - 6], parts[n - 5], parts[n - 4]);
        let d = NaiveDate::parse_from_str(&ymd, "%Y-%m-%d")
            .with_context(|| format!("{path}: no date in the path"))?;
        (d, parts[n - 7])
    } else {
        bail!("{path}: no date in the path");
    };
    Ok(Some(
        PageKey::new(lccn, date, ed, seq).with_context(|| path.to_owned())?,
    ))
}

/// Wrap `r` in the decompressor its first bytes call for.
fn decompress<'a>(r: impl Read + 'a) -> anyhow::Result<Box<dyn Read + 'a>> {
    let mut r = BufReader::new(r);
    let head = r.fill_buf()?.to_vec();
    Ok(match head.as_slice() {
        [0x1f, 0x8b, ..] => Box::new(flate2::read::MultiGzDecoder::new(r)),
        [b'B', b'Z', b'h', ..] => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        [0x28, 0xb5, 0x2f, 0xfd, ..] => Box::new(zstd::stream::read::Decoder::with_buffer(r)?),
        _ => Box::new(r),
    })
}

/// Stream every page in the archive at `path` to `on_page`. A page that
/// appears again with identical text is skipped and counted; with different
/// text it's an error, since there's no telling which copy is right. Some LoC
/// archives repeat whole issues (e.g. `az_agave_ver01`: 76 of 9,742 pages,
/// same `ocr.txt`, regenerated ALTO XML).
pub fn read_pages(
    path: &Path,
    on_page: impl FnMut(RawPage) -> anyhow::Result<()>,
) -> anyhow::Result<ArchiveStats> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    read_pages_from(file, on_page)
}

/// [`read_pages`] from any reader, e.g. a download being streamed.
pub fn read_pages_from(
    input: impl Read,
    mut on_page: impl FnMut(RawPage) -> anyhow::Result<()>,
) -> anyhow::Result<ArchiveStats> {
    let mut archive = tar::Archive::new(decompress(input)?);
    let mut stats = ArchiveStats::default();
    let mut seen: HashMap<PageKey, [u8; 32]> = HashMap::new();
    for entry in archive.entries().context("reading archive")? {
        let mut entry = entry.context("reading archive entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let name = entry.path()?.to_string_lossy().into_owned();
        let Some(key) = page_key_from_path(&name)? else {
            stats.skipped += 1;
            continue;
        };
        if entry.size() > MAX_PAGE_BYTES {
            bail!(
                "{name}: {} bytes is larger than any real OCR page",
                entry.size()
            );
        }
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
        entry.read_to_end(&mut bytes)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        match seen.entry(key.clone()) {
            Entry::Occupied(first) if *first.get() == digest => {
                stats.duplicates += 1;
                continue;
            }
            Entry::Occupied(_) => {
                bail!("{name}: page `{key}` appears twice in the archive with different text")
            }
            Entry::Vacant(v) => {
                v.insert(digest);
            }
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(e) => {
                stats.lossy += 1;
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };
        stats.pages += 1;
        on_page(RawPage { key, text })?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn page_keys_from_both_layouts() {
        let k = page_key_from_path("sn84026749/1896/07/10/ed-1/seq-3/ocr.txt")
            .unwrap()
            .unwrap();
        assert_eq!(k.to_string(), "sn84026749/1896-07-10/ed-1/seq-3");
        let k = page_key_from_path("./batch_x_ver01/sn84026749/1896-07-10/ed-2/seq-1/ocr.txt")
            .unwrap()
            .unwrap();
        assert_eq!(k.to_string(), "sn84026749/1896-07-10/ed-2/seq-1");
        assert!(
            page_key_from_path("sn84026749/1896/07/10/ed-1/seq-3/ocr.xml")
                .unwrap()
                .is_none()
        );
        assert!(page_key_from_path("manifest.txt").unwrap().is_none());
        assert!(page_key_from_path("sn1/1896/13/40/ed-1/seq-3/ocr.txt").is_err());
        assert!(page_key_from_path("SN1/1896-07-10/ed-1/seq-3/ocr.txt").is_err());
        assert!(page_key_from_path("sn1/1896-07-10/ed-0/seq-3/ocr.txt").is_err());
    }

    fn tar_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    #[test]
    fn reads_compressed_archives_and_skips_other_files() {
        let raw = tar_of(&[
            ("sn1/1896/07/10/ed-1/seq-1/ocr.txt", b"Hello"),
            ("sn1/1896/07/10/ed-1/seq-1/ocr.xml", b"<alto/>"),
            ("sn1/1896/07/10/ed-1/seq-2/ocr.txt", b"caf\xe9"),
        ]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bz.write_all(&raw).unwrap();
        for data in [
            raw.clone(),
            gz.finish().unwrap(),
            bz.finish().unwrap(),
            zstd::encode_all(raw.as_slice(), 3).unwrap(),
        ] {
            let f = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(f.path(), &data).unwrap();
            let mut pages = Vec::new();
            let stats = read_pages(f.path(), |p| {
                pages.push((p.key.to_string(), p.text));
                Ok(())
            })
            .unwrap();
            assert_eq!(
                stats,
                ArchiveStats {
                    pages: 2,
                    skipped: 1,
                    lossy: 1,
                    duplicates: 0
                }
            );
            assert_eq!(
                pages[0],
                ("sn1/1896-07-10/ed-1/seq-1".into(), "Hello".into())
            );
            assert_eq!(pages[1].1, "caf\u{fffd}");
        }
    }

    #[test]
    fn a_page_repeated_with_different_text_is_an_error() {
        let raw = tar_of(&[
            ("a/sn1/1896-07-10/ed-1/seq-1/ocr.txt", b"x"),
            ("b/sn1/1896/07/10/ed-1/seq-1/ocr.txt", b"y"),
        ]);
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), raw).unwrap();
        let err = read_pages(f.path(), |_| Ok(())).unwrap_err().to_string();
        assert!(err.contains("different text"), "{err}");
    }

    #[test]
    fn a_page_repeated_with_identical_text_is_read_once() {
        let raw = tar_of(&[
            ("sn1/1896/07/10/ed-1/seq-1/ocr.txt", b"same"),
            ("sn1/1896/07/10/ed-1/seq-1/ocr.xml", b"<alto v=1/>"),
            ("sn1/1896/07/10/ed-1/seq-1/ocr.txt", b"same"),
            ("sn1/1896/07/10/ed-1/seq-1/ocr.xml", b"<alto v=2/>"),
        ]);
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), raw).unwrap();
        let mut pages = 0;
        let stats = read_pages(f.path(), |_| {
            pages += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(pages, 1);
        assert_eq!((stats.pages, stats.duplicates, stats.skipped), (1, 1, 2));
    }
}
