//! Frozen attribution for the exact engine keys whose text a release publishes.

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::{Catalog, Engine, Model};

pub const FILE: &str = "credits.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Credit {
    pub key: String,
    pub version: String,
    pub engine: Engine,
    pub models: Vec<Model>,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Snapshot {
    pub schema: u32,
    pub credit_required: Vec<String>,
    pub engines: Vec<Credit>,
    pub japanese: JapaneseUsage,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct JapaneseUsage {
    pub pages: u64,
    pub by_engine: Vec<EngineUsage>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EngineUsage {
    pub source: String,
    pub engine: String,
    pub pages: u64,
}

impl Catalog {
    /// Legacy Japanese OCR predates catalog routing. Crediting it does not
    /// enable a route to the survey-only catalog entry.
    pub fn credit(&self, key: &str) -> anyhow::Result<Credit> {
        let (family, version, profile) = if let Some(rest) = key.strip_prefix("ndlocr-lite ") {
            if !(7..=40).contains(&rest.len()) || !rest.bytes().all(|b| b.is_ascii_hexdigit()) {
                bail!("missing OCR credit: {key}");
            }
            ("ndlocr-lite", rest, None)
        } else if let Some(rest) = key.strip_prefix("tesseract-") {
            let (version, profile) = rest.split_once('/').context("legacy Tesseract key")?;
            ("tesseract", version, Some(profile))
        } else {
            let (family, rest) = key.split_once('/').context("OCR key family")?;
            let (version, profile) = rest.split_once(' ').context("OCR key profile")?;
            (family, version, Some(profile))
        };
        let series = version.split('.').take(2).collect::<Vec<_>>().join(".");
        let engine = self
            .engine
            .iter()
            .find(|e| {
                e.family == family
                    && if family == "ndlocr-lite" {
                        profile.is_none()
                    } else {
                        e.id == format!("{family}-{version}")
                            || e.id == format!("{family}-{series}")
                    }
            })
            .with_context(|| format!("missing OCR credit: {key}"))?;
        let (mut models, languages) = if let Some(profile) = profile {
            let profile = engine
                .profile
                .iter()
                .find(|p| p.id == profile)
                .with_context(|| format!("missing OCR profile credit: {key}"))?;
            (
                profile
                    .models
                    .iter()
                    .map(|name| {
                        engine
                            .model
                            .iter()
                            .find(|m| &m.name == name)
                            .cloned()
                            .with_context(|| format!("missing OCR model credit: {key}: {name}"))
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?,
                profile.languages.clone(),
            )
        } else {
            (engine.model.clone(), vec!["jpn".to_owned()])
        };
        self.check_attribution(&engine.attribution)?;
        for model in &models {
            self.check_attribution(&model.attribution)?;
        }
        // Short keys omit model digests. A catalog/image pin cannot prove
        // which revision produced a retained reading, even on a full rebuild.
        // Only future per-reading provenance may supply a verified revision.
        for model in &mut models {
            model.version = None;
        }
        let mut engine = engine.clone();
        // The snapshot contains only selected models, not the survey inventory.
        engine.model.clear();
        engine.profile.clear();
        Ok(Credit {
            key: key.to_owned(),
            version: version.to_owned(),
            engine,
            models,
            languages,
        })
    }

    /// Release-time coverage check: an uncredited serving key blocks publication.
    pub fn credits<'a>(&self, keys: impl IntoIterator<Item = &'a str>) -> anyhow::Result<Snapshot> {
        let keys: std::collections::BTreeSet<_> = keys.into_iter().collect();
        Ok(Snapshot {
            schema: 1,
            japanese: JapaneseUsage::default(),
            credit_required: self.credit_required.clone(),
            engines: keys
                .into_iter()
                .map(|key| self.credit(key))
                .collect::<anyhow::Result<_>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::ENGINES;

    #[test]
    fn catalog_changes_cannot_relabel_retained_model_revisions() {
        let mut catalog = ENGINES.clone();
        let key = "tesseract/5.5.1 frak2021";
        let before = catalog.credits([key]).unwrap();
        assert!(before.engines[0].models[0].version.is_none());
        for model in &mut catalog
            .engine
            .iter_mut()
            .find(|e| e.id == "tesseract-5.5")
            .unwrap()
            .model
        {
            model.version = Some("a different revision in the next release".into());
        }
        let after = catalog.credits([key]).unwrap();
        assert_eq!(before.engines[0].key, after.engines[0].key);
        assert!(after.engines[0].models[0].version.is_none());
    }

    #[test]
    fn retired_routes_keep_their_published_credits() {
        let mut catalog = ENGINES.clone();
        catalog
            .engine
            .iter_mut()
            .find(|e| e.id == "tesseract-5.5")
            .unwrap()
            .production = false;
        assert!(catalog
            .check_production("tesseract-5.5", &["tessdata/eng.traineddata".into()])
            .is_err());
        assert_eq!(
            catalog.credit("tesseract/5.5.1 eng").unwrap().models.len(),
            1
        );
    }

    #[test]
    fn every_serving_key_needs_a_credit() {
        let credits = ENGINES
            .credits([
                "tesseract/5.5.1 eng",
                "ndlocr-lite 636d1cf",
                "tesseract/5.5.1 eng",
            ])
            .unwrap();
        assert_eq!(credits.engines.len(), 2);
        let ndl = &credits.engines[0];
        assert_eq!(
            ndl.engine.attribution.author.as_deref(),
            Some("National Diet Library")
        );
        assert_eq!(ndl.models[0].attribution.license, "LicenseRef-DINOv3");
        let tesseract = &credits.engines[1];
        assert_eq!(tesseract.models.len(), 1);
        assert_eq!(tesseract.models[0].name, "tessdata/eng.traineddata");
        assert!(tesseract.engine.model.is_empty());
        assert!(ENGINES.credits(["not-credited/1 eng"]).is_err());
        assert!(ENGINES.credits(["tesseract/5.5.1 german_print"]).is_err());
        assert!(ENGINES.credits(["tesseract/6.0 eng"]).is_err());
        assert_eq!(
            ENGINES
                .credit("tesseract-5.5.1/deu_latf+deu")
                .unwrap()
                .models
                .len(),
            2
        );
    }
}
