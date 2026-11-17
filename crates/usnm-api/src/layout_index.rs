//! Which pages have a stored layout (#2026), so `/v1/hits` items can say
//! `links.layout` only for those.
//!
//! The ALTO is at `ocr-local/alto/<doc_id>.<node>.<stamp>.xml` in the curated
//! store (`USNM_ALTO_URL`). The ids are listed in the background at startup
//! and again every [`REFRESH`]; a request only looks an id up in the last
//! listing and never lists the store. Until the first listing is done, or
//! with no store, no page has a link.
//!
//! Memory: the ids are one sorted `Vec<Box<str>>`. A LoC doc id is about 35
//! bytes, so an id costs about 16 (the box's pointer and length) + 48 (the
//! allocation, rounded up) = 64 bytes: about 1.6 MB for 25,000 ids and about
//! 64 MB for 1,000,000. A refresh holds the listing's paths (about 100 bytes
//! each) beside it for a moment: about 2.5 MB and 100 MB.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use usnm_store::{ObjectStore, StoreError};

use crate::AppState;

/// Where the pickup stores our OCR's ALTO.
pub(crate) const ALTO_DIR: &str = "ocr-local/alto";

/// How often the store is listed again.
pub const REFRESH: Duration = Duration::from_secs(3600);

/// The doc ids with stored ALTO, as of the last listing.
#[derive(Default)]
pub struct LayoutIndex {
    ids: ArcSwap<Vec<Box<str>>>,
    loaded: AtomicBool,
}

impl LayoutIndex {
    /// Whether `doc_id` (a bare LoC id) had stored ALTO at the last listing.
    pub fn has(&self, doc_id: &str) -> bool {
        self.ids
            .load()
            .binary_search_by(|id| id.as_ref().cmp(doc_id))
            .is_ok()
    }

    /// Whether a listing has finished.
    pub fn loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// Lists `store` and replaces the ids. Returns how many ids there are.
    /// A failed listing leaves the last one in place.
    pub async fn refresh(&self, store: &dyn ObjectStore) -> Result<usize, StoreError> {
        let paths = store.list(ALTO_DIR).await?;
        let ids = doc_ids(&paths);
        let n = ids.len();
        self.ids.store(Arc::new(ids));
        self.loaded.store(true, Ordering::Release);
        Ok(n)
    }
}

/// The sorted, distinct doc ids in `paths`: `ocr-local/alto/<doc_id>.<node>.<stamp>.xml`.
/// Anything else is skipped.
fn doc_ids(paths: &[String]) -> Vec<Box<str>> {
    let mut ids: Vec<Box<str>> = paths
        .iter()
        .filter_map(|p| {
            let name = p.strip_prefix(ALTO_DIR)?.strip_prefix('/')?;
            let (doc_id, rest) = name.split_once('.')?;
            let (node, stamp) = rest.strip_suffix(".xml")?.rsplit_once('.')?;
            (!doc_id.is_empty() && !node.is_empty() && !stamp.is_empty()).then(|| doc_id.into())
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.shrink_to_fit();
    ids
}

/// Lists the store now and then every [`REFRESH`], in the background. No-op
/// without `USNM_ALTO_URL`.
pub fn spawn(state: Arc<AppState>) {
    let Some(store) = state.alto.clone() else {
        return;
    };
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REFRESH);
        loop {
            tick.tick().await;
            match state.layout_ids.refresh(store.as_ref()).await {
                Ok(n) => tracing::info!(pages = n, "pages with a stored layout listed"),
                Err(e) => tracing::warn!(error = %e, "listing the stored layouts failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_sorted_distinct_and_only_from_alto_names() {
        let paths: Vec<String> = [
            "ocr-local/alto/b_1896-01-02_ed-1_seq-1.node1.20261101T000000Z.xml",
            "ocr-local/alto/a_1896-01-02_ed-1_seq-1.node2.20261101T000000Z.xml",
            "ocr-local/alto/a_1896-01-02_ed-1_seq-1.node1.20261102T000000Z.xml",
            "ocr-local/alto/a_1896-01-02_ed-1_seq-1.node1.20261102T000000Z.tmp",
            "ocr-local/alto/.node1.20261102T000000Z.xml",
            "ocr-local/alto/nostamp.xml",
            "ocr-local/other/c.node1.20261102T000000Z.xml",
        ]
        .map(String::from)
        .to_vec();
        let ids = doc_ids(&paths);
        assert_eq!(
            ids.iter().map(|i| &**i).collect::<Vec<_>>(),
            ["a_1896-01-02_ed-1_seq-1", "b_1896-01-02_ed-1_seq-1"]
        );
    }
}
