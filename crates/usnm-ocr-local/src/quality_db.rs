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
                "SELECT doc_id, node, engine, reason, created_by, \
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
