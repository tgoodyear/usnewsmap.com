//! What the Congress fetches share (#229): a paced, retrying HTTP client
//! behind a trait the tests replace with recorded responses ([`Fetch`],
//! [`Http`]), the raw store's create-only cache that keeps only responses a
//! check accepts ([`cached`]), and the Cosmos lock a fetch holds while it
//! runs ([`locked`]).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::AsyncWriteExt;
use usnm_store::ObjectStore;

/// Fetches a URL: its body, or `None` when the server has no such page (404).
#[async_trait::async_trait]
pub trait Fetch: Send + Sync {
    async fn get(&self, url: &str) -> anyhow::Result<Option<Vec<u8>>>;

    /// Write the body to `dest` (created or truncated) instead of holding it:
    /// its length, or `None` on a 404 (`dest` is then empty). The default
    /// reads the body with [`Fetch::get`]; [`Http`] streams it.
    async fn download(&self, url: &str, dest: &Path) -> anyhow::Result<Option<u64>> {
        match self.get(url).await? {
            Some(body) => {
                tokio::fs::write(dest, &body)
                    .await
                    .with_context(|| dest.display().to_string())?;
                Ok(Some(body.len() as u64))
            }
            None => Ok(None),
        }
    }
}

/// A server over HTTPS: one request at a time, `delay` apart, retrying 429,
/// 5xx and transport errors (a body cut short included) with a growing wait
/// (`Retry-After` when given).
pub struct Http {
    client: reqwest::Client,
    delay: Duration,
    retries: u32,
    /// Who the server is, in the logs (`UNT`, `GovInfo`).
    server: &'static str,
    last: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl Http {
    /// `user_agent` says who is asking and why, on every request.
    pub fn new(delay: Duration, user_agent: &str, server: &'static str) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(user_agent)
                .connect_timeout(Duration::from_secs(30))
                // A whole response, a large PDF included, at a slow rate:
                // 30 minutes covers 500 MB at under 300 KB/s.
                .timeout(Duration::from_secs(1800))
                .build()?,
            delay,
            retries: 6,
            server,
            last: tokio::sync::Mutex::new(None),
        })
    }

    /// One request, paced from the end of the last and retried; `body`
    /// reads a successful response (and may fail, which is retried).
    async fn request<T, F, Fut>(&self, url: &str, mut body: F) -> anyhow::Result<Option<T>>
    where
        F: FnMut(reqwest::Response) -> Fut + Send,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send,
    {
        let mut wait = self.delay;
        for attempt in 0..=self.retries {
            // Paced from the end of the last request, across retries too.
            let mut last = self.last.lock().await;
            if let Some(t) = *last {
                tokio::time::sleep_until(t + wait).await;
            }
            let sent = self.client.get(url).send().await;
            *last = Some(tokio::time::Instant::now());
            let retry_after = match sent {
                // The body can fail after the headers (a dropped
                // connection): an attempt like a failed send.
                Ok(r) if r.status().is_success() => {
                    let read = body(r).await;
                    *last = Some(tokio::time::Instant::now());
                    match read {
                        Ok(b) => return Ok(Some(b)),
                        Err(e) => {
                            tracing::warn!(url, server = self.server, error = %format!("{e:#}"), attempt, "reading the response failed");
                            None
                        }
                    }
                }
                Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
                Ok(r)
                    if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                        || r.status().is_server_error() =>
                {
                    tracing::warn!(
                        url,
                        server = self.server,
                        status = r.status().as_u16(),
                        attempt,
                        "the server asked us to wait"
                    );
                    r.headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| {
                            usnm_core::retry_after::parse(v, std::time::SystemTime::now())
                        })
                }
                Ok(r) => bail!("`{url}` answered {}", r.status()),
                Err(e) => {
                    tracing::warn!(url, server = self.server, error = %e, attempt, "request failed");
                    None
                }
            };
            wait = retry_wait(self.delay, attempt, retry_after);
        }
        bail!("`{url}` failed {} times", self.retries + 1)
    }
}

#[async_trait::async_trait]
impl Fetch for Http {
    async fn get(&self, url: &str) -> anyhow::Result<Option<Vec<u8>>> {
        self.request(url, |r| async move { Ok(r.bytes().await?.to_vec()) })
            .await
    }

    async fn download(&self, url: &str, dest: &Path) -> anyhow::Result<Option<u64>> {
        let got = self
            .request(url, |mut r| async move {
                // Each try starts the file again.
                let mut f = tokio::fs::File::create(dest)
                    .await
                    .with_context(|| dest.display().to_string())?;
                // Read before the body: it counts down as the body is read.
                let expected = r.content_length();
                let mut n = 0u64;
                while let Some(chunk) = r.chunk().await? {
                    f.write_all(&chunk).await?;
                    n += chunk.len() as u64;
                }
                f.flush().await?;
                if let Some(len) = expected {
                    anyhow::ensure!(n == len, "got {n} bytes of {len}");
                }
                Ok(n)
            })
            .await?;
        if got.is_none() {
            tokio::fs::write(dest, b"").await?;
        }
        Ok(got)
    }
}

/// The longest wait before a retry, whatever the server asks.
pub const MAX_RETRY_WAIT: Duration = Duration::from_secs(600);

/// The wait before the try after `attempt`: doubling from the pacing delay, at
/// least what the server asked (`Retry-After`), and at most
/// [`MAX_RETRY_WAIT`], so a long hint can't keep the job asleep past its
/// deadline margin.
pub fn retry_wait(delay: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    let doubled = delay
        .max(Duration::from_secs(1))
        .saturating_mul(2u32.saturating_pow(attempt + 1));
    doubled
        .max(retry_after.unwrap_or_default())
        .min(MAX_RETRY_WAIT)
}

/// A page the server has not (it answered 404), kept so a resumed fetch
/// doesn't ask again.
pub const RAW_MISSING: &[u8] = b"";

/// A response, from the raw store when it holds it, else fetched (and kept
/// there). `None` when the server has no such page. Only a response `valid`
/// accepts is kept, since the raw store is create-only: a bad answer kept
/// there would be read back on every resumed fetch. A kept copy `valid`
/// refuses (one kept before the check) is passed over and fetched again.
pub async fn cached(
    fetch: &dyn Fetch,
    raw: Option<&dyn ObjectStore>,
    path: &str,
    url: &str,
    valid: fn(&[u8]) -> bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    if let Some(raw) = raw {
        if let Some(body) = read_kept(raw, path).await? {
            if body == RAW_MISSING {
                return Ok(None);
            }
            if valid(&body) {
                return Ok(Some(body));
            }
            tracing::warn!(path, "a kept response isn't usable; fetching it again");
        }
    }
    let body = fetch.get(url).await?;
    if let Some(raw) = raw {
        if body.as_deref().is_none_or(valid) {
            keep(raw, path, body.as_deref().unwrap_or(RAW_MISSING)).await?;
        }
    }
    Ok(body)
}

/// A kept object, decompressed.
pub async fn read_kept(raw: &dyn ObjectStore, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    match raw.get(path).await? {
        Some(z) => Ok(Some(
            zstd::decode_all(&z[..]).with_context(|| path.to_owned())?,
        )),
        None => Ok(None),
    }
}

/// Keep `body` zstd-compressed. Create-only: a copy already there (a racing
/// run) is the same response.
pub async fn keep(raw: &dyn ObjectStore, path: &str, body: &[u8]) -> anyhow::Result<()> {
    let z = zstd::encode_all(body, 9)?;
    raw.put_new(path, z, "application/zstd").await?;
    Ok(())
}

/// Hold the Cosmos lock `name` while `body` runs, renewing it every five
/// minutes; `lost` is set if a renewal fails, and the body is expected to
/// stop between requests when it is. `name` is an item id in the `ops`
/// container, so it has no `/`.
pub async fn locked<T>(
    state: &crate::state::State,
    name: &'static str,
    owner: &str,
    body: impl std::future::Future<Output = anyhow::Result<T>>,
    lost: Arc<AtomicBool>,
) -> anyhow::Result<T> {
    let ttl = chrono::Duration::minutes(30);
    state.lock(name, owner, ttl).await?;
    let (s, o, flag) = (state.clone(), owner.to_owned(), lost.clone());
    let (stop, mut stopped) = tokio::sync::watch::channel(false);
    let renewer = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(300)) => {}
                _ = stopped.changed() => break,
            }
            if let Err(e) = s.lock(name, &o, ttl).await {
                tracing::error!(lock = name, error = %format!("{e:#}"), "lock renewal failed");
                flag.store(true, Ordering::SeqCst);
                break;
            }
        }
    });
    let result = body.await;
    // Stopped between renewals, not aborted: a renewal under way when the
    // body ends would otherwise land after the unlock and hold the lock.
    let _ = stop.send(true);
    let _ = renewer.await;
    let unlocked = state.unlock(name, owner).await;
    let out = result?;
    unlocked?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A download streams to the file, retries a 503, and leaves an empty
    /// file on a 404.
    #[tokio::test]
    async fn downloads_stream_to_a_file_and_retry() {
        use axum::http::StatusCode;
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let h = hits.clone();
        let big: &'static [u8] = Box::leak(vec![b'x'; 300_000].into_boxed_slice());
        let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let h = h.clone();
            async move {
                let n = h.fetch_add(1, Ordering::SeqCst);
                match uri.path() {
                    "/missing.pdf" => (StatusCode::NOT_FOUND, &b"no"[..]),
                    _ if n == 0 => (StatusCode::SERVICE_UNAVAILABLE, &b"later"[..]),
                    _ => (StatusCode::OK, big),
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = Http::new(Duration::from_millis(10), "usnm-test", "test").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("a.pdf");
        let n = http
            .download(&format!("http://{addr}/a.pdf"), &dest)
            .await
            .unwrap();
        assert_eq!(n, Some(300_000));
        assert_eq!(std::fs::read(&dest).unwrap().len(), 300_000);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "the 503 was retried");
        let n = http
            .download(&format!("http://{addr}/missing.pdf"), &dest)
            .await
            .unwrap();
        assert_eq!(n, None);
        assert!(std::fs::read(&dest).unwrap().is_empty());
    }
}
