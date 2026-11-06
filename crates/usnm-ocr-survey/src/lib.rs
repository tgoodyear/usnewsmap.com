//! The OCR gold set in Rust (#296, #457): text normalization and CER/WER scoring that give the same
//! numbers as `scripts/ocr-survey/gold.py`, and the versioned, checksummed bundle of public ground
//! truth that BOINC volunteers fetch, verify, materialize and score locally. docs/ocr-gold-set.md
//! has the rules and the source policy. [`position`] scores engines by where each word sits on the
//! page, against another engine's ALTO (#1438).

pub mod bench;
pub mod bundle;
pub mod position;
pub mod qwen_bench;
pub mod score;
pub use usnm_ocr_quality::{distance, text};
