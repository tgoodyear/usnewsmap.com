//! What the release reads from the quality database (`usnm_quality`) when it
//! revokes our text (#1956): which BOINC host read a page
//! (`quality.boinc_page_provenance`) and single-page holds
//! (`quality.boinc_page_holds`). Both are read at release time, on the replica,
//! as the read-only `quality_reader` role; nothing is exported to the lake.
//!
//! The connection comes from a DSN file (`USNM_QUALITY_DSN_FILE`, the
//! role's `/etc/usnm/quality-reader/dsn`): TLS 1.3 with the role's client
//! certificate, the session read-only. The file's content is never logged.
//!
//! [`QualityHolds`] is the seam the revocation loader uses, so tests can stand in
//! for the database.

use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::replace::{Provenance, ProvenanceLine};

/// The environment variable naming the DSN file of the read-only role.
pub const DSN_FILE_ENV: &str = "USNM_QUALITY_DSN_FILE";

/// What a release does about the quality database's holds (#1956). The default
/// is to require them: a release that could not read `quality.boinc_page_holds`
/// would publish pages that were revoked, so it stops instead. Not reading them
/// takes [`DbHolds::Skip`], which the operator asks for by name
/// (`--no-db-holds`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbHolds {
    /// Read the holds, stop if they can't be read or the table has no rows, and
    /// stop before publishing if fewer than `expect_revoked` pages came out
    /// revoked (`ocr_local.json`'s `kept.revoked`).
    Required { expect_revoked: u64 },
    /// Apply no database holds.
    Skip,
}

impl Default for DbHolds {
    fn default() -> Self {
        DbHolds::Required { expect_revoked: 0 }
    }
}

impl DbHolds {
    /// From `--no-db-holds` and `--expect-revoked`. A non-zero count with the
    /// opt-out is an error: nothing could meet it.
    pub fn from_flags(no_db_holds: bool, expect_revoked: Option<u64>) -> anyhow::Result<Self> {
        match (no_db_holds, expect_revoked) {
            (true, Some(n)) if n > 0 => bail!(
                "--expect-revoked {n} conflicts with --no-db-holds: a release without database \
                 holds revokes none of them"
            ),
            (true, _) => Ok(DbHolds::Skip),
            (false, n) => Ok(DbHolds::Required {
                expect_revoked: n.unwrap_or(0),
            }),
        }
    }

    /// The database to read holds from: `None` only for [`DbHolds::Skip`].
    /// Required with `USNM_QUALITY_DSN_FILE` unset or empty, or its file
    /// missing or unusable, is an error.
    pub fn open(self) -> anyhow::Result<Option<QualityDb>> {
        self.open_with(std::env::var(DSN_FILE_ENV))
    }

    fn open_with(
        self,
        dsn_file: Result<String, std::env::VarError>,
    ) -> anyhow::Result<Option<QualityDb>> {
        if self == DbHolds::Skip {
            tracing::warn!("--no-db-holds: quality.boinc_page_holds is not applied");
            return Ok(None);
        }
        match dsn_file {
            Ok(path) if !path.trim().is_empty() => Ok(Some(QualityDb::from_file(path.trim())?)),
            Ok(_) | Err(std::env::VarError::NotPresent) => bail!(
                "{DSN_FILE_ENV} is not set, so quality.boinc_page_holds can't be read and its \
                 revocations would ship; set it, or pass --no-db-holds for a release meant to run \
                 without them"
            ),
            Err(e) => bail!("{DSN_FILE_ENV}: {e}"),
        }
    }

    /// Whether the release must read at least one hold row.
    pub fn requires_rows(self) -> bool {
        matches!(self, DbHolds::Required { .. })
    }

    /// After the revocations are applied: `revoked` is the number of pages the
    /// release kept LoC's text on because they were revoked (the tally's
    /// `kept.revoked`, as in `ocr_local.json`; files and holds together). Logs
    /// it beside the expected count, and fails if it is below.
    pub fn check_revoked(self, revoked: u64) -> anyhow::Result<()> {
        let expected = match self {
            DbHolds::Required { expect_revoked } => expect_revoked,
            DbHolds::Skip => 0,
        };
        tracing::info!(expected, actual = revoked, "pages revoked (kept.revoked)");
        if revoked < expected {
            bail!(
                "{revoked} pages revoked, fewer than the {expected} expected (--expect-revoked): \
                 the database holds were not all applied, so nothing is published"
            );
        }
        Ok(())
    }
}

/// A hold on one page: its `doc_id`, and the `node` or the engine tag (or
/// both) of the reading it withdraws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHold {
    pub doc_id: String,
    pub node: Option<String>,
    pub engine: Option<String>,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// Set for a hold meant for a reading that isn't stored yet: the release
    /// doesn't stop because it matches no stored page. Every other hold that
    /// matches none stops the release (`replace::check_holds`).
    pub allow_zero_match: bool,
}

/// The environment variable naming the DSN file of the role that writes
/// `quality.boinc_page_holds` (`usnm-ingest holds add`): `postgres`, or a role
/// with INSERT on the table. The readers have SELECT only.
pub const WRITER_DSN_FILE_ENV: &str = "USNM_QUALITY_HOLDS_WRITER_DSN_FILE";

/// A hold to add. See [`HoldsWriter::add`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewHold {
    pub doc_id: String,
    pub node: Option<String>,
    pub engine: Option<String>,
    pub reason: String,
    pub created_by: String,
    pub allow_zero_match: bool,
}

/// The primary, reached with the writer's DSN file. Holds are written only
/// through this (`usnm-ingest holds add`), after the caller has checked that
/// the row matches a stored page.
pub struct HoldsWriter {
    config: tokio_postgres::Config,
    tls: tokio_postgres_rustls::MakeRustlsConnect,
    label: String,
}

impl HoldsWriter {
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var(WRITER_DSN_FILE_ENV) {
            Ok(path) if !path.trim().is_empty() => Self::from_file(path.trim()),
            _ => {
                bail!("{WRITER_DSN_FILE_ENV} is not set: adding a hold needs the writer's DSN file")
            }
        }
    }

    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let dsn = std::fs::read_to_string(path)
            .with_context(|| format!("reading the holds writer DSN file `{path}`"))?;
        let (mut config, tls, label) = usnm_store::pgmq::connection_options(dsn.trim())
            .with_context(|| format!("the holds writer DSN file `{path}`"))?;
        config
            .application_name("usnm-holds-add")
            .connect_timeout(Duration::from_secs(15));
        Ok(Self { config, tls, label })
    }

    /// Inserts the hold. Returns false when the same page and reading are
    /// already held (the table keeps one row for each).
    pub async fn add(&self, h: &NewHold) -> anyhow::Result<bool> {
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .with_context(|| format!("connecting to {}", self.label))?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let n = client
            .execute(
                "INSERT INTO quality.boinc_page_holds \
                 (doc_id, node, engine, reason, created_by, allow_zero_match) \
                 VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING",
                &[
                    &h.doc_id,
                    &h.node,
                    &h.engine,
                    &h.reason,
                    &h.created_by,
                    &h.allow_zero_match,
                ],
            )
            .await
            .context("inserting into quality.boinc_page_holds");
        task.abort();
        Ok(n? == 1)
    }
}

/// One page's row of `quality.qe_loc_scores` (the `qe-loc-v1` text-only run).
#[derive(Debug, Clone, PartialEq)]
pub struct QeLocScore {
    pub doc_id: String,
    pub archive: String,
    pub language: Option<String>,
    pub language_source: String,
    pub qe_version: String,
    pub resources_lock: String,
    pub oov: Option<f64>,
    pub charlm_bpc: Option<f64>,
    pub lm_oov: Option<f64>,
    pub known3: Option<i64>,
    pub no_words: Option<bool>,
    pub words: Option<i64>,
    pub null_reason: Option<String>,
    /// The BOINC host that computed the row, and the app version it ran.
    pub host_id: i64,
    pub app_version: String,
}

/// The environment variable naming the DSN file of the role that writes
/// `quality.qe_loc_scores` (`quality_text_writer`, INSERT only).
pub const QE_WRITER_DSN_FILE_ENV: &str = "USNM_QUALITY_QE_WRITER_DSN_FILE";

/// Writes `quality.qe_loc_scores`, in batches, never changing a row already there.
pub struct QeLocWriter {
    config: tokio_postgres::Config,
    tls: tokio_postgres_rustls::MakeRustlsConnect,
    label: String,
}

impl QeLocWriter {
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var(QE_WRITER_DSN_FILE_ENV) {
            Ok(path) if !path.trim().is_empty() => Self::from_file(path.trim()),
            _ => bail!("{QE_WRITER_DSN_FILE_ENV} is not set: writing qe scores needs the writer's DSN file"),
        }
    }

    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let dsn = std::fs::read_to_string(path)
            .with_context(|| format!("reading the qe writer DSN file `{path}`"))?;
        let (mut config, tls, label) = usnm_store::pgmq::connection_options(dsn.trim())
            .with_context(|| format!("the qe writer DSN file `{path}`"))?;
        config
            .application_name("usnm-qe-loc-collect")
            .connect_timeout(Duration::from_secs(15));
        Ok(Self { config, tls, label })
    }

    /// Inserts the rows in one statement a batch of `batch` rows; returns how many were new.
    pub async fn insert(&self, rows: &[QeLocScore], batch: usize) -> anyhow::Result<u64> {
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .with_context(|| format!("connecting to {}", self.label))?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let mut new = 0;
        for chunk in rows.chunks(batch.max(1)) {
            let col = |f: fn(&QeLocScore) -> String| chunk.iter().map(f).collect::<Vec<_>>();
            let opt =
                |f: fn(&QeLocScore) -> Option<String>| chunk.iter().map(f).collect::<Vec<_>>();
            let n = client
                .execute(
                    "INSERT INTO quality.qe_loc_scores \
                     (doc_id, archive, language, language_source, qe_version, resources_lock, \
                      oov, charlm_bpc, lm_oov, known3, no_words, words, null_reason, host_id, app_version) \
                     SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], \
                      $6::text[], $7::float8[], $8::float8[], $9::float8[], $10::int8[], $11::bool[], \
                      $12::int8[], $13::text[], $14::int8[], $15::text[]) \
                     ON CONFLICT (doc_id, qe_version) WHERE NOT invalid DO NOTHING",
                    &[
                        &col(|r| r.doc_id.clone()),
                        &col(|r| r.archive.clone()),
                        &opt(|r| r.language.clone()),
                        &col(|r| r.language_source.clone()),
                        &col(|r| r.qe_version.clone()),
                        &col(|r| r.resources_lock.clone()),
                        &chunk.iter().map(|r| r.oov).collect::<Vec<_>>(),
                        &chunk.iter().map(|r| r.charlm_bpc).collect::<Vec<_>>(),
                        &chunk.iter().map(|r| r.lm_oov).collect::<Vec<_>>(),
                        &chunk.iter().map(|r| r.known3).collect::<Vec<_>>(),
                        &chunk.iter().map(|r| r.no_words).collect::<Vec<_>>(),
                        &chunk.iter().map(|r| r.words).collect::<Vec<_>>(),
                        &opt(|r| r.null_reason.clone()),
                        &chunk.iter().map(|r| r.host_id).collect::<Vec<_>>(),
                        &col(|r| r.app_version.clone()),
                    ],
                )
                .await
                .context("inserting into quality.qe_loc_scores");
            match n {
                Ok(n) => new += n,
                Err(e) => {
                    task.abort();
                    return Err(e);
                }
            }
        }
        task.abort();
        Ok(new)
    }
}

impl QeLocWriter {
    /// Marks every row of these hosts invalid (a failed spot check); returns how many rows.
    pub async fn mark_invalid(&self, host_ids: &[i64]) -> anyhow::Result<u64> {
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .with_context(|| format!("connecting to {}", self.label))?;
        let task = tokio::spawn(async move {
            let _ = connection.await;
        });
        let n = client
            .execute(
                "UPDATE quality.qe_loc_scores SET invalid = true \
                 WHERE host_id = ANY($1::int8[]) AND NOT invalid",
                &[&host_ids],
            )
            .await
            .context("marking quality.qe_loc_scores rows invalid");
        task.abort();
        n
    }
}

/// One live row of `quality.excluded_qwen_readings`: a `usnm_qwen` unit (its
/// `job_id`), or one crop of it, whose readings stay out of the quality tables
/// until the unit is re-read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedQwenReading {
    pub job_id: String,
    pub wu_id: i64,
    /// None excludes the whole unit.
    pub crop_id: Option<String>,
}

/// The quality database as the revocation loader sees it.
#[async_trait]
pub trait QualityHolds: Send + Sync {
    /// Every row of `quality.boinc_page_provenance`.
    async fn provenance(&self) -> anyhow::Result<Provenance>;
    /// Every row of `quality.boinc_page_holds`.
    async fn page_holds(&self) -> anyhow::Result<Vec<PageHold>>;
}

/// The replica, reached with a DSN file.
pub struct QualityDb {
    config: tokio_postgres::Config,
    tls: tokio_postgres_rustls::MakeRustlsConnect,
    label: String,
}

impl QualityDb {
    /// The database named by `USNM_QUALITY_DSN_FILE`; `None` when it isn't set.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        match std::env::var(DSN_FILE_ENV) {
            Ok(path) if !path.trim().is_empty() => Ok(Some(Self::from_file(path.trim())?)),
            Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
            Err(e) => bail!("{DSN_FILE_ENV}: {e}"),
        }
    }

    /// The database a DSN file names (a libpq keyword line or a `postgres://` URL).
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let dsn = std::fs::read_to_string(path)
            .with_context(|| format!("reading the quality DSN file `{path}`"))?;
        let (mut config, tls, label) = usnm_store::pgmq::connection_options(dsn.trim())
            .with_context(|| format!("the quality DSN file `{path}`"))?;
        // Read-only at the session as well as by the role's grants, and no
        // query may hold the replica for long.
        config
            .application_name("usnm-release")
            .options("-c default_transaction_read_only=on -c statement_timeout=60000")
            .connect_timeout(Duration::from_secs(15));
        Ok(Self { config, tls, label })
    }

    async fn connect(
        &self,
    ) -> anyhow::Result<(tokio_postgres::Client, tokio::task::JoinHandle<()>)> {
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .with_context(|| format!("connecting to {}", self.label))?;
        let label = self.label.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!(db = %label, error = %e, "quality database connection closed");
            }
        });
        Ok((client, task))
    }
}

impl QualityDb {
    /// The exclusions in force: every row of `quality.excluded_qwen_readings`
    /// not yet released (`released_at IS NULL`), read by `quality collect`.
    pub async fn excluded_qwen_readings(&self) -> anyhow::Result<Vec<ExcludedQwenReading>> {
        let (client, task) = self.connect().await?;
        let rows = client
            .query(
                "SELECT job_id, wu_id, crop_id FROM quality.excluded_qwen_readings \
                 WHERE released_at IS NULL ORDER BY job_id, crop_id NULLS FIRST",
                &[],
            )
            .await
            .context("reading quality.excluded_qwen_readings");
        task.abort();
        Ok(rows?
            .into_iter()
            .map(|row| ExcludedQwenReading {
                job_id: row.get("job_id"),
                wu_id: row.get("wu_id"),
                crop_id: row.get("crop_id"),
            })
            .collect())
    }
}

impl std::fmt::Debug for QualityDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QualityDb")
            .field("location", &self.label)
            .finish()
    }
}

#[async_trait]
impl QualityHolds for QualityDb {
    async fn provenance(&self) -> anyhow::Result<Provenance> {
        let (client, task) = self.connect().await?;
        let rows = client
            .query(
                "SELECT doc_id, node, status, hostid, candidate_host_ids \
                 FROM quality.boinc_page_provenance",
                &[],
            )
            .await
            .context("reading quality.boinc_page_provenance");
        task.abort();
        let mut out = Provenance::default();
        for row in rows? {
            out.add_row(ProvenanceLine {
                doc_id: row.get("doc_id"),
                node: row.get("node"),
                status: row.get("status"),
                hostid: row
                    .get::<_, Option<i32>>("hostid")
                    .map(u64::try_from)
                    .transpose()
                    .context("a negative hostid")?,
                candidate_host_ids: row
                    .get::<_, Option<Vec<i32>>>("candidate_host_ids")
                    .map(|c| c.into_iter().map(u64::try_from).collect::<Result<_, _>>())
                    .transpose()
                    .context("a negative candidate host id")?,
            })?;
        }
        Ok(out)
    }

    async fn page_holds(&self) -> anyhow::Result<Vec<PageHold>> {
        let (client, task) = self.connect().await?;
        let rows = client
            .query(
                "SELECT doc_id, node, engine, reason, created_by, allow_zero_match, \
                        to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS created_at \
                 FROM quality.boinc_page_holds ORDER BY hold_id",
                &[],
            )
            .await
            .context("reading quality.boinc_page_holds");
        task.abort();
        rows?
            .into_iter()
            .map(|row| {
                let at: String = row.get("created_at");
                Ok(PageHold {
                    doc_id: row.get("doc_id"),
                    node: row.get("node"),
                    engine: row.get("engine"),
                    reason: row.get("reason"),
                    created_by: row.get("created_by"),
                    allow_zero_match: row.get("allow_zero_match"),
                    created_at: DateTime::parse_from_rfc3339(&at)
                        .with_context(|| format!("a hold's created_at `{at}`"))?
                        .with_timezone(&Utc),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_make_the_setting() {
        assert_eq!(
            DbHolds::from_flags(false, None).unwrap(),
            DbHolds::default()
        );
        assert_eq!(
            DbHolds::from_flags(false, Some(7)).unwrap(),
            DbHolds::Required { expect_revoked: 7 }
        );
        assert_eq!(DbHolds::from_flags(true, None).unwrap(), DbHolds::Skip);
        assert_eq!(DbHolds::from_flags(true, Some(0)).unwrap(), DbHolds::Skip);
        let err = DbHolds::from_flags(true, Some(1)).unwrap_err().to_string();
        assert!(err.contains("conflicts with --no-db-holds"), "{err}");
    }

    #[test]
    fn a_required_release_fails_without_a_usable_dsn_file() {
        let required = DbHolds::default();
        let unset = required.open_with(Err(std::env::VarError::NotPresent));
        assert!(unset.unwrap_err().to_string().contains(DSN_FILE_ENV));
        assert!(required.open_with(Ok("  ".into())).is_err());
        let missing = required.open_with(Ok("/nonexistent/usnm/quality/dsn".into()));
        assert!(missing.is_err());
        // An opt-out reads nothing, whatever the variable says.
        let skip = DbHolds::Skip.open_with(Err(std::env::VarError::NotPresent));
        assert!(skip.unwrap().is_none());
    }

    #[test]
    fn fewer_revoked_than_expected_fails_and_at_or_above_passes() {
        let want = DbHolds::Required {
            expect_revoked: 31786,
        };
        assert!(want.check_revoked(0).is_err());
        let err = want.check_revoked(31785).unwrap_err().to_string();
        assert!(err.contains("31785") && err.contains("31786"), "{err}");
        want.check_revoked(31786).unwrap();
        want.check_revoked(40000).unwrap();
        // No expectation, or the opt-out, passes at any count.
        DbHolds::default().check_revoked(0).unwrap();
        DbHolds::Skip.check_revoked(0).unwrap();
    }
}
