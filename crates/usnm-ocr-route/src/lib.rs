//! Engine catalog and the single production licence gate (13 §13.7.2).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{bail, Context};
use serde::Deserialize;

pub const ENGINES_TOML: &str = include_str!("../../../ocr/engines/engines.toml");
pub static ENGINES: LazyLock<Catalog> =
    LazyLock::new(|| Catalog::parse(ENGINES_TOML).expect("built-in engines.toml"));

/// OSD-enabled modes implicitly read a second model, absent from the current
/// catalog and provenance. Do not permit them until that dependency is tracked.
pub fn check_tesseract_psm(psm: u64) -> anyhow::Result<()> {
    if matches!(psm, 0 | 1 | 12) {
        bail!("Tesseract psm {psm} uses the uncataloged osd.traineddata model");
    }
    if psm > 13 {
        bail!("invalid Tesseract psm {psm}");
    }
    Ok(())
}

/// Code and weights use exactly the same attribution fields. Presence of
/// `restricted` or `undecided` is a veto, even when the string is empty.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Attribution {
    #[serde(default)]
    pub license: String,
    pub author: Option<String>,
    pub source: Option<String>,
    pub credit: Option<String>,
    pub fetched: Option<toml::value::Datetime>,
    pub note: Option<String>,
    pub terms: Option<String>,
    pub restricted: Option<String>,
    pub undecided: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Model {
    pub name: String,
    #[serde(flatten)]
    pub attribution: Attribution,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub id: String,
    pub models: Vec<String>,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Engine {
    pub id: String,
    pub family: String,
    #[serde(default)]
    pub production: bool,
    #[serde(flatten)]
    pub attribution: Attribution,
    #[serde(default)]
    pub model: Vec<Model>,
    #[serde(default)]
    pub profile: Vec<Profile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    pub production_licenses: Vec<String>,
    #[serde(default)]
    pub credit_required: Vec<String>,
    pub engine: Vec<Engine>,
}

fn present(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|s| !s.trim().is_empty())
}

impl Catalog {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::parse(&std::fs::read_to_string(path).with_context(|| path.display().to_string())?)
            .with_context(|| path.display().to_string())
    }

    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let catalog: Self = toml::from_str(text).context("parsing engines.toml")?;
        if catalog.schema != 1 {
            bail!("unsupported engines schema {}", catalog.schema);
        }
        unique(catalog.engine.iter().map(|e| e.id.as_str()), "engine")?;
        for e in &catalog.engine {
            unique(e.model.iter().map(|m| m.name.as_str()), "model")?;
            unique(e.profile.iter().map(|p| p.id.as_str()), "profile")?;
            for p in &e.profile {
                if p.models.is_empty() || p.languages.is_empty() {
                    bail!("{} profile {} needs models and languages", e.id, p.id);
                }
                unique(p.models.iter().map(String::as_str), "profile model")?;
                for name in &p.models {
                    if !e.model.iter().any(|m| &m.name == name) {
                        bail!("{} profile {} names unknown model {name}", e.id, p.id);
                    }
                }
            }
        }
        Ok(catalog)
    }

    pub fn engine(&self, id: &str) -> anyhow::Result<&Engine> {
        self.engine
            .iter()
            .find(|e| e.id == id)
            .with_context(|| format!("unknown engine {id}"))
    }

    fn check_attribution(&self, a: &Attribution) -> anyhow::Result<()> {
        if a.restricted.is_some() || a.undecided.is_some() {
            bail!("restricted or undecided terms");
        }
        for (field, value) in [
            ("author", &a.author),
            ("source", &a.source),
            ("credit", &a.credit),
        ] {
            if !present(value) {
                bail!("missing {field}");
            }
        }
        match a.license.as_str() {
            "none" => {
                if !present(&a.note) {
                    bail!("missing note for license=none");
                }
                if !a
                    .fetched
                    .is_some_and(|d| d.date.is_some() && d.time.is_none() && d.offset.is_none())
                {
                    bail!("missing fetched date for license=none");
                }
            }
            "unknown" | "" => bail!("unchecked license"),
            license if self.production_licenses.iter().any(|l| l == license) => {}
            license => bail!("license {license} is not cleared"),
        }
        if a.license.starts_with("LicenseRef-") && !present(&a.terms) {
            bail!("missing terms for custom license");
        }
        Ok(())
    }

    /// The production gate used by both worker execution paths and routes check.
    /// `models` must be the complete set the selected profile actually reads,
    /// not merely the models the local catalog expects it to read.
    pub fn check_production(&self, engine: &str, models: &[String]) -> anyhow::Result<()> {
        let e = self.engine(engine)?;
        if !e.production {
            bail!("engine {engine}: production=false");
        }
        self.check_attribution(&e.attribution)
            .with_context(|| format!("engine {engine}"))?;
        if models.is_empty() {
            bail!("engine {engine}: profile has no models");
        }
        for name in models {
            let model = e
                .model
                .iter()
                .find(|m| &m.name == name)
                .with_context(|| format!("engine {engine}: unknown model {name}"))?;
            self.check_attribution(&model.attribution)
                .with_context(|| format!("engine {engine}, model {name}"))?;
        }
        Ok(())
    }

    pub fn check_profile(&self, engine: &str, profile: &str) -> anyhow::Result<()> {
        let e = self.engine(engine)?;
        let p = e
            .profile
            .iter()
            .find(|p| p.id == profile)
            .with_context(|| format!("engine {engine}: unknown profile {profile}"))?;
        self.check_production(engine, &p.models)
    }

    /// Validate all candidates (including cascades) named by a production route
    /// file. Matching and runtime fallback policy are separate from this gate.
    pub fn check_routes(&self, text: &str) -> anyhow::Result<()> {
        let routes: Routes = toml::from_str(text).context("parsing routes.toml")?;
        if routes.version == 0 || routes.route.is_empty() {
            bail!("routes need a positive version and at least one route");
        }
        unique(routes.route.iter().map(|r| r.id.as_str()), "route")?;
        for r in routes.route {
            if r.candidates.is_empty() || r.fallback_after == Some(0) {
                bail!("route {} needs candidates and fallback_after >= 1", r.id);
            }
            for c in r.candidates.iter().chain(r.cascade.iter().map(|c| &c.then)) {
                let e = self.engine(&c.engine)?;
                if c.profile == "auto" {
                    if e.profile.is_empty() {
                        bail!("engine {} has no profiles", e.id);
                    }
                    for p in &e.profile {
                        self.check_profile(&e.id, &p.id)
                            .with_context(|| format!("route {}", r.id))?;
                    }
                } else {
                    self.check_profile(&e.id, &c.profile)
                        .with_context(|| format!("route {}", r.id))?;
                }
            }
        }
        Ok(())
    }
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str) -> anyhow::Result<()> {
    let mut seen = BTreeSet::new();
    for v in values {
        if v.trim().is_empty() || !seen.insert(v) {
            bail!("empty or duplicate {kind}: {v}");
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct Routes {
    version: u32,
    route: Vec<Route>,
}
#[derive(Deserialize)]
struct Route {
    id: String,
    candidates: Vec<Candidate>,
    fallback_after: Option<u32>,
    cascade: Option<Cascade>,
}
#[derive(Deserialize)]
struct Candidate {
    engine: String,
    profile: String,
}
#[derive(Deserialize)]
struct Cascade {
    then: Candidate,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTES: &str = r#"
version = 1
[[route]]
id = "test"
candidates = [{ engine = "tesseract-5.5", profile = "eng" }]
"#;

    fn entry(c: &mut Catalog, code: bool) -> &mut Attribution {
        if code {
            &mut c.engine[0].attribution
        } else {
            &mut c.engine[0].model[0].attribution
        }
    }

    fn outcome(c: &Catalog, expected: bool) {
        let result = c.check_profile("tesseract-5.5", "eng");
        assert_eq!(result.is_ok(), expected, "{result:?}");
        assert_eq!(c.check_routes(ROUTES).is_ok(), expected);
    }

    #[test]
    fn osd_enabled_modes_are_refused_until_cataloged_and_recorded() {
        for psm in 0..=14 {
            assert_eq!(
                check_tesseract_psm(psm).is_ok(),
                !matches!(psm, 0 | 1 | 12 | 14)
            );
        }
    }

    #[test]
    fn cleared_tier_code_and_models_missing_fields() {
        for code in [true, false] {
            outcome(&ENGINES, true);
            for field in ["author", "source", "credit"] {
                for value in [None, Some(" \t".to_owned())] {
                    let mut c = ENGINES.clone();
                    let a = entry(&mut c, code);
                    match field {
                        "author" => a.author = value,
                        "source" => a.source = value,
                        "credit" => a.credit = value,
                        _ => unreachable!(),
                    }
                    outcome(&c, false);
                }
            }
        }
    }

    #[test]
    fn attributed_tier_code_and_models_missing_fields() {
        for code in [true, false] {
            let mut c = ENGINES.clone();
            let a = entry(&mut c, code);
            a.license = "none".into();
            a.note = Some("No explicit licence; checked the source and its parents".into());
            outcome(&c, true);
            for field in ["author", "source", "fetched", "note", "credit"] {
                let mut missing = c.clone();
                let a = entry(&mut missing, code);
                match field {
                    "author" => a.author = None,
                    "source" => a.source = None,
                    "fetched" => a.fetched = None,
                    "note" => a.note = None,
                    "credit" => a.credit = None,
                    _ => unreachable!(),
                }
                outcome(&missing, false);
            }
            entry(&mut c, code).note = Some("  ".into());
            outcome(&c, false);
        }
    }

    #[test]
    fn excluded_and_undecided_veto_cleared_and_fully_attributed_entries() {
        for code in [true, false] {
            for license in ["Apache-2.0", "none"] {
                for veto in ["restricted", "undecided"] {
                    for value in ["", "source clause https://example.org/terms"] {
                        let mut c = ENGINES.clone();
                        let a = entry(&mut c, code);
                        a.license = license.into();
                        a.note = Some("No explicit licence; checked source".into());
                        if veto == "restricted" {
                            a.restricted = Some(value.into());
                        } else {
                            a.undecided = Some(value.into());
                        }
                        outcome(&c, false);
                    }
                }
            }
            for license in ["unknown", "AGPL-3.0", ""] {
                let mut c = ENGINES.clone();
                entry(&mut c, code).license = license.into();
                // Even adding unknown/none to the cleared list must not bypass their rules.
                if license != "AGPL-3.0" {
                    c.production_licenses.push(license.into());
                }
                outcome(&c, false);
            }
            let mut c = ENGINES.clone();
            c.production_licenses.push("none".into());
            entry(&mut c, code).license = "none".into();
            outcome(&c, false);
        }
        let mut c = ENGINES.clone();
        c.engine[0].production = false;
        outcome(&c, false);
    }

    #[test]
    fn custom_terms_and_all_profile_models_are_checked() {
        let mut c = ENGINES.clone();
        let a = entry(&mut c, false);
        a.license = "LicenseRef-MinerU".into();
        a.terms = None;
        outcome(&c, false);
        entry(&mut c, false).terms = Some("https://example.org/license".into());
        outcome(&c, true);
        let p = &mut c.engine[0]
            .profile
            .iter_mut()
            .find(|p| p.id == "eng")
            .unwrap();
        p.models.push("tessdata/deu.traineddata".into());
        c.engine[0]
            .model
            .iter_mut()
            .find(|m| m.name == "tessdata/deu.traineddata")
            .unwrap()
            .attribution
            .restricted = Some("excluded".into());
        outcome(&c, false);
        assert!(c.check_production("missing", &["model".into()]).is_err());
        assert!(c
            .check_production("tesseract-5.5", &["missing".into()])
            .is_err());
        assert!(c.check_production("tesseract-5.5", &[]).is_err());
    }

    #[test]
    fn inventory_and_minimal_routes_load_and_all_shipped_models_are_gated() {
        ENGINES
            .check_routes(include_str!("../../../ocr/routing/routes.toml"))
            .unwrap();
        for line in include_str!("../../../ocr/engines/tesseract/models.lock").lines() {
            if let Some(name) = line
                .split_whitespace()
                .next()
                .filter(|n| !n.starts_with('#'))
            {
                ENGINES
                    .check_production("tesseract-5.5", &[name.into()])
                    .unwrap();
            }
        }
        for e in &ENGINES.engine {
            for a in std::iter::once(&e.attribution).chain(e.model.iter().map(|m| &m.attribution)) {
                assert!(present(&a.author) && present(&a.source) && present(&a.credit));
                if a.license == "none" {
                    assert!(present(&a.note) && a.fetched.is_some());
                }
                if a.license.starts_with("LicenseRef-") {
                    assert!(present(&a.terms));
                }
            }
        }
    }

    #[test]
    fn loader_rejects_ambiguous_catalogs_and_bad_references() {
        assert!(Catalog::parse(&ENGINES_TOML.replacen("schema = 1", "schema = 2", 1)).is_err());
        assert!(Catalog::parse(&format!(
            "{ENGINES_TOML}\n[[engine]]\nid=\"tesseract-5.5\"\nfamily=\"tesseract\"\n"
        ))
        .is_err());
        assert!(Catalog::parse(&ENGINES_TOML.replacen(
            "name = \"tessdata/eng.traineddata\"",
            "name = \"missing\"",
            1
        ))
        .is_err());
        assert!(Catalog::parse(&ENGINES_TOML.replacen(
            "models = [\"tessdata/frak2021.traineddata\"]",
            "models = []",
            1
        ))
        .is_err());
    }

    #[test]
    fn route_auto_cascade_and_invalid_candidate_lists() {
        assert!(ENGINES
            .check_routes(&ROUTES.replace("profile = \"eng\"", "profile = \"auto\""))
            .is_ok());
        assert!(ENGINES
            .check_routes(&ROUTES.replace("profile = \"eng\"", "profile = \"missing\""))
            .is_err());
        assert!(ENGINES
            .check_routes(&ROUTES.replace(
                "candidates = [{ engine = \"tesseract-5.5\", profile = \"eng\" }]",
                "candidates = []"
            ))
            .is_err());
        assert!(ENGINES
            .check_routes(&format!("{ROUTES}\nfallback_after = 0\n"))
            .is_err());
        assert!(ENGINES.check_routes(&format!("{ROUTES}\n[route.cascade]\nthen = {{engine=\"easyocr-survey\",profile=\"auto\"}}\n")).is_err());
    }
}
