//! Pipeline state (05 §5.9.1): the document store behind the ingest
//! pipeline's work queue, locks and index runs, and the typed items in it.
//!
//! The ingest pipeline writes this state. The API reads it, read-only, for
//! the public status page, which is why it lives apart from the pipeline's
//! heavier dependencies.

pub mod cosmos;
pub mod docs;
pub mod ocr_local;
pub mod state;
pub mod summary;

/// The Cosmos DB database that holds every container.
pub const DATABASE: &str = "usnm";

/// A process-unique id for leases and attempt paths: the lock owners of
/// [`state::State`]. It starts with where the process runs, so a held-by
/// message names it (#1133): [`host_name`], then the process id and the
/// clock's nanoseconds.
pub fn owner_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{}-{}-{nanos:08x}", host_name(), std::process::id())
}

/// Where this process runs, as a path segment:
/// - a Container Apps replica's name (`CONTAINER_APP_REPLICA_NAME`): a job's
///   container has no `HOSTNAME`, and its binary is PID 1;
/// - `HOSTNAME`, which shells and containers set;
/// - the kernel's hostname, for a systemd unit on an on-prem host, which
///   sets neither;
/// - `local`, when none of them is a safe path segment.
pub fn host_name() -> String {
    let env = |k: &str| std::env::var(k).ok();
    host_from(
        env("CONTAINER_APP_REPLICA_NAME"),
        env("HOSTNAME"),
        std::fs::read_to_string("/proc/sys/kernel/hostname").ok(),
    )
}

fn host_from(replica: Option<String>, hostname: Option<String>, kernel: Option<String>) -> String {
    [replica, hostname, kernel]
        .into_iter()
        .flatten()
        .map(|h| h.trim().to_owned())
        .find(|h| usnm_store::is_safe_segment(h))
        .unwrap_or_else(|| "local".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(s: &str) -> Option<String> {
        Some(s.to_owned())
    }

    #[test]
    fn the_host_is_the_replica_then_hostname_then_the_kernels() {
        let replica = some("usnm-ocrs-caj-prod-29882117-x2k9p");
        assert_eq!(
            host_from(replica.clone(), some("box"), some("usnm-batch\n")),
            "usnm-ocrs-caj-prod-29882117-x2k9p"
        );
        assert_eq!(host_from(None, some("box"), some("usnm-batch\n")), "box");
        // A systemd unit: no environment, the kernel's name with its newline.
        assert_eq!(host_from(None, None, some("usnm-batch\n")), "usnm-batch");
        assert_eq!(host_from(None, None, None), "local");
    }

    #[test]
    fn a_name_that_isnt_a_path_segment_is_skipped() {
        assert_eq!(
            host_from(some(""), some("a/b"), some("usnm-batch")),
            "usnm-batch"
        );
        assert_eq!(host_from(None, some("has space"), some("..")), "local");
    }

    #[test]
    fn the_owner_starts_with_the_host() {
        let id = owner_id();
        assert!(id.starts_with(&format!("{}-", host_name())), "{id}");
        assert!(usnm_store::is_safe_segment(&id), "{id}");
    }
}
