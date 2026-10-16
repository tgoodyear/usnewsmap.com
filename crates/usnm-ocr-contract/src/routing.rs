//! Production engine selection, independent of node capabilities.
use serde::{Deserialize, Serialize};

/// The engine family selected for a page. Missing fields in legacy jobs
/// mean Tesseract; serializing that default must not change job identities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineKind {
    #[default]
    #[serde(rename = "tesseract")]
    Tesseract,
    #[serde(rename = "ndlocr-lite")]
    NdlocrLite,
}

impl EngineKind {
    pub fn for_language(language: &str) -> Self {
        match language {
            "jpn" => Self::NdlocrLite,
            _ => Self::Tesseract,
        }
    }

    pub fn family(self) -> &'static str {
        match self {
            Self::Tesseract => "tesseract",
            Self::NdlocrLite => "ndlocr-lite",
        }
    }

    pub fn is_tesseract(&self) -> bool {
        *self == Self::Tesseract
    }
}

/// An engine and its model profile, before an image/version is pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineRoute {
    pub kind: EngineKind,
    pub profile: String,
}
