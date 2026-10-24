//! [`ScanSource`], and loc.gov as one.

use async_trait::async_trait;
use usnm_core::ids::PageKey;

use crate::loc::{FetchError, LocClient, Scan};

/// Where a page's scan comes from (loc.gov, or a stand-in in tests).
#[async_trait]
pub trait ScanSource: Send + Sync + std::fmt::Debug {
    /// The page's full scan. `batch` (the message's) finds it in the cache.
    async fn scan(&self, key: &PageKey, batch: &str) -> Result<Scan, FetchError>;
    /// Until when the source holds every request (loc.gov said to wait).
    fn held_until(&self) -> Option<tokio::time::Instant> {
        None
    }
    /// The page's full-size IIIF JPEG (`<iiif_base>/full/full/0/default.jpg`),
    /// read when the engine crashes on its JP2 (#454).
    async fn jpeg(&self, key: &PageKey, batch: &str, iiif_base: &str) -> Result<Scan, FetchError>;
    /// Delete a buffer-set scan the page no longer needs (best effort).
    async fn evict(&self, _url: &str) {}
}

#[async_trait]
impl ScanSource for LocClient {
    async fn scan(&self, key: &PageKey, _batch: &str) -> Result<Scan, FetchError> {
        LocClient::scan(self, key).await
    }

    async fn jpeg(
        &self,
        _key: &PageKey,
        _batch: &str,
        iiif_base: &str,
    ) -> Result<Scan, FetchError> {
        self.iiif_scan(iiif_base.to_owned(), crate::loc::ImageSize::Full)
            .await
    }

    fn held_until(&self) -> Option<tokio::time::Instant> {
        LocClient::held_until(self)
    }
}
