//! Assimilation: turn a validated canonical result into provenance-tagged parts for the
//! `ocr-sync` intake. Nothing here writes to curated storage.
//!
//! Two things are stubbed behind traits, each with a TODO:
//!
//! * [`ProvenanceSource`]: BOINC's script assimilator gives us the result id but not the host
//!   id or the app version. The real implementation reads them from the BOINC database.
//! * [`ResultSink`]: the real sink writes to the `ocr-out` intake with a managed identity
//!   that can write only `pages/*.boinc.*` and `alto/*.boinc.*`. Names follow `ocr-sync`; see
//!   docs/ocr-sync-provenance.md for what else it needs.

use crate::result::{Engine, ResultFile};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    /// Two or more results agreed.
    Replicated,
    /// One result, from a host BOINC trusts.
    SingleTrusted,
    /// One result from an untrusted host. Adaptive replication should prevent this, so it is
    /// flagged for `ocr-sync` and the sampled re-OCR.
    SingleUntrustedFallback,
}

/// What BOINC knows about the canonical result.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultMeta {
    pub app_version: String,
    pub credit: f64,
    pub host_id: u64,
    pub n_agreeing: u32,
    pub result_id: u64,
    pub trusted_host: bool,
    pub user_id: u64,
}

/// Provenance written with every part. Fields are in alphabetical order. No email, no IP.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub app_version: String,
    pub boinc_host_id: u64,
    pub boinc_user_id: u64,
    pub credit: f64,
    pub result_id: u64,
    pub source: String,
    pub validation_state: ValidationState,
    pub wu: String,
}

/// One page, ready for `ocr-sync`. Fields are in alphabetical order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OcrPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alto: Option<String>,
    pub blank: bool,
    pub engine: Engine,
    pub input_md5: String,
    pub page_id: String,
    pub provenance: Provenance,
    pub text: String,
    pub text_sha256: String,
}

pub fn validation_state(m: &ResultMeta) -> ValidationState {
    if m.n_agreeing >= 2 {
        ValidationState::Replicated
    } else if m.trusted_host {
        ValidationState::SingleTrusted
    } else {
        ValidationState::SingleUntrustedFallback
    }
}

pub fn build_parts(r: &ResultFile, m: &ResultMeta) -> Vec<OcrPart> {
    let prov = Provenance {
        app_version: m.app_version.clone(),
        boinc_host_id: m.host_id,
        boinc_user_id: m.user_id,
        credit: m.credit,
        result_id: m.result_id,
        source: "boinc".into(),
        validation_state: validation_state(m),
        wu: r.wu.clone(),
    };
    r.pages
        .iter()
        .map(|p| OcrPart {
            alto: p.alto.clone(),
            blank: p.blank,
            engine: r.engine.clone(),
            input_md5: p.input_md5.clone(),
            page_id: p.page_id.clone(),
            provenance: prov.clone(),
            text: p.text.clone(),
            text_sha256: p.text_sha256.clone(),
        })
        .collect()
}

/// The node name BOINC results carry in `ocr-sync` part names. `ocr-sync` accepts 1 to 32
/// letters, digits and hyphens.
pub const NODE: &str = "boinc";

/// The two blob names `ocr-sync` reads for one page, relative to `ocr-out`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncNames {
    pub alto: String,
    pub part: String,
}

/// A page key as `ocr-sync` names things: `<lccn>_<yyyy-mm-dd>_ed-<n>_seq-<n>` (the doc id) or
/// the slash form. Returns the doc id and the issue part of the name.
fn split_page_key(page_id: &str) -> Result<(String, String), String> {
    let doc_id = page_id.replace('/', "_");
    let bad = || format!("page id {page_id:?} is not lccn_yyyy-mm-dd_ed-N_seq-N");
    let mut it = doc_id.split('_');
    let (lccn, date, ed, seq) = (
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
    );
    if it.next().is_some() {
        return Err(bad());
    }
    let num = |s: &str, p: &str| {
        s.strip_prefix(p)
            .filter(|n| {
                !n.is_empty() && !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit())
            })
            .is_some()
    };
    let d = date.as_bytes();
    let date_ok = d.len() == 10
        && d.iter().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if lccn.is_empty()
        || !lccn.bytes().all(|b| b.is_ascii_alphanumeric())
        || !date_ok
        || !num(ed, "ed-")
        || !num(seq, "seq-")
    {
        return Err(bad());
    }
    Ok((doc_id.clone(), format!("{lccn}_{date}_{ed}")))
}

/// `YYYYMMDDTHHMMSSffffffZ` for `secs` since the epoch plus `micros`. `ocr-sync` takes the
/// stamp as the part's identity, so each page of a result gets its own (`micros` counts up).
pub fn stamp(secs: u64, micros: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}{micros:06}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// Names `ocr-sync` reads for a page: `pages/<lccn>_<date>_ed-<n>.<node>.<stamp>.parquet` and
/// `alto/<doc_id>.<node>.<stamp>.xml`. See docs/ocr-sync-provenance.md.
pub fn sync_names(page_id: &str, stamp: &str) -> Result<SyncNames, String> {
    let (doc_id, issue) = split_page_key(page_id)?;
    Ok(SyncNames {
        alto: format!("alto/{doc_id}.{NODE}.{stamp}.xml"),
        part: format!("pages/{issue}.{NODE}.{stamp}.parquet"),
    })
}

pub trait ProvenanceSource {
    fn lookup(&self, result_id: u64) -> Result<ResultMeta, String>;
}

pub trait ResultSink {
    /// Write the ALTO first, then the part: `ocr-sync` looks for the ALTO when it reads a part.
    fn write_page(&mut self, names: &SyncNames, part: &OcrPart) -> Result<(), String>;
}

/// TODO: read host id, user id, app version, credit and the count of agreeing results for
/// `result_id` from the BOINC MySQL database (tables `result`, `host`, `app_version`,
/// `workunit`; trust from `host_app_version.consecutive_valid`). It needs a read-only DB user
/// and the DB password from the environment. Until then this refuses to run.
pub struct UnimplementedProvenance;

impl ProvenanceSource for UnimplementedProvenance {
    fn lookup(&self, _result_id: u64) -> Result<ResultMeta, String> {
        Err(
            "TODO: BOINC database lookup is not implemented; pass --host-id, --user-id, \
             --app-version, --credit and --agreeing for a manual run"
                .into(),
        )
    }
}

/// TODO: write the part to the `ocr-out` intake (blob `name`) with the container's managed
/// identity, using a conditional create so a retry cannot overwrite. This sink refuses.
pub struct UnimplementedSink;

impl ResultSink for UnimplementedSink {
    fn write_page(&mut self, _names: &SyncNames, _part: &OcrPart) -> Result<(), String> {
        Err(
            "TODO: the ocr-sync intake write is not implemented; use --out-dir for a local run"
                .into(),
        )
    }
}

/// Writes parts as JSON files under a local directory. For tests and manual runs.
pub struct LocalDirSink {
    pub root: std::path::PathBuf,
}

impl LocalDirSink {
    fn create(&self, rel: &str, bytes: &[u8]) -> Result<(), String> {
        let path = self.root.join(rel);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // create_new: never overwrite an earlier file of the same name.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(bytes).map_err(|e| e.to_string())
    }
}

impl ResultSink for LocalDirSink {
    /// The part is JSON here (the name ends `.json`, not `.parquet`): the overlay Parquet
    /// writer belongs to the real sink.
    fn write_page(&mut self, names: &SyncNames, part: &OcrPart) -> Result<(), String> {
        if let Some(alto) = &part.alto {
            self.create(&names.alto, alto.as_bytes())?;
        }
        let json = serde_json::to_string(part).map_err(|e| e.to_string())?;
        self.create(&names.part.replace(".parquet", ".json"), json.as_bytes())
    }
}

/// Assimilate one canonical result. `now_secs` is the server's clock; each page's stamp is that
/// time with the page's index as microseconds, so no two pages share a name. Names are all
/// worked out before anything is written. Returns the number of parts written.
pub fn assimilate(
    r: &ResultFile,
    meta: &dyn ProvenanceSource,
    result_id: u64,
    now_secs: u64,
    sink: &mut dyn ResultSink,
) -> Result<usize, String> {
    let m = meta.lookup(result_id)?;
    let parts = build_parts(r, &m);
    let mut named = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        let micros = u32::try_from(result_id % 1_000).unwrap_or(0) * 1_000
            + u32::try_from(i).map_err(|e| e.to_string())?;
        named.push((sync_names(&p.page_id, &stamp(now_secs, micros))?, p));
    }
    for (names, p) in &named {
        sink.write_page(names, p)?;
    }
    Ok(parts.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::result::tests::good;

    pub struct Fixed(ResultMeta);
    impl ProvenanceSource for Fixed {
        fn lookup(&self, _id: u64) -> Result<ResultMeta, String> {
            Ok(self.0.clone())
        }
    }

    fn meta(agreeing: u32, trusted: bool) -> ResultMeta {
        ResultMeta {
            app_version: "usnm_ocr 1.00 x86_64-pc-linux-gnu".into(),
            credit: 12.5,
            host_id: 42,
            n_agreeing: agreeing,
            result_id: 7,
            trusted_host: trusted,
            user_id: 9,
        }
    }

    #[derive(Default)]
    struct Mem(Vec<(SyncNames, OcrPart)>);
    impl ResultSink for Mem {
        fn write_page(&mut self, n: &SyncNames, p: &OcrPart) -> Result<(), String> {
            self.0.push((n.clone(), p.clone()));
            Ok(())
        }
    }

    #[test]
    fn state_follows_agreement_then_trust() {
        assert_eq!(
            validation_state(&meta(2, false)),
            ValidationState::Replicated
        );
        assert_eq!(
            validation_state(&meta(3, true)),
            ValidationState::Replicated
        );
        assert_eq!(
            validation_state(&meta(1, true)),
            ValidationState::SingleTrusted
        );
        assert_eq!(
            validation_state(&meta(1, false)),
            ValidationState::SingleUntrustedFallback
        );
    }

    #[test]
    fn parts_carry_provenance_and_no_personal_data() {
        let r = good("eins zwei drei", "vier fuenf sechs");
        let parts = build_parts(&r, &meta(2, false));
        assert_eq!(parts.len(), 2);
        let p = &parts[0].provenance;
        assert_eq!(p.source, "boinc");
        assert_eq!(p.boinc_host_id, 42);
        assert_eq!(p.validation_state, ValidationState::Replicated);
        let j = serde_json::to_string(&parts[0]).unwrap();
        assert!(j.contains("\"validation_state\":\"replicated\""));
        assert!(!j.contains("email") && !j.contains("ip_addr"));
        // Alphabetical keys at the top level.
        assert!(j.find("\"blank\"").unwrap() < j.find("\"engine\"").unwrap());
        assert!(j.find("\"provenance\"").unwrap() < j.find("\"text\"").unwrap());
    }

    fn keyed(mut r: ResultFile) -> ResultFile {
        r.pages[0].page_id = "sn83045081_1904-07-29_ed-1_seq-7".into();
        r.pages[0].alto = Some("<alto/>".into());
        r.pages[1].page_id = "sn83045081/1904-07-29/ed-1/seq-8".into();
        r
    }

    const NOW: u64 = 1_792_000_000;

    #[test]
    fn stamps_are_ocr_sync_stamps() {
        assert_eq!(stamp(0, 0), "19700101T000000000000Z");
        assert_eq!(stamp(1_792_000_000, 42), "20261014T174640000042Z");
        // 29 February.
        assert_eq!(stamp(1_709_164_800, 1), "20240229T000000000001Z");
        assert_eq!(stamp(NOW, 5).len(), 22);
    }

    #[test]
    fn names_follow_ocr_sync() {
        let n = sync_names("sn83045081/1904-07-29/ed-1/seq-7", "20261014T170640000042Z").unwrap();
        assert_eq!(
            n.part,
            "pages/sn83045081_1904-07-29_ed-1.boinc.20261014T170640000042Z.parquet"
        );
        assert_eq!(
            n.alto,
            "alto/sn83045081_1904-07-29_ed-1_seq-7.boinc.20261014T170640000042Z.xml"
        );
        assert_eq!(
            sync_names("sn83045081_1904-07-29_ed-1_seq-7", "20261014T170640000042Z").unwrap(),
            n
        );
    }

    #[test]
    fn bad_page_ids_are_refused() {
        for bad in [
            "p0",
            "../../x",
            "sn1_1904-07-29_ed-0_seq-1",
            "sn1_1904-07-29_ed-1_seq-01",
            "sn1_1904-7-29_ed-1_seq-1",
            "sn1_1904-07-29_ed-1_seq-1_x",
            "s n_1904-07-29_ed-1_seq-1",
        ] {
            assert!(sync_names(bad, "20261014T170640000042Z").is_err(), "{bad}");
        }
    }

    #[test]
    fn assimilate_writes_each_page_once_with_distinct_names() {
        let r = keyed(good("eins zwei drei", "vier fuenf sechs"));
        let mut sink = Mem::default();
        let n = assimilate(&r, &Fixed(meta(1, true)), 7, NOW, &mut sink).unwrap();
        assert_eq!(n, 2);
        assert_ne!(sink.0[0].0.part, sink.0[1].0.part);
        assert!(sink.0[1].0.part.contains(".boinc."));
        assert_eq!(
            sink.0[1].1.provenance.validation_state,
            ValidationState::SingleTrusted
        );
    }

    #[test]
    fn a_bad_page_id_writes_nothing() {
        let r = good("eins zwei drei", "vier fuenf sechs");
        let mut sink = Mem::default();
        assert!(assimilate(&r, &Fixed(meta(1, true)), 7, NOW, &mut sink).is_err());
        assert!(sink.0.is_empty());
    }

    #[test]
    fn stubs_refuse_instead_of_pretending() {
        let r = keyed(good("eins zwei drei", "vier fuenf sechs"));
        let mut sink = Mem::default();
        assert!(assimilate(&r, &UnimplementedProvenance, 1, NOW, &mut sink).is_err());
        assert!(sink.0.is_empty());
        assert!(assimilate(&r, &Fixed(meta(2, true)), 1, NOW, &mut UnimplementedSink).is_err());
    }

    #[test]
    fn local_dir_sink_refuses_to_overwrite() {
        let dir = std::env::temp_dir().join(format!("usnm-boinc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let r = keyed(good("eins zwei drei", "vier fuenf sechs"));
        let mut sink = LocalDirSink { root: dir.clone() };
        assimilate(&r, &Fixed(meta(2, false)), 1, NOW, &mut sink).unwrap();
        let n = sync_names("sn83045081_1904-07-29_ed-1_seq-7", &stamp(NOW, 1_000)).unwrap();
        assert!(dir.join(&n.alto).exists());
        assert!(dir.join(n.part.replace(".parquet", ".json")).exists());
        assert!(assimilate(&r, &Fixed(meta(2, false)), 1, NOW, &mut sink).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
