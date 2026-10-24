//! Trying a store request again after a failure that may pass: a dropped or
//! hung connection, a busy service. Blob GETs, the blocks and commit of a
//! streamed upload, and the on-prem publish's other requests
//! (`usnm-ingest publish-staged`) go through [`retrying`].

use std::future::Future;
use std::time::Duration;

use crate::StoreError;

/// How many tries a request gets, and the waits between them: `first_wait`
/// after the first failed try, doubling after each, at most `max_wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub tries: u32,
    pub first_wait: Duration,
    pub max_wait: Duration,
}

impl Retry {
    /// 12 tries, 0.5 s doubling to 60 s: about 5 minutes of waits in all,
    /// enough to ride out a host's network adapter resetting for a few
    /// minutes (#283).
    pub const STANDARD: Retry = Retry {
        tries: 12,
        first_wait: Duration::from_millis(500),
        max_wait: Duration::from_secs(60),
    };

    /// The wait after the `attempt`th failed try (from 1).
    pub fn wait(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(31);
        self.first_wait
            .saturating_mul(1 << doublings)
            .min(self.max_wait)
    }

    /// Every wait of a request whose tries all fail.
    pub fn total_wait(&self) -> Duration {
        (1..self.tries).map(|a| self.wait(a)).sum()
    }
}

/// Whether a failed request may succeed when tried again: a transport
/// failure (connection, timeout, a body cut short), 408, 429 or a 5xx.
pub fn retryable(e: &StoreError) -> bool {
    match e {
        StoreError::Io(_) => true,
        StoreError::Http { status, .. } => matches!(*status, 408 | 429 | 500..=599),
        _ => false,
    }
}

/// [`retryable`], and a token that couldn't be fetched: the identity
/// endpoint reaches Entra ID over the same network, so an outage that fails
/// a request can fail a token refresh too. For the writes of a long upload,
/// where a misconfigured identity has already failed the first request.
pub fn retryable_write(e: &StoreError) -> bool {
    retryable(e) || matches!(e, StoreError::Credential(_))
}

/// Run `f` (given the try's number, from 1) until it succeeds, fails in a
/// way `again` says won't pass, or has failed `retry.tries` times. Each
/// failure is logged with its try, the wait before the next and the error.
pub async fn retrying<T, F, Fut>(
    op: &str,
    path: &str,
    retry: Retry,
    again: fn(&StoreError) -> bool,
    mut f: F,
) -> Result<T, StoreError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    let mut attempt = 1;
    loop {
        match f(attempt).await {
            Err(e) if again(&e) && attempt < retry.tries => {
                let wait = retry.wait(attempt);
                tracing::warn!(
                    op,
                    path,
                    attempt,
                    tries = retry.tries,
                    wait_secs = wait.as_secs_f64(),
                    error = %e,
                    "store request failed; trying again"
                );
                tokio::time::sleep(wait).await;
                attempt += 1;
            }
            Err(e) if again(&e) => {
                tracing::warn!(
                    op,
                    path,
                    attempt,
                    tries = retry.tries,
                    error = %e,
                    "store request failed on its last try"
                );
                return Err(e);
            }
            r => return r,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_double_to_the_cap() {
        let r = Retry::STANDARD;
        assert_eq!(r.wait(1), Duration::from_millis(500));
        assert_eq!(r.wait(2), Duration::from_secs(1));
        assert_eq!(r.wait(8), Duration::from_secs(60));
        assert_eq!(r.wait(40), Duration::from_secs(60));
        // The standard schedule rides out about 5 minutes.
        assert!(
            r.total_wait() >= Duration::from_secs(300),
            "{:?}",
            r.total_wait()
        );
    }

    #[test]
    fn what_is_tried_again() {
        let http = |status| StoreError::Http {
            op: "put",
            path: "p".into(),
            status,
        };
        for s in [408, 429, 500, 503] {
            assert!(retryable(&http(s)), "{s}");
        }
        for s in [400, 403, 404, 409, 412] {
            assert!(!retryable_write(&http(s)), "{s}");
        }
        assert!(retryable(&StoreError::Io("reset".into())));
        assert!(!retryable(&StoreError::Credential("x".into())));
        assert!(retryable_write(&StoreError::Credential("x".into())));
        assert!(!retryable_write(&StoreError::AlreadyExists("p".into())));
        assert!(!retryable_write(&StoreError::InvalidPath("p".into())));
    }

    #[tokio::test]
    async fn tries_until_success_a_refusal_or_the_last_try() {
        let fast = Retry {
            tries: 4,
            first_wait: Duration::from_millis(1),
            max_wait: Duration::from_millis(2),
        };
        let mut seen = Vec::new();
        let r = retrying("op", "p", fast, retryable, |a| {
            seen.push(a);
            async move {
                if a < 3 {
                    Err(StoreError::Io("reset".into()))
                } else {
                    Ok(a)
                }
            }
        })
        .await;
        assert_eq!(r.unwrap(), 3);
        assert_eq!(seen, [1, 2, 3]);

        let mut n = 0;
        let r: Result<(), _> = retrying("op", "p", fast, retryable, |_| {
            n += 1;
            async {
                Err(StoreError::Http {
                    op: "op",
                    path: "p".into(),
                    status: 403,
                })
            }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(n, 1);

        let mut n = 0;
        let r: Result<(), _> = retrying("op", "p", fast, retryable, |_| {
            n += 1;
            async { Err(StoreError::Io("reset".into())) }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(n, 4);
    }
}
