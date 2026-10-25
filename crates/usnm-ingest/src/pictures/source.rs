//! Where the dataset's Parquet files are read from: a directory, or the
//! Hugging Face Hub at a pinned revision (#1102).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::DATASET;
use crate::source::{fetch, get_json, hex};

/// The Hub.
pub const HUB: &str = "https://huggingface.co";

/// Where the files come from.
#[derive(Debug, Clone)]
pub enum Source {
    /// `{dir}/{config}/*.parquet`, as `huggingface-cli download` lays them out.
    Dir(PathBuf),
    /// The Hub at `base` (`https://huggingface.co`): the files under each
    /// config are listed at the revision and downloaded one at a time into
    /// `scratch`, each deleted once it has been read.
    Hub { base: String, scratch: PathBuf },
}

/// A file of the dataset, before it is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetFile {
    /// Its path in the dataset (`cartoons/train-00000-of-00001.parquet`).
    pub name: String,
    /// The sha256 and size the source lists (the Hub's LFS object id and
    /// size), which the bytes are checked against.
    pub sha256: Option<String>,
    pub size: Option<u64>,
}

/// A file ready to read. A downloaded file is deleted when this is dropped.
#[derive(Debug)]
pub struct Local {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    delete: bool,
}

impl Drop for Local {
    fn drop(&mut self) {
        if self.delete {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// One entry of the Hub's `/api/datasets/{repo}/tree/{revision}/{path}`.
#[derive(Debug, Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Debug, Deserialize)]
struct Lfs {
    oid: String,
    #[serde(default)]
    size: Option<u64>,
}

impl Source {
    /// The Parquet files of one config, by name.
    pub async fn list(&self, revision: &str, config: &str) -> anyhow::Result<Vec<DatasetFile>> {
        let mut files = match self {
            Source::Dir(dir) => list_dir(&dir.join(config), config)?,
            Source::Hub { base, .. } => {
                let url = format!(
                    "{}/api/datasets/{DATASET}/tree/{revision}/{config}",
                    base.trim_end_matches('/')
                );
                let body = get_json(&url)
                    .await?
                    .with_context(|| format!("{url}: not found (is the revision right?)"))?;
                parse_tree(&body, config).with_context(|| url.clone())?
            }
        };
        files.sort_by(|a, b| a.name.cmp(&b.name));
        if files.is_empty() {
            bail!("no Parquet files for the `{config}` config at {self}");
        }
        Ok(files)
    }

    /// A file on local disk, with its size and sha256: the file itself for a
    /// directory, a download checked against the Hub's own for the Hub.
    pub async fn get(&self, revision: &str, file: &DatasetFile) -> anyhow::Result<Local> {
        match self {
            Source::Dir(dir) => {
                let path = dir.join(&file.name);
                let (bytes, sha256) = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || digest_file(&path)
                })
                .await
                .context("hashing a dataset file stopped")??;
                Ok(Local {
                    path,
                    bytes,
                    sha256,
                    delete: false,
                })
            }
            Source::Hub { base, scratch } => {
                tokio::fs::create_dir_all(scratch)
                    .await
                    .with_context(|| format!("creating {}", scratch.display()))?;
                let url = format!(
                    "{}/datasets/{DATASET}/resolve/{revision}/{}",
                    base.trim_end_matches('/'),
                    file.name
                );
                let path = scratch.join(file.name.replace('/', "__"));
                let sha256 = match fetch(&url, &path).await {
                    Ok(sha256) => sha256,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(e);
                    }
                };
                // Deleted from here on, whatever happens next.
                let bytes = std::fs::metadata(&path)?.len();
                let local = Local {
                    path,
                    bytes,
                    sha256,
                    delete: true,
                };
                if let Some(want) = file.sha256.as_deref() {
                    if want != local.sha256 {
                        bail!(
                            "`{}` downloaded with sha256 {}, but the Hub lists {want}",
                            file.name,
                            local.sha256
                        );
                    }
                }
                if let Some(want) = file.size {
                    if want != local.bytes {
                        bail!(
                            "`{}` downloaded as {} bytes, but the Hub lists {want}",
                            file.name,
                            local.bytes
                        );
                    }
                }
                Ok(local)
            }
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Dir(dir) => write!(f, "{}", dir.display()),
            Source::Hub { base, .. } => write!(f, "{base}"),
        }
    }
}

/// `{dir}/*.parquet`, named `{config}/{file}`.
fn list_dir(dir: &Path, config: &str) -> anyhow::Result<Vec<DatasetFile>> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".parquet") && entry.file_type()?.is_file() {
            files.push(DatasetFile {
                name: format!("{config}/{name}"),
                sha256: None,
                size: None,
            });
        }
    }
    Ok(files)
}

/// The Parquet files in a tree listing of `config`.
fn parse_tree(body: &[u8], config: &str) -> anyhow::Result<Vec<DatasetFile>> {
    let entries: Vec<TreeEntry> = serde_json::from_slice(body).context("the tree listing")?;
    let prefix = format!("{config}/");
    Ok(entries
        .into_iter()
        .filter(|e| e.kind == "file" && e.path.starts_with(&prefix) && e.path.ends_with(".parquet"))
        .filter(|e| {
            // One level only, and no way out of the scratch directory.
            !e.path[prefix.len()..].contains('/') && !e.path.contains("..")
        })
        .map(|e| DatasetFile {
            name: e.path,
            sha256: e.lfs.as_ref().map(|l| l.oid.clone()),
            // A large file's own size is the LFS object's; its entry's is the pointer's.
            size: match &e.lfs {
                Some(lfs) => lfs.size,
                None => e.size,
            },
        })
        .collect())
}

/// The size and sha256 of a file on disk.
fn digest_file(path: &Path) -> anyhow::Result<(u64, String)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        bytes += n as u64;
    }
    Ok((bytes, hex(&hash.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_parquet_files_of_a_config() {
        let body = br#"[
            {"type":"file","oid":"a1","size":134,"path":"cartoons/train-00000-of-00002.parquet",
             "lfs":{"oid":"0f","size":71381230,"pointerSize":134}},
            {"type":"file","oid":"a2","size":900,"path":"cartoons/train-00001-of-00002.parquet"},
            {"type":"file","oid":"a3","size":5,"path":"cartoons/README.md"},
            {"type":"directory","oid":"a4","size":0,"path":"cartoons/more.parquet"},
            {"type":"file","oid":"a5","size":5,"path":"cartoons/sub/deep.parquet"},
            {"type":"file","oid":"a6","size":5,"path":"maps/train-00000-of-00001.parquet"}
        ]"#;
        let files = parse_tree(body, "cartoons").unwrap();
        assert_eq!(
            files,
            [
                DatasetFile {
                    name: "cartoons/train-00000-of-00002.parquet".into(),
                    sha256: Some("0f".into()),
                    size: Some(71_381_230),
                },
                DatasetFile {
                    name: "cartoons/train-00001-of-00002.parquet".into(),
                    sha256: None,
                    size: Some(900),
                },
            ]
        );
        assert!(parse_tree(b"{}", "cartoons").is_err());
    }
}
