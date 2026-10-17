//! Deterministic, damage-conditioned OCR engine and profile router (`damage-v1`).
//!
//! Selects the OCR engine family, language/script model profile, page
//! segmentation mode (PSM), image scale factor, column-slicing policy, and
//! GPU fallback escalation from a page's physical image features
//! ([`ImageFeatures`]), layout geometry ([`LayoutFeatures`]), and optional
//! lexical damage or 3-cornered-hat legibility ceiling.
//!
//! Thresholds are calibrated against the 15,451-page / 34,459-reader BOINC
//! `usnm_quality` dataset across 14 reader configurations:
//! - `ours-tesseract` cuts lexical `damage_rate` from `0.3167` (`loc-hist`) to
//!   `0.1797` overall (winning on 93.9% of pages), and is the default CPU workhorse.
//! - On German and multi-script pages (`deu`, `srp`, `ces`), script-aware
//!   profiles (`frak2021`, `deu_latf+deu`, `srp+srp_latn`) cut CER from `0.434`
//!   (`tess-deu`) / `0.588` (`tess-latn`) down to `0.181–0.204`.
//! - On high-resolution scans (`width >= 3000`) with heavy bleed-through or
//!   speckle (`bleed_through >= 0.15` and `ink_density >= 0.22`) and normal
//!   type size (`line_height_px >= 28`), `0.5x` downscaled Tesseract
//!   (`tess-half`) reduces `garbage_share` (`0.0120` vs `0.0151`) and improves
//!   `function_share` (`0.2830` vs `0.2717`) at ~3x throughput.
//! - On dense broadsheets (`columns >= 6` or `line_height_px < 26`), `psm = 6`
//!   with pre-sliced columns recovers +29% more known words than `psm = 4`
//!   while avoiding cross-column line bridging.
//! - On severely degraded scans (`mean_word_conf < 0.55`, `prior_damage_rate >= 0.30`,
//!   or faint low-contrast ink `ink_density < 0.12 && contrast < 90`),
//!   Tesseract lexical `damage_rate` spikes to `0.3554`, whereas `surya`
//!   (and `usnm_qwen` VLM on crops) achieves `0.0475` `damage_rate` (`0.0051`
//!   `garbage_share`) when columns are pre-sliced.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::quality::PageFeatures;

/// Identifier for this routing rule in `ocr_engine` tags (`route=damage-v1@r1`).
pub const ROUTE_ID: &str = "damage-v1";
/// Revision of the `damage-v1` routing thresholds.
pub const ROUTE_VERSION: u32 = 1;

/// Target OCR engine family selected for the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EngineFamily {
    /// CPU Tesseract 5 LSTM with language/script profile and tuned PSM/scale.
    Tesseract,
    /// GPU/CPU Surya line-level recognition on column-sliced crops.
    Surya,
    /// GPU Qwen2.5-VL vision-language transcription on severe damage / low-legibility crops.
    QwenVlm,
}

impl EngineFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tesseract => "tesseract",
            Self::Surya => "surya",
            Self::QwenVlm => "qwen-vlm",
        }
    }
}

/// Primary physical/layout/lexical regime that governed the route decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DamageRegime {
    /// Clean or moderate scan where full-resolution Tesseract with standard layout wins.
    CleanStandard,
    /// Historical Fraktur or multi-script newspaper requiring dual/script models.
    MultiScriptFraktur,
    /// High-bleed or heavy-speckle high-res scan where 0.5x box-downscaling acts as a low-pass filter.
    HighBleedSpeckle,
    /// Dense 6+ column broadsheet or small type (<26 px) requiring column slicing and PSM 6.
    DenseBroadsheet,
    /// Severely degraded scan (low confidence, high lexical damage, or faint/blurred ink) escalated to GPU OCR/VLM.
    SeverelyDegraded,
}

impl DamageRegime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CleanStandard => "clean_standard",
            Self::MultiScriptFraktur => "multi_script_fraktur",
            Self::HighBleedSpeckle => "high_bleed_speckle",
            Self::DenseBroadsheet => "dense_broadsheet",
            Self::SeverelyDegraded => "severely_degraded",
        }
    }
}

/// Complete deterministic routing decision for a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteDecision {
    /// Whether the worker should deskew the image before segmentation (`|skew_deg| >= 0.8`).
    pub deskew: bool,
    /// Selected OCR engine family.
    pub family: EngineFamily,
    /// Whether the page should be queued for GPU neural/VLM transcription.
    pub gpu_escalate: bool,
    /// Model profile identifier (e.g. `frak2021`, `deu_latf+deu`, `srp+srp_latn`, `eng`).
    pub profile: String,
    /// Tesseract Page Segmentation Mode (`4` = single column of variable sizes, `6` = uniform block).
    pub psm: u8,
    /// Ordered human- and machine-readable reasons for the routing decision.
    pub reasons: Vec<String>,
    /// Classified damage/layout regime.
    pub regime: DamageRegime,
    /// Route identifier (`damage-v1`).
    pub route_id: String,
    /// Route revision (`1`).
    pub route_version: u32,
    /// Image scaling factor prior to OCR (`1.0` full resolution, `0.5` half resolution).
    pub scale: f64,
    /// Whether multi-column pages must be sliced into column strips before OCR.
    pub slice_columns: bool,
}

/// Canonical ISO 639-3 code for a newspaper language code (normalizing
/// ISO 639-2B library codes used in Chronicling America metadata).
pub fn canonical_language(language: &str) -> &str {
    match language.trim() {
        "" | "und" => "eng",
        "ger" => "deu",
        "fre" => "fra",
        "cze" => "ces",
        "rum" => "ron",
        "ice" => "isl",
        "wel" => "cym",
        "dut" => "nld",
        "slo" => "slk",
        other => other,
    }
}

/// Ordered candidate profiles for a language, best first.
pub fn profile_candidates(language: &str) -> Vec<&'static str> {
    match canonical_language(language) {
        "deu" => vec!["frak2021", "deu_latf+deu", "deu"],
        "srp" => vec!["srp+srp_latn", "srp"],
        "hrv" | "bos" => vec!["hrv", "srp_latn"],
        "ces" => vec!["ces+deu_latf", "ces"],
        "nob" | "nno" | "nor" => vec!["nor"],
        "zho" => vec!["chi_tra", "chi_sim"],
        "yid" => vec!["yid", "heb"],
        "ell" | "grc" => vec!["ell", "grc"],
        "eng" => vec!["eng"],
        "spa" => vec!["spa"],
        "ita" => vec!["ita"],
        "fra" => vec!["fra"],
        "pol" => vec!["pol"],
        "fin" => vec!["fin"],
        "swe" => vec!["swe"],
        "slv" => vec!["slv"],
        "rus" => vec!["rus"],
        "heb" => vec!["heb"],
        "chr" => vec!["chr"],
        "haw" => vec!["haw+eng", "eng"],
        "hun" => vec!["hun"],
        "lav" => vec!["lav"],
        "lit" => vec!["lit"],
        "por" => vec!["por"],
        "ron" => vec!["ron"],
        "slk" => vec!["slk"],
        "ukr" => vec!["ukr"],
        "dan" => vec!["dan"],
        "nld" => vec!["nld"],
        "isl" => vec!["isl"],
        "gle" => vec!["gle"],
        "cym" => vec!["cym"],
        _ => vec!["eng"],
    }
}

/// Deterministic page routing decision from `PageFeatures` and optional prior
/// lexical `damage_rate` (from `usnm_core::damage`) and 3-cornered-hat
/// `legibility_ceiling`.
pub fn route_page(
    language: &str,
    page: &PageFeatures,
    prior_damage_rate: Option<f64>,
    legibility_ceiling: Option<f64>,
) -> RouteDecision {
    let lang = canonical_language(language);
    let primary_profile = profile_candidates(lang)
        .first()
        .copied()
        .unwrap_or(lang)
        .to_owned();

    let img = page.image.as_ref();
    let lay = page.layout.as_ref();

    let cols = lay.map(|l| l.columns).unwrap_or(1);
    let line_height = lay.and_then(|l| l.line_height_px).unwrap_or(34.0);
    let mean_conf = lay.and_then(|l| l.mean_word_conf);

    let blur = img.map(|i| i.blur).unwrap_or(5000.0);
    let contrast = img.map(|i| i.contrast).unwrap_or(180.0);
    let bleed = img.map(|i| i.bleed_through).unwrap_or(0.08);
    let ink = img.map(|i| i.ink_density).unwrap_or(0.20);
    let noise = img.map(|i| i.noise).unwrap_or(2.0);
    let skew_deg = img.map(|i| i.skew_deg).unwrap_or(0.0);
    let width = img.map(|i| i.width).unwrap_or(2400);

    let mut reasons = Vec::new();
    let is_multiscript = matches!(lang, "deu" | "srp" | "ces" | "yid" | "zho" | "haw");
    if is_multiscript {
        reasons.push(format!("script_profile:{lang}->{primary_profile}"));
    }

    let deskew = skew_deg.abs() >= 0.8;
    if deskew {
        reasons.push(format!("deskew:skew_deg={skew_deg:.2}"));
    }

    let dense_broadsheet = cols >= 6 || line_height < 26.0;
    if cols >= 6 {
        reasons.push(format!("dense_columns:cols={cols}"));
    }
    if line_height < 26.0 {
        reasons.push(format!("small_type:line_height_px={line_height:.1}"));
    }

    let low_conf = mean_conf.is_some_and(|c| c < 0.55);
    let high_lexical_damage = prior_damage_rate.is_some_and(|d| d >= 0.30);
    let faint_or_washed = (ink < 0.12 && contrast < 95.0) || contrast < 65.0 || blur < 900.0;
    let low_ceiling = legibility_ceiling.is_some_and(|c| c < 0.55);

    let severely_degraded = low_conf || high_lexical_damage || faint_or_washed || low_ceiling;

    if severely_degraded {
        if let Some(c) = mean_conf.filter(|&c| c < 0.55) {
            reasons.push(format!("low_word_conf:{c:.3}"));
        }
        if let Some(d) = prior_damage_rate.filter(|&d| d >= 0.30) {
            reasons.push(format!("high_prior_damage:{d:.3}"));
        }
        if faint_or_washed {
            reasons.push(format!(
                "faint_or_blurred:ink={ink:.3},contrast={contrast:.1},blur={blur:.1}"
            ));
        }
        if let Some(ceil) = legibility_ceiling.filter(|&c| c < 0.55) {
            reasons.push(format!("low_legibility_ceiling:{ceil:.3}"));
        }

        // Choose QwenVlm for extreme damage / collapsed ceiling, Surya for column-sliced line OCR.
        let extreme = legibility_ceiling.is_some_and(|c| c < 0.40)
            || prior_damage_rate.is_some_and(|d| d >= 0.50)
            || (faint_or_washed && low_conf);
        let family = if extreme {
            reasons.push("escalate:qwen-vlm-crops".to_owned());
            EngineFamily::QwenVlm
        } else {
            reasons.push("escalate:surya-column-slices".to_owned());
            EngineFamily::Surya
        };

        return RouteDecision {
            deskew,
            family,
            gpu_escalate: true,
            profile: primary_profile,
            psm: if dense_broadsheet { 6 } else { 4 },
            reasons,
            regime: DamageRegime::SeverelyDegraded,
            route_id: ROUTE_ID.to_owned(),
            route_version: ROUTE_VERSION,
            scale: 1.0,
            // Neural readers require column/region slicing on multi-column broadsheets
            // to avoid the full-page completeness drop (0.34–0.41 vs 0.83).
            slice_columns: cols >= 2 || dense_broadsheet,
        };
    }

    // High-resolution scan with bleed-through or top-decile median-filter noise (p90 = 14.5) and comfortable type size:
    // 0.5x downscaling (`tess-half`) smooths speckle/halftone dots, cutting garbage_share by 20% at 3x speed.
    let high_bleed_speckle = width >= 2800
        && line_height >= 28.0
        && ((bleed >= 0.18 && ink >= 0.25) || noise >= 14.5);
    if high_bleed_speckle {
        reasons.push(format!(
            "half_scale_denoise:bleed={bleed:.3},ink={ink:.3},noise={noise:.2},lh={line_height:.1}"
        ));
    }

    let psm = if dense_broadsheet { 6 } else { 4 };
    let scale = if high_bleed_speckle { 0.5 } else { 1.0 };
    let slice_columns = dense_broadsheet;

    let regime = if is_multiscript {
        DamageRegime::MultiScriptFraktur
    } else if dense_broadsheet {
        DamageRegime::DenseBroadsheet
    } else if high_bleed_speckle {
        DamageRegime::HighBleedSpeckle
    } else {
        reasons.push("standard_full_res_tesseract".to_owned());
        DamageRegime::CleanStandard
    };

    RouteDecision {
        deskew,
        family: EngineFamily::Tesseract,
        gpu_escalate: false,
        profile: primary_profile,
        psm,
        reasons,
        regime,
        route_id: ROUTE_ID.to_owned(),
        route_version: ROUTE_VERSION,
        scale,
        slice_columns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::quality::{ImageFeatures, LayoutFeatures};

    fn sample_features(
        width: u32,
        bleed_through: f64,
        ink_density: f64,
        contrast: f64,
        blur: f64,
        columns: usize,
        line_height_px: f64,
        mean_word_conf: f64,
    ) -> PageFeatures {
        PageFeatures {
            image: Some(ImageFeatures {
                bleed_through,
                blur,
                contrast,
                height: 5200,
                ink_density,
                noise: 2.5,
                scale: 4,
                skew_deg: 0.2,
                threshold: 165,
                width,
            }),
            layout: Some(LayoutFeatures {
                blocks: columns * 3,
                columns,
                line_height_px: Some(line_height_px),
                lines: 250,
                mean_word_conf: Some(mean_word_conf),
                words: 2200,
            }),
        }
    }

    #[test]
    fn clean_english_page_uses_full_res_tesseract_psm4() {
        let page = sample_features(3600, 0.08, 0.19, 195.0, 6800.0, 4, 36.0, 0.82);
        let d = route_page("eng", &page, Some(0.11), Some(0.88));
        assert_eq!(d.family, EngineFamily::Tesseract);
        assert_eq!(d.regime, DamageRegime::CleanStandard);
        assert_eq!(d.profile, "eng");
        assert_eq!(d.psm, 4);
        assert_eq!(d.scale, 1.0);
        assert!(!d.slice_columns);
        assert!(!d.gpu_escalate);
    }

    #[test]
    fn german_fraktur_page_selects_frak2021_profile() {
        let page = sample_features(3800, 0.09, 0.21, 180.0, 7200.0, 4, 48.0, 0.74);
        let d = route_page("ger", &page, Some(0.14), Some(0.85));
        assert_eq!(d.family, EngineFamily::Tesseract);
        assert_eq!(d.regime, DamageRegime::MultiScriptFraktur);
        assert_eq!(d.profile, "frak2021");
        assert!(!d.gpu_escalate);
    }

    #[test]
    fn high_bleed_high_res_page_routes_to_half_scale_tesseract() {
        // Matches Czech/German/English high-bleed scans where tess-half beats full-res on garbage_share.
        let page = sample_features(4200, 0.23, 0.31, 175.0, 6500.0, 5, 42.0, 0.66);
        let d = route_page("eng", &page, Some(0.21), Some(0.76));
        assert_eq!(d.family, EngineFamily::Tesseract);
        assert_eq!(d.regime, DamageRegime::HighBleedSpeckle);
        assert_eq!(d.scale, 0.5);
        assert_eq!(d.psm, 4);
        assert!(!d.gpu_escalate);
    }

    #[test]
    fn dense_seven_column_small_type_broadsheet_uses_psm6_and_column_slicing() {
        let page = sample_features(4400, 0.10, 0.22, 190.0, 6200.0, 7, 23.5, 0.75);
        let d = route_page("eng", &page, Some(0.17), Some(0.81));
        assert_eq!(d.family, EngineFamily::Tesseract);
        assert_eq!(d.regime, DamageRegime::DenseBroadsheet);
        assert_eq!(d.psm, 6);
        assert_eq!(d.scale, 1.0);
        assert!(d.slice_columns);
    }

    #[test]
    fn low_confidence_or_faint_page_escalates_to_gpu_with_column_slicing() {
        // Moderate-to-severe degradation -> Surya on column slices.
        let page = sample_features(3600, 0.14, 0.18, 170.0, 5400.0, 6, 32.0, 0.41);
        let d = route_page("eng", &page, Some(0.34), Some(0.68));
        assert_eq!(d.regime, DamageRegime::SeverelyDegraded);
        assert_eq!(d.family, EngineFamily::Surya);
        assert!(d.gpu_escalate);
        assert!(d.slice_columns);

        // Extreme faintness + low confidence -> Qwen VLM crops.
        let faint_page = sample_features(3600, 0.04, 0.06, 58.0, 750.0, 7, 28.0, 0.35);
        let d_extreme = route_page("eng", &faint_page, Some(0.55), Some(0.38));
        assert_eq!(d_extreme.regime, DamageRegime::SeverelyDegraded);
        assert_eq!(d_extreme.family, EngineFamily::QwenVlm);
        assert!(d_extreme.gpu_escalate);
        assert!(d_extreme.slice_columns);
    }
}
