//! Per-build trust for the native OCR builds (#1418, docs/boinc/apps/native-ocr.md).
//!
//! `boinc/project/native-build-trust.json` is a reviewed file, compiled into this binary. It
//! records two things:
//!
//! - **Builds.** Each native build digest (`engine.image`), its platform and Tesseract
//!   (`engine.base`), where it was built, and the calibrated challenges it was run on, with the
//!   total edit counts. A host is admitted for a native digest only when its build has a recorded
//!   challenge run on the scope's calibrated challenge, judged against the scope's threshold, at
//!   or under it ([`BuildTrust::refuse`]).
//! - **Equivalences.** Pairs of digests whose output was shown to be identical. Results compare
//!   byte for byte only within one digest or across a listed pair ([`BuildTrust::comparable`]).
//!   A pair says nothing about trust: a host trusted for one digest is not trusted for the other.
//!   Pairs aren't transitive: each comparable pair is listed.
//!
//! Worker images (the container builds) aren't listed. Their trust is unchanged: each host earns
//! it from its own gold reports against the calibrated threshold.
use super::immutable_image;
use anyhow::{ensure, Context};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// The reviewed file this binary was built with.
const EMBEDDED: &str = include_str!("../../../../boinc/project/native-build-trust.json");
const VERSION: u32 = 1;
/// The longest evidence line, in bytes.
const MAX_EVIDENCE: usize = 512;
/// The platforms a native build is made for (`boinc/scripts/check-native-ocr-build.py`).
pub const PLATFORMS: [&str; 4] = [
    "x86_64-pc-linux-gnu",
    "arm64-apple-darwin",
    "x86_64-apple-darwin",
    "windows_x86_64",
];

/// One run of a calibrated challenge through a build, on a project machine.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Challenge {
    /// Character edits against the reference truth, summed over every crop.
    pub char_edits: u64,
    pub crops: u32,
    pub evidence: String,
    /// The challenge's manifest, as the trust scope names it.
    pub manifest_sha256: String,
    /// The threshold the run was judged against: the scope's `max_cer` at the time.
    pub max_cer: f64,
    /// Reference characters, summed over every crop.
    pub ref_chars: u64,
}

impl Challenge {
    /// The run's character error rate over the whole challenge, as the gold report sums it.
    pub fn cer(&self) -> f64 {
        self.char_edits as f64 / self.ref_chars as f64
    }

    /// Whether this run passes `manifest_sha256` at `max_cer`: the same challenge, judged
    /// against the same threshold, at or under it. A run judged against another threshold is
    /// no evidence for this one, as in `Trust::admit_hosts`.
    pub fn passes(&self, manifest_sha256: &str, max_cer: f64) -> bool {
        self.manifest_sha256 == manifest_sha256 && self.max_cer == max_cer && self.cer() <= max_cer
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub artifact_sha256: String,
    pub commit: String,
    /// The `boinc-ocr-native.yml` run that built it.
    pub run: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub challenges: Vec<Challenge>,
    pub engine_base: String,
    pub platform: String,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Equivalence {
    /// Two distinct digests, in ascending order.
    pub digests: [String; 2],
    pub evidence: String,
}

/// `native-build-trust.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildTrust {
    pub builds: BTreeMap<String, Build>,
    pub equivalent: Vec<Equivalence>,
    pub v: u32,
}

fn line_ok(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_EVIDENCE && !text.chars().any(char::is_control)
}

impl BuildTrust {
    pub fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let t: Self = serde_json::from_slice(bytes).context("reading native build trust")?;
        t.validate()?;
        Ok(t)
    }

    /// The file this binary was built with.
    pub fn embedded() -> anyhow::Result<&'static Self> {
        static PARSED: OnceLock<Result<BuildTrust, String>> = OnceLock::new();
        PARSED
            .get_or_init(|| Self::parse(EMBEDDED.as_bytes()).map_err(|e| format!("{e:#}")))
            .as_ref()
            .map_err(|e| anyhow::anyhow!("the embedded native-build-trust.json is invalid: {e}"))
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.v == VERSION, "unsupported native build trust version");
        for (image, build) in &self.builds {
            ensure!(
                immutable_image(image),
                "invalid native build digest {image}"
            );
            ensure!(
                immutable_image(&build.engine_base),
                "invalid engine base for {image}"
            );
            ensure!(
                PLATFORMS.contains(&build.platform.as_str()),
                "unknown platform {} for {image}",
                build.platform
            );
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(&build.source.artifact_sha256)
                    && build.source.commit.len() == 40
                    && build
                        .source
                        .commit
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    && build.source.run > 0,
                "invalid source for {image}"
            );
            for c in &build.challenges {
                ensure!(
                    usnm_ocr_contract::boinc::is_sha256(&c.manifest_sha256)
                        && c.crops > 0
                        && c.ref_chars > 0
                        && c.char_edits <= c.ref_chars.saturating_mul(16)
                        && c.max_cer.is_finite()
                        && (0.0..=1.0).contains(&c.max_cer)
                        && line_ok(&c.evidence),
                    "invalid challenge for {image}"
                );
            }
        }
        let mut seen = BTreeSet::new();
        for pair in &self.equivalent {
            let [a, b] = &pair.digests;
            ensure!(
                immutable_image(a) && immutable_image(b) && a < b,
                "an equivalence names two distinct digests in ascending order"
            );
            ensure!(line_ok(&pair.evidence), "an equivalence needs its evidence");
            ensure!(seen.insert((a, b)), "duplicate equivalence {a} {b}");
        }
        Ok(())
    }

    /// Why a host may not be trusted for `image` in a scope calibrated on `manifest_sha256` at
    /// `max_cer`, if it may not. A listed native build needs a recorded challenge run that
    /// [`Challenge::passes`]. An unlisted digest is refused when `native` says it is a native
    /// build (its evidence came from `usnm_gold_native`, or it's for a native campaign or unit);
    /// a worker image is never listed, and is left to its own gold reports, as before.
    pub fn refuse(
        &self,
        image: &str,
        manifest_sha256: &str,
        max_cer: f64,
        native: bool,
    ) -> Option<String> {
        match self.builds.get(image) {
            Some(build) if build.challenges.iter().any(|c| c.passes(manifest_sha256, max_cer)) => {
                None
            }
            Some(build) => Some(format!(
                "native build {image} ({}) has no challenge run on {manifest_sha256} at CER <= {max_cer} in native-build-trust.json",
                build.platform
            )),
            None if native => Some(format!(
                "{image} is not a native build in native-build-trust.json: a build is trusted only after its challenge is recorded there"
            )),
            None => None,
        }
    }

    /// Whether results from builds `a` and `b` may be compared byte for byte: the same digest,
    /// or a listed pair.
    pub fn comparable(&self, a: &str, b: &str) -> bool {
        if !immutable_image(a) || !immutable_image(b) {
            return false;
        }
        if a == b {
            return true;
        }
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        self.equivalent
            .iter()
            .any(|p| p.digests[0] == lo && p.digests[1] == hi)
    }
}

/// `base` with the build `image` for `platform` added, with one run of the challenge
/// `manifest_sha256` judged at `max_cer` (of 82794 reference characters) if `edits` is given.
#[cfg(test)]
pub(crate) fn with_build(
    mut base: BuildTrust,
    image: &str,
    platform: &str,
    manifest_sha256: &str,
    max_cer: f64,
    edits: Option<u64>,
) -> BuildTrust {
    base.builds.insert(
        image.to_owned(),
        Build {
            challenges: edits
                .into_iter()
                .map(|char_edits| Challenge {
                    char_edits,
                    crops: 30,
                    evidence: "a test run".into(),
                    manifest_sha256: manifest_sha256.into(),
                    max_cer,
                    ref_chars: 82794,
                })
                .collect(),
            engine_base: format!("sha256:{}", "6".repeat(64)),
            platform: platform.into(),
            source: Source {
                artifact_sha256: "5".repeat(64),
                commit: "4".repeat(40),
                run: 1,
            },
        },
    );
    base.validate().unwrap();
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1";
    const C353: &str = "sha256:c353f7bff920d6d1ff2daff5bd3e2a8a27c589121fcdde0b7e2168bf3c174994";
    const MACOS: &str = "sha256:78065a0a43d8f8bf67c00c014ae2b0540d51d0038e4050dbbd509e14b63fa836";
    const WORKER_247C: &str =
        "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
    const WORKER_DAC9: &str =
        "sha256:dac9c123a52d7b10281d3204a815233da1fe9819afdadd4341a034a95441eb34";
    const CHALLENGE: &str = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
    const MAX_CER: f64 = 0.08734328574534386;

    fn windows() -> String {
        format!("sha256:{}", "7".repeat(64))
    }

    /// The embedded file with a fake Windows build added, `edits` its challenge's edit count.
    fn with_windows(edits: Option<u64>) -> BuildTrust {
        with_build(
            BuildTrust::embedded().unwrap().clone(),
            &windows(),
            "windows_x86_64",
            CHALLENGE,
            MAX_CER,
            edits,
        )
    }

    #[test]
    fn the_embedded_file_is_valid_and_records_each_platform_challenge() {
        let t = BuildTrust::embedded().unwrap();
        let linux = &t.builds[LINUX];
        assert_eq!(linux.platform, "x86_64-pc-linux-gnu");
        assert_eq!(linux.challenges[0].char_edits, 5929);
        let macos = &t.builds[MACOS];
        assert_eq!(macos.platform, "arm64-apple-darwin");
        assert_eq!(
            macos.engine_base,
            "sha256:4af660d0a8181c9734c1ac577f4d91089b5ceb17930b8a0a5a217beb99c82032"
        );
        // 0.0719 on the calibrated English challenge, under 0.0873.
        let cer = macos.challenges[0].cer();
        assert!((cer - 0.0719).abs() < 5e-5, "{cer}");
        assert!(macos.challenges[0].passes(CHALLENGE, MAX_CER));
        // Worker images are never listed as builds.
        assert!(!t.builds.contains_key(WORKER_247C) && !t.builds.contains_key(WORKER_DAC9));
    }

    #[test]
    fn linux_keeps_its_challenge_and_its_proven_equivalence() {
        let t = BuildTrust::embedded().unwrap();
        for image in [LINUX, C353] {
            assert_eq!(t.refuse(image, CHALLENGE, MAX_CER, true), None);
        }
        // Byte-identical pages are listed; equal challenge edit counts alone are not.
        assert!(t.comparable(LINUX, WORKER_DAC9) && t.comparable(WORKER_DAC9, LINUX));
        assert!(!t.comparable(LINUX, WORKER_247C));
        assert!(!t.comparable(C353, WORKER_247C));
        assert!(!t.comparable(C353, LINUX));
        // A worker image is still judged by its own gold alone.
        assert_eq!(t.refuse(WORKER_247C, CHALLENGE, MAX_CER, false), None);
    }

    #[test]
    fn a_digest_compares_only_with_itself_or_a_listed_equivalent() {
        let t = BuildTrust::embedded().unwrap();
        assert!(t.comparable(MACOS, MACOS));
        for other in [LINUX, C353, WORKER_247C, WORKER_DAC9] {
            assert!(!t.comparable(MACOS, other), "{other}");
            assert!(!t.comparable(other, MACOS), "{other}");
        }
        assert!(!t.comparable("sha256:short", "sha256:short"));
    }

    #[test]
    fn a_digest_without_a_passing_challenge_is_refused() {
        let t = BuildTrust::embedded().unwrap();
        let unknown = format!("sha256:{}", "9".repeat(64));
        assert!(t
            .refuse(&unknown, CHALLENGE, MAX_CER, true)
            .unwrap()
            .contains("not a native build"));
        // The macOS run is evidence for this challenge and threshold only.
        assert!(t.refuse(MACOS, &"e".repeat(64), MAX_CER, true).is_some());
        assert!(t.refuse(MACOS, CHALLENGE, 0.07, true).is_some());
        assert!(t.refuse(MACOS, CHALLENGE, MAX_CER, true).is_none());
        // A listed build is refused whatever the caller says it is.
        assert!(t.refuse(MACOS, CHALLENGE, 0.07, false).is_some());
    }

    #[test]
    fn a_second_platform_follows_the_same_path() {
        // No challenge recorded: refused.
        let t = with_windows(None);
        assert!(t.refuse(&windows(), CHALLENGE, MAX_CER, true).is_some());
        // Over the threshold: refused.
        let t = with_windows(Some(7300));
        assert!(t.refuse(&windows(), CHALLENGE, MAX_CER, true).is_some());
        // Under it: admitted, and comparable with nothing but itself.
        let t = with_windows(Some(6000));
        assert!(t.refuse(&windows(), CHALLENGE, MAX_CER, true).is_none());
        for other in [LINUX, MACOS, WORKER_247C] {
            assert!(!t.comparable(&windows(), other));
        }
    }

    #[test]
    fn invalid_files_are_refused() {
        let good: serde_json::Value = serde_json::from_str(EMBEDDED).unwrap();
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = good.clone();
            edit(&mut v);
            BuildTrust::parse(&serde_json::to_vec(&v).unwrap()).is_err()
        };
        assert!(bad(&|v| v["v"] = 2.into()));
        assert!(bad(
            &|v| v["builds"][LINUX]["platform"] = "aarch64-unknown-linux-gnu".into()
        ));
        assert!(bad(
            &|v| v["builds"][LINUX]["challenges"][0]["ref_chars"] = 0.into()
        ));
        assert!(bad(&|v| v["builds"][LINUX]["extra"] = 1.into()));
        assert!(bad(
            &|v| v["equivalent"][0]["digests"] = serde_json::json!([LINUX, WORKER_247C])
        ));
        assert!(bad(
            &|v| v["equivalent"][0]["digests"] = serde_json::json!([LINUX, LINUX])
        ));
        assert!(bad(&|v| {
            let first = v["equivalent"][0].clone();
            v["equivalent"].as_array_mut().unwrap().push(first);
        }));
        assert!(bad(&|v| v["equivalent"][0]["evidence"] = "".into()));
    }
}
