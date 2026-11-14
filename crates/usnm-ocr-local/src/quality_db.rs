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
