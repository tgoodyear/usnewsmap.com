//! HMAC-SHA-256 (RFC 2104), written out so the cloud adapters need no `hmac` crate. The tests
//! pin it to the RFC 4231 vectors. Shared by the validation webhook (`validatord`) and the
//! S3-style request signer (`sigv4`).
use sha2::{Digest, Sha256};

/// The MAC of the concatenated `parts`.
pub(super) fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// [`hmac_sha256`] in lowercase hex.
// Only the validation webhook (a feature) and the tests use the hex form.
#[cfg_attr(not(any(test, feature = "cloud-validatord")), allow(dead_code))]
pub(super) fn hmac_sha256_hex(key: &[u8], parts: &[&[u8]]) -> String {
    hex(&hmac_sha256(key, parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_matches_the_rfc_4231_vectors() {
        let hex = |parts: &[&[u8]], key: &[u8]| hmac_sha256_hex(key, parts);
        assert_eq!(
            hex(&[b"Hi ", b"There"], &[0x0b; 20]),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Test case 2: a short key and a message in two parts.
        assert_eq!(
            hex(&[b"what do ya want ", b"for nothing?"], b"Jefe"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // A key longer than the block is hashed first (test case 6).
        assert_eq!(
            hex(
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"],
                &[0xaa; 131]
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }
}
