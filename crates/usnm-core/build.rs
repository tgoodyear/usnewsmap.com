//! Decompresses the word lists under `data/`.
//!
//! The lists are committed as `<name>.txt.zst` (`data/README.md`). Each
//! `data/**/*.zst` is written, without the `.zst`, to the same relative path
//! under `$OUT_DIR/data/`, where the crate's `include_str!`s read it. The
//! damage lists (`data/damage-words/`) are only needed by the `damage`
//! feature, so they are left compressed without it.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=data");
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("data");
    let damage = env::var_os("CARGO_FEATURE_DAMAGE").is_some();
    decompress_dir(&manifest.join("data"), &out, damage, true).expect("decompressing data/");
}

fn decompress_dir(src: &Path, dst: &Path, damage: bool, top: bool) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    let mut entries = fs::read_dir(src)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if top && name == "damage-words" && !damage {
                continue;
            }
            decompress_dir(&path, &dst.join(&*name), damage, false)?;
        } else if let Some(stem) = name.strip_suffix(".zst") {
            let target = dst.join(stem);
            let mut input = fs::File::open(&path)?;
            let mut output = fs::File::create(&target)?;
            zstd::stream::copy_decode(&mut input, &mut output)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
        }
    }
    Ok(())
}
