//! Native BOINC worker binary for `usnm_quality` (#533).
//!
//! Runs directly in a BOINC slot on Linux, macOS, and Windows without Docker,
//! Podman, `boinc-buda-runner`, or `libboinc_api`: resolves inputs (direct
//! `<copy_file/>` or `<soft_link>` XML), computes the deterministic quality
//! report and confusion pairs, writes `result.json`, `confusions.json`,
//! `diag.txt`, `fraction_done`, and finishes by writing `boinc_finish_called`
//! (`"0\n"`).
use std::path::Path;

fn main() -> anyhow::Result<()> {
    usnm_ocr_quality::slot::run_slot(Path::new("."))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use usnm_ocr_contract::{
        boinc::sha256,
        quality::{
            Engine, Input, Inputs as JobInputs, Job, Outcome, PageSize, ReaderSpec, ResultManifest,
            TextReader, Texts, VERSION,
        },
    };
    use usnm_ocr_quality::slot::{
        run_slot, ALTO_FILE, CONFUSIONS_FILE, DIAG_FILE, FINISH_FILE, FRACTION_FILE, JOB_FILE,
        PAGE_FILE, PLACES_FILE, RESULT_FILE, TEXTS_FILE,
    };

    const TRUTH: &str =
        "The mayor of Albany spoke with the council about bridges and roads across the river";

    fn synthetic_jpeg(w: u32, h: u32) -> Vec<u8> {
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

    #[allow(clippy::type_complexity)]
    fn alto_xml(
        width: u32,
        height: u32,
        blocks: &[Vec<Vec<(u32, u32, u32, u32, &str, f64)>>],
    ) -> String {
        let mut s = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<alto xmlns=\"http://www.loc.gov/standards/alto/ns-v3#\"><Description><MeasurementUnit>pixel</MeasurementUnit></Description><Layout><Page ID=\"P1\" WIDTH=\"{width}\" HEIGHT=\"{height}\" PHYSICAL_IMG_NR=\"1\"><PrintSpace HPOS=\"0\" VPOS=\"0\" WIDTH=\"{width}\" HEIGHT=\"{height}\">"
        );
        for (bi, lines) in blocks.iter().enumerate() {
            let all: Vec<&(u32, u32, u32, u32, &str, f64)> = lines.iter().flatten().collect();
            let bx0 = all.iter().map(|w| w.0).min().unwrap_or(0);
            let by0 = all.iter().map(|w| w.1).min().unwrap_or(0);
            let bx1 = all.iter().map(|w| w.2).max().unwrap_or(0);
            let by1 = all.iter().map(|w| w.3).max().unwrap_or(0);
            s.push_str(&format!(
                "<TextBlock ID=\"B{bi}\" HPOS=\"{bx0}\" VPOS=\"{by0}\" WIDTH=\"{}\" HEIGHT=\"{}\">",
                bx1 - bx0,
                by1 - by0
            ));
            for (li, words) in lines.iter().enumerate() {
                let lx0 = words.iter().map(|w| w.0).min().unwrap_or(0);
                let ly0 = words.iter().map(|w| w.1).min().unwrap_or(0);
                let lx1 = words.iter().map(|w| w.2).max().unwrap_or(0);
                let ly1 = words.iter().map(|w| w.3).max().unwrap_or(0);
                s.push_str(&format!(
                    "<TextLine ID=\"B{bi}L{li}\" HPOS=\"{lx0}\" VPOS=\"{ly0}\" WIDTH=\"{}\" HEIGHT=\"{}\">",
                    lx1 - lx0,
                    ly1 - ly0
                ));
                for (wi, (x0, y0, x1, y1, text, wc)) in words.iter().enumerate() {
                    let text = text
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('"', "&quot;");
                    s.push_str(&format!(
                        "<String ID=\"B{bi}L{li}W{wi}\" HPOS=\"{x0}\" VPOS=\"{y0}\" WIDTH=\"{}\" HEIGHT=\"{}\" CONTENT=\"{text}\" WC=\"{wc:.2}\"/>",
                        x1 - x0,
                        y1 - y0
                    ));
                    if wi + 1 < words.len() {
                        s.push_str("<SP/>");
                    }
                }
                s.push_str("</TextLine>");
            }
            s.push_str("</TextBlock>");
        }
        s.push_str("</PrintSpace></Page></Layout></alto>\n");
        s
    }

    fn write_synthetic_unit(dir: &Path) -> Job {
        std::fs::create_dir_all(dir).unwrap();
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
        let alto = alto_xml(
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
        let texts_bytes = serde_json::to_vec_pretty(&texts).unwrap();
        let places = b"Albany\nTroy\n";
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            crop_seed: 522,
            doc_id: "local".into(),
            engine: Engine {
                image: format!("sha256:{}", "a".repeat(64)),
            },
            function_words: vec!["with".into(), "the".into(), "about".into()],
            group: "local".into(),
            inputs: JobInputs {
                alto: Input::of(alto.as_bytes(), "local:loc.xml"),
                page: Input::of(&page, "local:page.jpg"),
                places: Input::of(places, "local:places.txt"),
                texts: Input::of(&texts_bytes, "local:texts.json"),
            },
            k_crops: 4,
            language: "eng".into(),
            readers: texts
                .readers
                .iter()
                .map(|r| ReaderSpec {
                    name: r.name.clone(),
                    source: r.source.clone(),
                })
                .collect(),
        };
        job.id = job.identity().unwrap();
        std::fs::write(dir.join(PAGE_FILE), &page).unwrap();
        std::fs::write(dir.join(ALTO_FILE), alto.as_bytes()).unwrap();
        std::fs::write(dir.join(TEXTS_FILE), &texts_bytes).unwrap();
        std::fs::write(dir.join(PLACES_FILE), places).unwrap();
        std::fs::write(dir.join(JOB_FILE), serde_json::to_vec_pretty(&job).unwrap()).unwrap();
        job
    }

    #[test]
    fn golden_determinism_and_boinc_finish_called() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = tmp.path().join("slot");
        write_synthetic_unit(&slot);
        run_slot(&slot).unwrap();

        assert_eq!(
            std::fs::read_to_string(slot.join(FINISH_FILE)).unwrap(),
            "0\n"
        );
        assert_eq!(
            std::fs::read_to_string(slot.join(FRACTION_FILE)).unwrap(),
            "1.0\n"
        );

        let result_bytes = std::fs::read(slot.join(RESULT_FILE)).unwrap();
        let confusions_bytes = std::fs::read(slot.join(CONFUSIONS_FILE)).unwrap();
        let diag_bytes = std::fs::read(slot.join(DIAG_FILE)).unwrap();
        let result: ResultManifest = serde_json::from_slice(&result_bytes).unwrap();
        assert_eq!(result.outcome, Outcome::Ok);
        result.check(&confusions_bytes, &diag_bytes).unwrap();

        let canonical_bytes = serde_json::to_vec(&result.canonical()).unwrap();
        assert_eq!(
            sha256(&canonical_bytes),
            "7eaca929fe05fcd39d6423dfb269e6cbec6ccdd8d80b4d385946e8898c99215a"
        );
        assert_eq!(
            sha256(&confusions_bytes),
            "8c5731b9b855fe02102b74081aba55d7a719f6d42acc9ae040ac67e1a4a04df6"
        );

        // Verify BOINC <soft_link> resolution produces identical canonical output.
        let project_dir = tmp.path().join("projects/usnewsmap");
        let soft_slot = tmp.path().join("soft_slot");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(&soft_slot).unwrap();
        for name in [PAGE_FILE, ALTO_FILE, TEXTS_FILE, PLACES_FILE, JOB_FILE] {
            let phys = project_dir.join(format!("phys_{name}"));
            std::fs::copy(slot.join(name), &phys).unwrap();
            std::fs::write(
                soft_slot.join(name),
                format!("<soft_link>{}</soft_link>\n", phys.display()),
            )
            .unwrap();
        }
        run_slot(&soft_slot).unwrap();
        assert_eq!(
            std::fs::read_to_string(soft_slot.join(FINISH_FILE)).unwrap(),
            "0\n"
        );
        let soft_result: ResultManifest =
            serde_json::from_slice(&std::fs::read(soft_slot.join(RESULT_FILE)).unwrap()).unwrap();
        assert_eq!(
            sha256(&serde_json::to_vec(&soft_result.canonical()).unwrap()),
            sha256(&canonical_bytes)
        );
        assert_eq!(
            sha256(&std::fs::read(soft_slot.join(CONFUSIONS_FILE)).unwrap()),
            sha256(&confusions_bytes)
        );
    }
}
