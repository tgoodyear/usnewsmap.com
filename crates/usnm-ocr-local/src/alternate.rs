//! Explicit third-party staging under the normal import lock. Nothing staged
//! here is read by the release, requested-work ledger or scheduler.

use std::io::Cursor;

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_core::ids::PageKey;
use usnm_state::state::State;
use usnm_store::ObjectStore;

use crate::{checks, sync::Lease, LOCK, ROOT};

const POLICY: &str = "loc-whole-page-alto-document-order-v1";
const MAX_IMAGE: usize = 128 << 20;
const MAX_EVIDENCE: usize = 1 << 20;

/// Caller-supplied attestations are retained, not treated as scientific or legal
/// approval. Even rights marked cleared cannot select a staged reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub doc_id: String,
    pub source_image_sha256: String,
    pub baseline_sha256: String,
    pub alto_sha256: String,
    pub text_sha256: String,
    pub artifact_url: String,
    pub retrieved_at: chrono::DateTime<chrono::Utc>,
    /// Use the literal `unknown` where the artifact discloses no engine/version.
    pub engine: String,
    pub engine_version: String,
    pub version_evidence: String,
    pub registration_sha256: String,
    pub rights_sha256: String,
    pub text_rights: Rights,
    pub alto_rights: Rights,
    pub layout_rights: Rights,
    /// Explicit source-image DPI for non-pixel ALTO. No rotation/rescaling guess.
    pub dpi: Option<f64>,
    pub reading_order: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rights {
    Unknown,
    Cleared,
    Restricted,
}

/// Exact input bytes; text is the separately published text, not a transcription.
pub struct Inputs<'a> {
    pub image: &'a [u8],
    pub baseline: &'a [u8],
    pub alto: &'a [u8],
    pub text: &'a [u8],
    pub registration: &'a [u8],
    pub rights: &'a [u8],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    doc_id: String,
    source_image_sha256: String,
    baseline_sha256: String,
    orientation: String,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RightsEvidence {
    alto_sha256: String,
    text_sha256: String,
    text: Rights,
    alto: Rights,
    layout: Rights,
    evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub format: String,
    pub candidate: String,
    pub binding: Binding,
    pub width: u32,
    pub height: u32,
    /// Always false: this adapter cannot change release or queue eligibility.
    pub selected: bool,
}

pub(super) fn digest(bytes: &[u8]) -> String {
    usnm_core::digest::hex(&Sha256::digest(bytes))
}

fn checked(bytes: &[u8], expected: &str, max: usize, label: &str) -> anyhow::Result<()> {
    ensure!(
        expected.len() == 64
            && expected
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "{label}: invalid SHA256"
    );
    ensure!(
        !bytes.is_empty() && bytes.len() <= max,
        "{label}: size bound"
    );
    ensure!(digest(bytes) == expected, "{label}: checksum mismatch");
    Ok(())
}

// The worker parser repairs/clamps geometry and defaults absent units. A
// third-party registration must instead reject ambiguous/out-of-frame geometry.
fn strict_geometry(xml: &[u8]) -> anyhow::Result<()> {
    use quick_xml::events::Event;
    use quick_xml::name::ResolveResult;
    let version = usnm_alto::check(xml, &usnm_alto::Limits::default())?.version;
    let namespace = usnm_alto::NAMESPACES
        .iter()
        .find(|(_, v)| *v == version)
        .context("unsupported ALTO namespace")?
        .0;
    let mut reader = quick_xml::NsReader::from_reader(xml);
    let mut buffer = Vec::new();
    let mut units = Vec::new();
    let mut unit_count = 0;
    let mut in_unit = false;
    let mut dimensions = None;
    loop {
        let (ns, event) = reader.read_resolved_event_into(&mut buffer)?;
        let ours = matches!(ns, ResolveResult::Bound(n) if n.as_ref() == namespace);
        match event {
            Event::Start(ref e) | Event::Empty(ref e) if ours => {
                let local = e.local_name();
                if local.as_ref() == "MeasurementUnit" {
                    unit_count += 1;
                    ensure!(unit_count == 1, "ambiguous ALTO measurement units");
                    ensure!(!in_unit, "nested ALTO measurement units");
                    ensure!(
                        matches!(event, Event::Start(_)),
                        "empty ALTO measurement units"
                    );
                    in_unit = true;
                }
                let number = |name: &str| -> anyhow::Result<f64> {
                    let mut found = None;
                    for attr in e.attributes().with_checks(true) {
                        let attr = attr?;
                        if attr.key.as_ref() == name {
                            found = Some(attr.value.parse::<f64>()?);
                        }
                    }
                    let n = found.context("ALTO geometry attribute missing")?;
                    ensure!(n.is_finite(), "non-finite ALTO coordinate");
                    Ok(n)
                };
                match local.as_ref() {
                    "Page" => {
                        ensure!(dimensions.is_none(), "multiple ALTO pages");
                        dimensions = Some((number("WIDTH")?, number("HEIGHT")?));
                    }
                    "TextBlock" | "TextLine" | "String" => {
                        let (width, height) = dimensions.context("ALTO geometry outside Page")?;
                        let (x, y, w, h) = (
                            number("HPOS")?,
                            number("VPOS")?,
                            number("WIDTH")?,
                            number("HEIGHT")?,
                        );
                        ensure!(
                            x >= 0.0
                                && y >= 0.0
                                && w > 0.0
                                && h > 0.0
                                && x + w <= width
                                && y + h <= height,
                            "ALTO geometry outside registered page"
                        );
                    }
                    _ => {}
                }
            }
            Event::Text(ref t) if in_unit => units.push(
                t.xml_content(quick_xml::XmlVersion::Implicit1_0)
                    .trim()
                    .to_owned(),
            ),
            Event::End(ref e) if ours && e.local_name().as_ref() == "MeasurementUnit" => {
                in_unit = false;
            }
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    ensure!(
        units.len() == 1 && ["pixel", "inch1200", "mm10"].contains(&units[0].as_str()),
        "ambiguous ALTO measurement units"
    );
    ensure!(dimensions.is_some(), "missing ALTO page");
    Ok(())
}

fn validate(binding: &Binding, input: &Inputs<'_>) -> anyhow::Result<Receipt> {
    let key = PageKey::from_doc_id(&binding.doc_id)?;
    ensure!(key.doc_id() == binding.doc_id, "noncanonical page identity");
    ensure!(
        binding.reading_order == POLICY,
        "unsupported reading-order policy"
    );
    ensure!(
        binding.artifact_url.starts_with("https://")
            && binding.artifact_url.len() <= 2048
            && !binding
                .artifact_url
                .chars()
                .any(|c| c.is_whitespace() || c.is_control()),
        "invalid artifact retrieval URL"
    );
    for (name, value) in [
        ("engine", &binding.engine),
        ("engine version", &binding.engine_version),
        ("version evidence", &binding.version_evidence),
    ] {
        ensure!(
            !value.trim().is_empty()
                && value.len() <= checks::MAX_ENGINE_CHARS
                && !value.chars().any(char::is_control),
            "invalid {name}; use explicit unknown where undisclosed"
        );
    }
    if let Some(dpi) = binding.dpi {
        ensure!(
            dpi.is_finite() && dpi > 0.0 && dpi <= 10000.0,
            "invalid registered DPI"
        );
    }
    for (bytes, hash, max, label) in [
        (
            input.image,
            &binding.source_image_sha256,
            MAX_IMAGE,
            "image",
        ),
        (
            input.baseline,
            &binding.baseline_sha256,
            checks::MAX_TEXT_BYTES,
            "baseline",
        ),
        (
            input.alto,
            &binding.alto_sha256,
            checks::MAX_ALTO_BYTES as usize,
            "ALTO",
        ),
        (
            input.text,
            &binding.text_sha256,
            checks::MAX_TEXT_BYTES,
            "text",
        ),
        (
            input.registration,
            &binding.registration_sha256,
            MAX_EVIDENCE,
            "registration",
        ),
        (input.rights, &binding.rights_sha256, MAX_EVIDENCE, "rights"),
    ] {
        checked(bytes, hash, max, label)?;
    }
    let registration: Registration =
        serde_json::from_slice(input.registration).context("invalid registration evidence")?;
    ensure!(
        registration.doc_id == binding.doc_id
            && registration.source_image_sha256 == binding.source_image_sha256
            && registration.baseline_sha256 == binding.baseline_sha256
            && registration.orientation == "upright"
            && !registration.evidence.trim().is_empty(),
        "registration does not bind this page, scan and baseline"
    );
    let rights: RightsEvidence =
        serde_json::from_slice(input.rights).context("invalid rights evidence")?;
    ensure!(
        rights.alto_sha256 == binding.alto_sha256
            && rights.text_sha256 == binding.text_sha256
            && rights.text == binding.text_rights
            && rights.alto == binding.alto_rights
            && rights.layout == binding.layout_rights
            && !rights.evidence.trim().is_empty(),
        "rights evidence does not bind these artifacts"
    );
    std::str::from_utf8(input.baseline).context("baseline is not UTF-8")?;
    let text = std::str::from_utf8(input.text).context("text is not UTF-8")?;
    ensure!(
        !text.trim().is_empty() && !text.chars().any(|c| c.is_control() && !c.is_whitespace()),
        "invalid candidate text"
    );
    let (width, height) = image::ImageReader::new(Cursor::new(input.image))
        .with_guessed_format()?
        .into_dimensions()
        .context("source image dimensions")?;
    let page = usnm_alto::parse(
        input.alto,
        &usnm_alto::ParseOptions {
            limits: usnm_alto::Limits::default(),
            dpi: binding.dpi,
            ..Default::default()
        },
    )
    .context("third-party ALTO refused")?;
    strict_geometry(input.alto)?;
    ensure!(
        (page.width - f64::from(width)).abs() <= 1.0
            && (page.height - f64::from(height)).abs() <= 1.0,
        "registered image/ALTO dimensions mismatch"
    );
    for rect in page.blocks.iter().flat_map(|b| {
        std::iter::once(&b.rect).chain(
            b.lines
                .iter()
                .flat_map(|l| std::iter::once(&l.rect).chain(l.words.iter().map(|w| &w.rect))),
        )
    }) {
        ensure!(
            [rect.x0, rect.y0, rect.x1, rect.y1]
                .iter()
                .all(|n| n.is_finite())
                && rect.x0 >= 0.0
                && rect.y0 >= 0.0
                && rect.width() > 0.0
                && rect.height() > 0.0
                && rect.x1 <= page.width + 1.0
                && rect.y1 <= page.height + 1.0,
            "ALTO geometry outside registered page"
        );
    }
    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    ensure!(
        normalize(text) == normalize(&usnm_alto::text(&page)),
        "published text differs from ALTO document-order reading"
    );
    let candidate = digest(&serde_json::to_vec(binding)?);
    Ok(Receipt {
        format: "usnm-ocr-alternate-stage/1".into(),
        candidate,
        binding: binding.clone(),
        width,
        height,
        selected: false,
    })
}

/// A lost response or interrupted write can replay only byte-identical objects.
pub(super) async fn put_exact(
    store: &dyn ObjectStore,
    path: &str,
    bytes: &[u8],
    kind: &str,
) -> anyhow::Result<()> {
    if !store.put_new(path, bytes.to_vec(), kind).await? {
        let prior = store.get(path).await?.context("staged object vanished")?;
        ensure!(prior == bytes, "immutable staged object conflict: {path}");
    }
    Ok(())
}

/// Stage a single registered whole-page candidate, committing its receipt last.
/// Does not consume source artifacts, alter requested work or write selectable parts.
pub async fn stage(
    state: &State,
    owner: &str,
    curated: &dyn ObjectStore,
    binding: &Binding,
    input: &Inputs<'_>,
) -> anyhow::Result<Receipt> {
    let receipt = validate(binding, input)?;
    let lease = Lease::take(state, owner).await?;
    let result: anyhow::Result<Receipt> = async {
        let prefix = format!("{ROOT}/alternates/{}/{}", binding.doc_id, receipt.candidate);
        for (name, bytes, kind) in [
            ("source-image", input.image, "application/octet-stream"),
            ("baseline.txt", input.baseline, "text/plain; charset=utf-8"),
            ("source.alto.xml", input.alto, "application/xml"),
            ("source.txt", input.text, "text/plain; charset=utf-8"),
            (
                "registration",
                input.registration,
                "application/octet-stream",
            ),
            ("rights", input.rights, "application/octet-stream"),
        ] {
            lease.check()?;
            put_exact(curated, &format!("{prefix}/{name}"), bytes, kind).await?;
        }
        lease.check()?;
        put_exact(
            curated,
            &format!("{prefix}/receipt.json"),
            &serde_json::to_vec_pretty(&receipt)?,
            "application/json",
        )
        .await?;
        tracing::info!(
            doc_id = %binding.doc_id,
            candidate = %receipt.candidate,
            "staged unselected third-party reading"
        );
        Ok(receipt)
    }
    .await;
    drop(lease);
    let unlocked = state.unlock(LOCK, owner).await;
    let receipt = result?;
    unlocked?;
    Ok(receipt)
}

/// Read and revalidate a committed candidate, including every retained artifact.
pub async fn read(
    curated: &dyn ObjectStore,
    doc_id: &str,
    candidate: &str,
) -> anyhow::Result<Receipt> {
    ensure!(
        PageKey::from_doc_id(doc_id)?.doc_id() == doc_id,
        "noncanonical page identity"
    );
    ensure!(
        candidate.len() == 64
            && candidate
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid candidate identity"
    );
    let prefix = format!("{ROOT}/alternates/{doc_id}/{candidate}");
    let objects = curated.list_info(&prefix).await?;
    let get = |name, max| {
        let path = format!("{prefix}/{name}");
        let object = objects.iter().find(|o| o.path == path);
        async move {
            read_retained(
                curated,
                object.context("candidate has no committed receipt or retained artifact")?,
                max,
            )
            .await
        }
    };
    let bytes = get("receipt.json", 65536).await?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    ensure!(
        receipt.binding.doc_id == doc_id && receipt.candidate == candidate,
        "candidate receipt identity mismatch"
    );
    let image = get("source-image", MAX_IMAGE as u64).await?;
    let baseline = get("baseline.txt", checks::MAX_TEXT_BYTES as u64).await?;
    let alto = get("source.alto.xml", checks::MAX_ALTO_BYTES).await?;
    let text = get("source.txt", checks::MAX_TEXT_BYTES as u64).await?;
    let registration = get("registration", MAX_EVIDENCE as u64).await?;
    let rights = get("rights", MAX_EVIDENCE as u64).await?;
    let checked = validate(
        &receipt.binding,
        &Inputs {
            image: &image,
            baseline: &baseline,
            alto: &alto,
            text: &text,
            registration: &registration,
            rights: &rights,
        },
    )?;
    ensure!(
        checked == receipt,
        "candidate receipt differs from retained artifacts"
    );
    Ok(receipt)
}

pub(super) async fn read_retained(
    curated: &dyn ObjectStore,
    object: &usnm_store::ObjectInfo,
    max: u64,
) -> anyhow::Result<Vec<u8>> {
    ensure!(object.size <= max, "retained object exceeds size bound");
    curated
        .get_if_match(&object.path, &object.etag, max)
        .await?
        .context("retained object missing or changed during read")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use usnm_state::docs::MemoryDocs;

    struct Fixture {
        image: Vec<u8>,
        baseline: Vec<u8>,
        alto: Vec<u8>,
        text: Vec<u8>,
        registration: Vec<u8>,
        rights: Vec<u8>,
        binding: Binding,
    }

    impl Fixture {
        fn new(text: &str) -> Self {
            let image = image::RgbImage::new(100, 200);
            let mut encoded = Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .unwrap();
            let image = encoded.into_inner();
            let baseline = b"original baseline remains byte exact".to_vec();
            let alto = format!(
                r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
                    <Description><MeasurementUnit>pixel</MeasurementUnit></Description>
                    <Layout><Page WIDTH="100" HEIGHT="200"><PrintSpace>
                    <TextBlock HPOS="1" VPOS="1" WIDTH="90" HEIGHT="100">
                    <TextLine HPOS="1" VPOS="1" WIDTH="90" HEIGHT="20">
                    <String HPOS="1" VPOS="1" WIDTH="90" HEIGHT="20" CONTENT="{text}"/>
                    </TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#
            )
            .into_bytes();
            let text = text.as_bytes().to_vec();
            let doc_id = "sn83045462_1890-05-03_ed-1_seq-7".to_owned();
            let registration = serde_json::to_vec(&serde_json::json!({
                "doc_id":doc_id, "source_image_sha256":digest(&image),
                "baseline_sha256":digest(&baseline), "orientation":"upright",
                "evidence":"Synthetic exact-scan binding, not production evidence"
            }))
            .unwrap();
            let rights = serde_json::to_vec(&serde_json::json!({
                "alto_sha256":digest(&alto), "text_sha256":digest(&text),
                "text":"unknown", "alto":"unknown", "layout":"unknown",
                "evidence":"Synthetic unknown rights; not a legal clearance"
            }))
            .unwrap();
            let binding = Binding {
                doc_id,
                source_image_sha256: digest(&image),
                baseline_sha256: digest(&baseline),
                alto_sha256: digest(&alto),
                text_sha256: digest(&text),
                artifact_url: "https://example.invalid/synthetic/alignment".into(),
                retrieved_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                engine: "unknown".into(),
                engine_version: "unknown".into(),
                version_evidence: "Synthetic artifact discloses no engine/version".into(),
                registration_sha256: digest(&registration),
                rights_sha256: digest(&rights),
                text_rights: Rights::Unknown,
                alto_rights: Rights::Unknown,
                layout_rights: Rights::Unknown,
                dpi: None,
                reading_order: POLICY.into(),
            };
            Self {
                image,
                baseline,
                alto,
                text,
                registration,
                rights,
                binding,
            }
        }

        fn inputs(&self) -> Inputs<'_> {
            Inputs {
                image: &self.image,
                baseline: &self.baseline,
                alto: &self.alto,
                text: &self.text,
                registration: &self.registration,
                rights: &self.rights,
            }
        }

        fn update_alto(&mut self, alto: Vec<u8>) {
            self.alto = alto;
            self.binding.alto_sha256 = digest(&self.alto);
            let mut rights: serde_json::Value = serde_json::from_slice(&self.rights).unwrap();
            rights["alto_sha256"] = self.binding.alto_sha256.clone().into();
            self.rights = serde_json::to_vec(&rights).unwrap();
            self.binding.rights_sha256 = digest(&self.rights);
        }

        fn clear_synthetic_rights(&mut self) {
            let mut rights: serde_json::Value = serde_json::from_slice(&self.rights).unwrap();
            for name in ["text", "alto", "layout"] {
                rights[name] = "cleared".into();
            }
            self.rights = serde_json::to_vec(&rights).unwrap();
            self.binding.rights_sha256 = digest(&self.rights);
            self.binding.text_rights = Rights::Cleared;
            self.binding.alto_rights = Rights::Cleared;
            self.binding.layout_rights = Rights::Cleared;
        }
    }

    #[tokio::test]
    async fn alternate_stage_replays_retains_conflicts_and_never_enters_release() {
        let temp = tempfile::tempdir().unwrap();
        let store = usnm_store::open(temp.path().to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        let first = Fixture::new("first reading");
        let expected = validate(&first.binding, &first.inputs()).unwrap();
        let prefix = format!(
            "{ROOT}/alternates/{}/{}",
            first.binding.doc_id, expected.candidate
        );
        store
            .put_new(
                &format!("{prefix}/source-image"),
                first.image.clone(),
                "image/png",
            )
            .await
            .unwrap();
        let a = stage(
            &state,
            "stage",
            store.as_ref(),
            &first.binding,
            &first.inputs(),
        )
        .await
        .unwrap();
        let paths = store.list(ROOT).await.unwrap();
        assert_eq!(paths.len(), 7);
        assert!(!a.selected);
        assert_eq!(a.binding.engine, "unknown");
        let replay = stage(
            &state,
            "stage",
            store.as_ref(),
            &first.binding,
            &first.inputs(),
        )
        .await
        .unwrap();
        assert_eq!(a, replay);
        assert_eq!(store.list(ROOT).await.unwrap(), paths);
        let other = Fixture::new("different reading");
        let b = stage(
            &state,
            "stage",
            store.as_ref(),
            &other.binding,
            &other.inputs(),
        )
        .await
        .unwrap();
        assert_ne!(a.candidate, b.candidate);
        assert_eq!(store.list(ROOT).await.unwrap().len(), 14);
        let prefix = format!("{ROOT}/alternates/{}/{}", first.binding.doc_id, a.candidate);
        assert_eq!(
            store
                .get(&format!("{prefix}/baseline.txt"))
                .await
                .unwrap()
                .unwrap(),
            first.baseline
        );
        assert_eq!(
            store
                .get(&format!("{prefix}/source.alto.xml"))
                .await
                .unwrap()
                .unwrap(),
            first.alto
        );
        assert_eq!(
            store
                .get(&format!("{prefix}/source.txt"))
                .await
                .unwrap()
                .unwrap(),
            first.text
        );
        let listing = crate::replace::Listing::list(store.as_ref()).await.unwrap();
        assert_eq!(listing.parts(), 0);
        assert!(listing
            .load(store.as_ref(), ["sn83045462"].into_iter(), &[])
            .await
            .unwrap()
            .pages
            .is_empty());
        assert!(store
            .list(&format!("{ROOT}/queued"))
            .await
            .unwrap()
            .is_empty());
        assert!(store
            .list(&format!("{ROOT}/outcomes"))
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn alternate_stage_refuses_malformed_geometry_text_hash_and_registration() {
        let mut f = Fixture::new("reading");
        f.binding.text_sha256 = "0".repeat(64);
        assert!(validate(&f.binding, &f.inputs())
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        let mut f = Fixture::new("reading");
        f.binding.doc_id = "sn83045462_1890-05-03_ed-1_seq-8".into();
        assert!(validate(&f.binding, &f.inputs())
            .unwrap_err()
            .to_string()
            .contains("registration"));
        for (from, to, reason) in [
            ("WIDTH=\"100\"", "WIDTH=\"120\"", "dimensions mismatch"),
            ("HPOS=\"1\"", "HPOS=\"-1\"", "geometry outside"),
            (
                "CONTENT=\"reading\"",
                "CONTENT=\"other\"",
                "differs from ALTO",
            ),
            (
                "<MeasurementUnit>pixel</MeasurementUnit>",
                "",
                "measurement units",
            ),
            (
                "<MeasurementUnit>pixel</MeasurementUnit>",
                "<MeasurementUnit/><MeasurementUnit>pixel</MeasurementUnit>",
                "measurement units",
            ),
            ("<TextLine HPOS=\"1\"", "<TextLine", "attribute missing"),
        ] {
            let mut f = Fixture::new("reading");
            let altered = String::from_utf8(f.alto.clone())
                .unwrap()
                .replace(from, to)
                .into_bytes();
            f.update_alto(altered);
            let error = validate(&f.binding, &f.inputs()).unwrap_err();
            assert!(format!("{error:#}").contains(reason), "{error:#}");
        }
        let mut f = Fixture::new("reading");
        f.update_alto(b"not XML".to_vec());
        assert!(validate(&f.binding, &f.inputs()).is_err());
    }

    #[tokio::test]
    async fn alternate_stage_obeys_import_lock_and_detects_immutable_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let store = usnm_store::open(temp.path().to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        let f = Fixture::new("reading");
        state
            .lock(LOCK, "another-import", chrono::Duration::minutes(15))
            .await
            .unwrap();
        assert!(
            stage(&state, "stage", store.as_ref(), &f.binding, &f.inputs())
                .await
                .is_err()
        );
        assert!(store.list(ROOT).await.unwrap().is_empty());
        state.unlock(LOCK, "another-import").await.unwrap();
        let receipt = validate(&f.binding, &f.inputs()).unwrap();
        let prefix = format!(
            "{ROOT}/alternates/{}/{}",
            f.binding.doc_id, receipt.candidate
        );
        // Simulate an interrupted write: an exact prefix can resume, but a
        // conflicting object must fail without committing a receipt.
        store
            .put_new(
                &format!("{prefix}/source-image"),
                f.image.clone(),
                "image/png",
            )
            .await
            .unwrap();
        store
            .put_new(
                &format!("{prefix}/baseline.txt"),
                b"conflicting".to_vec(),
                "text/plain",
            )
            .await
            .unwrap();
        assert!(
            stage(&state, "stage", store.as_ref(), &f.binding, &f.inputs())
                .await
                .is_err()
        );
        assert!(!store
            .exists(&format!("{prefix}/receipt.json"))
            .await
            .unwrap());
        assert_eq!(
            store
                .get(&format!("{prefix}/baseline.txt"))
                .await
                .unwrap()
                .unwrap(),
            b"conflicting"
        );
    }

    #[tokio::test]
    async fn alternate_decisions_keep_evidence_and_revoke_to_baseline_without_release_effect() {
        use crate::alternate_decisions::{self as decisions, Action, Decision, Subject};
        let temp = tempfile::tempdir().unwrap();
        let store = usnm_store::open(temp.path().to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        let mut f = Fixture::new("first reading");
        f.clear_synthetic_rights();
        let a = stage(&state, "stage", store.as_ref(), &f.binding, &f.inputs())
            .await
            .unwrap();
        let mut other = Fixture::new("second reading");
        other.clear_synthetic_rights();
        let b = stage(
            &state,
            "stage",
            store.as_ref(),
            &other.binding,
            &other.inputs(),
        )
        .await
        .unwrap();
        let subject = Subject {
            doc_id: f.binding.doc_id.clone(),
            source_image_sha256: f.binding.source_image_sha256.clone(),
            baseline_sha256: f.binding.baseline_sha256.clone(),
        };
        assert!(decisions::read(store.as_ref(), &subject)
            .await
            .unwrap()
            .is_none());
        let comparison = |candidate: &str| {
            serde_json::to_vec(&serde_json::json!({
            "subject":subject,"candidate":candidate,"reference_sha256":digest(b"synthetic human reference"),
            "reference_status":"human-verified","reviewed_by":"synthetic-fixture-only",
            "compatibility_accepted":true,
            "legacy_ws_v1":{"reference_chars":100,"baseline_edits":20,"candidate_edits":10},
            "raw_v1":{"reference_chars":110,"baseline_edits":25,"candidate_edits":12}
        })).unwrap()
        };
        let evidence_a = comparison(&a.candidate);
        let mut request = Decision {
            subject: subject.clone(),
            previous: None,
            action: Action::Select {
                candidate: a.candidate.clone(),
                comparison_sha256: digest(&evidence_a),
            },
            reason: "Synthetic reviewed comparison, not production acceptance".into(),
            evidence: "https://example.invalid/synthetic-review".into(),
        };
        let first = decisions::decide(
            &state,
            "decide",
            store.as_ref(),
            &request,
            Some(&evidence_a),
        )
        .await
        .unwrap();
        assert_eq!(first.selected, Some(a.candidate.clone()));
        assert_eq!(
            first,
            decisions::decide(
                &state,
                "decide",
                store.as_ref(),
                &request,
                Some(&evidence_a)
            )
            .await
            .unwrap()
        );
        assert_eq!(
            decisions::read(store.as_ref(), &subject).await.unwrap(),
            Some(first.clone())
        );
        let evidence_b = comparison(&b.candidate);
        request.action = Action::Select {
            candidate: b.candidate.clone(),
            comparison_sha256: digest(&evidence_b),
        };
        assert!(decisions::decide(
            &state,
            "decide",
            store.as_ref(),
            &request,
            Some(&evidence_b)
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("stale"));
        request.previous = Some(first.id);
        let second = decisions::decide(
            &state,
            "decide",
            store.as_ref(),
            &request,
            Some(&evidence_b),
        )
        .await
        .unwrap();
        assert_eq!(second.selected, Some(b.candidate.clone()));
        request.previous = Some(second.id);
        request.action = Action::Revoke {
            candidate: b.candidate.clone(),
        };
        let revoked = decisions::decide(&state, "decide", store.as_ref(), &request, None)
            .await
            .unwrap();
        assert_eq!(revoked.selected, None);
        request.previous = Some(revoked.id.clone());
        request.action = Action::Select {
            candidate: b.candidate.clone(),
            comparison_sha256: digest(&evidence_b),
        };
        assert!(decisions::decide(
            &state,
            "decide",
            store.as_ref(),
            &request,
            Some(&evidence_b)
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("revoked"));
        request.action = Action::Select {
            candidate: a.candidate.clone(),
            comparison_sha256: digest(&evidence_a),
        };
        let restored = decisions::decide(
            &state,
            "decide",
            store.as_ref(),
            &request,
            Some(&evidence_a),
        )
        .await
        .unwrap();
        let evidence_path = format!(
            "{ROOT}/alternate-decisions/{}/{}/{}/evidence/{}.json",
            subject.doc_id,
            subject.source_image_sha256,
            subject.baseline_sha256,
            digest(&evidence_a)
        );
        store
            .put(&evidence_path, b"{}".to_vec(), "application/json")
            .await
            .unwrap();
        assert!(decisions::read(store.as_ref(), &subject).await.is_err());
        store
            .put(&evidence_path, evidence_a.clone(), "application/json")
            .await
            .unwrap();
        assert_eq!(
            decisions::read(store.as_ref(), &subject).await.unwrap(),
            Some(restored.clone())
        );
        request.previous = Some(restored.id);
        request.action = Action::RetainBaseline;
        let baseline = decisions::decide(&state, "decide", store.as_ref(), &request, None)
            .await
            .unwrap();
        assert_eq!(baseline.selected, None);
        assert_eq!(
            decisions::read(store.as_ref(), &subject).await.unwrap(),
            Some(baseline)
        );
        assert_eq!(
            read(store.as_ref(), &subject.doc_id, &a.candidate)
                .await
                .unwrap(),
            a
        );
        assert_eq!(
            read(store.as_ref(), &subject.doc_id, &b.candidate)
                .await
                .unwrap(),
            b
        );
        assert_eq!(
            crate::replace::Listing::list(store.as_ref())
                .await
                .unwrap()
                .parts(),
            0
        );
    }

    #[tokio::test]
    async fn alternate_decisions_refuse_unknown_rights_and_misbound_comparison() {
        use crate::alternate_decisions::{self as decisions, Action, Decision, Subject};
        let temp = tempfile::tempdir().unwrap();
        let store = usnm_store::open(temp.path().to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        let mut f = Fixture::new("reading");
        let mut a = stage(&state, "stage", store.as_ref(), &f.binding, &f.inputs())
            .await
            .unwrap();
        let subject = Subject {
            doc_id: f.binding.doc_id.clone(),
            source_image_sha256: f.binding.source_image_sha256.clone(),
            baseline_sha256: f.binding.baseline_sha256.clone(),
        };
        let mut request = Decision {
            subject: subject.clone(),
            previous: None,
            action: Action::Select {
                candidate: a.candidate,
                comparison_sha256: digest(b"{}"),
            },
            reason: "Synthetic decision".into(),
            evidence: "Synthetic evidence".into(),
        };
        assert!(
            decisions::decide(&state, "decide", store.as_ref(), &request, Some(b"{}"))
                .await
                .unwrap_err()
                .to_string()
                .contains("rights")
        );
        assert!(decisions::read(store.as_ref(), &subject)
            .await
            .unwrap()
            .is_none());
        f.clear_synthetic_rights();
        a = stage(&state, "stage", store.as_ref(), &f.binding, &f.inputs())
            .await
            .unwrap();
        let mut report = serde_json::json!({
            "subject":subject,"candidate":a.candidate,
            "reference_sha256":digest(b"synthetic"),"reference_status":"machine-output",
            "reviewed_by":"synthetic","compatibility_accepted":true,
            "legacy_ws_v1":{"reference_chars":100,"baseline_edits":20,"candidate_edits":10},
            "raw_v1":{"reference_chars":100,"baseline_edits":20,"candidate_edits":10}});
        for case in 0..3 {
            if case == 1 {
                report["reference_status"] = "human-verified".into();
                report["raw_v1"]["candidate_edits"] = 30.into();
            }
            if case == 2 {
                report["raw_v1"]["candidate_edits"] = 10.into();
                report["subject"]["source_image_sha256"] = "0".repeat(64).into();
            }
            let bytes = serde_json::to_vec(&report).unwrap();
            request.action = Action::Select {
                candidate: a.candidate.clone(),
                comparison_sha256: digest(&bytes),
            };
            assert!(
                decisions::decide(&state, "decide", store.as_ref(), &request, Some(&bytes))
                    .await
                    .is_err()
            );
            assert!(decisions::read(store.as_ref(), &subject)
                .await
                .unwrap()
                .is_none());
        }
        request.action = Action::Reject {
            candidate: a.candidate.clone(),
        };
        let rejected = decisions::decide(&state, "decide", store.as_ref(), &request, None)
            .await
            .unwrap();
        assert_eq!(rejected.selected, None);
        // A missing retained artifact is an error, never a successful baseline fallback.
        let prefix = format!("{ROOT}/alternates/{}/{}", subject.doc_id, a.candidate);
        store
            .put(
                &format!("{prefix}/source.txt"),
                b"corrupt".to_vec(),
                "text/plain",
            )
            .await
            .unwrap();
        assert!(read(store.as_ref(), &subject.doc_id, &a.candidate)
            .await
            .is_err());
        let history_gap = format!(
            "{ROOT}/alternate-decisions/{}/{}/{}/history/000002.json",
            subject.doc_id, subject.source_image_sha256, subject.baseline_sha256
        );
        store
            .put_new(
                &history_gap,
                serde_json::to_vec(&rejected).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        assert!(decisions::read(store.as_ref(), &subject)
            .await
            .unwrap_err()
            .to_string()
            .contains("gap"));
    }
}
