//! The verified-file record (#1722): after a model file's full SHA-256 check against
//! `weights.lock` passes, `usnm_qwen` writes `<file>.usnm-verified` beside it, holding the
//! file's identity (device, inode, size, mtime and ctime in nanoseconds), the digest and the
//! pin it was checked against. A later task skips the hash only when every field still
//! matches; anything else is a full hash, exactly as before the record existed.
//!
//! The record is trusted only where the identity fields carry a guarantee: Linux and macOS
//! (ctime can't be set by an unprivileged process; restoring mtime moves it), on local
//! filesystems known to keep stable inode numbers and a real change time. Everywhere else,
//! Windows included, every check is a full hash. The threat analysis is in
//! `docs/boinc/apps/quality.md` under "Model file checks".
use anyhow::Context as _;
use sha2::Digest as _;
use std::{
    ffi::OsStr,
    fmt::Write as _,
    fs::{File, Metadata},
    io::Read as _,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use usnm_ocr_quality::slot::write_atomic;

/// Set to `1`, `true` or `yes` to hash every model file on every check, as before #1722.
pub const ENV_FULL_HASH: &str = "USNM_QWEN_FULL_HASH";
/// Appended to the model file's name to name its record.
pub const RECORD_SUFFIX: &str = ".usnm-verified";
const RECORD_VERSION: u32 = 1;
const MAX_RECORD: u64 = 4096;
/// A file changed less than this long before its hash began may still change within the
/// same timestamp tick (1 s on HFS+, a few ms on ext4), so no record is written for it.
pub const RACY_MARGIN: Duration = Duration::from_secs(3);

/// How a model file was checked, for the diagnostics. Holds the logical name from
/// `weights.lock`, never a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeightsCheck {
    pub file: String,
    pub kind: CheckKind,
    pub ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    /// The record matched; the file was not read.
    CacheHit,
    /// The whole file was hashed. `miss` says why the record wasn't used, `record` what
    /// happened to the record afterwards.
    FullHash {
        miss: &'static str,
        record: &'static str,
    },
}

impl std::fmt::Display for WeightsCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            CheckKind::CacheHit => write!(f, "{}: cache hit ({} ms)", self.file, self.ms),
            CheckKind::FullHash { miss, record } => write!(
                f,
                "{}: full hash, {miss}, record {record} ({} ms)",
                self.file, self.ms
            ),
        }
    }
}

/// Whether `value` (the [`ENV_FULL_HASH`] variable) asks for full hashing.
pub fn full_hash_forced_from(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"))
}

pub fn full_hash_forced() -> bool {
    full_hash_forced_from(std::env::var_os(ENV_FULL_HASH).as_deref())
}

/// The fields of a file that must not have moved since it was hashed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ns(secs: i64, nsec: i64) -> Option<i64> {
    if !(0..1_000_000_000).contains(&nsec) {
        return None;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(nsec)
}

/// The identity of a regular file, or `None` where this platform gives no ctime guarantee.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn identity_of(meta: &Metadata) -> Option<Identity> {
    use std::os::unix::fs::MetadataExt as _;
    if !meta.is_file() {
        return None;
    }
    Some(Identity {
        dev: meta.dev(),
        ino: meta.ino(),
        size: meta.size(),
        mtime_ns: ns(meta.mtime(), meta.mtime_nsec())?,
        ctime_ns: ns(meta.ctime(), meta.ctime_nsec())?,
    })
}

/// Windows and every other platform: no record is ever trusted. On Windows, stable `std`
/// has no change time and no file index, and the last-write time can be set by anyone who
/// can write the file.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn identity_of(_meta: &Metadata) -> Option<Identity> {
    None
}

/// Whether `f` sits on a local filesystem with stable inode numbers and a change time the
/// kernel keeps. FAT and exFAT synthesise inode numbers and have no change time; network
/// and FUSE filesystems report whatever the server or daemon says; overlay copies up.
#[cfg(target_os = "linux")]
pub(crate) fn trusted_fs(f: &File) -> bool {
    // The magic is 32 bits; `f_type` is wider on 64-bit targets.
    rustix::fs::fstatfs(f).is_ok_and(|st| trusted_fs_magic(st.f_type as u32))
}

/// Linux `statfs` magic numbers of the filesystems [`trusted_fs`] accepts.
#[cfg(target_os = "linux")]
pub(crate) fn trusted_fs_magic(magic: u32) -> bool {
    const EXT4: u32 = 0xEF53;
    const XFS: u32 = 0x5846_5342;
    const BTRFS: u32 = 0x9123_683E;
    const ZFS: u32 = 0x2FC1_2FC1;
    const F2FS: u32 = 0xF2F5_2010;
    const TMPFS: u32 = 0x0102_1994;
    matches!(magic, EXT4 | XFS | BTRFS | ZFS | F2FS | TMPFS)
}

#[cfg(target_os = "macos")]
pub(crate) fn trusted_fs(f: &File) -> bool {
    rustix::fs::fstatfs(f).is_ok_and(|st| {
        let name: Vec<u8> = st
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        trusted_fs_name(&name)
    })
}

/// macOS `f_fstypename` values of the filesystems [`trusted_fs`] accepts.
#[cfg(target_os = "macos")]
pub(crate) fn trusted_fs_name(name: &[u8]) -> bool {
    matches!(name, b"apfs" | b"hfs")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn trusted_fs(_f: &File) -> bool {
    false
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    v: u32,
    /// The logical name in `weights.lock`.
    file: String,
    /// The digest `weights.lock` pinned when the file was checked.
    pin: String,
    /// The digest the full hash computed (equal to `pin`, or no record is written).
    sha256: String,
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i64,
    ctime_ns: i64,
}

impl Record {
    fn identity(&self) -> Identity {
        Identity {
            dev: self.dev,
            ino: self.ino,
            size: self.size,
            mtime_ns: self.mtime_ns,
            ctime_ns: self.ctime_ns,
        }
    }
}

pub fn record_path(file: &Path) -> Option<PathBuf> {
    let mut name = file.file_name()?.to_os_string();
    name.push(RECORD_SUFFIX);
    Some(file.with_file_name(name))
}

/// What the cache checks need besides the file, so tests can relax the filesystem check
/// (temporary directories may sit on any filesystem) and the racy margin.
#[derive(Clone, Copy)]
pub(crate) struct Policy {
    pub require_trusted_fs: bool,
    pub racy_margin: Duration,
}

impl Policy {
    pub const LIVE: Self = Self {
        require_trusted_fs: true,
        racy_margin: RACY_MARGIN,
    };
}

fn margin_ns(p: Policy) -> i64 {
    i64::try_from(p.racy_margin.as_nanos()).unwrap_or(i64::MAX)
}

/// `Ok(())` when the record beside `path` proves the file is the one hashed against `want`
/// for `logical`; otherwise the reason it doesn't, for the diagnostics.
pub(crate) fn lookup(
    logical: &str,
    path: &Path,
    want: &str,
    policy: Policy,
) -> Result<(), &'static str> {
    let f = File::open(path).map_err(|_| "file_unreadable")?;
    let meta = f.metadata().map_err(|_| "file_unreadable")?;
    let now = identity_of(&meta).ok_or("platform_unsupported")?;
    if policy.require_trusted_fs && !trusted_fs(&f) {
        return Err("filesystem_unsupported");
    }
    let rp = record_path(path).ok_or("no_record")?;
    let rmeta = match std::fs::symlink_metadata(&rp) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("no_record"),
        Err(_) => return Err("record_unreadable"),
    };
    if !rmeta.file_type().is_file() || rmeta.len() > MAX_RECORD {
        return Err("record_unreadable");
    }
    let rident = identity_of(&rmeta).ok_or("record_unreadable")?;
    let mut bytes = Vec::new();
    File::open(&rp)
        .and_then(|r| r.take(MAX_RECORD + 1).read_to_end(&mut bytes))
        .map_err(|_| "record_unreadable")?;
    let rec: Record = serde_json::from_slice(&bytes).map_err(|_| "record_parse")?;
    if rec.v != RECORD_VERSION {
        return Err("record_version");
    }
    if rec.file != logical {
        return Err("record_other_file");
    }
    if rec.pin != want {
        return Err("pin_changed");
    }
    if rec.sha256 != rec.pin || !usnm_ocr_contract::boinc::is_sha256(&rec.sha256) {
        return Err("record_inconsistent");
    }
    if rec.identity() != now {
        return Err("identity_changed");
    }
    // Written after a hash that began at least the racy margin after the file last changed,
    // so a record whose own mtime isn't that far past the file's is stale or forged.
    let changed = now.mtime_ns.max(now.ctime_ns);
    if rident.mtime_ns < changed.saturating_add(margin_ns(policy)) {
        return Err("record_older_than_file");
    }
    Ok(())
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// The file's SHA-256, and its identity when it held still through the whole read: the same
/// before and after through the open handle, and the same through `path` afterwards.
pub(crate) fn hash_file(path: &Path) -> anyhow::Result<(String, Option<Identity>)> {
    let mut f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let before = f.metadata().ok().as_ref().and_then(identity_of);
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hex(&hasher.finalize());
    let after = f.metadata().ok().as_ref().and_then(identity_of);
    let by_path = std::fs::metadata(path).ok().as_ref().and_then(identity_of);
    let stable = before.filter(|b| Some(*b) == after && Some(*b) == by_path);
    Ok((digest, stable))
}

fn epoch_ns(t: SystemTime) -> Option<i64> {
    i64::try_from(t.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok()
}

/// Hashes `path` in full and, when it matches `want` and held still, writes its record.
/// Returns the digest and what happened to the record. A record that can't be written is
/// not an error: the next task hashes again.
pub(crate) fn hash_and_record(
    logical: &str,
    path: &Path,
    want: &str,
    policy: Policy,
) -> anyhow::Result<(String, &'static str)> {
    let started = epoch_ns(SystemTime::now());
    let (got, stable) = hash_file(path)?;
    if got != want {
        // A record left from an earlier good hash must not vouch for these bytes later
        // (silent corruption that kept the identity, caught here by a forced hash).
        let removed = record_path(path).is_some_and(|rp| {
            std::fs::symlink_metadata(&rp).is_ok_and(|m| !m.is_dir())
                && std::fs::remove_file(&rp).is_ok()
        });
        let outcome = if removed {
            "not written: mismatch, old record removed"
        } else {
            "not written: mismatch"
        };
        return Ok((got, outcome));
    }
    let Some(identity) = stable else {
        return Ok((got, "not written: no stable identity"));
    };
    if policy.require_trusted_fs {
        let trusted = File::open(path).is_ok_and(|f| trusted_fs(&f));
        if !trusted {
            return Ok((got, "not written: filesystem unsupported"));
        }
    }
    let changed = identity.mtime_ns.max(identity.ctime_ns);
    let settled = started.is_some_and(|s| changed.saturating_add(margin_ns(policy)) <= s);
    if !settled {
        return Ok((got, "not written: changed too recently"));
    }
    let Some(rp) = record_path(path) else {
        return Ok((got, "not written: no path"));
    };
    let rec = Record {
        v: RECORD_VERSION,
        file: logical.to_owned(),
        pin: want.to_owned(),
        sha256: got.clone(),
        dev: identity.dev,
        ino: identity.ino,
        size: identity.size,
        mtime_ns: identity.mtime_ns,
        ctime_ns: identity.ctime_ns,
    };
    let Ok(bytes) = serde_json::to_vec(&rec) else {
        return Ok((got, "not written: serialization"));
    };
    match write_atomic(&rp, &bytes) {
        Ok(()) => Ok((got, "written")),
        Err(_) => Ok((got, "not written: write failed")),
    }
}

/// Checks `path` against `want`: a cache hit when the record proves it unchanged, else a
/// full hash. Returns the computed digest on a full hash (the caller compares it, so a
/// mismatch reads exactly as before) or `None` on a hit.
pub(crate) fn check(
    logical: &str,
    path: &Path,
    want: &str,
    force_full: bool,
    policy: Policy,
) -> anyhow::Result<(Option<String>, WeightsCheck)> {
    let start = Instant::now();
    // A symlinked model keeps its record beside the real file, so every alias shares it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let real = std::fs::canonicalize(path).ok();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let real: Option<PathBuf> = None;
    let path = real.as_deref().unwrap_or(path);
    let miss = if force_full {
        "forced"
    } else {
        match lookup(logical, path, want, policy) {
            Ok(()) => {
                return Ok((
                    None,
                    WeightsCheck {
                        file: logical.to_owned(),
                        kind: CheckKind::CacheHit,
                        ms: elapsed_ms(start),
                    },
                ))
            }
            Err(reason) => reason,
        }
    };
    let (got, record) = hash_and_record(logical, path, want, policy)?;
    Ok((
        Some(got),
        WeightsCheck {
            file: logical.to_owned(),
            kind: CheckKind::FullHash { miss, record },
            ms: elapsed_ms(start),
        },
    ))
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: Policy = Policy {
        require_trusted_fs: false,
        racy_margin: Duration::ZERO,
    };
    const SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

    fn digest(bytes: &[u8]) -> String {
        hex(&sha2::Sha256::digest(bytes))
    }

    #[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
    struct Fixture {
        _tmp: tempfile::TempDir,
        dir: PathBuf,
        path: PathBuf,
        good: Vec<u8>,
        want: String,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("projects/usnewsmap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.gguf");
        let good = b"the reviewed weights, byte for byte".to_vec();
        std::fs::write(&path, &good).unwrap();
        let want = digest(&good);
        Fixture {
            _tmp: tmp,
            dir,
            path,
            good,
            want,
        }
    }

    fn run(fx: &Fixture, want: &str) -> (Option<String>, WeightsCheck) {
        check("model.gguf", &fx.path, want, false, TEST).unwrap()
    }

    fn miss_of(c: &WeightsCheck) -> &'static str {
        match c.kind {
            CheckKind::FullHash { miss, .. } => miss,
            CheckKind::CacheHit => panic!("expected a full hash, got a cache hit"),
        }
    }

    /// Verifies once (writing the record) and returns the fixture.
    fn verified() -> Fixture {
        let fx = fixture();
        let (got, c) = run(&fx, &fx.want);
        assert_eq!(got.as_deref(), Some(fx.want.as_str()));
        if SUPPORTED {
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "no_record",
                    record: "written"
                }
            );
        }
        fx
    }

    #[test]
    fn env_switch_parses_and_is_off_by_default() {
        assert!(!full_hash_forced_from(None));
        for off in ["", "0", "false", "no", "off"] {
            assert!(!full_hash_forced_from(Some(OsStr::new(off))), "{off}");
        }
        for on in ["1", "true", "YES", " yes "] {
            assert!(full_hash_forced_from(Some(OsStr::new(on))), "{on}");
        }
    }

    #[test]
    fn a_verified_unchanged_file_is_a_cache_hit_on_linux_and_macos_only() {
        let fx = verified();
        let (got, c) = run(&fx, &fx.want);
        if SUPPORTED {
            assert_eq!(got, None);
            assert_eq!(c.kind, CheckKind::CacheHit);
            assert!(c.to_string().starts_with("model.gguf: cache hit"));
        } else {
            assert_eq!(got.as_deref(), Some(fx.want.as_str()));
            assert_eq!(miss_of(&c), "platform_unsupported");
            assert!(!record_path(&fx.path).unwrap().exists());
        }
    }

    #[test]
    fn the_switch_forces_a_full_hash_even_with_a_good_record() {
        let fx = verified();
        let (got, c) = check("model.gguf", &fx.path, &fx.want, true, TEST).unwrap();
        assert_eq!(got.as_deref(), Some(fx.want.as_str()));
        assert_eq!(miss_of(&c), "forced");
    }

    #[test]
    fn a_mismatch_writes_no_record() {
        let fx = fixture();
        std::fs::write(&fx.path, b"tampered").unwrap();
        let (got, c) = run(&fx, &fx.want);
        assert_ne!(got.as_deref(), Some(fx.want.as_str()));
        if SUPPORTED {
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "no_record",
                    record: "not written: mismatch"
                }
            );
        }
        assert!(!record_path(&fx.path).unwrap().exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod unix {
        use super::*;

        fn pause() {
            // Long enough for every filesystem's ctime tick, so a rewrite moves ctime.
            std::thread::sleep(Duration::from_millis(1100));
        }

        fn record_json(fx: &Fixture) -> serde_json::Value {
            serde_json::from_slice(&std::fs::read(record_path(&fx.path).unwrap()).unwrap()).unwrap()
        }

        fn write_record_json(fx: &Fixture, v: &serde_json::Value) {
            let rp = record_path(&fx.path).unwrap();
            let mtime = std::fs::metadata(&rp).unwrap().modified().unwrap();
            std::fs::write(&rp, serde_json::to_vec(v).unwrap()).unwrap();
            // Keep the record's own mtime, so only the edited field can cause the miss.
            File::options()
                .write(true)
                .open(&rp)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
        }

        #[test]
        fn the_record_holds_identity_digest_and_pin() {
            use std::os::unix::fs::MetadataExt as _;
            let fx = verified();
            let rec = record_json(&fx);
            let meta = std::fs::metadata(&fx.path).unwrap();
            assert_eq!(rec["v"], 1);
            assert_eq!(rec["file"], "model.gguf");
            assert_eq!(rec["pin"], fx.want.as_str());
            assert_eq!(rec["sha256"], fx.want.as_str());
            assert_eq!(rec["dev"], meta.dev());
            assert_eq!(rec["ino"], meta.ino());
            assert_eq!(rec["size"], meta.size());
            assert_eq!(
                rec["mtime_ns"],
                meta.mtime() * 1_000_000_000 + meta.mtime_nsec()
            );
            assert_eq!(
                rec["ctime_ns"],
                meta.ctime() * 1_000_000_000 + meta.ctime_nsec()
            );
            // No temporary file is left beside it.
            let names: Vec<_> = std::fs::read_dir(&fx.dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert_eq!(names.len(), 2, "{names:?}");
        }

        #[test]
        fn same_size_rewrite_with_restored_mtime_is_caught_by_ctime() {
            let fx = verified();
            let mtime = std::fs::metadata(&fx.path).unwrap().modified().unwrap();
            pause();
            let mut bad = fx.good.clone();
            bad[0] ^= 0xff;
            std::fs::write(&fx.path, &bad).unwrap();
            File::options()
                .write(true)
                .open(&fx.path)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
            let meta = std::fs::metadata(&fx.path).unwrap();
            assert_eq!(meta.modified().unwrap(), mtime);
            assert_eq!(meta.len(), fx.good.len() as u64);
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(miss_of(&c), "identity_changed");
            assert_eq!(got.as_deref(), Some(digest(&bad).as_str()));
        }

        #[test]
        fn a_replacement_by_rename_is_caught_by_inode() {
            let fx = verified();
            let mtime = std::fs::metadata(&fx.path).unwrap().modified().unwrap();
            let mut bad = fx.good.clone();
            bad[1] ^= 0xff;
            let staged = fx.dir.join("staged");
            std::fs::write(&staged, &bad).unwrap();
            File::options()
                .write(true)
                .open(&staged)
                .unwrap()
                .set_modified(mtime)
                .unwrap();
            std::fs::rename(&staged, &fx.path).unwrap();
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(miss_of(&c), "identity_changed");
            assert_ne!(got.as_deref(), Some(fx.want.as_str()));
        }

        #[test]
        fn a_truncated_file_is_a_full_hash() {
            let fx = verified();
            File::options()
                .write(true)
                .open(&fx.path)
                .unwrap()
                .set_len(5)
                .unwrap();
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(miss_of(&c), "identity_changed");
            assert_eq!(got.as_deref(), Some(digest(&fx.good[..5]).as_str()));
        }

        #[test]
        fn a_partial_download_has_no_record_to_use() {
            let fx = fixture();
            std::fs::write(&fx.path, &fx.good[..10]).unwrap();
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(miss_of(&c), "no_record");
            assert_ne!(got.as_deref(), Some(fx.want.as_str()));
            // The download finishes: still a full hash, then a record.
            std::fs::write(&fx.path, &fx.good).unwrap();
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(miss_of(&c), "no_record");
            assert_eq!(got.as_deref(), Some(fx.want.as_str()));
            assert_eq!(run(&fx, &fx.want).1.kind, CheckKind::CacheHit);
        }

        #[test]
        fn a_record_copied_from_another_slot_is_refused() {
            let a = verified();
            let b = fixture();
            std::fs::write(&b.path, b"other bytes, not the pinned ones!!!").unwrap();
            std::fs::copy(record_path(&a.path).unwrap(), record_path(&b.path).unwrap()).unwrap();
            let (got, c) = run(&b, &a.want);
            assert_eq!(miss_of(&c), "identity_changed");
            assert_ne!(got.as_deref(), Some(a.want.as_str()));
        }

        #[test]
        fn a_record_for_another_logical_file_is_refused() {
            let fx = verified();
            let (_, c) = check("mmproj.gguf", &fx.path, &fx.want, false, TEST).unwrap();
            assert_eq!(miss_of(&c), "record_other_file");
        }

        #[test]
        fn a_changed_pin_forces_a_full_hash_and_refuses_the_old_file() {
            let fx = verified();
            let new_pin = digest(b"the next reviewed weights");
            let (got, c) = run(&fx, &new_pin);
            assert_eq!(miss_of(&c), "pin_changed");
            assert_eq!(got.as_deref(), Some(fx.want.as_str()));
            assert_ne!(got.as_deref(), Some(new_pin.as_str()));
        }

        #[test]
        fn a_record_whose_digest_disagrees_with_its_pin_is_refused() {
            let fx = verified();
            let mut rec = record_json(&fx);
            rec["sha256"] = digest(b"x").into();
            write_record_json(&fx, &rec);
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_inconsistent");
        }

        #[test]
        fn each_identity_field_is_checked() {
            for field in ["dev", "ino", "size", "mtime_ns", "ctime_ns"] {
                let fx = verified();
                let mut rec = record_json(&fx);
                let v = rec[field].as_i64().unwrap();
                rec[field] = (v + 1).into();
                write_record_json(&fx, &rec);
                assert_eq!(
                    miss_of(&run(&fx, &fx.want).1),
                    "identity_changed",
                    "{field}"
                );
            }
        }

        #[test]
        fn unreadable_or_malformed_records_are_full_hashes() {
            let fx = verified();
            let rp = record_path(&fx.path).unwrap();
            let mtime = std::fs::metadata(&rp).unwrap().modified().unwrap();
            for (bytes, want) in [
                (&b""[..], "record_parse"),
                (&b"{not json"[..], "record_parse"),
                (&b"{\"v\":1}"[..], "record_parse"),
            ] {
                std::fs::write(&rp, bytes).unwrap();
                File::options()
                    .write(true)
                    .open(&rp)
                    .unwrap()
                    .set_modified(mtime)
                    .unwrap();
                assert_eq!(miss_of(&run(&fx, &fx.want).1), want);
            }
            let mut rec = record_json(&verified());
            rec["extra"] = 1.into();
            std::fs::write(&rp, serde_json::to_vec(&rec).unwrap()).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_parse");
            let mut rec = record_json(&verified());
            rec["v"] = 2.into();
            std::fs::write(&rp, serde_json::to_vec(&rec).unwrap()).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_version");
            // A directory, an oversized file and a symlink are not records.
            std::fs::remove_file(&rp).unwrap();
            std::fs::create_dir(&rp).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_unreadable");
            std::fs::remove_dir(&rp).unwrap();
            std::fs::write(&rp, vec![b' '; 5000]).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_unreadable");
            std::fs::remove_file(&rp).unwrap();
            let other = verified();
            std::os::unix::fs::symlink(record_path(&other.path).unwrap(), &rp).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_unreadable");
            std::fs::remove_file(&rp).unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "no_record");
            // The full hash that follows any of these writes a fresh, good record.
            assert_eq!(run(&fx, &fx.want).1.kind, CheckKind::CacheHit);
        }

        #[test]
        fn a_record_older_than_the_file_is_refused() {
            // Clock set forward when the file was written, or a record restored from a
            // backup: the record's own mtime is before the file's change.
            let fx = verified();
            let rp = record_path(&fx.path).unwrap();
            let meta = std::fs::metadata(&fx.path).unwrap();
            let earlier = meta.modified().unwrap() - Duration::from_secs(60);
            File::options()
                .write(true)
                .open(&rp)
                .unwrap()
                .set_modified(earlier)
                .unwrap();
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "record_older_than_file");
        }

        #[test]
        fn a_file_changed_within_the_racy_margin_gets_no_record() {
            let fx = fixture();
            let live_margin = Policy {
                require_trusted_fs: false,
                racy_margin: RACY_MARGIN,
            };
            let (got, c) = check("model.gguf", &fx.path, &fx.want, false, live_margin).unwrap();
            assert_eq!(got.as_deref(), Some(fx.want.as_str()));
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "no_record",
                    record: "not written: changed too recently"
                }
            );
            assert!(!record_path(&fx.path).unwrap().exists());
            // A file with an mtime in the future (clock set back since) is never settled.
            let future = SystemTime::now() + Duration::from_secs(3600);
            File::options()
                .write(true)
                .open(&fx.path)
                .unwrap()
                .set_modified(future)
                .unwrap();
            let (_, c) = run(&fx, &fx.want);
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "no_record",
                    record: "not written: changed too recently"
                }
            );
        }

        #[test]
        fn a_record_written_too_soon_after_the_change_is_refused() {
            // A record whose mtime is within the margin of the file's change could have
            // been written in the same timestamp tick as a later rewrite.
            let fx = verified();
            let live_margin = Policy {
                require_trusted_fs: false,
                racy_margin: RACY_MARGIN,
            };
            assert_eq!(
                lookup("model.gguf", &fx.path, &fx.want, live_margin),
                Err("record_older_than_file")
            );
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn the_filesystem_list_is_the_documented_one() {
            for magic in [
                0xEF53,
                0x5846_5342,
                0x9123_683E,
                0x2FC1_2FC1,
                0xF2F5_2010,
                0x0102_1994,
            ] {
                assert!(trusted_fs_magic(magic), "{magic:#x}");
            }
            // overlayfs, NFS, FUSE, SMB2, CIFS, vfat, exFAT
            for magic in [
                0x794C_7630,
                0x6969,
                0x6573_5546,
                0xFE53_4D42,
                0xFF53_4D42,
                0x4D44,
                0x2011_BAB0,
            ] {
                assert!(!trusted_fs_magic(magic), "{magic:#x}");
            }
        }

        #[cfg(target_os = "macos")]
        #[test]
        fn the_filesystem_list_is_the_documented_one() {
            for name in [&b"apfs"[..], &b"hfs"[..]] {
                assert!(trusted_fs_name(name));
            }
            let others: [&[u8]; 7] = [
                b"msdos", b"exfat", b"nfs", b"smbfs", b"macfuse", b"apfsx", b"",
            ];
            for name in others {
                assert!(!trusted_fs_name(name));
            }
        }

        #[test]
        fn the_live_policy_checks_the_filesystem_first() {
            // Whatever the temporary directory sits on, an untrusted filesystem is a full
            // hash before any record is read, and a trusted one gets as far as the record.
            let fx = fixture();
            let trusted = trusted_fs(&File::open(&fx.path).unwrap());
            let want = if trusted {
                "no_record"
            } else {
                "filesystem_unsupported"
            };
            assert_eq!(
                lookup("model.gguf", &fx.path, &fx.want, Policy::LIVE),
                Err(want)
            );
        }

        #[test]
        fn symlinked_aliases_share_the_record_beside_the_real_file() {
            let fx = verified();
            // Two slots link the same model: both hit the record beside it, and neither
            // link gets a record of its own.
            let links = [fx.dir.join("slot1.gguf"), fx.dir.join("slot2.gguf")];
            for link in &links {
                std::os::unix::fs::symlink(&fx.path, link).unwrap();
                let (got, c) = check("model.gguf", link, &fx.want, false, TEST).unwrap();
                assert_eq!(c.kind, CheckKind::CacheHit);
                assert_eq!(got, None);
                assert!(!record_path(link).unwrap().exists());
            }
            // Re-pointed at different bytes, a link is checked against those bytes.
            let other = fx.dir.join("other.gguf");
            std::fs::write(&other, b"different weights, same name").unwrap();
            std::fs::remove_file(&links[0]).unwrap();
            std::os::unix::fs::symlink(&other, &links[0]).unwrap();
            let (got, c) = check("model.gguf", &links[0], &fx.want, false, TEST).unwrap();
            assert_eq!(miss_of(&c), "no_record");
            assert_ne!(got.as_deref(), Some(fx.want.as_str()));
        }

        #[test]
        fn a_mismatch_removes_the_old_record() {
            // Corruption that kept size and timestamps: only a forced hash sees it, and the
            // record must not let the next unforced check skip the file.
            let fx = verified();
            let rp = record_path(&fx.path).unwrap();
            let rec = std::fs::read(&rp).unwrap();
            std::fs::write(&fx.path, b"corrupted weights, same length!!!!!").unwrap();
            // Put the record back as if the identity hadn't moved.
            let mut v: serde_json::Value = serde_json::from_slice(&rec).unwrap();
            {
                use std::os::unix::fs::MetadataExt as _;
                let m = std::fs::metadata(&fx.path).unwrap();
                v["size"] = m.size().into();
                v["mtime_ns"] = (m.mtime() * 1_000_000_000 + m.mtime_nsec()).into();
                v["ctime_ns"] = (m.ctime() * 1_000_000_000 + m.ctime_nsec()).into();
            }
            std::fs::write(&rp, serde_json::to_vec(&v).unwrap()).unwrap();
            assert_eq!(run(&fx, &fx.want).1.kind, CheckKind::CacheHit);
            let (got, c) = check("model.gguf", &fx.path, &fx.want, true, TEST).unwrap();
            assert_ne!(got.as_deref(), Some(fx.want.as_str()));
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "forced",
                    record: "not written: mismatch, old record removed"
                }
            );
            assert!(!rp.exists());
            assert_eq!(miss_of(&run(&fx, &fx.want).1), "no_record");
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn other_platforms_always_hash_and_never_write_a_record() {
        let fx = fixture();
        for _ in 0..2 {
            let (got, c) = run(&fx, &fx.want);
            assert_eq!(got.as_deref(), Some(fx.want.as_str()));
            assert_eq!(
                c.kind,
                CheckKind::FullHash {
                    miss: "platform_unsupported",
                    record: "not written: no stable identity"
                }
            );
        }
        assert!(!record_path(&fx.path).unwrap().exists());
    }
}
