//! Fixtures the modules' tests share: a synthetic page, fake weights and a staged unit.
use crate::{locks::EMBEDDED_LLAMA_LOCK, JOB_FILE};
use std::path::Path;
use usnm_ocr_contract::{
    boinc::sha256,
    quality::Input,
    qwen::{Crop, Engine, Job, Model, PROMPT_VERSION, VERSION},
};
use usnm_ocr_quality::slot::PAGE_FILE;

pub(crate) fn synthetic_jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::GrayImage::from_pixel(w, h, image::Luma([220]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

pub(crate) const FAKE_GGUF: &[u8] = b"fake-gguf-weights";
pub(crate) const FAKE_MMPROJ: &[u8] = b"fake-mmproj-weights";

pub(crate) fn test_weights_lock_bytes() -> Vec<u8> {
    let gguf_sum = sha256(FAKE_GGUF);
    let mmproj_sum = sha256(FAKE_MMPROJ);
    format!(
        "# synthetic weights.lock for unit tests\n\
         Qwen3VL-8B-Instruct-Q4_K_M.gguf https://example.invalid/8b.gguf {gguf_sum}\n\
         mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf https://example.invalid/8b.mmproj {mmproj_sum}\n\
         Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.gguf {gguf_sum}\n\
         mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf https://example.invalid/2b.mmproj {mmproj_sum}\n"
    )
    .into_bytes()
}

pub(crate) fn write_qwen_unit(dir: &Path) -> Job {
    std::fs::create_dir_all(dir).unwrap();
    let page = synthetic_jpeg(200, 200);
    let weights_lock = test_weights_lock_bytes();
    std::fs::write(dir.join("weights.lock"), &weights_lock).unwrap();
    let models = vec![
        Model {
            file: "Qwen3VL-8B-Instruct-Q4_K_M.gguf".into(),
            min_gpu_mb: 7000,
            mmproj: "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf".into(),
            name: "qwen3-vl-8b-q4km".into(),
        },
        Model {
            file: "Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            min_gpu_mb: 0,
            mmproj: "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            name: "qwen3-vl-2b-q8".into(),
        },
    ];
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        cpu_timeout_secs: 600,
        crops: vec![
            Crop {
                rect: [10, 10, 100, 60],
                id: "c01".into(),
                loc_text: "first crop".into(),
                stratum: "wc-low/top".into(),
            },
            Crop {
                rect: [10, 80, 100, 140],
                id: "c02".into(),
                loc_text: "second crop".into(),
                stratum: "wc-high/bottom".into(),
            },
        ],
        doc_id: "sn85055101_1917-04-03_ed-1_seq-3".into(),
        engine: Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", sha256(EMBEDDED_LLAMA_LOCK)),
            models,
            weights: format!("sha256:{}", sha256(&weights_lock)),
        },
        gpu_timeout_secs: 300,
        group: "serbian".into(),
        input: Input::of(&page, "local:page.jpg"),
        language: "srp".into(),
        max_tokens_per_1000px2: 4,
        pages: vec![],
        prompt_version: PROMPT_VERSION,
    };
    job.id = job.identity().unwrap();
    std::fs::write(dir.join(PAGE_FILE), &page).unwrap();
    std::fs::write(dir.join(JOB_FILE), serde_json::to_vec_pretty(&job).unwrap()).unwrap();
    job
}
