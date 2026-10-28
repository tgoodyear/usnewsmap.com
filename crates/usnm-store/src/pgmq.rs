//! Work queues in PostgreSQL with the pgmq extension (#967): the on-prem
//! queue container (home-infra `scripts/lab/queue-ct.sh`, schema in
//! `scripts/onprem/queue/setup.sql`).
//!
//! The location is a libpq-style URL with the client certificate's files:
//!
//! ```text
//! postgres://usnm-queue.tjg.fyi/usnm_queue?user=boinc_claim
//!     &sslcert=/etc/usnm/queue/client.crt&sslkey=/etc/usnm/queue/client.key
//!     &sslrootcert=/etc/usnm/queue/ca.pem
//! ```
//!
//! TLS is always on and verifies the server's name against `sslrootcert`;
//! the role logs in with its certificate (CN = role). A password in the URL
//! is refused: the queue database has none.
//!
//! pgmq queue names are table names, so the queues' Azure names map by
//! turning `-` into `_` (`ocr-bulk` is `ocr_bulk`). A message's text is kept
//! as a JSON string, so it comes back byte for byte (a `jsonb` object would
//! reorder its keys, and consumers hash the text). A received message's pop
//! receipt is its visibility timestamp: [`Queue::delete`] removes it only
//! while that lease is still the current one, and otherwise fails with
//! [`LeaseLost`], as a stale Azure pop receipt does. [`Queue::release`] keeps
//! the trait's default, a copy sent and then the original deleted: pgmq reads
//! visible messages in `msg_id` order, so a message given back in place would
//! stay at the front and stop a scan there each time (the copy goes to the back,
//! as on Azure).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use async_trait::async_trait;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::Mutex;
use tokio_postgres::{Client, Config};
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::queue::{LeaseLost, Queue, Received};

/// A pgmq queue service: one database, connected on first use.
pub struct PgmqQueue {
    config: Config,
    tls: MakeRustlsConnect,
    label: String,
    client: Mutex<Option<Connected>>,
}

struct Connected {
    client: Arc<Client>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for PgmqQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgmqQueue")
            .field("location", &self.label)
            .finish()
    }
}

/// What a `postgres://` location names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    pub cert: String,
    pub key: String,
    pub root: String,
}

impl Location {
    pub fn parse(location: &str) -> anyhow::Result<Self> {
        let url = reqwest::Url::parse(location).context("queue location")?;
        ensure!(
            matches!(url.scheme(), "postgres" | "postgresql"),
            "a pgmq location is postgres://"
        );
        ensure!(
            url.password().is_none(),
            "the queue database takes certificates, not passwords"
        );
        ensure!(
            url.username().is_empty(),
            "name the role with ?user=, not in the authority"
        );
        let host = url
            .host_str()
            .context("the location has no host")?
            .to_owned();
        let database = url.path().trim_start_matches('/').to_owned();
        ensure!(
            !database.is_empty() && !database.contains('/'),
            "the location names one database"
        );
        let (mut user, mut cert, mut key, mut root) = (None, None, None, None);
        for (k, v) in url.query_pairs() {
            let slot = match k.as_ref() {
                "user" => &mut user,
                "sslcert" => &mut cert,
                "sslkey" => &mut key,
                "sslrootcert" => &mut root,
                "sslmode" if v == "verify-full" => continue,
                other => bail!("unsupported queue location parameter `{other}`"),
            };
            ensure!(slot.is_none(), "`{k}` is given twice");
            *slot = Some(v.into_owned());
        }
        let need = |v: Option<String>, name: &str| {
            v.with_context(|| format!("the location needs ?{name}="))
        };
        Ok(Self {
            host,
            port: url.port().unwrap_or(5432),
            database,
            user: need(user, "user")?,
            cert: need(cert, "sslcert")?,
            key: need(key, "sslkey")?,
            root: need(root, "sslrootcert")?,
        })
    }
}

/// The pgmq name of a queue: `-` becomes `_`; then only `[a-z0-9_]`, as
/// pgmq names its tables after it. Checked here so it can go into SQL.
pub fn pg_name(queue: &str) -> anyhow::Result<String> {
    let name = queue.replace('-', "_");
    ensure!(
        !name.is_empty()
            && name.len() <= 47
            && name.starts_with(|c: char| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "`{queue}` isn't a queue name"
    );
    Ok(name)
}

impl PgmqQueue {
    pub fn new(location: &str) -> anyhow::Result<Self> {
        let loc = Location::parse(location)?;
        let roots = CertificateDer::pem_file_iter(&loc.root)
            .with_context(|| format!("reading {}", loc.root))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parsing {}", loc.root))?;
        let mut store = rustls::RootCertStore::empty();
        for cert in roots {
            store.add(cert).context("adding the queue CA")?;
        }
        ensure!(!store.is_empty(), "{} holds no certificate", loc.root);
        let chain = CertificateDer::pem_file_iter(&loc.cert)
            .with_context(|| format!("reading {}", loc.cert))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parsing {}", loc.cert))?;
        let key = PrivateKeyDer::from_pem_file(&loc.key)
            .with_context(|| format!("reading {}", loc.key))?;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS 1.3")?
        .with_root_certificates(store)
        .with_client_auth_cert(chain, key)
        .context("the client certificate and key")?;
        let mut config = Config::new();
        config
            .host(&loc.host)
            .port(loc.port)
            .dbname(&loc.database)
            .user(&loc.user)
            .ssl_mode(tokio_postgres::config::SslMode::Require)
            .application_name("usnm-store")
            .connect_timeout(Duration::from_secs(15));
        Ok(Self {
            config,
            tls: MakeRustlsConnect::new(tls),
            label: format!(
                "postgres://{}:{}/{} as {}",
                loc.host, loc.port, loc.database, loc.user
            ),
            client: Mutex::new(None),
        })
    }

    /// Apply a server-side deadline, reconnecting if already in use.
    pub fn with_statement_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        let millis =
            i32::try_from(timeout.as_millis()).context("statement timeout is too large")?;
        ensure!(
            millis > 0,
            "statement timeout must be at least one millisecond"
        );
        self.config
            .options(format!("-c statement_timeout={millis}"));
        drop(self.client.get_mut().take());
        Ok(self)
    }

    /// Abort the transport, rather than leaving cancelled requests in its queue.
    pub async fn discard_connection(&self) {
        drop(self.client.lock().await.take());
    }

    /// The connection, made again if it has closed.
    async fn client(&self) -> anyhow::Result<Arc<Client>> {
        let mut slot = self.client.lock().await;
        if let Some(connected) = slot.as_ref() {
            if !connected.client.is_closed() {
                return Ok(connected.client.clone());
            }
        }
        let (client, connection) = self
            .config
            .connect(self.tls.clone())
            .await
            .with_context(|| format!("connecting to {}", self.label))?;
        let label = self.label.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::warn!(queue = %label, error = %e, "queue connection closed");
            }
        });
        let client = Arc::new(client);
        *slot = Some(Connected {
            client: client.clone(),
            task,
        });
        Ok(client)
    }
}

/// A message's text: a JSON string's contents, or any other JSON as written.
fn text_of(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    }
}

#[async_trait]
impl Queue for PgmqQueue {
    async fn receive(&self, queue: &str, visibility: Duration) -> anyhow::Result<Option<Received>> {
        let name = pg_name(queue)?;
        let vt = i32::try_from(visibility.as_secs()).context("visibility timeout")?;
        ensure!(vt > 0, "a visibility timeout of at least a second");
        let client = self.client().await?;
        let row = client
            .query_opt(
                "SELECT msg_id, read_ct, vt::text, message FROM pgmq.read($1, $2, 1)",
                &[&name, &vt],
            )
            .await
            .with_context(|| format!("reading {name}"))?;
        Ok(row.map(|row| Received {
            queue: queue.to_owned(),
            id: row.get::<_, i64>(0).to_string(),
            pop_receipt: row.get(2),
            dequeue_count: u32::try_from(row.get::<_, i32>(1)).unwrap_or(0),
            text: text_of(row.get(3)),
        }))
    }

    async fn delete(&self, msg: &Received) -> anyhow::Result<()> {
        let name = pg_name(&msg.queue)?;
        let id: i64 = msg.id.parse().context("message id")?;
        let client = self.client().await?;
        let sql =
            format!("DELETE FROM pgmq.q_{name} WHERE msg_id = $1 AND vt = $2::text::timestamptz");
        let n = client
            .execute(&sql, &[&id, &msg.pop_receipt])
            .await
            .with_context(|| format!("deleting {} from {name}", msg.id))?;
        if n == 0 {
            return Err(LeaseLost(msg.id.clone()).into());
        }
        Ok(())
    }

    async fn send(&self, queue: &str, text: &str) -> anyhow::Result<()> {
        let name = pg_name(queue)?;
        let client = self.client().await?;
        client
            .query_one("SELECT pgmq.send($1, to_jsonb($2::text))", &[&name, &text])
            .await
            .with_context(|| format!("sending to {name}"))?;
        Ok(())
    }

    async fn approximate_count(&self, queue: &str) -> anyhow::Result<u64> {
        let name = pg_name(queue)?;
        let client = self.client().await?;
        let row = client
            .query_one("SELECT queue_length FROM pgmq.metrics($1)", &[&name])
            .await
            .with_context(|| format!("counting {name}"))?;
        Ok(u64::try_from(row.get::<_, i64>(0)).unwrap_or(0))
    }

    async fn peek(&self, queue: &str, n: u32) -> anyhow::Result<Vec<String>> {
        ensure!((1..=32).contains(&n), "peek takes 1 to 32 messages");
        let name = pg_name(queue)?;
        let client = self.client().await?;
        let sql = format!(
            "SELECT message FROM pgmq.q_{name} WHERE vt <= clock_timestamp() ORDER BY msg_id LIMIT $1"
        );
        let rows = client
            .query(&sql, &[&i64::from(n)])
            .await
            .with_context(|| format!("peeking {name}"))?;
        Ok(rows.into_iter().map(|r| text_of(r.get(0))).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "postgres://usnm-queue.tjg.fyi/usnm_queue?user=boinc_claim&sslcert=/c/client.crt&sslkey=/c/client.key&sslrootcert=/c/ca.pem";

    #[test]
    fn parses_a_certificate_location() {
        let loc = Location::parse(GOOD).unwrap();
        assert_eq!(loc.host, "usnm-queue.tjg.fyi");
        assert_eq!(
            (loc.port, loc.database.as_str(), loc.user.as_str()),
            (5432, "usnm_queue", "boinc_claim")
        );
        assert_eq!(
            (loc.cert.as_str(), loc.key.as_str(), loc.root.as_str()),
            ("/c/client.crt", "/c/client.key", "/c/ca.pem")
        );
        assert!(Location::parse(&format!("{GOOD}&sslmode=verify-full")).is_ok());
    }

    #[test]
    fn refuses_passwords_weaker_tls_and_missing_files() {
        for bad in [
            "postgres://boinc_claim:pw@usnm-queue/usnm_queue?sslcert=a&sslkey=b&sslrootcert=c",
            "postgres://usnm-queue/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c&sslmode=disable",
            "postgres://usnm-queue/usnm_queue?user=u&sslcert=a&sslkey=b",
            "postgres://usnm-queue/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c&password=x",
            "postgres://usnm-queue/?user=u&sslcert=a&sslkey=b&sslrootcert=c",
            "https://usnm-queue/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c",
            "postgres://usnm-queue/usnm_queue?user=u&user=v&sslcert=a&sslkey=b&sslrootcert=c",
        ] {
            assert!(Location::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn maps_and_checks_queue_names() {
        assert_eq!(pg_name("ocr-bulk").unwrap(), "ocr_bulk");
        assert_eq!(pg_name("ocr-deadletter").unwrap(), "ocr_deadletter");
        for bad in [
            "",
            "OCR",
            "ocr bulk",
            "ocr;drop",
            "1ocr",
            "ocr.bulk",
            &"q".repeat(48),
        ] {
            assert!(pg_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keeps_a_messages_text() {
        assert_eq!(
            text_of(serde_json::json!("{\"b\":1,\"a\":2}")),
            "{\"b\":1,\"a\":2}"
        );
        assert_eq!(text_of(serde_json::json!({"a": 1})), "{\"a\":1}");
    }

    fn live_database_location() -> Option<String> {
        let location = std::env::var("USNM_TEST_PGMQ").ok();
        assert!(
            location.is_some() || std::env::var("USNM_TEST_PGMQ_REQUIRED").is_err(),
            "USNM_TEST_PGMQ_REQUIRED is set but USNM_TEST_PGMQ isn't"
        );
        location
    }

    #[tokio::test]
    async fn server_deadline_and_discard_allow_a_fresh_connection() {
        let Some(location) = live_database_location() else {
            return;
        };
        let q = PgmqQueue::new(&location)
            .unwrap()
            .with_statement_timeout(Duration::from_millis(50))
            .unwrap();
        let client = q.client().await.unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            client.simple_query("SELECT pg_sleep(30)"),
        )
        .await
        .expect("server statement deadline did not fire")
        .expect_err("blocked statement should be cancelled");
        assert_eq!(
            error.code(),
            Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
        );
        q.discard_connection().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while !client.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("discarded transport remained open");
        let next = q.client().await.unwrap();
        assert!(!Arc::ptr_eq(&client, &next));
        next.simple_query("SELECT 1").await.unwrap();
    }

    /// Against a real queue database: set USNM_TEST_PGMQ to a location whose
    /// role may send, read and delete on `ocr_deadletter` (CI's queue job uses
    /// scripts/onprem/queue/ci-fixture.sh with ingest_producer).
    #[tokio::test]
    async fn round_trip_against_a_live_database() {
        let Some(location) = live_database_location() else {
            return;
        };
        let q = PgmqQueue::new(&location).unwrap();
        let text = format!(
            "{{\"z\":1,\"a\":\"usnm-store test {}\"}}",
            std::process::id()
        );
        q.send("ocr-deadletter", &text).await.unwrap();
        assert!(q.peek("ocr-deadletter", 32).await.unwrap().contains(&text));
        let mut got = None;
        while let Some(m) = q
            .receive("ocr-deadletter", Duration::from_secs(30))
            .await
            .unwrap()
        {
            if m.text == text {
                got = Some(m);
                break;
            }
        }
        let m = got.expect("the sent message comes back");
        assert_eq!(m.dequeue_count, 1);
        // A release sends a copy to the back and deletes the original: the copy comes
        // back with a new id, read once; releasing the original again is a lost lease.
        q.release(&m).await.unwrap();
        assert!(q
            .delete(&m)
            .await
            .unwrap_err()
            .downcast_ref::<LeaseLost>()
            .is_some());
        let mut again = None;
        while let Some(r) = q
            .receive("ocr-deadletter", Duration::from_secs(30))
            .await
            .unwrap()
        {
            if r.text == text {
                again = Some(r);
                break;
            }
        }
        let copy = again.expect("the released copy comes back");
        assert_ne!(copy.id, m.id);
        assert_eq!(copy.dequeue_count, 1);
        let m = copy;
        let stale = Received {
            pop_receipt: "2000-01-01 00:00:00+00".into(),
            ..m.clone()
        };
        assert!(q
            .delete(&stale)
            .await
            .unwrap_err()
            .downcast_ref::<LeaseLost>()
            .is_some());
        q.delete(&m).await.unwrap();
        assert!(q
            .delete(&m)
            .await
            .unwrap_err()
            .downcast_ref::<LeaseLost>()
            .is_some());
    }
}
