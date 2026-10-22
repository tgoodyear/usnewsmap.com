//! Surya on the gold crops (#968): one `usnm_surya` crop unit for each row of
//! a gold-set manifest (`docs/ocr-gold-set.md`, "Crop format"), so Surya's
//! reading of exactly the boxes a person transcribes can be scored with
//! `scripts/ocr-survey/gold.py score --hyp surya=FILE`.
//!
//! The pages are the gold set's own: each row names the page's tile.loc.gov
//! IIIF JPEG and its sha256, and a page whose bytes differ from that
//! checksum is not cut, because the gold set pins pixels by it. A unit is
//! `units/<doc_id>.<crop_id>` holding `page.jpg` (the box), `surya.json` and
//! `gold.json` (the manifest row, verbatim). [`hyp`] reads the results back
//! as the hypothesis file `gold.py score` takes.
//!
//! LoC politeness is as for `surya stage`: one request at a time, at least
//! `gap` (5 s or more) apart, no retries; a 403, 429, 5xx, a network
//! failure or an HTML page in place of the JPEG writes the campaign's HOLD
//! file and stops the run.
use super::{
    check_no_hold,
    crops::{cut_jpeg, unit_name},
    units, JOB_FILE, PAGE_FILE,
};
use crate::{atomic, read, scans};
use anyhow::{bail, ensure, Context};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::Duration,
};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::{
    boinc::{is_sha256, sha256},
    surya::{CropRef, Engine, Input, InputMode, Job, VERSION},
};
use usnm_ocr_worker::loc::{FetchError, ImageSize, LocClient, IIIF_ROOT, MAX_IMAGE_BYTES};

/// The manifest row kept in each unit, verbatim.
pub const GOLD_FILE: &str = "gold.json";
/// The job group of every gold unit.
pub const GROUP: &str = "gold";
/// What a full-size IIIF JPEG URL ends with.
const FULL_JPEG: &str = "/full/full/0/default.jpg";
const MAX_MANIFEST: usize = 8 * 1024 * 1024;

pub const MIN_GAP: Duration = scans::MIN_GAP;

#[derive(Debug, Clone)]
pub struct Options {
    /// The gold manifest (`manifest.jsonl`, one row per crop).
    pub manifest: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    /// Deadline for the one `whole` run, on a GPU and on a CPU.
    pub timeout_secs: u32,
    /// Crops to stage, at most.
    pub max_crops: usize,
    pub gap: Duration,
    /// Read `<dir>/<page_id>.jpg` instead of fetching the page.
    pub pages_dir: Option<PathBuf>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Crop units written.
    pub staged: usize,
    /// Crops whose unit was already there.
    pub already: usize,
    /// Crops whose page differs from the manifest's checksum.
    pub mismatch: usize,
    /// Pages that could not be fetched.
    pub missing: usize,
}

/// The fields of a manifest row that staging reads; the row is kept whole.
#[derive(Debug, Deserialize)]
struct Row {
    crop_id: String,
    page_id: String,
    lang: String,
    #[serde(rename = "box")]
    rect: [u32; 4],
    page_image_url: String,
    page_image_sha256: String,
    #[serde(default)]
    lccn: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    edition: Option<u16>,
    #[serde(default)]
    seq: Option<u16>,
}

struct Entry {
    row: Row,
    /// The manifest line, as it was.
    raw: String,
    doc_id: String,
}

/// The project's doc id (`<lccn>_<date>_ed-<n>_seq-<n>`, as `PageKey::doc_id`
/// writes it) of a gold page id, which spells the edition and sequence
/// `ed1_seq1`. The `ed-1_seq-1` form is read too.
pub fn doc_id_of(page_id: &str) -> anyhow::Result<String> {
    let parts: Vec<&str> = page_id.split('_').collect();
    let [lccn, date, ed, seq] = parts.as_slice() else {
        bail!("{page_id:?} is not <lccn>_<date>_ed<n>_seq<n>");
    };
    let number = |s: &str, prefix: &str| -> anyhow::Result<u16> {
        let n = s.strip_prefix(prefix).unwrap_or(s);
        let n = n.strip_prefix('-').unwrap_or(n);
        ensure!(
            !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
            "{page_id:?}: {s:?} is not a number after {prefix:?}"
        );
        n.parse().with_context(|| format!("{page_id:?}: {s:?}"))
    };
    let ed = number(ed, "ed")?;
    let seq = number(seq, "seq")?;
    let key: PageKey = format!("{lccn}/{date}/ed-{ed}/seq-{seq}")
        .parse()
        .with_context(|| format!("{page_id:?} is not a page id"))?;
    let doc_id = key.doc_id();
    crate::assess::check_doc_id(&doc_id)?;
    Ok(doc_id)
}

fn read_manifest(path: &Path) -> anyhow::Result<Vec<Entry>> {
    let text = String::from_utf8(read(path, MAX_MANIFEST)?).context("the manifest is not UTF-8")?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: Row =
            serde_json::from_str(line).with_context(|| format!("manifest line {}", i + 1))?;
        let at = |e: anyhow::Error| e.context(format!("manifest line {} ({})", i + 1, row.crop_id));
        // The crop id names the unit directory and its markers, so it is held to
        // the contract's grammar here, before any path is built from it.
        ensure!(
            !row.crop_id.is_empty()
                && row.crop_id.len() <= 32
                && row
                    .crop_id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "manifest line {}: invalid crop id {:?}",
            i + 1,
            row.crop_id
        );
        ensure!(
            seen.insert(row.crop_id.clone()),
            "manifest line {}: crop id {} appears twice",
            i + 1,
            row.crop_id
        );
        ensure!(
            is_sha256(&row.page_image_sha256),
            "manifest line {} ({}): page_image_sha256 is not a sha256",
            i + 1,
            row.crop_id
        );
        let doc_id = doc_id_of(&row.page_id).map_err(at)?;
        let key = PageKey::from_doc_id(&doc_id)?;
        let same = |what: &str, ours: String, theirs: Option<String>| {
            ensure!(
                theirs.is_none_or(|t| t == ours),
                "manifest line {} ({}): {what} differs from the page id",
                i + 1,
                row.crop_id
            );
            Ok(())
        };
        same("lccn", key.lccn.clone(), row.lccn.clone())?;
        same("date", key.date.to_string(), row.date.clone())?;
        same(
            "edition",
            key.edition.to_string(),
            row.edition.map(|n| n.to_string()),
        )?;
        same("seq", key.seq.to_string(), row.seq.map(|n| n.to_string()))?;
        out.push(Entry {
            row,
            raw: line.to_owned(),
            doc_id,
        });
    }
    ensure!(!out.is_empty(), "the manifest has no rows");
    Ok(out)
}

/// The gold unit's job: the page's doc id, the row's language, the crop's
/// JPEG as the input with the page's URL and the box as its fragment,
/// `whole` only. The crop id is the row's (`cze-01`), the stratum
/// `gold/<lang>`, the group `gold`; all fit the contract's grammar as they
/// are.
fn gold_job(e: &Entry, jpeg: &[u8], engine: &Engine, timeout_secs: u32) -> anyhow::Result<Job> {
    let [x0, y0, x1, y1] = e.row.rect;
    let crop = CropRef {
        id: e.row.crop_id.clone(),
        rect: [x0, y0, x1, y1],
        stratum: format!("gold/{}", e.row.lang),
    };
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        doc_id: e.doc_id.clone(),
        group: GROUP.into(),
        language: e.row.lang.clone(),
        input: Input {
            bytes: jpeg.len() as u64,
            sha256: sha256(jpeg),
            url: format!("{}#{}", e.row.page_image_url, crop.xywh()),
        },
        engine: engine.clone(),
        gpu_modes: vec![InputMode::Whole],
        cpu_modes: vec![InputMode::Whole],
        gpu_timeout_secs: timeout_secs,
        cpu_timeout_secs: timeout_secs,
        crop: Some(crop),
    };
    job.id = job.identity()?;
    job.validate().context("staged gold job")?;
    Ok(job)
}

/// Stages up to `max_crops` crop units, in manifest order. A page is read
/// (or fetched) once for all the crops it has among them, and every crop of
/// it is cut before any unit of the page is written.
pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(opts.gap >= MIN_GAP, "--gap-secs must be at least 5");
    let loc = match opts.pages_dir {
        Some(_) => None,
        None => Some(scans::loc_client(opts.gap, None)?),
    };
    run_with(opts, loc.as_ref(), IIIF_ROOT).await
}

async fn run_with(
    opts: &Options,
    loc: Option<&LocClient>,
    iiif_root: &str,
) -> anyhow::Result<Summary> {
    crate::qwen::check_digest(&opts.engine.image, "--engine-image")?;
    crate::qwen::check_digest(&opts.engine.llama, "--engine-llama")?;
    crate::qwen::check_digest(&opts.engine.weights, "--engine-weights")?;
    check_no_hold(&opts.root)?;
    let entries = read_manifest(&opts.manifest)?;
    let dir = units(&opts.root);
    std::fs::create_dir_all(&dir)?;
    let marker =
        |e: &Entry, ext: &str| dir.join(format!("{}.{ext}", unit_name(&e.doc_id, &e.row.crop_id)));
    let mut summary = Summary::default();
    let mut missing_pages = BTreeSet::new();
    // The crops still to stage, grouped by page in order of first appearance.
    let mut pages: Vec<(&str, Vec<&Entry>)> = Vec::new();
    let mut pending = 0;
    for e in &entries {
        let unit = dir.join(unit_name(&e.doc_id, &e.row.crop_id));
        if unit.join(JOB_FILE).exists() {
            summary.already += 1;
        } else if marker(e, "mismatch").exists() {
            summary.mismatch += 1;
        } else if marker(e, "missing").exists() {
            missing_pages.insert(e.row.page_id.as_str());
        } else if pending < opts.max_crops {
            pending += 1;
            match pages.iter_mut().find(|(p, _)| *p == e.row.page_id) {
                Some((_, rows)) => rows.push(e),
                None => pages.push((e.row.page_id.as_str(), vec![e])),
            }
        }
    }
    for (page_id, rows) in pages {
        let fetched = match (&opts.pages_dir, loc) {
            (Some(pages_dir), _) => {
                let path = pages_dir.join(format!("{page_id}.jpg"));
                if path.is_file() {
                    Ok(Some(read(&path, MAX_IMAGE_BYTES)?))
                } else {
                    // No marker: a page put there later is picked up.
                    missing_pages.insert(page_id);
                    continue;
                }
            }
            (None, Some(loc)) => fetch(loc, iiif_root, &rows[0].row.page_image_url).await,
            (None, None) => bail!("no page source"),
        };
        let bytes = match fetched {
            Ok(Some(b)) => b,
            Ok(None) => {
                for e in &rows {
                    atomic(
                        &marker(e, "missing"),
                        b"tile.loc.gov does not have the page\n",
                    )?;
                }
                missing_pages.insert(page_id);
                continue;
            }
            Err(e) => {
                atomic(
                    &crate::assess::hold_path(&opts.root),
                    format!("{page_id}: {e:#}\n").as_bytes(),
                )?;
                return Err(e.context(format!("stopped at {page_id}")));
            }
        };
        eprintln!("{page_id}: {} bytes", bytes.len());
        let digest = sha256(&bytes);
        let mut image = None;
        let mut staged = Vec::new();
        for e in rows {
            if digest != e.row.page_image_sha256 {
                atomic(
                    &marker(e, "mismatch"),
                    format!(
                        "the page's sha256 is {digest}, the manifest has {}\n",
                        e.row.page_image_sha256
                    )
                    .as_bytes(),
                )?;
                summary.mismatch += 1;
                continue;
            }
            if image.is_none() {
                image = Some(
                    image::load_from_memory(&bytes)
                        .with_context(|| format!("{page_id}: decoding the page"))?,
                );
            }
            let jpeg = cut_jpeg(image.as_ref().expect("decoded above"), e.row.rect)
                .with_context(|| format!("{}: cutting the crop", e.row.crop_id))?;
            let job = gold_job(e, &jpeg, &opts.engine, opts.timeout_secs)
                .with_context(|| format!("{}: job", e.row.crop_id))?;
            staged.push((e, jpeg, job));
        }
        for (e, jpeg, job) in staged {
            let unit = dir.join(unit_name(&e.doc_id, &e.row.crop_id));
            std::fs::create_dir_all(&unit)?;
            atomic(&unit.join(PAGE_FILE), &jpeg)?;
            atomic(&unit.join(GOLD_FILE), format!("{}\n", e.raw).as_bytes())?;
            // The job last: a unit with a job is complete.
            atomic(&unit.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
            summary.staged += 1;
        }
    }
    summary.missing = missing_pages.len();
    Ok(summary)
}

/// The page at `url`, one paced request; `None` for a 404.
async fn fetch(
    loc: &LocClient,
    iiif_root: &str,
    url: &str,
) -> Result<Option<Vec<u8>>, anyhow::Error> {
    let base = url
        .strip_suffix(FULL_JPEG)
        .filter(|b| b.starts_with(iiif_root) && b.len() > iiif_root.len())
        .with_context(|| format!("{url} is not a full-size IIIF JPEG on {iiif_root}"))?;
    match loc.iiif_scan(base.to_owned(), ImageSize::Full).await {
        Ok(s) => {
            ensure!(s.url == url, "fetched {} for {url}", s.url);
            Ok(Some(s.bytes.to_vec()))
        }
        Err(FetchError::Permanent(_)) => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

#[derive(Debug, Clone)]
pub struct HypOptions {
    pub root: PathBuf,
    /// The assimilator's results directory.
    pub spool: PathBuf,
    pub out: PathBuf,
}

#[derive(Debug, Default, PartialEq)]
pub struct HypSummary {
    /// Gold crops with a successful `whole` read, written to the file.
    pub read: usize,
    /// Gold crops staged without one: still out, given up on, or failed.
    pub unread: usize,
}

/// Writes `{"crop_id", "text"}` per gold crop unit with a successful read,
/// sorted by crop id: the file `gold.py score --hyp surya=FILE` and
/// `usnm-ocr-survey score` take.
pub fn hyp(opts: &HypOptions) -> anyhow::Result<HypSummary> {
    let results = super::collect::results(&opts.root, &opts.spool)?;
    let dir = units(&opts.root);
    let mut gold = 0usize;
    if dir.exists() {
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path();
            if p.join(GOLD_FILE).exists() && p.join(JOB_FILE).exists() {
                gold += 1;
            }
        }
    }
    let mut rows: BTreeMap<String, String> = BTreeMap::new();
    for (doc_id, crops) in &results.crops {
        for (crop_id, c) in crops {
            if !dir
                .join(unit_name(doc_id, crop_id))
                .join(GOLD_FILE)
                .exists()
            {
                continue;
            }
            if c.outcome == usnm_ocr_contract::surya::Outcome::Ok {
                rows.insert(crop_id.clone(), c.text.clone());
            }
        }
    }
    let mut out = String::new();
    for (crop_id, text) in &rows {
        out.push_str(&serde_json::to_string(
            &serde_json::json!({"crop_id": crop_id, "text": text}),
        )?);
        out.push('\n');
    }
    if let Some(parent) = opts.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    atomic(&opts.out, out.as_bytes())?;
    Ok(HypSummary {
        read: rows.len(),
        unread: gold.saturating_sub(rows.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surya::collect::tests::{crop_job, unit_with_result};
    use image::GenericImageView as _;
    use usnm_ocr_contract::surya::Outcome;
    use usnm_ocr_worker::standin::{Scripted, StandIn};

    fn engine() -> Engine {
        Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", "b".repeat(64)),
            surya: "0.22.1".into(),
            weights: format!("sha256:{}", "c".repeat(64)),
        }
    }

    /// A page JPEG with some structure, so a crop is not a flat colour.
    fn page_jpeg(w: u32, h: u32, shade: u8) -> Vec<u8> {
        let img = image::GrayImage::from_fn(w, h, |x, y| {
            image::Luma([((x * 3 + y * 5) % 200) as u8 + shade % 50])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .encode_image(&img)
            .unwrap();
        out.into_inner()
    }

    const BASE: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp:xx:batch_xx_ver01:data:sn83045348:0001:1876030701:0190";

    /// A manifest row; the page's lccn, date, edition and sequence follow
    /// from its id.
    fn row(crop_id: &str, page_id: &str, rect: [u32; 4], url: &str, sha: &str) -> String {
        let key = PageKey::from_doc_id(&doc_id_of(page_id).unwrap()).unwrap();
        serde_json::json!({
            "crop_id": crop_id, "page_id": page_id, "group": "czech", "lang": "cze",
            "era": "1870s", "stratum": "czech/1870s", "lccn": key.lccn,
            "date": key.date.to_string(), "edition": key.edition, "seq": key.seq,
            "title": "Pokrok západu", "box": rect, "image": format!("images/{crop_id}.jpg"),
            "page_image_url": url, "page_image_sha256": sha,
        })
        .to_string()
    }

    /// `line` with one field of the row set.
    fn with(line: &str, key: &str, value: serde_json::Value) -> String {
        let mut v: serde_json::Value = serde_json::from_str(line).unwrap();
        v[key] = value;
        v.to_string()
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        opts: Options,
        pages: PathBuf,
        page_b: Vec<u8>,
    }

    /// Three rows: two on page A, one on page B (a different issue).
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let pages = tmp.path().join("pages");
        std::fs::create_dir_all(&pages).unwrap();
        let (a, b) = (page_jpeg(300, 200, 0), page_jpeg(240, 180, 7));
        let pa = "sn83045348_1876-03-07_ed1_seq1";
        let pb = "sn83045348_1879-06-25_ed1_seq4";
        std::fs::write(pages.join(format!("{pa}.jpg")), &a).unwrap();
        std::fs::write(pages.join(format!("{pb}.jpg")), &b).unwrap();
        let url = |n: u32| format!("{BASE}{n}/full/full/0/default.jpg");
        let lines = [
            row("cze-01", pa, [10, 20, 110, 70], &url(1), &sha256(&a)),
            row("cze-02", pa, [150, 100, 290, 190], &url(1), &sha256(&a)),
            row("cze-03", pb, [0, 0, 120, 60], &url(2), &sha256(&b)),
        ];
        std::fs::write(tmp.path().join("manifest.jsonl"), lines.join("\n") + "\n").unwrap();
        Fixture {
            opts: Options {
                manifest: tmp.path().join("manifest.jsonl"),
                root: tmp.path().join("surya"),
                engine: engine(),
                timeout_secs: 600,
                max_crops: 50,
                gap: MIN_GAP,
                pages_dir: Some(pages.clone()),
            },
            pages,
            page_b: b,
            _tmp: tmp,
        }
    }

    fn unit_of(f: &Fixture, crop: &str, page_doc: &str) -> PathBuf {
        units(&f.opts.root).join(unit_name(page_doc, crop))
    }

    #[test]
    fn doc_ids_come_from_either_spelling() {
        assert_eq!(
            doc_id_of("sn83045348_1876-03-07_ed1_seq1").unwrap(),
            "sn83045348_1876-03-07_ed-1_seq-1"
        );
        assert_eq!(
            doc_id_of("sn83045348_1876-03-07_ed-2_seq-12").unwrap(),
            "sn83045348_1876-03-07_ed-2_seq-12"
        );
        for bad in [
            "sn83045348_1876-03-07_ed1",
            "sn83045348_1876-13-07_ed1_seq1",
            "sn83045348_1876-03-07_ed0_seq1",
            "sn83045348_1876-03-07_edx_seq1",
            "SN1_1876-03-07_ed1_seq1",
            "sn1_1876-03-07_ed1_seq",
        ] {
            assert!(doc_id_of(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn stages_one_unit_per_manifest_row_from_a_pages_dir() {
        let f = fixture();
        let s = run(&f.opts).await.unwrap();
        assert_eq!(
            s,
            Summary {
                staged: 3,
                already: 0,
                mismatch: 0,
                missing: 0
            }
        );
        let doc_a = "sn83045348_1876-03-07_ed-1_seq-1";
        let doc_b = "sn83045348_1879-06-25_ed-1_seq-4";
        let manifest = std::fs::read_to_string(&f.opts.manifest).unwrap();
        let lines: Vec<&str> = manifest.lines().collect();
        for (crop, doc, rect, line) in [
            ("cze-01", doc_a, [10, 20, 110, 70], lines[0]),
            ("cze-02", doc_a, [150, 100, 290, 190], lines[1]),
            ("cze-03", doc_b, [0, 0, 120, 60], lines[2]),
        ] {
            let dir = unit_of(&f, crop, doc);
            let job: Job =
                serde_json::from_slice(&std::fs::read(dir.join(JOB_FILE)).unwrap()).unwrap();
            job.validate().unwrap();
            assert_eq!(job.doc_id, doc);
            assert_eq!(job.group, "gold");
            assert_eq!(job.language, "cze");
            assert_eq!(job.gpu_modes, vec![InputMode::Whole]);
            assert_eq!(job.cpu_modes, vec![InputMode::Whole]);
            assert_eq!((job.gpu_timeout_secs, job.cpu_timeout_secs), (600, 600));
            let c = job.crop.as_ref().unwrap();
            assert_eq!(c.id, crop);
            assert_eq!(c.rect, rect);
            assert_eq!(c.stratum, "gold/cze");
            let [x0, y0, x1, y1] = rect;
            assert!(job.input.url.starts_with("https://tile.loc.gov/"));
            assert!(
                job.input.url.ends_with(&format!(
                    "/full/full/0/default.jpg#xywh={x0},{y0},{},{}",
                    x1 - x0,
                    y1 - y0
                )),
                "{}",
                job.input.url
            );
            let jpeg = std::fs::read(dir.join(PAGE_FILE)).unwrap();
            assert_eq!(jpeg.len() as u64, job.input.bytes);
            assert_eq!(sha256(&jpeg), job.input.sha256);
            assert_eq!(
                image::load_from_memory(&jpeg).unwrap().dimensions(),
                (x1 - x0, y1 - y0)
            );
            assert_eq!(
                std::fs::read_to_string(dir.join(GOLD_FILE)).unwrap().trim(),
                line
            );
        }
        // A second run leaves the units alone.
        let before = std::fs::read(unit_of(&f, "cze-01", doc_a).join(JOB_FILE)).unwrap();
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already), (0, 3), "{s:?}");
        assert_eq!(
            std::fs::read(unit_of(&f, "cze-01", doc_a).join(JOB_FILE)).unwrap(),
            before
        );
        // A unit lost its job (a run stopped part way): it is staged again,
        // the others are not touched.
        std::fs::remove_file(unit_of(&f, "cze-02", doc_a).join(JOB_FILE)).unwrap();
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already), (1, 2), "{s:?}");
    }

    #[tokio::test]
    async fn max_crops_stops_in_file_order_and_the_rest_come_next_time() {
        let mut f = fixture();
        f.opts.max_crops = 2;
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already), (2, 0));
        assert!(unit_of(&f, "cze-01", "sn83045348_1876-03-07_ed-1_seq-1")
            .join(JOB_FILE)
            .exists());
        assert!(!unit_of(&f, "cze-03", "sn83045348_1879-06-25_ed-1_seq-4")
            .join(JOB_FILE)
            .exists());
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already), (1, 2));
    }

    #[tokio::test]
    async fn a_page_that_differs_from_its_checksum_is_not_cut() {
        let f = fixture();
        // Page B changed since the manifest was made.
        std::fs::write(
            f.pages.join("sn83045348_1879-06-25_ed1_seq4.jpg"),
            page_jpeg(240, 180, 30),
        )
        .unwrap();
        assert_ne!(f.page_b, page_jpeg(240, 180, 30));
        let s = run(&f.opts).await.unwrap();
        assert_eq!(
            s,
            Summary {
                staged: 2,
                already: 0,
                mismatch: 1,
                missing: 0
            }
        );
        let doc_b = "sn83045348_1879-06-25_ed-1_seq-4";
        let unit = unit_of(&f, "cze-03", doc_b);
        assert!(!unit.join(JOB_FILE).exists() && !unit.join(PAGE_FILE).exists());
        let marker = units(&f.opts.root).join(format!("{doc_b}.cze-03.mismatch"));
        let why = std::fs::read_to_string(marker).unwrap();
        assert_eq!(why.lines().count(), 1);
        assert!(why.contains("sha256"), "{why}");
        // The marker is not fetched again, and not staged.
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already, s.mismatch), (0, 2, 1));
    }

    #[tokio::test]
    async fn a_page_missing_from_the_pages_dir_is_counted_and_retried() {
        let f = fixture();
        std::fs::remove_file(f.pages.join("sn83045348_1879-06-25_ed1_seq4.jpg")).unwrap();
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.missing), (2, 1));
        std::fs::write(
            f.pages.join("sn83045348_1879-06-25_ed1_seq4.jpg"),
            &f.page_b,
        )
        .unwrap();
        let s = run(&f.opts).await.unwrap();
        assert_eq!((s.staged, s.already, s.missing), (1, 2, 0));
    }

    #[tokio::test]
    async fn bad_manifests_and_pins_are_refused_before_anything_is_written() {
        let mut f = fixture();
        let good = std::fs::read_to_string(&f.opts.manifest).unwrap();
        let first = good.lines().next().unwrap().to_owned();
        for (bad, why) in [
            (format!("{first}\n{first}\n"), "twice"),
            (with(&first, "crop_id", "Cze 01".into()), "invalid crop id"),
            // A traversal id must fail before a marker path is built from it: with the
            // checksum also wrong, the mismatch marker would otherwise land outside the root.
            (
                with(
                    &with(&first, "crop_id", "x/../../../escaped".into()),
                    "page_image_sha256",
                    "0".repeat(64).into(),
                ),
                "invalid crop id",
            ),
            (with(&first, "date", "1876-03-08".into()), "date differs"),
            (
                with(&first, "page_image_sha256", "abc".into()),
                "not a sha256",
            ),
            (
                with(&first, "box", serde_json::json!([50, 20, 50, 70])),
                "outside",
            ),
            (String::new(), "no rows"),
        ] {
            std::fs::write(&f.opts.manifest, bad).unwrap();
            let msg = format!("{:#}", run(&f.opts).await.unwrap_err());
            assert!(msg.contains(why), "{why}: {msg}");
            // Nothing was staged, and no marker escaped the root.
            let staged = std::fs::read_dir(units(&f.opts.root)).map_or(0, |d| d.count());
            assert_eq!(staged, 0, "{why}");
            for dir in f.opts.root.ancestors().take(4) {
                let escaped = std::fs::read_dir(dir).is_ok_and(|d| {
                    d.filter_map(|e| e.ok())
                        .any(|e| e.file_name().to_string_lossy().contains("escaped"))
                });
                assert!(!escaped, "{why}: a marker was written in {}", dir.display());
            }
        }
        std::fs::write(&f.opts.manifest, &good).unwrap();
        f.opts.engine.image = "sha256:abc".into();
        assert!(run(&f.opts).await.is_err());
        f.opts.engine = engine();
        f.opts.gap = Duration::from_secs(1);
        assert!(run(&f.opts).await.is_err());
    }

    /// A page fetched through the stand-in tile server, once for two rows.
    #[tokio::test]
    async fn fetches_each_page_once_and_holds_on_a_refusal() {
        let s = StandIn::start().await;
        let iiif = s.iiif_root();
        let loc = LocClient::new(usnm_ocr_worker::loc::LocConfig {
            api_gap: Duration::from_millis(20),
            tile_gap: Duration::from_millis(20),
            storage_gap: Duration::from_millis(20),
            attempts: 1,
            backoff: Duration::from_millis(10),
            iiif_root: iiif.clone(),
            item_probes: false,
            html_is_block: true,
            ..usnm_ocr_worker::loc::LocConfig::default()
        })
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let page = page_jpeg(300, 200, 0);
        let (pa, pb, pc) = (
            "sn83045348_1876-03-07_ed1_seq1",
            "sn83045348_1879-06-25_ed1_seq4",
            "sn83045348_1880-01-02_ed1_seq2",
        );
        let url = |n: u32| format!("{iiif}service:ndnp:xx:p{n}/full/full/0/default.jpg");
        s.put(url(1).strip_prefix(&s.root).unwrap(), page.clone());
        // Page 2 is not on the server; page 3 is refused.
        s.script(url(3).strip_prefix(&s.root).unwrap(), Scripted::Status(403));
        let lines = [
            row("cze-01", pa, [10, 20, 110, 70], &url(1), &sha256(&page)),
            row("cze-02", pa, [150, 100, 290, 190], &url(1), &sha256(&page)),
            row("cze-03", pb, [0, 0, 100, 50], &url(2), &sha256(&page)),
            row("cze-04", pc, [0, 0, 100, 50], &url(3), &sha256(&page)),
        ];
        let manifest = tmp.path().join("manifest.jsonl");
        std::fs::write(&manifest, lines.join("\n") + "\n").unwrap();
        let opts = Options {
            manifest,
            root: tmp.path().join("surya"),
            engine: engine(),
            timeout_secs: 600,
            max_crops: 50,
            gap: MIN_GAP,
            pages_dir: None,
        };
        let err = run_with(&opts, Some(&loc), &iiif).await.unwrap_err();
        assert!(format!("{err:#}").contains("stopped at"), "{err:#}");
        // Page 1 staged from one request, page 2 marked missing, page 3
        // held the run.
        assert_eq!(s.hits("p1/full/full/0/default.jpg"), 1);
        assert_eq!(s.hits("p2/full/full/0/default.jpg"), 1);
        assert_eq!(s.hits("p3/full/full/0/default.jpg"), 1);
        let dir = units(&opts.root);
        for c in ["cze-01", "cze-02"] {
            assert!(dir
                .join(format!("sn83045348_1876-03-07_ed-1_seq-1.{c}"))
                .join(JOB_FILE)
                .exists());
        }
        assert!(dir
            .join("sn83045348_1879-06-25_ed-1_seq-4.cze-03.missing")
            .exists());
        let hold = std::fs::read_to_string(crate::assess::hold_path(&opts.root)).unwrap();
        assert!(hold.starts_with(pc), "{hold}");
        // Nothing runs again until a person removes the hold.
        assert!(run_with(&opts, Some(&loc), &iiif).await.is_err());
        assert_eq!(s.hits("p3/full/full/0/default.jpg"), 1);
        std::fs::remove_file(crate::assess::hold_path(&opts.root)).unwrap();
        // Page 3 answers 404 now (the scripted 403 was used up): missing.
        let sum = run_with(&opts, Some(&loc), &iiif).await.unwrap();
        assert_eq!((sum.staged, sum.already, sum.missing), (0, 2, 2), "{sum:?}");
        assert_eq!(s.hits("p1/full/full/0/default.jpg"), 1);
        assert_eq!(s.hits("p2/full/full/0/default.jpg"), 1);
    }

    #[test]
    fn the_hypothesis_file_has_the_gold_crops_with_a_read() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, spool) = (tmp.path().join("surya"), tmp.path().join("spool"));
        let page = crate::surya::work::tests::job();
        let gold = |c: &Job| {
            let dir = units(&root).join(unit_name(&c.doc_id, &c.crop.as_ref().unwrap().id));
            std::fs::write(
                dir.join(GOLD_FILE),
                format!("{{\"crop_id\":\"{}\"}}\n", c.crop.as_ref().unwrap().id),
            )
            .unwrap();
        };
        // cze-02 and cze-01 read (written out of order), cze-03 failed,
        // cze-04 is still out; srp-01 is a quality crop with no gold.json.
        let jobs: Vec<Job> = ["cze-02", "cze-01", "cze-03", "cze-04", "srp-01"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let i = i as u32;
                crop_job(&page, id, [10, 20 + 50 * i, 500, 60 + 50 * i], "gold/cze")
            })
            .collect();
        unit_with_result(
            &root,
            &spool,
            &jobs[0],
            &[(InputMode::Whole, Outcome::Ok, "second \"line\"\nof text\n")],
        );
        unit_with_result(
            &root,
            &spool,
            &jobs[1],
            &[(InputMode::Whole, Outcome::Ok, "first\n")],
        );
        unit_with_result(
            &root,
            &spool,
            &jobs[2],
            &[(InputMode::Whole, Outcome::Failed, "")],
        );
        unit_with_result(
            &root,
            &spool,
            &jobs[4],
            &[(InputMode::Whole, Outcome::Ok, "not a gold crop\n")],
        );
        let pending = units(&root).join(unit_name(&jobs[3].doc_id, "cze-04"));
        std::fs::create_dir_all(&pending).unwrap();
        std::fs::write(
            pending.join(JOB_FILE),
            serde_json::to_vec(&jobs[3]).unwrap(),
        )
        .unwrap();
        for j in &jobs[..4] {
            gold(j);
        }
        let out = tmp.path().join("gold").join("surya.jsonl");
        let s = hyp(&HypOptions {
            root: root.clone(),
            spool: spool.clone(),
            out: out.clone(),
        })
        .unwrap();
        assert_eq!(s, HypSummary { read: 2, unread: 2 });
        let rows: Vec<serde_json::Value> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            rows,
            vec![
                serde_json::json!({"crop_id": "cze-01", "text": "first\n"}),
                serde_json::json!({"crop_id": "cze-02", "text": "second \"line\"\nof text\n"}),
            ]
        );
        // With no gold.json anywhere, nothing is written and nothing counts.
        for j in &jobs[..4] {
            let c = j.crop.as_ref().unwrap();
            std::fs::remove_file(
                units(&root)
                    .join(unit_name(&j.doc_id, &c.id))
                    .join(GOLD_FILE),
            )
            .unwrap();
        }
        let s = hyp(&HypOptions {
            root,
            spool,
            out: out.clone(),
        })
        .unwrap();
        assert_eq!(s, HypSummary { read: 0, unread: 0 });
        assert_eq!(std::fs::read(&out).unwrap(), b"");
    }
}
