//! A directory with the same layout as a Blob container.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::{validate_path, ObjectStore, StoreError, MAX_APPEND_BYTES, MAX_OBJECT_BYTES};

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

    async fn append(
        &self,
        path: &str,
        body: Vec<u8>,
        _content_type: &str,
    ) -> Result<(), StoreError> {
        use std::io::Write;
        validate_path(path)?;
        if body.len() > MAX_APPEND_BYTES {
            return Err(StoreError::TooLarge(path.into()));
        }
        let full = self.root.join(path);
        tokio::task::spawn_blocking(move || {
            let dir = full.parent().expect("validated paths have a parent");
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
            // One write with O_APPEND, so concurrent appends don't interleave.
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&full)
                .and_then(|mut f| f.write_all(&body))
                .map_err(|e| io(&full, e))
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
        s.append("log/a.jsonl", b"1\n".to_vec(), "").await.unwrap();
        s.append("log/a.jsonl", b"2\n".to_vec(), "").await.unwrap();
        assert_eq!(s.get("log/a.jsonl").await.unwrap().unwrap(), b"1\n2\n");
        assert!(s
            .append("log/a.jsonl", vec![b'x'; MAX_APPEND_BYTES + 1], "")
            .await
            .is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
