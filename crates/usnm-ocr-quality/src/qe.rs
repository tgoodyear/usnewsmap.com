//! The reference-free quality measure (`usnm_core::qe`, docs/reports/2026-11-ocr-quality-estimation)
//! of each reader's text, when the unit carries the language's resource file
//! (`qe.bin`). The server's crop join measures crop texts with the same code.
use crate::report::ReaderInput;
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{round6_opt, QeReport, ReaderReport},
};

/// One language's resources, loaded and checked, with the file's sha256.
pub struct Measure {
    resources: usnm_core::qe::Resources,
    sha256: String,
}

impl Measure {
    pub fn load(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(Self {
            resources: usnm_core::qe::Resources::from_bytes(bytes)?,
            sha256: sha256(bytes),
        })
    }

    /// ISO 639-3.
    pub fn language(&self) -> &str {
        self.resources.language()
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// The measures of one text, rounded as every result is.
    pub fn report(&self, text: &str) -> QeReport {
        let s = self.resources.score(text);
        QeReport {
            version: usnm_core::qe::VERSION.to_owned(),
            resources: self.sha256.clone(),
            words: s.words,
            known3: s.known3,
            chars: s.chars,
            no_words: s.no_words,
            oov: round6_opt(s.oov),
            charlm_bpc: round6_opt(s.charlm_bpc),
            lm_oov: round6_opt(s.lm_oov),
        }
    }

    /// Measures each reader's text (by name, from the texts the unit read).
    pub fn add(&self, readers: &mut [ReaderReport], texts: &[ReaderInput]) {
        for r in readers {
            if let Some(t) = texts.iter().find(|t| t.name == r.name) {
                r.qe = Some(self.report(&t.text));
            }
        }
    }
}
