//! Document state storage (05 §5.9.1): the few operations the pipeline needs
//! from Cosmos DB, behind a trait so tests run against memory.
//!
//! Every item is JSON with an `id`, lives in a named container and has a
//! partition key value. Writes that change shared state (claims, commits,
//! locks) use optimistic concurrency: `replace` succeeds only if the item's
//! ETag is unchanged since it was read.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

/// An item and the ETag it was read with.
#[derive(Debug, Clone)]
pub struct Versioned {
    pub doc: Value,
    pub etag: String,
}

#[async_trait]
pub trait DocStore: Send + Sync {
    async fn get(&self, container: &str, pk: &str, id: &str) -> anyhow::Result<Option<Versioned>>;

    /// Create an item; `None` if one with the same id already exists.
    async fn create(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
    ) -> anyhow::Result<Option<String>>;

    /// Replace an item only if its ETag still matches; `None` if it changed.
    async fn replace(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>>;

    /// Create or overwrite, unconditionally (idempotent records such as issues).
    async fn upsert(&self, container: &str, pk: &str, doc: &Value) -> anyhow::Result<()>;

    /// Items whose `field` is one of `values` (all items if `values` is empty).
    async fn list(
        &self,
        container: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<Versioned>>;
}

fn id_of(doc: &Value) -> anyhow::Result<&str> {
    doc["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("item has no string `id`"))
}

/// In-process store for tests and single-machine runs.
#[derive(Default)]
pub struct MemoryDocs {
    items: Mutex<HashMap<(String, String, String), Versioned>>,
    next_etag: Mutex<u64>,
}

impl MemoryDocs {
    fn etag(&self) -> String {
        let mut n = self.next_etag.lock().expect("etag counter");
        *n += 1;
        format!("\"{n}\"")
    }
}

#[async_trait]
impl DocStore for MemoryDocs {
    async fn get(&self, container: &str, pk: &str, id: &str) -> anyhow::Result<Option<Versioned>> {
        let items = self.items.lock().expect("items");
        Ok(items
            .get(&(container.into(), pk.into(), id.into()))
            .cloned())
    }

    async fn create(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
    ) -> anyhow::Result<Option<String>> {
        let key = (container.to_owned(), pk.to_owned(), id_of(doc)?.to_owned());
        let etag = self.etag();
        let mut items = self.items.lock().expect("items");
        if items.contains_key(&key) {
            return Ok(None);
        }
        items.insert(
            key,
            Versioned {
                doc: doc.clone(),
                etag: etag.clone(),
            },
        );
        Ok(Some(etag))
    }

    async fn replace(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        let key = (container.to_owned(), pk.to_owned(), id_of(doc)?.to_owned());
        let fresh = self.etag();
        let mut items = self.items.lock().expect("items");
        match items.get_mut(&key) {
            Some(v) if v.etag == etag => {
                *v = Versioned {
                    doc: doc.clone(),
                    etag: fresh.clone(),
                };
                Ok(Some(fresh))
            }
            Some(_) => Ok(None),
            None => anyhow::bail!("{container}/{}: no such item", key.2),
        }
    }

    async fn upsert(&self, container: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        let key = (container.to_owned(), pk.to_owned(), id_of(doc)?.to_owned());
        let etag = self.etag();
        self.items.lock().expect("items").insert(
            key,
            Versioned {
                doc: doc.clone(),
                etag,
            },
        );
        Ok(())
    }

    async fn list(
        &self,
        container: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<Versioned>> {
        let items = self.items.lock().expect("items");
        let mut out: Vec<Versioned> = items
            .iter()
            .filter(|((c, _, _), v)| {
                c == container
                    && (values.is_empty()
                        || v.doc[field].as_str().is_some_and(|s| values.contains(&s)))
            })
            .map(|(_, v)| v.clone())
            .collect();
        out.sort_by(|a, b| a.doc["id"].as_str().cmp(&b.doc["id"].as_str()));
        Ok(out)
    }
}

/// [`MemoryDocs`] saved to a JSON file after every write: local runs of the
/// CLI, one process at a time.
pub struct FileDocs {
    path: std::path::PathBuf,
    mem: MemoryDocs,
}

impl FileDocs {
    pub fn open(path: impl Into<std::path::PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        let mem = MemoryDocs::default();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let saved: Vec<(String, String, Value, String)> = serde_json::from_slice(&bytes)?;
                let mut items = mem.items.lock().expect("items");
                let mut max = 0;
                for (c, pk, doc, etag) in saved {
                    max = max.max(etag.trim_matches('"').parse::<u64>().unwrap_or(0));
                    let id = id_of(&doc)?.to_owned();
                    items.insert((c, pk, id), Versioned { doc, etag });
                }
                *mem.next_etag.lock().expect("etag") = max;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(Self { path, mem })
    }

    fn save(&self) -> anyhow::Result<()> {
        let items = self.mem.items.lock().expect("items");
        let mut saved: Vec<(&str, &str, &Value, &str)> = items
            .iter()
            .map(|((c, pk, _), v)| (c.as_str(), pk.as_str(), &v.doc, v.etag.as_str()))
            .collect();
        saved.sort_by(|a, b| (a.0, a.1, a.2["id"].as_str()).cmp(&(b.0, b.1, b.2["id"].as_str())));
        let tmp = self.path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&saved)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[async_trait]
impl DocStore for FileDocs {
    async fn get(&self, container: &str, pk: &str, id: &str) -> anyhow::Result<Option<Versioned>> {
        self.mem.get(container, pk, id).await
    }

    async fn create(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
    ) -> anyhow::Result<Option<String>> {
        let r = self.mem.create(container, pk, doc).await?;
        self.save()?;
        Ok(r)
    }

    async fn replace(
        &self,
        container: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        let r = self.mem.replace(container, pk, doc, etag).await?;
        self.save()?;
        Ok(r)
    }

    async fn upsert(&self, container: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        self.mem.upsert(container, pk, doc).await?;
        self.save()
    }

    async fn list(
        &self,
        container: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<Versioned>> {
        self.mem.list(container, field, values).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn optimistic_concurrency() {
        let m = MemoryDocs::default();
        let e1 = m
            .create("c", "p", &json!({"id": "a", "n": 1}))
            .await
            .unwrap()
            .unwrap();
        assert!(m
            .create("c", "p", &json!({"id": "a"}))
            .await
            .unwrap()
            .is_none());
        let e2 = m
            .replace("c", "p", &json!({"id": "a", "n": 2}), &e1)
            .await
            .unwrap()
            .unwrap();
        // A writer holding the old ETag loses.
        assert!(m
            .replace("c", "p", &json!({"id": "a", "n": 3}), &e1)
            .await
            .unwrap()
            .is_none());
        let got = m.get("c", "p", "a").await.unwrap().unwrap();
        assert_eq!((got.doc["n"].as_i64(), got.etag), (Some(2), e2));
        m.upsert("c", "q", &json!({"id": "b", "status": "x"}))
            .await
            .unwrap();
        assert_eq!(m.list("c", "status", &["x"]).await.unwrap().len(), 1);
        assert_eq!(m.list("c", "status", &[]).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn file_store_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let f = FileDocs::open(&path).unwrap();
        let e = f
            .create("c", "p", &json!({"id": "a"}))
            .await
            .unwrap()
            .unwrap();
        drop(f);
        let f = FileDocs::open(&path).unwrap();
        let got = f.get("c", "p", "a").await.unwrap().unwrap();
        assert_eq!(got.etag, e);
        // New ETags never collide with saved ones.
        let e2 = f
            .replace("c", "p", &json!({"id": "a", "n": 1}), &e)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(e, e2);
    }
}
