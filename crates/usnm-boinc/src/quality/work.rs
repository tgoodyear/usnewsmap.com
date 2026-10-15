//! One quality unit inside BOINC's job container or native slot. No network.
//!
//! [`compute`] is the whole computation as a library call: the page's
//! features, the readers' pairwise metrics, the hat, the consensus,
//! findability and confusion pairs, with timings. [`run`] wraps
//! [`usnm_ocr_quality::slot::run_slot`] in the slot.
use std::path::Path;
pub use usnm_ocr_quality::slot::{
    compute, cpu_ms, peak_rss_kib, prepare, run_slot, Computed, Inputs, Loaded, DEADLINE,
};

pub async fn run(work: &Path) -> anyhow::Result<()> {
    let work = work.to_owned();
    tokio::task::spawn_blocking(move || run_slot(&work)).await?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::quality::{outputs, PAGE_FILE};
    use std::path::PathBuf;
    use usnm_ocr_contract::quality::{Engine, Job, Outcome, PageSize, TextReader, Texts};

    /// A light page with dark text-like rows, as a JPEG.
    pub(crate) fn synthetic_jpeg(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::GrayImage::from_pixel(w, h, image::Luma([235]));
        let mut row = 60;
        while row + 6 < h - 60 {
            for x in 40..w - 40 {
                if (x / 10) % 10 == 9 {
                    continue;
                }
                for dy in 0..6 {
                    img.put_pixel(x, row + dy, image::Luma([30]));
                }
            }
            row += 24;
        }
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    pub(crate) const TRUTH: &str =
        "The mayor of Albany spoke with the council about bridges and roads across the river";

    /// A synthetic unit: page, ALTO with the truth's words in two lines,
    /// three readers (one exact, two corrupted), two place names.
    pub(crate) fn unit(dir: &Path) -> Job {
        let page = synthetic_jpeg(1000, 1400);
        let words: Vec<&str> = TRUTH.split(' ').collect();
        fn line<'a>(ws: &[&'a str], y: u32) -> Vec<(u32, u32, u32, u32, &'a str, f64)> {
            ws.iter()
                .enumerate()
                .map(|(i, w)| {
                    (
                        50 + i as u32 * 95,
                        y,
                        50 + i as u32 * 95 + 85,
                        y + 30,
                        *w,
                        0.9,
                    )
                })
                .collect()
        }
        let alto = super::super::alto_xml(
            1000,
            1400,
            &[vec![line(&words[..8], 60), line(&words[8..], 100)]],
        );
        let texts = Texts {
            page: Some(PageSize {
                height: 1400,
                width: 1000,
            }),
            readers: vec![
                TextReader {
                    alto: false,
                    name: "loc".into(),
                    regions: vec![],
                    source: "loc-alto".into(),
                    text: TRUTH.into(),
                },
                TextReader {
                    alto: false,
                    name: "as".into(),
                    regions: vec![],
                    source: "american-stories".into(),
                    text: "The mayor of Albany spoke with the counci1 about bridges and roads across the rivor".into(),
                },
                TextReader {
                    alto: false,
                    name: "ours-tesseract".into(),
                    regions: vec![],
                    source: "ocr-out:tesseract".into(),
                    text: "The rnayor of Albany spoke with the council about bridges and roads".into(),
                },
            ],
        };
        super::super::stage::local_unit(&super::super::stage::LocalUnit {
            page: &page,
            alto: alto.as_bytes(),
            texts: &texts,
            places: "Albany\nTroy\n",
            dir,
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
            },
            function_words: vec!["with".into(), "the".into(), "about".into()],
            crop_seed: 522,
            k_crops: 4,
        })
        .unwrap()
    }

    #[test]
    fn compute_on_a_synthetic_page() {
        let tmp = tempfile::tempdir().unwrap();
        let job = unit(tmp.path());
        let l = prepare(tmp.path()).unwrap();
        assert_eq!(l.job, job);
        let c = compute(&Inputs {
            job: &l.job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
        })
        .unwrap();
        let r = &c.result;
        assert_eq!(r.outcome, Outcome::Ok);
        assert_eq!(r.readers.len(), 3);
        assert!(r.hat.as_ref().unwrap().determined);
        // The exact reader's estimate is near zero, not zero: the two
        // corrupted readers' errors overlap in the dropped tail.
        let loc = r.readers[0].est_cer.unwrap();
        assert!(loc < 0.02, "{loc}");
        assert!(r.readers[1].est_cer.unwrap() > loc);
        assert!(r.readers[2].est_cer.unwrap() > loc);
        assert!(r.readers[2].completeness.unwrap() < 1.0);
        assert_eq!(r.readers[0].findable_places, Some(1.0));
        let page = r.page.as_ref().unwrap();
        let img = page.image.as_ref().unwrap();
        assert_eq!((img.width, img.height), (1000, 1400));
        assert!(img.skew_deg.abs() <= 0.1);
        let layout = page.layout.as_ref().unwrap();
        assert_eq!((layout.blocks, layout.lines, layout.words), (1, 2, 15));
        assert_eq!(layout.columns, 1);
        let confusions: std::collections::BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&c.confusions).unwrap();
        assert_eq!(confusions.len(), 3);
        // The same inputs give the same bytes.
        let again = compute(&Inputs {
            job: &l.job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
        })
        .unwrap();
        assert_eq!(again.result.canonical(), r.canonical());
        assert_eq!(again.confusions, c.confusions);
    }

    #[tokio::test]
    async fn unit_writes_all_outputs_and_validates() {
        let tmp = tempfile::tempdir().unwrap();
        unit(tmp.path()).validate().unwrap();
        run(tmp.path()).await.unwrap();
        let files: Vec<PathBuf> = outputs().iter().map(|n| tmp.path().join(n)).collect();
        let l = crate::quality::validate::load(&files).unwrap();
        assert_eq!(l.result.outcome, Outcome::Ok);
        assert!(tmp.path().join("fraction_done").exists());
        // A broken page still returns a result, with the reason.
        std::fs::write(tmp.path().join(PAGE_FILE), b"not the page").unwrap();
        run(tmp.path()).await.unwrap();
        let l = crate::quality::validate::load(&files).unwrap();
        assert_eq!(l.result.outcome, Outcome::UnitError);
        assert!(l.result.unit_error.unwrap().contains("checksum"));
    }
}
