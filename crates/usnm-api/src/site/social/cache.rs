use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use usnm_store::ObjectStore;

use super::RenderError;

pub(super) const LIFETIME: Duration = Duration::from_secs(24 * 3600);
const STORE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_PNG_BYTES: usize = 8 * 1024 * 1024;
type Persistent = Option<(Arc<dyn ObjectStore>, String)>;

pub(crate) struct Service {
    images: moka::future::Cache<String, Arc<Image>>,
    renders: Arc<tokio::sync::Semaphore>,
    pub(super) frontend: tokio::sync::OnceCell<String>,
}

struct Image {
    body: Vec<u8>,
    stored: AtomicBool,
    persistence: tokio::sync::Mutex<()>,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            images: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(LIFETIME)
                .build(),
            renders: Arc::new(tokio::sync::Semaphore::new(2)),
            frontend: tokio::sync::OnceCell::new(),
        }
    }
}

impl Service {
    pub(super) fn idle(&self) -> bool {
        self.renders.available_permits() == 2
    }

    /// Read a published image without admitting a new render.
    pub(super) async fn lookup(&self, key: &str, persistent: Persistent) -> Option<Vec<u8>> {
        if let Some(image) = self.images.get(key).await {
            retry_persistence(image.clone(), persistent);
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
                }),
            )
            .await;
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
                retry_persistence(image.clone(), persistent);
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
                }))
            })
            .await?;
        persist(&image, persistent).await;
        Ok(image.body.clone())
    }
}

fn retry_persistence(image: Arc<Image>, persistent: Persistent) {
    if persistent.is_some() && !image.stored.load(Ordering::Acquire) {
        // A ready PNG must not wait on storage and lose the sharing deadline.
        // `persist` coalesces writes and bounds the detached retry.
        tokio::spawn(async move { persist(&image, persistent).await });
    }
}

async fn persist(image: &Image, persistent: Persistent) {
    let Ok(_turn) = image.persistence.try_lock() else {
        return;
    };
    if let Some((store, path)) = persistent {
        if !image.stored.load(Ordering::Acquire) {
            match tokio::time::timeout(STORE_TIMEOUT, async {
                if store
                    .put_new(&path, image.body.clone(), "image/png")
                    .await?
                    || read(store.as_ref(), &path).await.is_some()
                    || store
                        .put_new(&path, image.body.clone(), "image/png")
                        .await?
                {
                    image.stored.store(true, Ordering::Release);
                } else {
                    tracing::warn!("search preview persistence conflict remains unresolved");
                }
                Ok::<_, usnm_store::StoreError>(())
            })
            .await
            {
                Ok(Ok(())) => {}
                // Store errors can contain the query-derived filename.
                Ok(Err(_)) => {
                    tracing::warn!("search preview persistence failed");
                }
                Err(_) => tracing::warn!("search preview persistence timed out"),
            }
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
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            self.release.notified().await;
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
