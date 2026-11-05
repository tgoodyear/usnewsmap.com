use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
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
// Each in-flight eviction holds its PNG (at most `MAX_PNG_BYTES`) in case an
// access during the delete needs it restored.
const DELETE_CONCURRENCY: usize = 8;
// Lock acquisition and eviction admission end before the lock could be judged
// stale, leaving margin for clock skew and for admitted evictions to finish:
// a PNG delete is awaited to resolution (the blob client allows 120 seconds).
const EVICTION_BUDGET: Duration = Duration::from_secs(7 * 60);
// Bounds each other store operation of an admitted eviction, which is never
// cancelled between deleting a PNG and restoring it.
const EVICTION_OP_TIMEOUT: Duration = Duration::from_secs(10);
// Paths whose access marker was written within `ACCESS_INTERVAL`.
const ACCESSED_PATHS: u64 = 16_384;
const PUBLICATION_RECHECK: Duration = Duration::from_secs(5 * 60);
const EVICTION_PASSES: usize = 3;
const SCAN_IDLE: u8 = 0;
const SCAN_RUNNING: u8 = 1;
const SCAN_RERUN: u8 = 2;
type Persistent = Option<(Arc<dyn ObjectStore>, String)>;

pub(crate) struct Service {
    images: moka::future::Cache<String, Arc<Image>>,
    renders: Arc<tokio::sync::Semaphore>,
    quota: Arc<Quota>,
    pub(super) frontend: tokio::sync::OnceCell<String>,
}

/// One process's quota scans: at most one task runs, and new PNGs written
/// while it runs request a single rerun so they are counted.
struct Quota {
    last_check: AtomicU64,
    scan: AtomicU8,
    /// Throttles access markers per path, apart from PNG bodies, so reads the
    /// memory cache does not retain are still written at most hourly.
    accessed: moka::future::Cache<String, ()>,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            last_check: AtomicU64::new(0),
            scan: AtomicU8::new(SCAN_IDLE),
            accessed: moka::future::Cache::builder()
                .max_capacity(ACCESSED_PATHS)
                .time_to_live(ACCESS_INTERVAL)
                .build(),
        }
    }
}

struct Image {
    body: Vec<u8>,
    stored: AtomicBool,
    persistence: tokio::sync::Mutex<()>,
    last_access: AtomicU64,
    /// When the stored PNG was last seen in the store, in Unix seconds.
    verified: AtomicU64,
    revalidating: AtomicBool,
}

impl Image {
    fn new(body: Vec<u8>, stored: bool) -> Arc<Self> {
        Arc::new(Self {
            body,
            stored: AtomicBool::new(stored),
            persistence: tokio::sync::Mutex::new(()),
            last_access: AtomicU64::new(0),
            verified: AtomicU64::new(if stored { now_secs() } else { 0 }),
            revalidating: AtomicBool::new(false),
        })
    }

    fn mark_stored(&self) {
        self.verified.store(now_secs(), Ordering::Release);
        self.stored.store(true, Ordering::Release);
    }

    fn recently_verified(&self) -> bool {
        now_secs().saturating_sub(self.verified.load(Ordering::Acquire))
            < PUBLICATION_RECHECK.as_secs()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .max(1)
}

impl Default for Service {
    fn default() -> Self {
        Self {
            images: moka::future::Cache::builder()
                .max_capacity(1024)
                .time_to_live(LIFETIME)
                .build(),
            renders: Arc::new(tokio::sync::Semaphore::new(2)),
            quota: Arc::default(),
            frontend: tokio::sync::OnceCell::new(),
        }
    }
}

impl Service {
    pub(super) fn idle(&self) -> bool {
        self.renders.available_permits() == 2
    }

    /// Metadata discovery never waits for storage or advertises a local-only
    /// PNG. Another replica may have evicted it, so a publication not seen in
    /// the store recently is rechecked in the background and not advertised
    /// until it is confirmed or republished.
    pub(super) async fn is_published(&self, key: &str, persistent: Persistent) -> bool {
        let Some(image) = self.images.get(key).await else {
            return false;
        };
        if !image.stored.load(Ordering::Acquire) {
            return false;
        }
        if image.recently_verified() {
            return true;
        }
        revalidate(image, persistent, self.quota.clone());
        false
    }

    /// Read a published image without admitting a new render.
    pub(super) async fn lookup(&self, key: &str, persistent: Persistent) -> Option<Vec<u8>> {
        if let Some(image) = self.images.get(key).await {
            self.refresh(&image, &persistent);
            return Some(image.body.clone());
        }
        let (store, path) = persistent?;
        let body = read(store.as_ref(), &path).await?;
        let image = Image::new(body.clone(), true);
        // Record the access even if memory pressure drops the entry at once.
        self.images.insert(key.to_owned(), image.clone()).await;
        self.schedule_quota_check(&Some((store.clone(), path.clone())));
        touch_recent(image, Some((store, path)), self.quota.clone());
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
                self.refresh(&image, &persistent);
                return Ok(image.body.clone());
            }
        }
        let image = self
            .images
            .try_get_with(key, async {
                if let Some((store, path)) = &persistent {
                    if let Some(body) = read(store.as_ref(), path).await {
                        return Ok(Image::new(body, true));
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
                Ok(Image::new(body, false))
            })
            .await?;
        persist(&image, persistent.clone(), self.quota.clone()).await;
        if image.stored.load(Ordering::Acquire) {
            self.schedule_quota_check(&persistent);
        }
        touch_recent(image.clone(), persistent, self.quota.clone());
        Ok(image.body.clone())
    }

    /// Background upkeep for an image served from memory.
    fn refresh(&self, image: &Arc<Image>, persistent: &Persistent) {
        if image.stored.load(Ordering::Acquire) {
            self.schedule_quota_check(persistent);
        }
        retry_persistence(image.clone(), persistent.clone(), self.quota.clone());
        revalidate(image.clone(), persistent.clone(), self.quota.clone());
        touch_recent(image.clone(), persistent.clone(), self.quota.clone());
    }

    fn schedule_quota_check(&self, persistent: &Persistent) {
        let Some((store, _)) = persistent else {
            return;
        };
        schedule_quota_scan(store.clone(), self.quota.clone(), false);
    }
}

fn schedule_quota_scan(store: Arc<dyn ObjectStore>, quota: Arc<Quota>, force: bool) {
    let now = now_secs();
    if !force {
        let last = quota.last_check.load(Ordering::Acquire);
        if last != 0 && now.saturating_sub(last) < 60 * 60 {
            return;
        }
        if quota
            .last_check
            .compare_exchange(last, now, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
    } else {
        quota.last_check.store(now, Ordering::Release);
    }
    let started =
        quota
            .scan
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                SCAN_IDLE => Some(SCAN_RUNNING),
                SCAN_RUNNING if force => Some(SCAN_RERUN),
                _ => None,
            });
    if started != Ok(SCAN_IDLE) {
        return;
    }
    tokio::spawn(async move {
        loop {
            enforce_quota(store.clone(), PREVIEW_QUOTA_BYTES).await;
            let finished = quota
                .scan
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| match state {
                    SCAN_RERUN => Some(SCAN_RUNNING),
                    _ => Some(SCAN_IDLE),
                });
            if finished != Ok(SCAN_RERUN) {
                break;
            }
        }
    });
}

fn retry_persistence(image: Arc<Image>, persistent: Persistent, quota: Arc<Quota>) {
    if persistent.is_some() && !image.stored.load(Ordering::Acquire) {
        // A ready PNG must not wait on storage and lose the sharing deadline.
        // `persist` coalesces writes and bounds the detached retry.
        tokio::spawn(async move {
            persist(&image, persistent.clone(), quota.clone()).await;
            if image.stored.load(Ordering::Acquire) {
                if let Some((store, _)) = &persistent {
                    schedule_quota_scan(store.clone(), quota.clone(), false);
                }
            }
            touch_recent(image, persistent, quota);
        });
    }
}

/// Confirm off the response path that a published PNG is still stored, and
/// republish it from memory if another replica's eviction removed it.
fn revalidate(image: Arc<Image>, persistent: Persistent, quota: Arc<Quota>) {
    if persistent.is_none()
        || !image.stored.load(Ordering::Acquire)
        || image.recently_verified()
        || image.revalidating.swap(true, Ordering::AcqRel)
    {
        return;
    }
    tokio::spawn(async move {
        confirm_stored(&image, persistent, quota).await;
        image.revalidating.store(false, Ordering::Release);
    });
}

async fn confirm_stored(image: &Image, persistent: Persistent, quota: Arc<Quota>) {
    let Some((store, path)) = &persistent else {
        return;
    };
    match tokio::time::timeout(STORE_TIMEOUT, store.exists(path)).await {
        Ok(Ok(true)) => image.verified.store(now_secs(), Ordering::Release),
        Ok(Ok(false)) => {
            image.stored.store(false, Ordering::Release);
            persist(image, persistent.clone(), quota).await;
        }
        Ok(Err(_)) | Err(_) => {
            tracing::warn!("search preview publication check failed");
        }
    }
}

fn touch_recent(image: Arc<Image>, persistent: Persistent, quota: Arc<Quota>) {
    let Some((store, path)) = persistent.clone() else {
        return;
    };
    if !image.stored.load(Ordering::Acquire) {
        return;
    }
    let now = now_secs();
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
        // A read the memory cache did not retain starts with a fresh `Image`.
        if !quota
            .accessed
            .entry(path.clone())
            .or_insert(())
            .await
            .is_fresh()
        {
            return;
        }
        let marker = access_path(&path);
        match tokio::time::timeout(
            STORE_TIMEOUT,
            store.put(&marker, Vec::new(), "application/octet-stream"),
        )
        .await
        {
            Ok(Ok(())) => {
                // Eviction claims the marker before deleting the PNG and checks
                // it again afterward; checking the PNG only after this write
                // means one side always sees the other and the preview survives.
                confirm_stored(&image, persistent, quota).await;
            }
            Ok(Err(_)) | Err(_) => {
                image.last_access.store(0, Ordering::Release);
                quota.accessed.invalidate(&path).await;
                tracing::warn!("search preview access timestamp update failed");
            }
        }
    });
}

fn access_path(path: &str) -> String {
    format!("search-preview-lru/{}.access", digest(path))
}

enum Publication {
    Created,
    Existing,
    Conflict,
}

async fn persist(image: &Image, persistent: Persistent, quota: Arc<Quota>) {
    let Ok(_turn) = image.persistence.try_lock() else {
        return;
    };
    let Some((store, path)) = persistent else {
        return;
    };
    if image.stored.load(Ordering::Acquire) {
        return;
    }
    // One deadline covers the create, the conflict check and repair, and the
    // retry, so a rendered PNG never waits on several storage timeouts.
    let publication = tokio::time::timeout(STORE_TIMEOUT, async {
        if store
            .put_new(&path, image.body.clone(), "image/png")
            .await?
        {
            return Ok(Publication::Created);
        }
        if read(store.as_ref(), &path).await.is_some() {
            return Ok(Publication::Existing);
        }
        Ok::<_, usnm_store::StoreError>(
            if store
                .put_new(&path, image.body.clone(), "image/png")
                .await?
            {
                Publication::Created
            } else {
                Publication::Conflict
            },
        )
    })
    .await;
    match publication {
        Ok(Ok(Publication::Created)) => {
            image.mark_stored();
            schedule_quota_scan(store, quota, true);
        }
        Ok(Ok(Publication::Existing)) => image.mark_stored(),
        Ok(Ok(Publication::Conflict)) => {
            tracing::warn!("search preview persistence conflict remains unresolved");
        }
        // Store errors can contain the query-derived filename.
        Ok(Err(_)) => tracing::warn!("search preview persistence failed"),
        Err(_) => tracing::warn!("search preview persistence timed out"),
    }
}

async fn enforce_quota(store: Arc<dyn ObjectStore>, quota: u64) {
    let (lock, deadline) = loop {
        // The deadline starts before this attempt can create the lock.
        let deadline = tokio::time::Instant::now() + EVICTION_BUDGET;
        match tokio::time::timeout_at(deadline, acquire_eviction_lock(store.as_ref())).await {
            Ok(Ok(Some(lock))) => break (lock, deadline),
            Ok(Ok(None)) => tokio::time::sleep(Duration::from_secs(5)).await,
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("search preview quota lock failed");
                return;
            }
        }
    };
    match evict_lru(store.as_ref(), quota, deadline).await {
        Ok(evicted) if evicted > 0 => {
            tracing::info!(
                bytes = evicted,
                "evicted least-recently-used search previews"
            )
        }
        Ok(_) => {}
        Err(_) => tracing::warn!("search preview quota scan failed"),
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        // The leading Unix time lets waiters age the lock when the store
        // reports no object timestamps.
        let token = format!(
            "{}-{}-{}-{}",
            now.as_secs(),
            std::process::id(),
            now.subsec_nanos(),
            TOKEN.fetch_add(1, Ordering::Relaxed),
        );
        if store
            .put_new(EVICTION_LOCK, token.clone().into_bytes(), "text/plain")
            .await?
        {
            let Some(lock) = store
                .list_info("search-preview-quota-lock")
                .await?
                .into_iter()
                .find(|object| object.path == EVICTION_LOCK)
            else {
                return Ok(None);
            };
            // A delayed listing can see a lock another replica took after
            // breaking this one as stale; only the written token proves ours.
            let owned = store
                .get_if_match(EVICTION_LOCK, &lock.etag, 256)
                .await?
                .is_some_and(|body| body == token.as_bytes());
            return Ok(owned.then_some(lock));
        }
        let Some(lock) = store
            .list_info("search-preview-quota-lock")
            .await?
            .into_iter()
            .find(|object| object.path == EVICTION_LOCK)
        else {
            continue;
        };
        let written = match lock.modified.or(lock.created) {
            Some(time) => time,
            // Every holder writes its start time, so an unreadable time is a
            // corrupt lock and is broken rather than blocking forever.
            None => match store.get(EVICTION_LOCK).await? {
                Some(body) => lock_written(&body).unwrap_or(UNIX_EPOCH),
                None => continue,
            },
        };
        if SystemTime::now()
            .duration_since(written)
            .unwrap_or_default()
            > EVICTION_LOCK_STALE
        {
            store.delete_if_match(EVICTION_LOCK, &lock.etag).await?;
            continue;
        }
        return Ok(None);
    }
    Ok(None)
}

fn lock_written(body: &[u8]) -> Option<SystemTime> {
    let seconds = std::str::from_utf8(body)
        .ok()?
        .split('-')
        .next()?
        .parse::<u64>()
        .ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

enum Eviction {
    Evicted(usnm_store::ObjectInfo),
    /// Accessed since the listing; kept as now among the most recent.
    Recent,
    /// Gone or rewritten since the listing, so the listed sizes are stale.
    Changed,
}

/// Evict until under quota. The deadline stops admitting victims and bounds
/// listing; it never cancels an admitted eviction.
async fn evict_lru(
    store: &dyn ObjectStore,
    quota: u64,
    deadline: tokio::time::Instant,
) -> Result<u64, usnm_store::StoreError> {
    let mut evicted = 0_u64;
    for _ in 0..EVICTION_PASSES {
        let (bytes, settled) = evict_pass(store, quota, deadline).await?;
        evicted = evicted.saturating_add(bytes);
        if settled {
            break;
        }
    }
    Ok(evicted)
}

/// Evict from one listing. Unsettled when a delete missed or failed: the
/// listing no longer accounts for the store, so no newer PNG is chosen from
/// it and the caller lists again.
async fn evict_pass(
    store: &dyn ObjectStore,
    quota: u64,
    deadline: tokio::time::Instant,
) -> Result<(u64, bool), usnm_store::StoreError> {
    let timed_out = |_| usnm_store::StoreError::Io("search preview quota scan timed out".into());
    let mut images = tokio::time::timeout_at(deadline, store.list_info("search-preview"))
        .await
        .map_err(timed_out)??
        .into_iter()
        .filter(|object| {
            object.path.starts_with("search-preview/") && object.path.ends_with(".png")
        })
        .collect::<Vec<_>>();
    let markers = tokio::time::timeout_at(deadline, store.list_info("search-preview-lru"))
        .await
        .map_err(timed_out)??;
    let mut by_path = markers
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
        cleanup_orphan_markers(store, &by_path, &active_markers, deadline).await;
        return Ok((0, true));
    }
    images.sort_by_cached_key(|image| {
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
    let mut settled = true;
    let mut candidates = candidates.into_iter();
    while settled && used > quota {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!("search preview quota scan timed out");
            break;
        }
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
        let results = stream::iter(batch)
            .map(|(image, marker)| evict_one(store, image, marker))
            .buffer_unordered(DELETE_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        for result in results {
            match result {
                Ok(Eviction::Evicted(image)) => {
                    used = used.saturating_sub(image.size);
                    evicted = evicted.saturating_add(image.size);
                    by_path.remove(&access_path(&image.path));
                }
                Ok(Eviction::Recent) => {}
                Ok(Eviction::Changed) => settled = false,
                Err(_) => {
                    tracing::warn!("search preview eviction delete failed");
                    settled = false;
                }
            }
        }
    }
    cleanup_orphan_markers(store, &by_path, &active_markers, deadline).await;
    if settled && used > quota {
        tracing::warn!(bytes = used, "search preview byte quota remains exceeded");
    }
    Ok((evicted, settled))
}

/// Evict one listed PNG unless it was accessed or changed since the listing.
async fn evict_one(
    store: &dyn ObjectStore,
    image: usnm_store::ObjectInfo,
    marker: Option<usnm_store::ObjectInfo>,
) -> Result<Eviction, usnm_store::StoreError> {
    async fn bounded<T>(
        operation: impl Future<Output = Result<T, usnm_store::StoreError>>,
    ) -> Result<T, usnm_store::StoreError> {
        tokio::time::timeout(EVICTION_OP_TIMEOUT, operation)
            .await
            .map_err(|_| usnm_store::StoreError::Io("search preview eviction timed out".into()))?
    }
    let marker_path = access_path(&image.path);
    // Claim the sampled access time first: an access since the listing
    // rewrites the marker and keeps the PNG.
    let claimed = match &marker {
        Some(marker) => bounded(store.delete_if_match(&marker.path, &marker.etag)).await?,
        None => !bounded(store.exists(&marker_path)).await?,
    };
    if !claimed {
        // A lost claim can also mean age expiry removed both objects.
        return Ok(if bounded(store.exists(&image.path)).await? {
            Eviction::Recent
        } else {
            Eviction::Changed
        });
    }
    let body =
        match bounded(store.get_if_match(&image.path, &image.etag, MAX_PNG_BYTES as u64)).await {
            Ok(Some(body)) => Some(body),
            Ok(None) => return Ok(Eviction::Changed),
            // An oversized object is never served, so it is never restored.
            Err(usnm_store::StoreError::TooLarge(_)) => None,
            Err(error) => return Err(error),
        };
    // Awaited to resolution: a delete abandoned on a timeout could still
    // remove the PNG after recovery found it present.
    let accessed = match store.delete_if_match(&image.path, &image.etag).await {
        Ok(false) => return Ok(Eviction::Changed),
        // An access that wrote the marker after the claim checks the PNG only
        // after that write (`touch_recent`). Seeing its marker here restores
        // the PNG; otherwise the accessor sees the PNG gone and republishes it.
        Ok(true) => bounded(store.exists(&marker_path)).await,
        Err(error) => Err(error),
    };
    match (accessed, body) {
        (Ok(true), Some(body)) => {
            bounded(store.put_new(&image.path, body, "image/png")).await?;
            Ok(Eviction::Recent)
        }
        // Whether the PNG is gone or was accessed is unknown, so keep it and
        // relist. Overwriting changes its ETag, so a conditional delete still
        // in flight at the store cannot remove the restored copy.
        (Err(error), Some(body)) => {
            bounded(store.put(&image.path, body, "image/png")).await?;
            Err(error)
        }
        (accessed, _) => accessed.map(|_| Eviction::Evicted(image)),
    }
}

async fn cleanup_orphan_markers(
    store: &dyn ObjectStore,
    markers: &HashMap<String, usnm_store::ObjectInfo>,
    active: &std::collections::HashSet<String>,
    deadline: tokio::time::Instant,
) {
    let cleanup = async {
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
    };
    // Marker cleanup is safe to cancel; the next scan resumes it.
    if tokio::time::timeout_at(deadline, cleanup).await.is_err() {
        tracing::warn!("orphaned search preview access marker cleanup timed out");
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

    fn later() -> tokio::time::Instant {
        tokio::time::Instant::now() + EVICTION_BUDGET
    }

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
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
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
    enum Interleave {
        Access(String),
        Expire(Vec<String>),
        Replace(String, Vec<u8>),
        WaitUntil(tokio::time::Instant),
    }

    #[derive(Debug, PartialEq)]
    enum Fault {
        Exists,
        /// The delete commits but the caller sees an error.
        DeleteCommitted,
        /// The delete is still in flight when the caller sees an error.
        DeleteDeferred,
    }

    #[derive(Debug)]
    struct ProbeStore {
        inner: usnm_store::LocalStore,
        root: std::path::PathBuf,
        /// One-shot action run when an operation first targets the path.
        interleave: std::sync::Mutex<Option<(String, Vec<Interleave>)>>,
        hide_times: bool,
        /// One-shot failures by operation and path.
        faults: std::sync::Mutex<Vec<(Fault, String)>>,
        marker_puts: std::sync::atomic::AtomicUsize,
        marker_delete_failures: std::sync::atomic::AtomicUsize,
        scans: std::sync::atomic::AtomicUsize,
        scan_gate: tokio::sync::Semaphore,
    }

    impl ProbeStore {
        fn new(root: &std::path::Path) -> Self {
            Self {
                inner: usnm_store::LocalStore::new(root),
                root: root.to_owned(),
                interleave: std::sync::Mutex::new(None),
                hide_times: false,
                faults: std::sync::Mutex::new(Vec::new()),
                marker_puts: std::sync::atomic::AtomicUsize::new(0),
                marker_delete_failures: std::sync::atomic::AtomicUsize::new(0),
                scans: std::sync::atomic::AtomicUsize::new(0),
                scan_gate: tokio::sync::Semaphore::new(tokio::sync::Semaphore::MAX_PERMITS),
            }
        }

        fn on<const N: usize>(&self, trigger: impl Into<String>, actions: [Interleave; N]) {
            *self.interleave.lock().unwrap() = Some((trigger.into(), actions.into()));
        }

        fn fail(&self, fault: Fault, path: impl Into<String>) {
            self.faults.lock().unwrap().push((fault, path.into()));
        }

        fn faulted(&self, fault: Fault, path: &str) -> bool {
            let mut faults = self.faults.lock().unwrap();
            let found = faults.iter().position(|(f, p)| *f == fault && p == path);
            found.map(|index| faults.remove(index)).is_some()
        }

        fn fired(&self) -> bool {
            self.interleave.lock().unwrap().is_none()
        }

        async fn interleave(&self, target: &str) -> Result<(), usnm_store::StoreError> {
            let actions = {
                let mut hook = self.interleave.lock().unwrap();
                match hook.as_ref() {
                    Some((trigger, _)) if trigger == target => hook.take().map(|(_, a)| a),
                    _ => None,
                }
            };
            for action in actions.unwrap_or_default() {
                match action {
                    Interleave::Access(marker) => {
                        self.inner
                            .put(&marker, Vec::new(), "application/octet-stream")
                            .await?
                    }
                    Interleave::Expire(paths) => {
                        for path in paths {
                            tokio::fs::remove_file(self.root.join(path)).await.unwrap();
                        }
                    }
                    Interleave::Replace(path, body) => {
                        self.inner.put(&path, body, "text/plain").await?
                    }
                    Interleave::WaitUntil(deadline) => tokio::time::sleep_until(deadline).await,
                }
            }
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for ProbeStore {
        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            self.inner.get(path).await
        }
        async fn put_new(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            self.inner.put_new(path, body, content_type).await
        }
        async fn put(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<(), usnm_store::StoreError> {
            if path.ends_with(".access") {
                self.marker_puts.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.put(path, body, content_type).await
        }
        async fn exists(&self, path: &str) -> Result<bool, usnm_store::StoreError> {
            if self.faulted(Fault::Exists, path) {
                return Err(usnm_store::StoreError::Io("injected".into()));
            }
            self.inner.exists(path).await
        }
        async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
            self.inner.list(prefix).await
        }
        async fn list_info(
            &self,
            prefix: &str,
        ) -> Result<Vec<usnm_store::ObjectInfo>, usnm_store::StoreError> {
            if prefix == "search-preview" {
                self.scans.fetch_add(1, Ordering::SeqCst);
                self.scan_gate.acquire().await.unwrap().forget();
            }
            let mut objects = self.inner.list_info(prefix).await?;
            self.interleave(prefix).await?;
            if self.hide_times {
                for object in &mut objects {
                    object.created = None;
                    object.modified = None;
                }
            }
            Ok(objects)
        }
        async fn delete_if_match(
            &self,
            path: &str,
            etag: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            self.interleave(path).await?;
            if path.ends_with(".access")
                && self
                    .marker_delete_failures
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                    .is_ok()
            {
                return Err(usnm_store::StoreError::Io("injected".into()));
            }
            if self.faulted(Fault::DeleteDeferred, path) {
                return Err(usnm_store::StoreError::Io("injected".into()));
            }
            let deleted = self.inner.delete_if_match(path, etag).await?;
            if self.faulted(Fault::DeleteCommitted, path) {
                return Err(usnm_store::StoreError::Io("injected".into()));
            }
            Ok(deleted)
        }
        async fn get_if_match(
            &self,
            path: &str,
            etag: &str,
            max_bytes: u64,
        ) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            self.inner.get_if_match(path, etag, max_bytes).await
        }
    }

    #[tokio::test]
    async fn eviction_lock_requires_its_own_token_after_creation() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        // Another replica breaks this lock as stale and takes it before the
        // creating replica lists it.
        store.on(
            "search-preview-quota-lock",
            [Interleave::Replace(
                EVICTION_LOCK.to_owned(),
                b"1-2-3-4".to_vec(),
            )],
        );
        assert!(acquire_eviction_lock(&store).await.unwrap().is_none());
        assert_eq!(store.get(EVICTION_LOCK).await.unwrap().unwrap(), b"1-2-3-4");
    }

    #[tokio::test]
    async fn eviction_lock_ages_from_its_body_when_the_store_reports_no_times() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore {
            hide_times: true,
            ..ProbeStore::new(dir.path())
        };
        let first = acquire_eviction_lock(&store).await.unwrap().unwrap();
        assert!(acquire_eviction_lock(&store).await.unwrap().is_none());
        assert!(store
            .delete_if_match(EVICTION_LOCK, &first.etag)
            .await
            .unwrap());

        let abandoned = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .saturating_sub(EVICTION_LOCK_STALE + Duration::from_secs(60))
            .as_secs();
        for body in [format!("{abandoned}-1-0-0"), "corrupt".to_owned()] {
            store
                .put(EVICTION_LOCK, body.into_bytes(), "text/plain")
                .await
                .unwrap();
            let lock = acquire_eviction_lock(&store).await.unwrap().unwrap();
            assert!(store
                .delete_if_match(EVICTION_LOCK, &lock.etag)
                .await
                .unwrap());
        }
    }

    #[tokio::test]
    async fn marker_claim_errors_relist_instead_of_evicting_newer_previews() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        store.marker_delete_failures.store(1, Ordering::SeqCst);
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            body.len() as u64
        );
        assert!(store.get(old).await.unwrap().is_none());
        assert!(store.get(&access_path(old)).await.unwrap().is_none());
        assert_eq!(store.get(recent).await.unwrap().unwrap(), body);
        assert!(store.get(&access_path(recent)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn previews_accessed_after_the_listing_are_not_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let marker = access_path(old);
        store.on(marker.clone(), [Interleave::Access(marker)]);

        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            body.len() as u64
        );
        assert_eq!(store.get(old).await.unwrap().unwrap(), body);
        assert!(store.get(&access_path(old)).await.unwrap().is_some());
        assert!(store.get(recent).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn eviction_relists_after_a_preview_disappears_instead_of_evicting_newer_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        store.on(old, [Interleave::Expire(vec![old.to_owned()])]);

        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            0
        );
        assert!(store.get(old).await.unwrap().is_none());
        assert_eq!(store.get(recent).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn expiry_during_a_marker_claim_relists_instead_of_evicting_newer_previews() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let marker = access_path(old);
        store.on(
            marker.clone(),
            [Interleave::Expire(vec![old.to_owned(), marker])],
        );

        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            0
        );
        assert_eq!(store.get(recent).await.unwrap().unwrap(), body);
        assert!(store.get(&access_path(recent)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn accesses_during_a_preview_delete_restore_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The access lands after the marker claim but before the PNG delete,
        // and checks the PNG before the delete completes.
        store.on(old, [Interleave::Access(access_path(old))]);

        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            body.len() as u64
        );
        assert_eq!(store.get(old).await.unwrap().unwrap(), body);
        assert!(store.get(&access_path(old)).await.unwrap().is_some());
        assert!(store.get(recent).await.unwrap().is_none());
    }

    async fn two_previews_with_markers(store: &ProbeStore) -> (&'static str, &'static str) {
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        for path in [old, recent] {
            store.put(path, image(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (old, recent)
    }

    #[tokio::test]
    async fn deletes_that_commit_after_recovery_cannot_remove_the_restored_preview() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let (old, recent) = two_previews_with_markers(&store).await;
        let listed = store.list_info("search-preview/old-rev").await.unwrap();
        // The evictor sees an error while its delete is still in flight.
        store.fail(Fault::DeleteDeferred, old);

        let body = image();
        assert_eq!(
            evict_lru(&store, body.len() as u64, later()).await.unwrap(),
            body.len() as u64
        );
        assert!(store.get(recent).await.unwrap().is_none());
        // The delete then commits at the store.
        assert!(!store.delete_if_match(old, &listed[0].etag).await.unwrap());
        assert_eq!(store.get(old).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn unknown_delete_outcomes_and_failed_marker_checks_restore_the_preview() {
        for fault in [Fault::DeleteCommitted, Fault::Exists] {
            let dir = tempfile::tempdir().unwrap();
            let store = ProbeStore::new(dir.path());
            let (old, recent) = two_previews_with_markers(&store).await;
            // An access confirmed the PNG just before the delete.
            store.on(old, [Interleave::Access(access_path(old))]);
            let path = match fault {
                Fault::Exists => access_path(old),
                _ => old.to_owned(),
            };
            store.fail(fault, path);

            let body = image();
            assert_eq!(
                evict_lru(&store, body.len() as u64, later()).await.unwrap(),
                body.len() as u64
            );
            assert_eq!(store.get(old).await.unwrap().unwrap(), body);
            assert!(store.get(recent).await.unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn the_eviction_deadline_stops_admission_without_abandoning_a_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProbeStore::new(dir.path());
        let old = "search-preview/old-rev/old-search.png";
        let recent = "search-preview/new-rev/new-search.png";
        let body = image();
        for path in [old, recent] {
            store.put(path, body.clone(), "image/png").await.unwrap();
            store
                .put(&access_path(path), Vec::new(), "application/octet-stream")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // An access lands after the claim, then the deadline passes before
        // the PNG delete completes and before its marker is checked.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        store.on(
            old,
            [
                Interleave::Access(access_path(old)),
                Interleave::WaitUntil(deadline),
            ],
        );

        assert_eq!(
            evict_lru(&store, body.len() as u64, deadline)
                .await
                .unwrap(),
            0
        );
        assert!(store.fired());
        assert_eq!(store.get(old).await.unwrap().unwrap(), body);
        assert_eq!(store.get(recent).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn reads_not_retained_in_memory_write_one_access_marker_per_interval() {
        let dir = tempfile::tempdir().unwrap();
        let probe = Arc::new(ProbeStore::new(dir.path()));
        let store: Arc<dyn ObjectStore> = probe.clone();
        let path = "search-preview/0123456789abcdef/busy.png";
        let body = image();
        store.put(path, body.clone(), "image/png").await.unwrap();
        let service = Service {
            images: moka::future::Cache::new(0),
            ..Service::default()
        };

        for _ in 0..3 {
            assert_eq!(
                service
                    .lookup(path, Some((store.clone(), path.to_owned())))
                    .await
                    .unwrap(),
                body
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(probe.marker_puts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn accesses_after_an_eviction_republish_from_memory() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let path = "search-preview/0123456789abcdef/evicted.png";
        let service = Service::default();
        let body = image();
        // Verified just before another replica evicted it.
        service
            .images
            .insert(path.to_owned(), Image::new(body.clone(), true))
            .await;

        assert_eq!(
            service
                .lookup(path, Some((store.clone(), path.to_owned())))
                .await
                .unwrap(),
            body
        );
        tokio::time::timeout(STORE_TIMEOUT, async {
            while store.get(path).await.unwrap().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn publications_evicted_elsewhere_are_not_advertised_and_are_republished() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        let path = "search-preview/0123456789abcdef/evicted.png";
        let persistent = || Some((store.clone(), path.to_owned()));
        let service = Service::default();
        let body = image();
        service
            .get(path.to_owned(), persistent(), false, async {
                Ok(body.clone())
            })
            .await
            .unwrap();
        assert!(service.is_published(path, persistent()).await);

        // Another replica evicts the PNG while this one still holds it.
        let stored = store
            .list_info("search-preview/0123456789abcdef")
            .await
            .unwrap()
            .remove(0);
        assert!(store.delete_if_match(path, &stored.etag).await.unwrap());
        let cached = service.images.get(path).await.unwrap();
        cached.verified.store(1, Ordering::Release);
        assert!(!service.is_published(path, persistent()).await);
        tokio::time::timeout(STORE_TIMEOUT, async {
            while !service.is_published(path, persistent()).await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.get(path).await.unwrap().unwrap(), body);
    }

    #[tokio::test]
    async fn forced_quota_scans_coalesce_into_one_rerun() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ProbeStore {
            scan_gate: tokio::sync::Semaphore::new(0),
            ..ProbeStore::new(dir.path())
        });
        let quota = Arc::new(Quota::default());
        for _ in 0..16 {
            schedule_quota_scan(store.clone(), quota.clone(), true);
        }
        tokio::time::timeout(STORE_TIMEOUT, async {
            while store.scans.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        store.scan_gate.add_permits(64);
        tokio::time::timeout(STORE_TIMEOUT, async {
            while quota.scan.load(Ordering::SeqCst) != SCAN_IDLE {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.scans.load(Ordering::SeqCst), 2);
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
