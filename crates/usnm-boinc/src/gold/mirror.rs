//! Publish only the locked v1 LoC originals, without creating work or opening BLN600.
use super::{prepare, submit};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
struct Lock {
    bundle_version: u32,
    manifest_sha256: String,
    remotes: Vec<Remote>,
}

#[derive(Deserialize)]
struct Remote {
    kind: String,
    source: String,
    sha256: String,
    url: String,
}

#[derive(Debug, Serialize)]
pub struct Receipt {
    pub name: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub md5: String,
    pub sha256: String,
    pub already_present: bool,
}

fn selected(p: &prepare::Prepared) -> anyhow::Result<Vec<&prepare::Staged>> {
    let lock: Lock = serde_json::from_str(include_str!(
        "../../../../fixtures/ocr-gold/bundle-v1.lock.json"
    ))?;
    ensure!(
        p.bundle_version == lock.bundle_version && p.manifest_sha256 == lock.manifest_sha256,
        "campaign does not match the locked v1 manifest"
    );
    let mut selected = Vec::new();
    for r in lock.remotes.iter().filter(|r| {
        r.kind == "page-image"
            && r.source == "overproof-ca"
            && r.url.starts_with("https://tile.loc.gov/")
    }) {
        let name = format!("{}.jpg", r.sha256);
        let matches: Vec<_> = p.remotes.iter().filter(|s| s.name == name).collect();
        ensure!(
            matches.len() == 1,
            "missing or duplicate LoC original {name}"
        );
        let s = matches[0];
        ensure!(
            s.sha256 == r.sha256 && s.url.as_ref() == Some(&r.url),
            "LoC original differs from the lock: {name}"
        );
        selected.push(s);
    }
    ensure!(selected.len() == 18, "unexpected locked LoC file count");
    ensure!(
        p.remotes
            .iter()
            .filter(|s| s.name.ends_with(".jpg"))
            .count()
            == selected.len(),
        "unexpected JPEG original outside the LoC allowlist"
    );
    Ok(selected)
}

fn safe_path(root: &Path, path: &Path) -> anyhow::Result<()> {
    let relative = path
        .strip_prefix(root)
        .context("path outside expected root")?;
    ensure!(
        relative
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "non-normal path"
    );
    let mut current = root.to_owned();
    for part in relative.components() {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(!metadata.is_symlink(), "symlink: {}", current.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// All cache files and existing destinations are checked before publishing any file.
/// Receipts describe disk publication only; HTTP transfer and client reuse need separate tests.
pub async fn loc(root: &Path, project: &Path) -> anyhow::Result<Vec<Receipt>> {
    let root = root.canonicalize()?;
    let project = project.canonicalize()?;
    let download = project.join("download");
    ensure!(
        !std::fs::symlink_metadata(&download)?.is_symlink(),
        "download root must not be a symlink"
    );
    safe_path(&root, &root.join(prepare::PREPARED))?;
    let p = prepare::read(&root)?;
    stage(&root, &project, &download, selected(&p)?).await
}

async fn stage(
    root: &Path,
    project: &Path,
    download: &Path,
    files: Vec<&prepare::Staged>,
) -> anyhow::Result<Vec<Receipt>> {
    let mut plan = Vec::new();
    for s in files {
        let source = root.join("cache").join(&s.name);
        safe_path(root, &source)?;
        submit::verify_file(&source, s)?;
        let name = submit::remote_physical(s)?;
        let path = submit::hier_path(project, &name).await?;
        safe_path(download, &path)?;
        ensure!(
            path.file_name() == Some(name.as_ref()),
            "unexpected physical filename"
        );
        let already_present = path.exists();
        if already_present {
            submit::verify_file(&path, s)?;
        }
        plan.push((
            source,
            s,
            Receipt {
                name,
                path,
                bytes: s.bytes,
                md5: s.md5.clone(),
                sha256: s.sha256.clone(),
                already_present,
            },
        ));
    }
    let mut receipts = Vec::new();
    for (source, s, receipt) in plan {
        safe_path(download, &receipt.path)?;
        safe_path(root, &source)?;
        submit::publish_original(&receipt.path, &source, s)?;
        receipts.push(receipt);
    }
    Ok(receipts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_is_exactly_the_locked_loc_set_and_never_bln() {
        let lock: Lock = serde_json::from_str(include_str!(
            "../../../../fixtures/ocr-gold/bundle-v1.lock.json"
        ))
        .unwrap();
        let remotes: Vec<_> = lock
            .remotes
            .iter()
            .map(|r| prepare::Staged {
                bytes: 1,
                md5: "0".repeat(32),
                name: format!(
                    "{}.{}",
                    r.sha256,
                    if r.kind == "zip" { "zip" } else { "jpg" }
                ),
                sha256: r.sha256.clone(),
                url: Some(r.url.clone()),
            })
            .collect();
        let mut p = prepare::Prepared {
            bundle: remotes[0].clone(),
            bundle_version: lock.bundle_version,
            manifest_sha256: lock.manifest_sha256,
            crops: Default::default(),
            remotes,
        };
        assert_eq!(selected(&p).unwrap().len(), 18);
        assert!(selected(&p)
            .unwrap()
            .iter()
            .all(|s| s.name.ends_with(".jpg")));
        p.remotes.push(p.remotes[1].clone());
        assert!(selected(&p).is_err());
        p.remotes.pop();
        p.remotes[1].url = Some("https://unapproved.example/input.jpg".into());
        assert!(selected(&p).is_err());
        p.manifest_sha256 = "0".repeat(64);
        assert!(selected(&p).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stage_is_atomic_idempotent_and_preflights_every_file() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let project = root.join("project");
        let download = project.join("download");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        std::fs::create_dir_all(&download).unwrap();
        std::fs::create_dir_all(root.join("cache")).unwrap();
        let helper = project.join("bin/dir_hier_path");
        std::fs::write(&helper, "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut files = Vec::new();
        for body in [b"first".as_slice(), b"second".as_slice()] {
            let input = root.join("input");
            std::fs::write(&input, body).unwrap();
            let (bytes, md5, sha256) = prepare::md5_sha256(&input).unwrap();
            let name = format!("{sha256}.jpg");
            std::fs::rename(&input, root.join("cache").join(&name)).unwrap();
            files.push(prepare::Staged {
                bytes,
                md5,
                sha256,
                name,
                url: None,
            });
        }
        let second = root.join("cache").join(&files[1].name);
        std::fs::write(&second, b"bad").unwrap();
        assert!(stage(&root, &project, &download, files.iter().collect())
            .await
            .is_err());
        assert_eq!(std::fs::read_dir(&download).unwrap().count(), 0);
        std::fs::write(&second, b"second").unwrap();
        let receipts = stage(&root, &project, &download, files.iter().collect())
            .await
            .unwrap();
        assert_eq!(receipts.len(), 2);
        assert!(receipts.iter().all(|r| !r.already_present));
        for r in &receipts {
            assert_eq!(
                std::fs::metadata(&r.path).unwrap().permissions().mode() & 0o777,
                0o664
            );
        }
        let replay = stage(&root, &project, &download, files.iter().collect())
            .await
            .unwrap();
        assert!(replay.iter().all(|r| r.already_present));
        std::fs::write(&receipts[1].path, b"bad").unwrap();
        assert!(stage(&root, &project, &download, files.iter().collect())
            .await
            .is_err());
        std::fs::remove_file(&receipts[1].path).unwrap();
        symlink(&second, &receipts[1].path).unwrap();
        assert!(stage(&root, &project, &download, files.iter().collect())
            .await
            .is_err());
        assert!(safe_path(&download, &project.join("outside")).is_err());
        assert!(safe_path(&download, &download.join("../outside")).is_err());
        assert!(!project.join("templates").exists());
        assert!(!root.join("units").exists());
        assert!(!project.join("gold").exists());
    }
}
