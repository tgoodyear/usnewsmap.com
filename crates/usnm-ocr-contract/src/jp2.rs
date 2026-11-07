//! Whether a JP2 is cut short at its source (#1457), from its box, marker
//! and tile-part headers only, with no decoding.
//!
//! Some of LoC's JP2s stop before their codestream's end, and their bytes
//! are the bag's (they match its md5), so no re-fetch repairs them. A
//! decoder fails on one, or reads part of the page. [`truncation`] says why
//! a JP2 is cut short, so a scan fetcher, an exporter or a worker can keep
//! it from OCR instead.
//!
//! This is the quality feed's check (`jp2_truncation` in
//! `boinc/scripts/onprem/quality-feed/quality_feed.py`, #1551), with the
//! same reasons word for word. `fixtures/jp2-truncation-vectors.json`, which
//! that feed's tests write, holds the cases both must agree on.

/// The JP2 signature box: every JP2 file starts with it.
const SIGNATURE: &[u8; 12] = b"\x00\x00\x00\x0cjP  \r\n\x87\n";
const SOC: u16 = 0xFF4F;
const SIZ: u16 = 0xFF51;
const SOT: u16 = 0xFF90;
const EOC: u16 = 0xFFD9;

fn u16_at(raw: &[u8], pos: usize) -> u16 {
    u16::from_be_bytes([raw[pos], raw[pos + 1]])
}

fn u32_at(raw: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes(raw[pos..pos + 4].try_into().expect("four bytes"))
}

fn u64_at(raw: &[u8], pos: usize) -> u64 {
    u64::from_be_bytes(raw[pos..pos + 8].try_into().expect("eight bytes"))
}

/// A box type as Python's `repr` of its Latin-1 text prints it, quotes
/// included, so the reasons match the feed's.
fn py_repr(kind: &[u8]) -> String {
    let quote = if kind.contains(&b'\'') && !kind.contains(&b'"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for &b in kind {
        let c = char::from(b);
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // Python prints C0 and C1 controls, DEL, the no-break space and
            // the soft hyphen as escapes, and any other Latin-1 letter as is.
            _ if b < 0x20 || (0x7F..=0xA0).contains(&b) || b == 0xAD => {
                out.push_str(&format!("\\x{b:02x}"));
            }
            _ => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Where a JP2's codestream is: its start and end, or why the file ends
/// before it does. `None` when this isn't a JP2 the check can judge.
fn codestream(raw: &[u8]) -> Option<Result<(usize, usize), String>> {
    if raw.starts_with(b"\xff\x4f\xff\x51") {
        return Some(Ok((0, raw.len())));
    }
    if !raw.starts_with(SIGNATURE) {
        return None;
    }
    let len = raw.len();
    let mut pos = 0usize;
    while pos < len {
        if pos + 8 > len {
            return Some(Err(format!(
                "the file ends inside a box header at byte {pos}"
            )));
        }
        let mut length = u128::from(u32_at(raw, pos));
        let kind = &raw[pos + 4..pos + 8];
        let mut header = 8u128;
        if length == 1 {
            if pos + 16 > len {
                return Some(Err(format!(
                    "the file ends inside a box header at byte {pos}"
                )));
            }
            length = u128::from(u64_at(raw, pos + 8));
            header = 16;
        } else if length == 0 {
            length = (len - pos) as u128;
        }
        if length < header {
            return None;
        }
        let box_end = pos as u128 + length;
        if box_end > len as u128 {
            return Some(Err(format!(
                "the {} box declares {} bytes past the end of the file",
                py_repr(kind),
                box_end - len as u128
            )));
        }
        // Within the file, so it fits.
        let (box_end, header) = (box_end as usize, header as usize);
        if kind == b"jp2c" {
            return Some(Ok((pos + header, box_end)));
        }
        pos = box_end;
    }
    Some(Err("the file ends before its codestream box".into()))
}

/// Why a JP2's codestream is cut short: a box or marker segment that runs
/// past the end of the data, or a codestream that stops without its EOC
/// marker. `None` when it reaches EOC, or when it isn't a JP2 or its
/// structure is malformed in another way, which the decoder judges as
/// before. A bare codestream (no JP2 boxes) is judged too.
pub fn truncation(raw: &[u8]) -> Option<String> {
    let (start, end) = match codestream(raw)? {
        Ok(found) => found,
        Err(reason) => return Some(reason),
    };
    if end - start < 2 {
        return Some("the codestream ends before its SOC marker".into());
    }
    if u16_at(raw, start) != SOC {
        return None;
    }
    let mut pos = start + 2;
    let mut tiles: Option<u128> = None;
    // The main header, up to the first tile-part.
    loop {
        if pos + 4 > end {
            return Some("the codestream ends inside its main header".into());
        }
        let (marker, length) = (u16_at(raw, pos), usize::from(u16_at(raw, pos + 2)));
        if marker == SOT {
            break;
        }
        if marker >> 8 != 0xFF || length < 2 {
            return None;
        }
        if pos + 2 + length > end {
            return Some(format!(
                "main-header marker 0x{marker:04X} runs past the end of the codestream"
            ));
        }
        if marker == SIZ && length >= 38 {
            let field = |i: usize| u128::from(u32_at(raw, pos + 6 + 4 * i));
            let (x, y, xt, yt, xt0, yt0) =
                (field(0), field(1), field(4), field(5), field(6), field(7));
            if xt != 0 && yt != 0 && x > xt0 && y > yt0 {
                tiles = Some((x - xt0).div_ceil(xt) * (y - yt0).div_ceil(yt));
            }
        }
        pos += 2 + length;
    }
    let mut seen = std::collections::BTreeSet::new();
    // SOT to SOT by each tile-part's declared length (Psot).
    loop {
        if pos + 12 > end {
            return Some("the codestream ends inside a tile-part header".into());
        }
        let (marker, lsot, tile) = (u16_at(raw, pos), u16_at(raw, pos + 2), u16_at(raw, pos + 4));
        let (psot, part) = (u64::from(u32_at(raw, pos + 6)), raw[pos + 10]);
        if marker != SOT || lsot != 10 {
            return None;
        }
        seen.insert(tile);
        if psot == 0 {
            // The last tile-part, which runs to EOC.
            if end - pos < 14 || u16_at(raw, end - 2) != EOC {
                return Some(format!(
                    "tile {tile}'s last tile-part runs to the end of the codestream with no EOC marker"
                ));
            }
            return None;
        }
        if psot < 14 {
            return None;
        }
        let part_end = pos as u64 + psot;
        if part_end > end as u64 {
            return Some(format!(
                "tile-part {part} of tile {tile} declares {} bytes past the end of the codestream",
                part_end - end as u64
            ));
        }
        // Within the codestream, so it fits.
        pos = part_end as usize;
        if pos + 2 > end {
            let of = tiles
                .filter(|&n| n != 0)
                .map(|n| format!(" of {n}"))
                .unwrap_or_default();
            return Some(format!(
                "the codestream ends without an EOC marker after {}{of} tiles",
                seen.len()
            ));
        }
        match u16_at(raw, pos) {
            SOT => {}
            // EOC: complete. Anything else is for the decoder to judge.
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct Vector {
        name: String,
        hex: String,
        reason: Option<String>,
    }

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    #[test]
    fn agrees_with_the_quality_feed_on_every_shared_vector() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/jp2-truncation-vectors.json");
        let text = std::fs::read_to_string(path).expect("fixtures/jp2-truncation-vectors.json");
        let vectors: Vec<Vector> = serde_json::from_str(&text).expect("vectors");
        assert!(vectors.iter().any(|v| v.reason.is_none()));
        assert!(vectors.iter().any(|v| v.reason.is_some()));
        for v in vectors {
            assert_eq!(truncation(&bytes(&v.hex)), v.reason, "{}", v.name);
        }
    }
}
