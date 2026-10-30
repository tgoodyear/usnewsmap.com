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
}
