//! Where the dataset's Parquet files are read from: a directory, or the
//! Hugging Face Hub at a pinned revision (#1102).

use std::fs::File;
use std::io::{Read, Seek};
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
    /// `scratch`, each under a name of its own (so runs can share the
    /// directory) and deleted once it has been read.
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
    /// Where the file is, for messages. Read `file`, not this: the path can
    /// name another file by the time it is opened again.
    pub path: PathBuf,
    /// The file as it was opened to be hashed, at its start: `bytes` and
    /// `sha256` describe what is read through this handle and nothing else.
    pub file: File,
    pub bytes: u64,
    pub sha256: String,
    /// A download's own file in the scratch directory, deleted on drop.
    _download: Option<tempfile::TempPath>,
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

    /// A file on local disk, opened, with its size and sha256: the file itself
    /// for a directory, a download checked against the Hub's own for the Hub.
    /// The hash is taken from the handle that is returned, so what is read is
    /// what was checked.
    pub async fn get(&self, revision: &str, file: &DatasetFile) -> anyhow::Result<Local> {
        match self {
            Source::Dir(dir) => {
                let path = dir.join(&file.name);
                let (handle, bytes, sha256) = open_and_digest(path.clone()).await?;
                Ok(Local {
                    path,
                    file: handle,
                    bytes,
                    sha256,
                    _download: None,
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
                // A file of this download's own, made exclusively: named for
                // the revision and the dataset file, with a random part, so
                // that runs sharing the directory (the default work
                // directory) never write, read or delete each other's. It is
                // deleted when `download` is dropped, whatever happens next.
                let download = tempfile::Builder::new()
                    .prefix(&format!("{revision}-{}-", file.name.replace('/', "__")))
                    .suffix(".parquet")
                    .tempfile_in(scratch)
                    .with_context(|| format!("creating a file in {}", scratch.display()))?
                    .into_temp_path();
                let streamed = fetch(&url, &download).await?;
                let (handle, bytes, sha256) = open_and_digest(download.to_path_buf()).await?;
                // The stream's hash is of what the Hub sent, the handle's of
                // what is on disk to be read: both must be the same file.
                if streamed != sha256 {
                    bail!(
                        "`{}` was downloaded with sha256 {streamed} but reads back as {sha256}: \
                         the file in {} changed",
                        file.name,
                        scratch.display()
                    );
                }
                if let Some(want) = file.sha256.as_deref() {
                    if want != sha256 {
                        bail!(
                            "`{}` downloaded with sha256 {sha256}, but the Hub lists {want}",
                            file.name
                        );
                    }
                }
                if let Some(want) = file.size {
                    if want != bytes {
                        bail!(
                            "`{}` downloaded as {bytes} bytes, but the Hub lists {want}",
                            file.name
                        );
                    }
                }
                Ok(Local {
                    path: download.to_path_buf(),
                    file: handle,
                    bytes,
                    sha256,
                    _download: Some(download),
                })
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

/// `{dir}/*.parquet`, named `{config}/{file}`. A link to a file counts as the
/// file (a Hugging Face cache snapshot is made of them); a directory or a
/// special file does not, and a link to nothing is an error, not a shard that
/// is quietly left out.
fn list_dir(dir: &Path, config: &str) -> anyhow::Result<Vec<DatasetFile>> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("listing {}", dir.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.ends_with(".parquet") {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::metadata(&path)
            .with_context(|| format!("{} (a link to a file that is not there?)", path.display()))?;
        if meta.is_file() {
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

/// Open a file, hash what is read through the handle, and hand the handle
/// back at its start: the hash is of the file that is then read, whatever
/// happens to its path.
async fn open_and_digest(path: PathBuf) -> anyhow::Result<(File, u64, String)> {
    tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path).with_context(|| format!("opening {}", path.display()))?;
        let (bytes, sha256) =
            digest(&mut file).with_context(|| format!("hashing {}", path.display()))?;
        file.rewind()?;
        Ok((file, bytes, sha256))
    })
    .await
    .context("hashing a dataset file stopped")?
}

/// The size and sha256 of what `reader` holds from where it is.
fn digest(reader: &mut impl Read) -> anyhow::Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
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

    /// A Hugging Face cache snapshot is a directory of links to blobs: the
    /// links are the shards. Directories and special files still are not, and
    /// a link to nothing is an error rather than a shard left out.
    #[cfg(unix)]
    #[test]
    fn a_link_to_a_file_is_a_shard() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let blobs = dir.path().join("blobs");
        let config = dir.path().join("snapshots/rev/cartoons");
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        for (blob, name) in [
            ("b1", "train-00000-of-00002.parquet"),
            ("b2", "train-00001-of-00002.parquet"),
        ] {
            std::fs::write(blobs.join(blob), b"x").unwrap();
            symlink(blobs.join(blob), config.join(name)).unwrap();
        }
        // A regular file beside the links, a link that is not Parquet, a link
        // to a directory and a directory, both named like shards.
        std::fs::write(config.join("train-00002-of-00002.parquet"), b"x").unwrap();
        symlink(blobs.join("b1"), config.join("README.md")).unwrap();
        symlink(&blobs, config.join("dir-link.parquet")).unwrap();
        std::fs::create_dir(config.join("dir.parquet")).unwrap();

        let mut names: Vec<String> = list_dir(&config, "cartoons")
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "cartoons/train-00000-of-00002.parquet",
                "cartoons/train-00001-of-00002.parquet",
                "cartoons/train-00002-of-00002.parquet",
            ]
        );

        // A config of nothing but links is a config, not an empty one.
        let only_links = dir.path().join("snapshots/rev/maps");
        std::fs::create_dir_all(&only_links).unwrap();
        symlink(
            blobs.join("b2"),
            only_links.join("train-00000-of-00001.parquet"),
        )
        .unwrap();
        assert_eq!(list_dir(&only_links, "maps").unwrap().len(), 1);

        // A link to a blob that is not there.
        symlink(
            blobs.join("gone"),
            config.join("train-00003-of-00002.parquet"),
        )
        .unwrap();
        let err = format!("{:#}", list_dir(&config, "cartoons").unwrap_err());
        assert!(err.contains("train-00003-of-00002.parquet"), "{err}");
    }
}
