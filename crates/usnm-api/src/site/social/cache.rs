use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{stream, StreamExt};
use usnm_store::ObjectStore;

use super::{digest, RenderError};

pub(super) const LIFETIME: Duration = Duration::from_secs(24 * 3600);
const STORE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
const PREVIEW_QUOTA_BYTES: u64 = 5_000_000_000;
const ACCESS_INTERVAL: Duration = Duration::from_secs(60 * 60);
const EVICTION_LOCK: &str = "search-preview-quota-lock/eviction.lock";
const EVICTION_LOCK_STALE: Duration = Duration::from_secs(10 * 60);
const DELETE_CONCURRENCY: usize = 32;
type Persistent = Option<(Arc<dyn ObjectStore>, String)>;

pub(crate) struct Service {
    images: moka::future::Cache<String, Arc<Image>>,
    renders: Arc<tokio::sync::Semaphore>,
    quota_lock: Arc<tokio::sync::Mutex<()>>,
    last_quota_check: Arc<AtomicU64>,
    pub(super) frontend: tokio::sync::OnceCell<String>,
}

struct Image {
    body: Vec<u8>,
    stored: AtomicBool,
    persistence: tokio::sync::Mutex<()>,
    last_access: std::sync::atomic::AtomicU64,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            images: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(LIFETIME)
                .build(),
            renders: Arc::new(tokio::sync::Semaphore::new(2)),
            quota_lock: Arc::new(tokio::sync::Mutex::new(())),
            last_quota_check: Arc::new(AtomicU64::new(0)),
            frontend: tokio::sync::OnceCell::new(),
        }
    }
}

impl Service {
    pub(super) fn idle(&self) -> bool {
        self.renders.available_permits() == 2
    }

    /// Metadata discovery never waits for storage or advertises a local-only PNG.
    pub(super) async fn is_published(&self, key: &str) -> bool {
        self.images
            .get(key)
            .await
            .is_some_and(|image| image.stored.load(Ordering::Acquire))
    }

    /// Read a published image without admitting a new render.
    pub(super) async fn lookup(&self, key: &str, persistent: Persistent) -> Option<Vec<u8>> {
        if let Some(image) = self.images.get(key).await {
            if image.stored.load(Ordering::Acquire) {
                self.schedule_quota_check(&persistent);
            }
            retry_persistence(
                image.clone(),
                persistent.clone(),
                self.quota_lock.clone(),
                self.last_quota_check.clone(),
            );
            touch_recent(image.clone(), persistent);
            return Some(image.body.clone());
        }
        let (store, path) = persistent?;
        let body = read(store.as_ref(), &path).await?;
        self.images
            .insert(
                key.to_owned(),
                Arc::new(Image {
                    body: body.clone(),
                    stored: AtomicBool::new(true),
                    persistence: tokio::sync::Mutex::new(()),
                    last_access: std::sync::atomic::AtomicU64::new(0),
                }),
            )
            .await;
        if let Some(image) = self.images.get(key).await {
            self.schedule_quota_check(&Some((store.clone(), path.clone())));
            touch_recent(image, Some((store, path.to_owned())));
        }
        Some(body)
    }

    pub(super) async fn get(
        &self,
        key: String,
        persistent: Persistent,
        background: bool,
        render: impl Future<Output = Result<Vec<u8>, RenderError>>,
    ) -> Result<Vec<u8>, Arc<RenderError>> {
        if !background {
            if let Some(image) = self.images.get(&key).await {
                if image.stored.load(Ordering::Acquire) {
                    self.schedule_quota_check(&persistent);
                }
                retry_persistence(
                    image.clone(),
                    persistent.clone(),
                    self.quota_lock.clone(),
                    self.last_quota_check.clone(),
                );
                touch_recent(image.clone(), persistent);
                return Ok(image.body.clone());
            }
        }
        let image = self
            .images
            .try_get_with(key, async {
                if let Some((store, path)) = &persistent {
                    if let Some(body) = read(store.as_ref(), path).await {
                        return Ok(Arc::new(Image {
                            body,
                            stored: AtomicBool::new(true),
                            persistence: tokio::sync::Mutex::new(()),
                            last_access: std::sync::atomic::AtomicU64::new(0),
                        }));
                    }
                }
                // One background worker starts only with both slots free,
                // leaving the second slot for Share/crawler requests.
                let mut permit = self
                    .renders
                    .clone()
                    .try_acquire_many_owned(if background { 2 } else { 1 })
                    .map_err(|_| RenderError::Busy)?;
                if background {
                    drop(permit.split(1));
                }
                let body = render.await?;
                drop(permit);
                Ok(Arc::new(Image {
                    body,
                    stored: AtomicBool::new(false),
                    persistence: tokio::sync::Mutex::new(()),
                    last_access: std::sync::atomic::AtomicU64::new(0),
                }))
            })
            .await?;
        persist(
            &image,
            persistent.clone(),
            self.quota_lock.clone(),
            self.last_quota_check.clone(),
        )
        .await;
        if image.stored.load(Ordering::Acquire) {
            self.schedule_quota_check(&persistent);
        }
        touch_recent(image.clone(), persistent);
        Ok(image.body.clone())
    }

    fn schedule_quota_check(&self, persistent: &Persistent) {
        let Some((store, _)) = persistent else {
            return;
        };
        schedule_quota_scan(
            store.clone(),
            self.quota_lock.clone(),
            self.last_quota_check.clone(),
            false,
        );
    }
}

fn schedule_quota_scan(
    store: Arc<dyn ObjectStore>,
    quota_lock: Arc<tokio::sync::Mutex<()>>,
    last_check: Arc<AtomicU64>,
    force: bool,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .max(1);
    if !force {
        let last = last_check.load(Ordering::Acquire);
        if last != 0 && now.saturating_sub(last) < 60 * 60 {
            return;
        }
        if last_check
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
    } else {
        last_check.store(now, Ordering::Release);
    }
    tokio::spawn(async move {
        enforce_quota(store, quota_lock, PREVIEW_QUOTA_BYTES).await;
    });
}

fn retry_persistence(
    image: Arc<Image>,
    persistent: Persistent,
    quota_lock: Arc<tokio::sync::Mutex<()>>,
    last_quota_check: Arc<AtomicU64>,
) {
    if persistent.is_some() && !image.stored.load(Ordering::Acquire) {
        // A ready PNG must not wait on storage and lose the sharing deadline.
        // `persist` coalesces writes and bounds the detached retry.
        tokio::spawn(async move {
            persist(
                &image,
                persistent.clone(),
                quota_lock.clone(),
                last_quota_check.clone(),
            )
            .await;
            if image.stored.load(Ordering::Acquire) {
                if let Some((store, _)) = &persistent {
                    schedule_quota_scan(store.clone(), quota_lock, last_quota_check, false);
                }
            }
            touch_recent(image, persistent);
        });
    }
}

fn touch_recent(image: Arc<Image>, persistent: Persistent) {
    let Some((store, path)) = persistent else {
        return;
    };
    if !image.stored.load(Ordering::Acquire) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .max(1);
    let last = image.last_access.load(Ordering::Acquire);
    if last != 0 && now.saturating_sub(last) < ACCESS_INTERVAL.as_secs() {
        return;
    }
    if image
        .last_access
        .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    tokio::spawn(async move {
        let marker = access_path(&path);
        match tokio::time::timeout(
            STORE_TIMEOUT,
            store.put(&marker, Vec::new(), "application/octet-stream"),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => {
                image.last_access.store(0, Ordering::Release);
                tracing::warn!("search preview access timestamp update failed");
            }
        }
    });
}

fn access_path(path: &str) -> String {
    format!("search-preview-lru/{}.access", digest(path))
}

async fn persist(
    image: &Image,
    persistent: Persistent,
    quota_lock: Arc<tokio::sync::Mutex<()>>,
    last_quota_check: Arc<AtomicU64>,
) {
    let Ok(_turn) = image.persistence.try_lock() else {
        return;
    };
    if let Some((store, path)) = persistent {
        if !image.stored.load(Ordering::Acquire) {
            match tokio::time::timeout(
                STORE_TIMEOUT,
                store.put_new(&path, image.body.clone(), "image/png"),
            )
            .await
            {
                Ok(Ok(true)) => {
                    image.stored.store(true, Ordering::Release);
                    schedule_quota_scan(
                        store.clone(),
                        quota_lock.clone(),
                        last_quota_check.clone(),
                        true,
                    );
                }
                Ok(Ok(false)) if read(store.as_ref(), &path).await.is_some() => {
                    image.stored.store(true, Ordering::Release);
                }
                Ok(Ok(false)) => {
                    match tokio::time::timeout(
                        STORE_TIMEOUT,
                        store.put_new(&path, image.body.clone(), "image/png"),
                    )
                    .await
                    {
                        Ok(Ok(true)) => {
                            image.stored.store(true, Ordering::Release);
                            schedule_quota_scan(
                                store.clone(),
                                quota_lock.clone(),
                                last_quota_check.clone(),
                                true,
                            );
                        }
                        Ok(Ok(false)) => {
                            tracing::warn!(
                                "search preview persistence conflict remains unresolved"
                            );
                        }
                        Ok(Err(_)) => {
                            tracing::warn!("search preview persistence retry failed");
                        }
                        Err(_) => tracing::warn!("search preview persistence retry timed out"),
                    }
                }
                // Store errors can contain the query-derived filename.
                Ok(Err(_)) => {
                    tracing::warn!("search preview persistence failed");
                }
                Err(_) => tracing::warn!("search preview persistence timed out"),
            }
        }
    }
}

async fn enforce_quota(
    store: Arc<dyn ObjectStore>,
    local_lock: Arc<tokio::sync::Mutex<()>>,
    quota: u64,
) {
    let _local = local_lock.lock().await;
    let lock = loop {
        match acquire_eviction_lock(store.as_ref()).await {
            Ok(Some(lock)) => break lock,
            Ok(None) => tokio::time::sleep(Duration::from_secs(5)).await,
            Err(_) => {
                tracing::warn!("search preview quota lock failed");
                return;
            }
        }
    };
    let result = tokio::time::timeout(
        Duration::from_secs(9 * 60),
        evict_lru(store.as_ref(), quota),
    )
    .await;
    match result {
        Ok(Ok(evicted)) if evicted > 0 => {
            tracing::info!(
                bytes = evicted,
                "evicted least-recently-used search previews"
            )
        }
        Ok(Ok(_)) => {}
        Ok(Err(_)) => tracing::warn!("search preview quota scan failed"),
        Err(_) => tracing::warn!("search preview quota scan timed out"),
    }
    if store
        .delete_if_match(EVICTION_LOCK, &lock.etag)
        .await
        .is_err()
    {
        tracing::warn!("search preview quota lock release failed");
    }
}

async fn acquire_eviction_lock(
    store: &dyn ObjectStore,
) -> Result<Option<usnm_store::ObjectInfo>, usnm_store::StoreError> {
    static TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..3 {
        let token = format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            TOKEN.fetch_add(1, Ordering::Relaxed),
        );
        if store
            .put_new(EVICTION_LOCK, token.into_bytes(), "text/plain")
            .await?
        {
            let lock = store
                .list_info("search-preview-quota-lock")
                .await?
                .into_iter()
                .find(|object| object.path == EVICTION_LOCK);
            return Ok(lock);
        }
        let Some(lock) = store
            .list_info("search-preview-quota-lock")
            .await?
            .into_iter()
            .find(|object| object.path == EVICTION_LOCK)
        else {
            continue;
        };
        let last_modified = lock.modified.or(lock.created);
        if last_modified.is_some_and(|time| {
            SystemTime::now().duration_since(time).unwrap_or_default() > EVICTION_LOCK_STALE
        }) {
            store.delete_if_match(EVICTION_LOCK, &lock.etag).await?;
            continue;
        }
        return Ok(None);
    }
    Ok(None)
}

async fn evict_lru(store: &dyn ObjectStore, quota: u64) -> Result<u64, usnm_store::StoreError> {
    let mut images = store
        .list_info("search-preview")
        .await?
        .into_iter()
        .filter(|object| {
            object.path.starts_with("search-preview/") && object.path.ends_with(".png")
        })
        .collect::<Vec<_>>();
    let markers = store.list_info("search-preview-lru").await?;
    let by_path = markers
        .into_iter()
        .map(|object| (object.path.clone(), object))
        .collect::<HashMap<_, _>>();
    let active_markers = images
        .iter()
        .map(|image| access_path(&image.path))
        .collect::<std::collections::HashSet<_>>();
    let mut used = images
        .iter()
        .fold(0_u64, |total, image| total.saturating_add(image.size));
    if used <= quota {
        cleanup_orphan_markers(store, &by_path, &active_markers).await;
        return Ok(0);
    }
    images.sort_by_key(|image| {
        by_path
            .get(&access_path(&image.path))
            .and_then(|marker| marker.modified.or(marker.created))
            .or(image.modified)
            .or(image.created)
            .unwrap_or(UNIX_EPOCH)
    });
    let candidates = images
        .into_iter()
        .map(|image| {
            let marker = by_path.get(&access_path(&image.path)).cloned();
            (image, marker)
        })
        .collect::<Vec<_>>();
    let mut evicted = 0_u64;
    let mut candidates = candidates.into_iter();
    while used > quota {
        let excess = used - quota;
        let mut batch = Vec::new();
        let mut selected_bytes = 0_u64;
        while batch.len() < DELETE_CONCURRENCY && selected_bytes < excess {
            let Some(candidate) = candidates.next() else {
                break;
            };
            selected_bytes = selected_bytes.saturating_add(candidate.0.size);
            batch.push(candidate);
        }
        if batch.is_empty() {
            break;
        }
        let results = stream::iter(batch.iter().cloned())
            .map(|(image, marker)| async move {
                let deleted = store.delete_if_match(&image.path, &image.etag).await?;
                if deleted {
                    if let Some(marker) = marker {
                        if store
                            .delete_if_match(&marker.path, &marker.etag)
                            .await
                            .is_err()
                        {
                            tracing::warn!("search preview access marker eviction failed");
                        }
                    }
                    Ok::<_, usnm_store::StoreError>((deleted, image.size))
                } else {
                    Ok((false, 0))
                }
            })
            .buffer_unordered(DELETE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            match result {
                Ok((true, size)) => {
                    used = used.saturating_sub(size);
                    evicted = evicted.saturating_add(size);
                }
                Ok((false, _)) => {}
                Err(_) => tracing::warn!("search preview eviction delete failed"),
            }
        }
    }
    cleanup_orphan_markers(store, &by_path, &active_markers).await;
    if used > quota {
        tracing::warn!(bytes = used, "search preview byte quota remains exceeded");
    }
    Ok(evicted)
}

async fn cleanup_orphan_markers(
    store: &dyn ObjectStore,
    markers: &HashMap<String, usnm_store::ObjectInfo>,
    active: &std::collections::HashSet<String>,
) {
    for marker in markers
        .values()
        .filter(|marker| marker.path.ends_with(".access") && !active.contains(&marker.path))
    {
        if store
            .delete_if_match(&marker.path, &marker.etag)
            .await
            .is_err()
        {
            tracing::warn!("orphaned search preview access marker cleanup failed");
        }
    }
}

fn valid(body: &[u8]) -> bool {
    body.starts_with(b"\x89PNG\r\n\x1a\n")
        && body.get(16..20) == Some(1200_u32.to_be_bytes().as_slice())
        && body.get(20..24) == Some(630_u32.to_be_bytes().as_slice())
        && resvg::tiny_skia::Pixmap::decode_png(body)
            .is_ok_and(|image| image.width() == 1200 && image.height() == 630)
}

async fn remove_invalid(store: &dyn ObjectStore, path: &str) {
    let repair = async {
        let prefix = path.rsplit_once('/').map_or("", |(prefix, _)| prefix);
        let objects = store.list_info(prefix).await?;
        if let Some(object) = objects.iter().find(|object| object.path == path) {
            let invalid = if object.size > MAX_PNG_BYTES as u64 {
                true
            } else {
                store
                    .get_if_match(path, &object.etag, MAX_PNG_BYTES as u64)
                    .await?
                    .is_some_and(|body| !valid(&body))
            };
            if invalid && !store.delete_if_match(path, &object.etag).await? {
                tracing::warn!("invalid search preview changed before removal");
            }
        }
        Ok::<_, usnm_store::StoreError>(())
    };
    match tokio::time::timeout(STORE_TIMEOUT, repair).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => tracing::warn!("invalid search preview removal failed"),
        Err(_) => tracing::warn!("invalid search preview removal timed out"),
    }
}

async fn read(store: &dyn ObjectStore, path: &str) -> Option<Vec<u8>> {
    let result = tokio::time::timeout(STORE_TIMEOUT, async {
        let Some(mut chunks) = store.get_stream(path).await? else {
            return Ok(None);
        };
        let mut body = Vec::new();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > MAX_PNG_BYTES {
                return Err(usnm_store::StoreError::TooLarge(path.to_owned()));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Some(body))
    })
    .await;
    match result {
        Ok(Ok(Some(body))) => {
            if valid(&body) {
                Some(body)
            } else {
                tracing::warn!("persisted search preview is invalid; ignoring it");
                remove_invalid(store, path).await;
                None
            }
        }
        Ok(Ok(None)) => None,
        Ok(Err(usnm_store::StoreError::TooLarge(_))) => {
            tracing::warn!("persisted search preview is oversized");
            remove_invalid(store, path).await;
            None
        }
        Ok(Err(_)) => {
            tracing::warn!("search preview read failed");
            None
        }
        Err(_) => {
            tracing::warn!("search preview read timed out");
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        super::super::png(&super::super::Card::new("/datasets", "").unwrap()).unwrap()
    }

    async fn stored(service: &Service, path: &str) {
        tokio::time::timeout(STORE_TIMEOUT, async {
            while !service
                .images
                .get(path)
                .await
                .unwrap()
                .stored
                .load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn failed_persistence_retries_from_memory_without_rerendering() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        tokio::fs::write(&root, b"temporarily unavailable")
            .await
            .unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(&root));
        let path = "previews/example.png";
        let persistent = || Some((store.clone(), path.to_owned()));
        let service = Service::default();
        let body = image();
        assert_eq!(
            service
                .get(path.to_owned(), persistent(), false, async {
                    Ok(body.clone())
                })
                .await
                .unwrap(),
            body
        );
        tokio::fs::remove_file(&root).await.unwrap();
        tokio::fs::create_dir(&root).await.unwrap();
        assert_eq!(
            service
                .get(path.to_owned(), persistent(), false, async {
                    panic!("cached image must be reused")
                })
                .await
                .unwrap(),
            body
        );
        stored(&service, path).await;
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn cache_only_lookups_retry_failed_publication_without_rendering() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("store");
        tokio::fs::write(&root, b"temporarily unavailable")
            .await
            .unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(&root));
        let path = "search-preview/0123456789abcdef/custom.png";
        let persistent = || Some((store.clone(), path.to_owned()));
        let service = Service::default();
        let body = image();
        service
            .get(path.to_owned(), persistent(), false, async {
                Ok(body.clone())
            })
            .await
            .unwrap();
        tokio::fs::remove_file(&root).await.unwrap();
        tokio::fs::create_dir(&root).await.unwrap();
        assert_eq!(service.lookup(path, persistent()).await.unwrap(), body);
        stored(&service, path).await;
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
        let restarted = Service::default();
        assert_eq!(restarted.lookup(path, persistent()).await.unwrap(), body);
        assert!(restarted.lookup("missing.png", None).await.is_none());
    }

    #[tokio::test]
    async fn quota_evicts_oldest_preview_first_and_preserves_other_response_objects() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::LocalStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let response = "previews/shared/search-response.json";
        let body = image();
        store.put(old, body.clone(), "image/png").await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        store.put(recent, body.clone(), "image/png").await.unwrap();
        store
            .put(response, b"preserve".to_vec(), "application/json")
            .await
            .unwrap();
        let old_access = access_path(old);
        let recent_access = access_path(recent);
        store
            .put(&old_access, Vec::new(), "application/octet-stream")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        store
            .put(&recent_access, Vec::new(), "application/octet-stream")
            .await
            .unwrap();

        assert_eq!(
            evict_lru(&store, body.len() as u64).await.unwrap(),
            body.len() as u64
        );
        assert!(store.get(old).await.unwrap().is_none());
        assert_eq!(store.get(recent).await.unwrap().unwrap(), body);
        assert_eq!(store.get(response).await.unwrap().unwrap(), b"preserve");
        assert!(store.get(&old_access).await.unwrap().is_none());
        assert!(store.get(&recent_access).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn persistent_reads_record_hashed_access_markers() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let path = "search-preview/0123456789abcdef/quiet-words.png";
        let body = image();
        store.put(path, body.clone(), "image/png").await.unwrap();
        let service = Service::default();

        assert_eq!(
            service
                .lookup(path, Some((store.clone(), path.to_owned())))
                .await
                .unwrap(),
            body
        );
        let marker = access_path(path);
        tokio::time::timeout(STORE_TIMEOUT, async {
            while store.get(&marker).await.unwrap().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!marker.contains("quiet-words"));
    }

    #[tokio::test]
    async fn shared_quota_eviction_lock_serializes_processes() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::LocalStore::new(dir.path());
        let first = acquire_eviction_lock(&store).await.unwrap().unwrap();
        assert!(acquire_eviction_lock(&store).await.unwrap().is_none());
        assert!(store
            .delete_if_match(EVICTION_LOCK, &first.etag)
            .await
            .unwrap());
        assert!(acquire_eviction_lock(&store).await.unwrap().is_some());
    }

    #[derive(Debug)]
    struct SlowStore {
        inner: usnm_store::LocalStore,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        writes: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ObjectStore for SlowStore {
        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            self.inner.get(path).await
        }
        async fn put_new(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            if path.ends_with(".png") {
                self.writes.fetch_add(1, Ordering::SeqCst);
                self.started.notify_one();
                self.release.notified().await;
            }
            self.inner.put_new(path, body, content_type).await
        }
        async fn put(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<(), usnm_store::StoreError> {
            self.inner.put(path, body, content_type).await
        }
        async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
            self.inner.list(prefix).await
        }
    }

    #[tokio::test]
    async fn warm_images_do_not_wait_for_slow_storage_and_retries_coalesce() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SlowStore {
            inner: usnm_store::LocalStore::new(dir.path()),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            writes: std::sync::atomic::AtomicUsize::new(0),
        });
        let path = "search-preview/0123456789abcdef/custom.png";
        let persistent = || Some((store.clone() as Arc<dyn ObjectStore>, path.to_owned()));
        let service = Service::default();
        let body = image();
        service
            .get(path.to_owned(), None, false, async { Ok(body.clone()) })
            .await
            .unwrap();
        let ready = tokio::time::timeout(
            Duration::from_millis(250),
            service.get(path.to_owned(), persistent(), false, async {
                panic!("must not render")
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(ready, body);
        tokio::time::timeout(STORE_TIMEOUT, store.started.notified())
            .await
            .unwrap();
        for _ in 0..5 {
            let ready = tokio::time::timeout(
                Duration::from_millis(250),
                service.lookup(path, persistent()),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(ready, body);
        }
        tokio::task::yield_now().await;
        assert_eq!(store.writes.load(Ordering::SeqCst), 1);
        store.release.notify_one();
        stored(&service, path).await;
        assert_eq!(
            Service::default().lookup(path, persistent()).await.unwrap(),
            body
        );
    }

    #[tokio::test]
    async fn create_only_conflicts_repair_oversized_objects_instead_of_marking_them_stored() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let path = "previews/example.png";
        let service = Service::default();
        let body = image();
        assert_eq!(
            service
                .get(
                    path.to_owned(),
                    Some((store.clone(), path.to_owned())),
                    false,
                    async {
                        store
                            .put(path, vec![0; MAX_PNG_BYTES + 1], "image/png")
                            .await
                            .unwrap();
                        Ok(body.clone())
                    }
                )
                .await
                .unwrap(),
            body
        );
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
        let restarted = Service::default();
        assert_eq!(
            restarted
                .get(
                    path.to_owned(),
                    Some((store.clone(), path.to_owned())),
                    false,
                    async { panic!("repaired PNG must survive restart") }
                )
                .await
                .unwrap(),
            body
        );
    }

    #[tokio::test]
    async fn examples_survive_cache_restart_without_a_browser_and_corruption_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let path = "previews/version/example.png";
        let persistent = || Some((store.clone(), path.to_owned()));
        let first = Service::default();
        let body = image();
        assert_eq!(
            first
                .get(path.to_owned(), persistent(), false, async {
                    Ok(body.clone())
                })
                .await
                .unwrap(),
            body
        );
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
        let restarted = Service::default();
        let fail = || async { Err(RenderError::Failed("browser must not run".to_owned())) };
        assert_eq!(
            restarted
                .get(path.to_owned(), persistent(), false, fail())
                .await
                .unwrap(),
            body
        );
        store
            .put(path, b"broken".to_vec(), "image/png")
            .await
            .unwrap();
        let repaired = Service::default();
        assert_eq!(
            repaired
                .get(path.to_owned(), persistent(), false, async {
                    Ok(body.clone())
                })
                .await
                .unwrap(),
            body
        );
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn background_work_leaves_a_slot_for_sharing_and_defers_to_active_visitors() {
        let service = Service::default();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let background = service.get("background".to_owned(), None, true, async {
            started.send(()).unwrap();
            wait.await.unwrap();
            Ok(vec![1])
        });
        let visitor = async {
            ready.await.unwrap();
            assert_eq!(service.renders.available_permits(), 1);
            assert!(service
                .get("another-background".to_owned(), None, true, async {
                    Ok(vec![2])
                })
                .await
                .is_err());
            assert_eq!(
                service
                    .get("visitor".to_owned(), None, false, async { Ok(vec![3]) })
                    .await
                    .unwrap(),
                vec![3]
            );
            release.send(()).unwrap();
        };
        let (background, ()) = tokio::join!(background, visitor);
        assert_eq!(background.unwrap(), vec![1]);
        let _visitor = service.renders.clone().acquire_owned().await.unwrap();
        assert!(service
            .get("deferred".to_owned(), None, true, async { Ok(vec![4]) })
            .await
            .is_err());
    }

    #[tokio::test]
    async fn failed_captures_are_not_persisted_or_cached_and_two_hundred_examples_fit() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let service = Service::default();
        let path = "previews/failed.png";
        assert!(service
            .get(
                path.to_owned(),
                Some((store.clone(), path.to_owned())),
                false,
                async { Err(RenderError::Failed("browser failed".to_owned())) }
            )
            .await
            .is_err());
        assert!(store.get(path).await.unwrap().is_none());
        for i in 0..200 {
            service
                .get(format!("example-{i}"), None, false, async { Ok(vec![1]) })
                .await
                .unwrap();
        }
        service.images.run_pending_tasks().await;
        for i in 0..200 {
            assert!(service.images.get(&format!("example-{i}")).await.is_some());
        }
        assert_eq!(
            service
                .get(path.to_owned(), None, false, async { Ok(vec![2]) })
                .await
                .unwrap(),
            vec![2]
        );
    }
}
