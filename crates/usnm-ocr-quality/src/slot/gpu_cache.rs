//! Persistent private GPU kernel caches and CPU allocation.

use std::path::{Path, PathBuf};

/// Default 2 GiB cap for NVIDIA's PTX JIT cache (`CUDA_CACHE_MAXSIZE`), lifted
/// above the 256 MiB driver default so multi-model `ggml-cuda` PTX cubins (~85 MiB)
/// are not evicted between workunits.
pub const DEFAULT_CUDA_CACHE_MAXSIZE: &str = "2147483648";
/// Default 2 GiB cap for Mesa's Vulkan pipeline/shader cache (`MESA_SHADER_CACHE_MAX_SIZE`).
pub const DEFAULT_MESA_SHADER_CACHE_MAX_SIZE: &str = "2G";

/// The cache directory's per-user suffix: the effective uid on Unix, where
/// the directory sits in a shared `/var/tmp` or `/tmp`.
#[cfg(unix)]
fn cache_user_tag() -> String {
    rustix::process::geteuid().as_raw().to_string()
}

/// The cache directory's per-user suffix on Windows, whose temp directory is
/// already the user's own.
#[cfg(not(unix))]
fn cache_user_tag() -> String {
    for var in ["USERNAME", "USER", "LOGNAME"] {
        if let Ok(val) = std::env::var(var) {
            let clean: String = val
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .take(32)
                .collect();
            if !clean.is_empty() {
                return clean;
            }
        }
    }
    String::from("shared")
}

/// Makes `dir` (whose parent exists) a directory only this user can reach.
/// A missing one is created with mode 0700. One already there is used only
/// when it is a real directory, not a symlink, owned by this user, with no
/// group or other access; a directory someone else made at the predictable
/// path in a shared `/var/tmp` or `/tmp` is passed over.
#[cfg(unix)]
fn private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return false,
    }
    std::fs::symlink_metadata(dir).is_ok_and(|m| {
        m.file_type().is_dir()
            && m.uid() == rustix::process::geteuid().as_raw()
            && m.mode() & 0o077 == 0
    })
}

/// On Windows the candidates sit in the user's own temp directory.
#[cfg(not(unix))]
fn private_dir(dir: &Path) -> bool {
    std::fs::create_dir_all(dir).is_ok()
        && std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_dir())
}

/// `dir` is private to this user and holds writable `cuda` and `mesa`
/// subdirectories. The write probe is created exclusively, so an existing
/// file or symlink at its name is never opened.
pub(super) fn ensure_writable_cache_pair(dir: &Path) -> bool {
    if !private_dir(dir) {
        return false;
    }
    let cuda = dir.join("cuda");
    let mesa = dir.join("mesa");
    if std::fs::create_dir_all(&cuda).is_err() || std::fs::create_dir_all(&mesa).is_err() {
        return false;
    }
    let probe = cuda.join(format!(".writable.{}", std::process::id()));
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
    };
    let created = match create() {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // A probe left by an earlier process with this pid. Only this user
            // can write here, so it is ours to remove (a symlink is removed,
            // never followed).
            std::fs::remove_file(&probe).is_ok() && create().is_ok()
        }
        r => r.is_ok(),
    };
    if created {
        let _ = std::fs::remove_file(&probe);
    }
    created
}

/// Resolves a writable directory outside per-invocation scratch `TempDir`s so
/// CUDA (`CUDA_CACHE_PATH`) and Mesa Vulkan (`MESA_SHADER_CACHE_DIR`) keep
/// compiled PTX SASS cubins and SPIR-V shader pipelines (#794). The first
/// candidate that is private to this user and writable wins:
/// `/var/tmp/usnm-gpu-cache-<uid>`, then the same name in the temp directory,
/// then `gpu-cache` under `fallback_root`.
///
/// The cache outlives a workunit on native hosts only. The project's
/// containers run with a read-only root and a per-container `/tmp` tmpfs, so
/// there the cache lands in `/tmp` and lasts one workunit: it is shared by
/// that workunit's processes (Surya's input modes, for one) but gone for the
/// next.
pub fn persistent_gpu_cache_dir(fallback_root: &Path) -> PathBuf {
    let suffix = format!("usnm-gpu-cache-{}", cache_user_tag());
    let candidates = [
        #[cfg(unix)]
        PathBuf::from("/var/tmp").join(&suffix),
        std::env::temp_dir().join(&suffix),
        fallback_root.join("gpu-cache"),
    ];

    for dir in candidates {
        if ensure_writable_cache_pair(&dir) {
            return dir;
        }
    }
    fallback_root.to_path_buf()
}

/// Configures `CUDA_CACHE_PATH`, `CUDA_CACHE_MAXSIZE`, `MESA_SHADER_CACHE_DIR`,
/// and `MESA_SHADER_CACHE_MAX_SIZE` on `cmd` (preserving any explicit caller
/// environment overrides) so overriding `HOME` to an ephemeral `TempDir` does
/// not discard ~85 MiB of compiled GPU kernels after every workunit on a
/// native host (#794); see [`persistent_gpu_cache_dir`] for containers.
pub fn apply_gpu_cache_env(cmd: &mut std::process::Command, fallback_root: &Path) {
    let root = persistent_gpu_cache_dir(fallback_root);
    if std::env::var_os("CUDA_CACHE_PATH").is_none() {
        cmd.env("CUDA_CACHE_PATH", root.join("cuda"));
    }
    if std::env::var_os("CUDA_CACHE_MAXSIZE").is_none() {
        cmd.env("CUDA_CACHE_MAXSIZE", DEFAULT_CUDA_CACHE_MAXSIZE);
    }
    if std::env::var_os("MESA_SHADER_CACHE_DIR").is_none() {
        cmd.env("MESA_SHADER_CACHE_DIR", root.join("mesa"));
    }
    if std::env::var_os("MESA_SHADER_CACHE_MAX_SIZE").is_none() {
        cmd.env(
            "MESA_SHADER_CACHE_MAX_SIZE",
            DEFAULT_MESA_SHADER_CACHE_MAX_SIZE,
        );
    }
}

/// Selects the CPU worker thread count (`--threads`) when offloading inference
/// to a GPU: honors BOINC's `<ncpus>` allocation when higher, otherwise uses up
/// to 4 available host CPU threads for image decoding and multimodal patch
/// preparation (clamped to `2..=8`) (#794).
pub fn gpu_threads(ncpus: Option<f64>, cpu_threads: Option<u32>) -> u32 {
    let boinc_cpus = ncpus.map(|n| n.round().max(1.0) as u32).unwrap_or(0);
    let hw_cap = cpu_threads.unwrap_or(2).min(4);
    boinc_cpus.max(hw_cap).clamp(2, 8)
}
