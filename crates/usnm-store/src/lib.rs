//! Object storage for published reference data and the persistent response
//! cache (04 §4.3, 06 §6.5).
//!
//! Production reads and writes Azure Blob Storage over its private endpoint
//! with the app's managed identity (08 §8.3): Entra ID bearer tokens only, no
//! account keys or SAS. Development and tests use a local directory with the
//! same layout.

use std::sync::Arc;

use async_trait::async_trait;

pub mod blob;
pub mod credential;
pub mod local;

pub use blob::BlobStore;
pub use local::LocalStore;

/// Largest object `get` will read into memory.
pub const MAX_OBJECT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid object path `{0}`")]
    InvalidPath(String),
    #[error("invalid store location: {0}")]
    InvalidLocation(String),
    #[error("{0}")]
    Io(String),
    #[error("{op} `{path}` returned HTTP {status}")]
    Http {
        op: &'static str,
        path: String,
        status: u16,
    },
    #[error("credential: {0}")]
    Credential(String),
    #[error("object `{0}` exceeds {MAX_OBJECT_BYTES} bytes")]
    TooLarge(String),
}

#[async_trait]
pub trait ObjectStore: Send + Sync + std::fmt::Debug {
    /// Read a whole object; `None` if it doesn't exist.
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError>;

    /// Create an object unless one already exists at `path` (objects are
    /// immutable once written). Returns whether this call wrote it.
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, StoreError>;

    /// Create or replace an object. Only the publisher uses this, for the
    /// version pointer (`current.json`); everything else is create-only.
    async fn put(&self, path: &str, body: Vec<u8>, content_type: &str) -> Result<(), StoreError>;
}

/// Open a store: an `https://{account}.blob.core.windows.net/{container}[/prefix]`
/// URL uses Blob Storage with the ambient Entra ID credential
/// ([`credential::from_env`]); anything else is a local directory.
pub fn open(location: &str) -> Result<Arc<dyn ObjectStore>, StoreError> {
    if location.starts_with("https://") || location.starts_with("http://") {
        Ok(Arc::new(BlobStore::new(location, credential::from_env())?))
    } else {
        Ok(Arc::new(LocalStore::new(location)))
    }
}

/// A single path segment: `[A-Za-z0-9][A-Za-z0-9._-]{0,127}`. Version and
/// index ids are checked with this before they become part of a path.
pub fn is_safe_segment(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 128
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

/// Object paths are `/`-separated safe segments, so they need no URL encoding
/// and can't escape a local root.
pub fn validate_path(path: &str) -> Result<(), StoreError> {
    if path.len() <= 1024 && path.split('/').all(is_safe_segment) {
        Ok(())
    } else {
        Err(StoreError::InvalidPath(path.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        for ok in [
            "current.json",
            "fixture-v1/places.json",
            "v1/f1/ab12.json.zst",
        ] {
            assert!(validate_path(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "/abs", "a//b", "../x", "a/../b", ".hidden", "a b", "a?b", "a%2fb", "a\\b",
        ] {
            assert!(validate_path(bad).is_err(), "{bad}");
        }
    }
}
