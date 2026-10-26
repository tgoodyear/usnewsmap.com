//! The page images a `usnm_qwen` unit's crops are cut from, shared by the
//! container worker (`usnm-boinc qwen work`) and the native app
//! (`usnm-qwen-app`) (#794).
//!
//! A single-page unit decodes its page once, up front. A bundle checks every
//! page up front (slice checksum, size from the image header, every crop
//! inside its page) but decodes one page at a time, as the crops reach it,
//! and drops the previous page first. Decoded memory is therefore one page's
//! however many pages the bundle holds, and the `image` crate's default
//! 512 MiB allocation limit bounds that page.
use anyhow::{ensure, Context};
use image::GenericImageView as _;
use usnm_ocr_contract::qwen::{Crop, Job};

/// The decoded page for the crop being read: at most one at a time.
#[derive(Default)]
pub struct QwenPages {
    current: Option<(String, image::DynamicImage)>,
}

impl QwenPages {
    /// Checks `blob` (`page.jpg` as read: one page, or a bundle's pages end
    /// to end) against `job` before any model loads, and decodes a single
    /// page.
    pub fn open(job: &Job, blob: &[u8]) -> anyhow::Result<Self> {
        if job.pages.is_empty() {
            let page = image::load_from_memory(blob).context("decoding page.jpg")?;
            check_inside(&job.crops, page.dimensions(), "")?;
            return Ok(Self {
                current: Some((job.doc_id.clone(), page)),
            });
        }
        for bp in &job.pages {
            let slice = bp
                .input
                .extract(blob, "page")
                .with_context(|| format!("bundle page {}", bp.doc_id))?;
            let size = image::ImageReader::new(std::io::Cursor::new(slice))
                .with_guessed_format()
                .context("reading a bundle page header")?
                .into_dimensions()
                .with_context(|| format!("reading the size of {}", bp.doc_id))?;
            check_inside(&bp.crops, size, &bp.doc_id)?;
        }
        Ok(Self::default())
    }

    /// The decoded page `crop` is cut from. In a bundle the crop id is
    /// `<doc_id>:<crop id>`; moving to another page drops the decoded one
    /// before decoding the next.
    pub fn page_for(
        &mut self,
        job: &Job,
        blob: &[u8],
        crop: &Crop,
    ) -> anyhow::Result<&image::DynamicImage> {
        if job.pages.is_empty() {
            return self
                .current
                .as_ref()
                .map(|(_, page)| page)
                .context("missing decoded page");
        }
        let (doc, _) = crop
            .id
            .split_once(':')
            .context("bundle crop id missing doc_id prefix")?;
        if self.current.as_ref().is_none_or(|(d, _)| d != doc) {
            self.current = None;
            let bp = job
                .pages
                .iter()
                .find(|p| p.doc_id == doc)
                .with_context(|| format!("bundle page {doc} not found"))?;
            let slice = bp.input.extract(blob, "page")?;
            let page = image::load_from_memory(slice)
                .with_context(|| format!("decoding page.jpg for {doc}"))?;
            self.current = Some((doc.to_owned(), page));
        }
        self.current
            .as_ref()
            .map(|(_, page)| page)
            .context("missing decoded page")
    }

    /// The doc id of the page decoded now, if any.
    pub fn decoded(&self) -> Option<&str> {
        self.current.as_ref().map(|(d, _)| d.as_str())
    }
}

fn check_inside(crops: &[Crop], (w, h): (u32, u32), doc: &str) -> anyhow::Result<()> {
    for c in crops {
        let [_, _, x1, y1] = c.rect;
        let id = if doc.is_empty() {
            c.id.clone()
        } else {
            format!("{doc}:{}", c.id)
        };
        ensure!(
            x1 <= w && y1 <= h,
            "crop {id} {:?} is outside a {w}x{h} page",
            c.rect
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::{
        quality::{BundleSlice, Input},
        qwen::{BundlePage, Engine, VERSION},
    };

    fn jpeg(w: u32, h: u32, shade: u8) -> Vec<u8> {
        let img = image::GrayImage::from_pixel(w, h, image::Luma([shade]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageLuma8(img)
            .write_to(&mut out, image::ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn crop(id: &str, rect: [u32; 4]) -> Crop {
        Crop {
            rect,
            id: id.into(),
            loc_text: String::new(),
            stratum: "body".into(),
        }
    }

    fn bundle(pages: &[(&str, Vec<u8>, [u32; 4])]) -> (Job, Vec<u8>) {
        let mut blob = Vec::new();
        let mut bundle_pages = Vec::new();
        let mut crops = Vec::new();
        for (doc, bytes, rect) in pages {
            bundle_pages.push(BundlePage {
                crops: vec![crop("c01", *rect)],
                doc_id: (*doc).into(),
                group: "g".into(),
                input: BundleSlice::of(blob.len() as u64, bytes, "test"),
                language: "eng".into(),
            });
            crops.push(crop(&format!("{doc}:c01"), *rect));
            blob.extend_from_slice(bytes);
        }
        let job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 60,
            crops,
            doc_id: pages[0].0.into(),
            engine: Engine {
                image: String::new(),
                llama: String::new(),
                models: vec![],
                weights: String::new(),
            },
            gpu_timeout_secs: 60,
            group: "g".into(),
            input: Input::of(&blob, "bundle:page.jpg"),
            language: "eng".into(),
            max_tokens_per_1000px2: 4,
            pages: bundle_pages,
            prompt_version: 1,
        };
        (job, blob)
    }

    #[test]
    fn bundle_decodes_one_page_at_a_time() {
        let (job, blob) = bundle(&[
            ("page_one", jpeg(40, 30, 10), [0, 0, 20, 10]),
            ("page_two", jpeg(64, 48, 200), [0, 0, 64, 48]),
        ]);
        let mut pages = QwenPages::open(&job, &blob).unwrap();
        assert_eq!(pages.decoded(), None, "a bundle decodes nothing up front");

        let first = pages.page_for(&job, &blob, &job.crops[0]).unwrap();
        assert_eq!(first.dimensions(), (40, 30));
        assert_eq!(pages.decoded(), Some("page_one"));

        let second = pages.page_for(&job, &blob, &job.crops[1]).unwrap();
        assert_eq!(second.dimensions(), (64, 48));
        assert_eq!(pages.decoded(), Some("page_two"), "the first page is gone");
    }

    #[test]
    fn bundle_crop_outside_its_page_fails_before_decoding() {
        let (job, blob) = bundle(&[
            ("page_one", jpeg(40, 30, 10), [0, 0, 20, 10]),
            ("page_two", jpeg(64, 48, 200), [0, 0, 65, 48]),
        ]);
        let err = QwenPages::open(&job, &blob).err().unwrap();
        assert!(format!("{err:#}").contains("page_two:c01"), "{err:#}");
    }

    #[test]
    fn single_page_decodes_up_front() {
        let page = jpeg(40, 30, 10);
        let (mut job, _) = bundle(&[("page_one", page.clone(), [0, 0, 20, 10])]);
        job.pages.clear();
        job.crops = vec![crop("c01", [0, 0, 20, 10])];
        job.input = Input::of(&page, "page.jpg");
        let mut pages = QwenPages::open(&job, &page).unwrap();
        assert_eq!(pages.decoded(), Some("page_one"));
        let p = pages.page_for(&job, &page, &job.crops[0]).unwrap();
        assert_eq!(p.dimensions(), (40, 30));

        job.crops = vec![crop("c01", [0, 0, 41, 10])];
        assert!(QwenPages::open(&job, &page).is_err());
    }
}
