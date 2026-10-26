//! Bounded, descriptor-relative job output checks and scratch creation.

use std::path::Path;

use anyhow::{bail, Context};

use crate::engine::{checked, Sandbox};

/// Opens a page's out directory, refusing a symlink.
pub(super) fn open_dir(path: &Path) -> anyhow::Result<rustix::fd::OwnedFd> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("opening {}", path.display()))
}

/// Checks that a page's directory holds only regular files, `max` bytes
/// in all (the adapter writes nothing else, 13 §13.4.1).
pub(super) fn check_job_dir(dir: &rustix::fd::OwnedFd, max: u64) -> anyhow::Result<()> {
    use rustix::fs::{AtFlags, FileType};
    let mut total = 0u64;
    for entry in rustix::fs::Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_bytes() == b"." || name.to_bytes() == b".." {
            continue;
        }
        let st = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)?;
        if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
            bail!("{:?} in the page's directory isn't a regular file", name);
        }
        total = total.saturating_add(u64::try_from(st.st_size).unwrap_or(u64::MAX));
        if total > max {
            bail!("the page's directory holds over {max} bytes");
        }
    }
    Ok(())
}

/// Reads the regular file `name` in `dir`, at most `max` bytes, without
/// following a symlink.
pub(super) fn read_at(dir: &rustix::fd::OwnedFd, name: &str, max: u64) -> anyhow::Result<Vec<u8>> {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::io::Read;
    let fd = rustix::fs::openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(|| format!("opening {name}"))?;
    let st = rustix::fs::fstat(&fd)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        bail!("{name} isn't a regular file");
    }
    let mut buf = Vec::new();
    std::fs::File::from(fd)
        .take(max + 1)
        .read_to_end(&mut buf)?;
    if buf.len() as u64 > max {
        bail!("{name} is over {max} bytes");
    }
    Ok(buf)
}

/// Creates a page's out directory: mode 0700, writable by the container
/// user through the slot's ACL, and labelled at the slot's level.
pub(super) async fn make_out_dir(
    dir: &Path,
    sandbox: Option<&Sandbox>,
    level: Option<&str>,
) -> anyhow::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    if let Some(sb) = sandbox {
        let e = sb.info.engine_uid;
        // The mode's group bits are the ACL mask: open it again so the
        // inherited entries apply.
        checked(
            tokio::process::Command::new("setfacl")
                .args(["-m", &format!("u:{e}:rwx,m::rwx")])
                .arg(dir),
        )
        .await?;
        if let Some(level) = level {
            checked(
                tokio::process::Command::new("chcon")
                    .args(["-t", "container_file_t", "-l", level])
                    .arg(dir),
            )
            .await?;
        }
    }
    Ok(())
}
