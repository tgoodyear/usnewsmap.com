//! A directory with the same layout as a Blob container.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::{validate_path, ObjectStore, StoreError, MAX_OBJECT_BYTES};

#[derive(Debug, Clone)]
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn io(path: &Path, e: std::io::Error) -> StoreError {
    StoreError::Io(format!("{}: {e}", path.display()))
}

#[async_trait]
impl ObjectStore for LocalStore {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        validate_path(path)?;
        let full = self.root.join(path);
        match tokio::fs::metadata(&full).await {
            Ok(m) if m.len() > MAX_OBJECT_BYTES => return Err(StoreError::TooLarge(path.into())),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io(&full, e)),
        }
        match tokio::fs::read(&full).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&full, e)),
        }
    }

    async fn put(&self, path: &str, body: Vec<u8>, _content_type: &str) -> Result<(), StoreError> {
        validate_path(path)?;
        let full = self.root.join(path);
        tokio::task::spawn_blocking(move || {
            let dir = full.parent().expect("validated paths have a parent");
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
            // Readers see the old object or the new one, never a partial write.
            let tmp = dir.join(format!(
                ".tmp-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&tmp, &body).map_err(|e| io(&tmp, e))?;
            std::fs::rename(&tmp, &full).map_err(|e| io(&full, e))
        })
        .await
        .map_err(|e| StoreError::Io(e.to_string()))?
    }

    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        _content_type: &str,
    ) -> Result<bool, StoreError> {
        validate_path(path)?;
        let full = self.root.join(path);
        tokio::task::spawn_blocking(move || {
            let dir = full.parent().expect("validated paths have a parent");
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
            // Write a private temp file, then hard-link it into place: the
            // link fails if the object exists, so creation is atomic.
            let tmp = dir.join(format!(
                ".tmp-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&tmp, &body).map_err(|e| io(&tmp, e))?;
            let linked = std::fs::hard_link(&tmp, &full);
            let _ = std::fs::remove_file(&tmp);
            match linked {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(io(&full, e)),
            }
        })
        .await
        .map_err(|e| StoreError::Io(e.to_string()))?
    }

    async fn exists(&self, path: &str) -> Result<bool, StoreError> {
        validate_path(path)?;
        let full = self.root.join(path);
        match tokio::fs::metadata(&full).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io(&full, e)),
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        validate_path(prefix)?;
        let root = self.root.clone();
        let prefix = prefix.to_owned();
        tokio::task::spawn_blocking(move || {
            fn walk(dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), StoreError> {
                let entries = match std::fs::read_dir(dir) {
                    Ok(e) => e,
                    Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(io(dir, e)),
                };
                for entry in entries {
                    let entry = entry.map_err(|e| io(dir, e))?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let path = format!("{rel}/{name}");
                    // Temporary files and anything else that isn't a valid path.
                    if validate_path(&path).is_err() {
                        continue;
                    }
                    if entry.path().is_dir() {
                        walk(&entry.path(), &path, out)?;
                    } else {
                        out.push(path);
                    }
                }
                Ok(())
            }
            let mut out = Vec::new();
            walk(&root.join(&prefix), &prefix, &mut out)?;
            out.sort();
            Ok(out)
        })
        .await
        .map_err(|e| StoreError::Io(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_only_round_trip() {
        let dir = std::env::temp_dir().join(format!("usnm-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = LocalStore::new(&dir);
        assert_eq!(s.get("v1/a.json").await.unwrap(), None);
        assert!(s.put_new("v1/a.json", b"one".to_vec(), "").await.unwrap());
        assert!(!s.put_new("v1/a.json", b"two".to_vec(), "").await.unwrap());
        assert_eq!(s.get("v1/a.json").await.unwrap().unwrap(), b"one");
        s.put("v1/a.json", b"three".to_vec(), "").await.unwrap();
        assert_eq!(s.get("v1/a.json").await.unwrap().unwrap(), b"three");
        assert!(s.get("../etc/passwd").await.is_err());
        assert!(s.exists("v1/a.json").await.unwrap());
        assert!(!s.exists("v1/b.json").await.unwrap());
        assert!(s.put_new("log/d/2.jsonl", b"2".to_vec(), "").await.unwrap());
        assert!(s.put_new("log/d/1.jsonl", b"1".to_vec(), "").await.unwrap());
        assert!(s.put_new("logs/x.jsonl", b"x".to_vec(), "").await.unwrap());
        assert_eq!(
            s.list("log").await.unwrap(),
            ["log/d/1.jsonl", "log/d/2.jsonl"]
        );
        assert!(s.list("none").await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
