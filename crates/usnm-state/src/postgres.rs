//! Durable, concurrent pipeline documents in the on-prem PostgreSQL database.

use anyhow::{ensure, Context};
use async_trait::async_trait;
use serde_json::Value;
use tokio_postgres::{Client, Config, Row};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::docs::{DocStore, Versioned};

pub struct PostgresDocs {
    config: Config,
    tls: MakeRustlsConnect,
}

struct Session {
    client: Client,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PostgresDocs {
    pub fn new(location: &str) -> anyhow::Result<Self> {
        let (mut config, tls, _) = usnm_store::pgmq::connection_options(location)?;
        config
            .application_name("usnm-state")
            .options("-c statement_timeout=30000 -c lock_timeout=15000 -c synchronous_commit=on");
        Ok(Self { config, tls })
    }

    async fn session(&self) -> anyhow::Result<Session> {
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .context("connecting to the document database")?;
        let task = tokio::spawn(async move {
            if let Err(error) = connection.await {
                tracing::warn!(%error, "document database connection closed");
            }
        });
        Ok(Session { client, task })
    }
}

fn id(doc: &Value) -> anyhow::Result<&str> {
    doc["id"].as_str().context("item has no string `id`")
}

fn versioned(row: Row) -> Versioned {
    Versioned {
        doc: row.get("doc"),
        etag: row.get("etag"),
    }
}

#[async_trait]
impl DocStore for PostgresDocs {
    async fn get(&self, container: &str, pk: &str, id: &str) -> anyhow::Result<Option<Versioned>> {
        let session = self.session().await?;
        Ok(session
            .client
            .query_opt(
                "SELECT doc,etag FROM usnm_state.documents WHERE container=$1 AND pk=$2 AND id=$3",
                &[&container, &pk, &id],
            )
            .await?
            .map(versioned))
    }

    async fn create(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
    ) -> anyhow::Result<Option<String>> {
        let session = self.session().await?;
        Ok(session
            .client
            .query_opt(
                "INSERT INTO usnm_state.documents(container,pk,id,doc) VALUES($1,$2,$3,$4)
             ON CONFLICT(container,pk,id) DO NOTHING RETURNING etag",
                &[&container, &pk, &id(doc)?, &doc],
            )
            .await?
            .map(|row| row.get("etag")))
    }

    async fn replace(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        let session = self.session().await?;
        Ok(session
            .client
            .query_opt(
                "UPDATE usnm_state.documents SET doc=$4,
             etag='pg-' || nextval('usnm_state.document_versions')::text
             WHERE container=$1 AND pk=$2 AND id=$3 AND etag=$5 RETURNING etag",
                &[&container, &pk, &id(doc)?, &doc, &etag],
            )
            .await?
            .map(|row| row.get("etag")))
    }

    async fn upsert(&self, container: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        let session = self.session().await?;
        session
            .client
            .execute(
                "INSERT INTO usnm_state.documents(container,pk,id,doc) VALUES($1,$2,$3,$4)
             ON CONFLICT(container,pk,id) DO UPDATE SET doc=EXCLUDED.doc,
             etag='pg-' || nextval('usnm_state.document_versions')::text",
                &[&container, &pk, &id(doc)?, &doc],
            )
            .await?;
        Ok(())
    }

    async fn list(
        &self,
        container: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<Versioned>> {
        ensure!(!field.is_empty(), "document field must not be empty");
        let session = self.session().await?;
        let values: Vec<String> = values.iter().map(|value| (*value).to_owned()).collect();
        Ok(session
            .client
            .query(
                "SELECT doc,etag FROM usnm_state.documents WHERE container=$1
             AND (cardinality($3::text[])=0 OR
                  (jsonb_typeof(doc->$2)='string' AND doc->>$2 = ANY($3)))
             ORDER BY pk,id",
                &[&container, &field, &values],
            )
            .await?
            .into_iter()
            .map(versioned)
            .collect())
    }
}

/// Rows read per query when listing a container for verification.
const ENTRY_PAGE: i64 = 1000;

#[async_trait]
impl crate::migrate::Target for PostgresDocs {
    async fn import(
        &self,
        container: &str,
        rows: &[crate::migrate::Row],
    ) -> anyhow::Result<crate::migrate::Imported> {
        let session = self.session().await?;
        let pks: Vec<&str> = rows.iter().map(|r| r.pk.as_str()).collect();
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        let docs: Vec<&Value> = rows.iter().map(|r| &r.doc).collect();
        let etags: Vec<&str> = rows.iter().map(|r| r.etag.as_str()).collect();
        // A row already there is rewritten only when the source's ETag differs; `xmax = 0`
        // tells an inserted row from an updated one.
        let written = session
            .client
            .query(
                "INSERT INTO usnm_state.documents AS d (container,pk,id,doc,etag)
             SELECT $1, u.pk, u.id, u.doc, u.etag
             FROM unnest($2::text[], $3::text[], $4::jsonb[], $5::text[]) AS u(pk,id,doc,etag)
             ON CONFLICT (container,pk,id) DO UPDATE SET doc=EXCLUDED.doc, etag=EXCLUDED.etag
             WHERE d.etag IS DISTINCT FROM EXCLUDED.etag
             RETURNING (xmax = 0) AS inserted",
                &[&container, &pks, &ids, &docs, &etags],
            )
            .await
            .context("importing documents")?;
        let inserted = written.iter().filter(|r| r.get::<_, bool>(0)).count() as u64;
        let updated = written.len() as u64 - inserted;
        Ok(crate::migrate::Imported {
            inserted,
            updated,
            unchanged: rows.len() as u64 - written.len() as u64,
        })
    }

    async fn entries(&self, container: &str) -> anyhow::Result<Vec<crate::migrate::Entry>> {
        let session = self.session().await?;
        let mut out = Vec::new();
        let mut after: Option<(String, String)> = None;
        loop {
            let rows = match &after {
                None => {
                    session
                        .client
                        .query(
                            "SELECT pk,id,etag,doc FROM usnm_state.documents WHERE container=$1
                         ORDER BY pk,id LIMIT $2",
                            &[&container, &ENTRY_PAGE],
                        )
                        .await?
                }
                Some((pk, id)) => {
                    session
                        .client
                        .query(
                            "SELECT pk,id,etag,doc FROM usnm_state.documents WHERE container=$1
                         AND (pk,id) > ($2,$3) ORDER BY pk,id LIMIT $4",
                            &[&container, pk, id, &ENTRY_PAGE],
                        )
                        .await?
                }
            };
            let full = rows.len() as i64 == ENTRY_PAGE;
            for row in &rows {
                let (pk, id, etag): (String, String, String) =
                    (row.get("pk"), row.get("id"), row.get("etag"));
                let doc: Value = row.get("doc");
                out.push(crate::migrate::Entry::new(&pk, &id, &etag, &doc));
                after = Some((pk, id));
            }
            if !full {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn postgres_documents_preserve_cas_and_reopen() {
        let location = match std::env::var("USNM_TEST_DOCUMENTS") {
            Ok(location) => location,
            Err(_) => {
                assert!(
                    std::env::var_os("USNM_TEST_DOCUMENTS_REQUIRED").is_none(),
                    "USNM_TEST_DOCUMENTS is required"
                );
                eprintln!("skipping document database test: USNM_TEST_DOCUMENTS not set");
                return;
            }
        };
        let first = PostgresDocs::new(&location).unwrap();
        let second = PostgresDocs::new(&location).unwrap();
        let container = format!("test-{}", crate::owner_id());
        let doc = json!({"id":"one","status":"ready","nested":{"items":[1,2]}});
        let (one, two) = tokio::join!(
            first.create(&container, "partition", &doc),
            second.create(&container, "partition", &doc),
        );
        let winners: Vec<_> = [one.unwrap(), two.unwrap()].into_iter().flatten().collect();
        assert_eq!(winners.len(), 1);
        let old = &winners[0];
        let newer = json!({"id":"one","status":"done"});
        let (one, two) = tokio::join!(
            first.replace(&container, "partition", &newer, old),
            second.replace(&container, "partition", &newer, old),
        );
        let winners: Vec<_> = [one.unwrap(), two.unwrap()].into_iter().flatten().collect();
        assert_eq!(winners.len(), 1);
        assert_ne!(&winners[0], old);
        drop(first);
        drop(second);
        let reopened = PostgresDocs::new(&location).unwrap();
        let got = reopened
            .get(&container, "partition", "one")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.doc, newer);
        assert_eq!(got.etag, winners[0]);
        assert!(reopened
            .replace(&container, "partition", &doc, old)
            .await
            .unwrap()
            .is_none());
        assert!(reopened
            .list(&container, "status", &["ready"])
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            reopened
                .list(&container, "status", &["done"])
                .await
                .unwrap()
                .len(),
            1
        );
        reopened.upsert(&container, "other", &doc).await.unwrap();
        assert_eq!(reopened.list(&container, "id", &[]).await.unwrap().len(), 2);
        assert_eq!(
            reopened
                .select(&container, &[crate::docs::Field::Path("status")])
                .await
                .unwrap()
                .len(),
            2
        );
    }

    /// The migration's import against a real database: inserts, a rerun that changes nothing,
    /// an update only where the ETag differs, the original ETags kept, and a verification.
    #[tokio::test]
    async fn postgres_documents_import_keeps_etags_and_reruns_cleanly() {
        use crate::migrate::{self, Row, Target};
        let Ok(location) = std::env::var("USNM_TEST_DOCUMENTS") else {
            assert!(
                std::env::var_os("USNM_TEST_DOCUMENTS_REQUIRED").is_none(),
                "USNM_TEST_DOCUMENTS is required"
            );
            eprintln!("skipping document import test: USNM_TEST_DOCUMENTS not set");
            return;
        };
        let pg = PostgresDocs::new(&location).unwrap();
        let container = format!("import-{}", crate::owner_id());
        let rows: Vec<Row> = (0..1203)
            .map(|i| {
                let id = format!("d{i:04}");
                Row {
                    pk: format!("p{}", i % 3),
                    doc: json!({"id": id, "n": i, "text": "Ünïcode ✓", "z": {"b": 1, "a": [1.5, 1e16, 2.5e-7, 1.0, 1.2345678901234568e18]}}),
                    etag: format!("\"00{i:04}-cosmos\""),
                    id,
                }
            })
            .collect();
        let mut imported = migrate::Imported::default();
        for chunk in rows.chunks(500) {
            let r = pg.import(&container, chunk).await.unwrap();
            imported.inserted += r.inserted;
        }
        assert_eq!(imported.inserted, 1203);
        let again = pg.import(&container, &rows[..500]).await.unwrap();
        assert_eq!(
            again,
            migrate::Imported {
                inserted: 0,
                updated: 0,
                unchanged: 500
            }
        );
        let mut changed = rows[7].clone();
        changed.doc["n"] = json!(-1);
        changed.etag = "\"changed\"".into();
        let r = pg
            .import(&container, &[changed.clone(), rows[8].clone()])
            .await
            .unwrap();
        assert_eq!(
            r,
            migrate::Imported {
                inserted: 0,
                updated: 1,
                unchanged: 1
            }
        );
        let got = pg
            .get(&container, &changed.pk, &changed.id)
            .await
            .unwrap()
            .unwrap();
        // JSONB writes 1e16 out in full, so compare canonical forms.
        assert_eq!(
            (migrate::canonical(&got.doc), got.etag),
            (migrate::canonical(&changed.doc), changed.etag.clone())
        );
        // The pipeline's own conditional replace works on an imported ETag.
        assert!(pg
            .replace(&container, &rows[9].pk, &rows[9].doc, &rows[9].etag)
            .await
            .unwrap()
            .is_some());
        // Entries come back across more than one listing page, and match the source's.
        let mut source: Vec<migrate::Entry> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 9)
            .map(|(i, r)| {
                let r = if i == 7 { &changed } else { r };
                migrate::Entry::new(&r.pk, &r.id, &r.etag, &r.doc)
            })
            .collect();
        let mut target = pg.entries(&container).await.unwrap();
        assert_eq!(target.len(), 1203);
        migrate::digest(&mut source);
        migrate::digest(&mut target);
        let (n, listed) = migrate::differences(&source, &target, 10);
        assert_eq!(n, 1, "{listed:?}");
        assert_eq!(
            (listed[0].id.as_str(), listed[0].kind),
            ("d0009", "missing_in_source")
        );
    }
}
