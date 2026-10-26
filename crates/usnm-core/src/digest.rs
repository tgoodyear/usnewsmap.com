//! Lowercase hex and sha256 helpers, shared so each crate does not carry its own copy.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// `bytes` as lowercase hex, two digits per byte.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The sha256 of `bytes`, as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// The sha256 of the file at `path`, as lowercase hex, read in 64 KiB chunks.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_pads_each_byte() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0a, 0xff]), "000aff");
    }

    #[test]
    fn sha256_of_abc() {
        let want = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(sha256_hex(b"abc"), want);
    }

    #[test]
    fn sha256_file_matches_sha256_hex_across_buffer_sizes() {
        let dir = std::env::temp_dir().join(format!("usnm-core-digest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for len in [0usize, 1, 65_535, 65_536, 65_537, 200_000] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let p = dir.join(format!("f{len}"));
            std::fs::write(&p, &data).unwrap();
            assert_eq!(sha256_file(&p).unwrap(), sha256_hex(&data), "{len}");
        }
        assert!(sha256_file(&dir.join("missing")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
