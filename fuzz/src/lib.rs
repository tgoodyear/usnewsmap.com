#![forbid(unsafe_code)]

use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use peak_alloc::PeakAlloc;
use std::sync::LazyLock;
use usnm_alto::{AltoError, Limits};
use usnm_ocr_contract::message::decode_dead_letter;

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

pub const MAX_INPUT: usize = 1024 * 1024;
const MIB: usize = 1024 * 1024;

fn bounded<T>(budget: usize, f: impl FnOnce() -> T) -> T {
    let baseline = ALLOC.current_usage();
    ALLOC.reset_peak_usage();
    let result = f();
    assert!(
        ALLOC.peak_usage().saturating_sub(baseline) <= budget,
        "parser exceeded its additional live Rust-heap budget"
    );
    result
}

static PART: LazyLock<Bytes> =
    LazyLock::new(|| Bytes::from_static(include_bytes!("../corpus/read_node_part/valid.parquet")));

pub fn node_part(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    bounded(64 * MIB, || {
        if let Ok(rows) = usnm_overlay::read_node_part(Bytes::copy_from_slice(data)) {
            assert!(rows.len() <= usnm_overlay::MAX_NODE_PART_ROWS as usize);
        }
    });

    // Raw mutations usually fail at the footer. Also insert the input into a
    // real column chunk, retaining valid metadata, to exercise the capped
    // Compact Thrift page-header scan without decoding attacker-sized pages.
    bounded(64 * MIB, || {
        let builder = ParquetRecordBatchReaderBuilder::try_new(PART.clone()).unwrap();
        let column = builder.metadata().row_group(0).column(0);
        let start = column
            .dictionary_page_offset()
            .unwrap_or(column.data_page_offset()) as usize;
        let len = column.compressed_size() as usize;
        let mut file = PART.to_vec();
        let n = len.min(data.len());
        file[start..start + n].copy_from_slice(&data[..n]);
        let _ = usnm_overlay::check_uncompressed(
            &file,
            builder.metadata(),
            usnm_overlay::MAX_NODE_PART_UNCOMPRESSED,
        );
    });
}

pub fn alto(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    // The same XML is tested with production limits and tighter budgets so
    // mutations hit depth/element/attribute/byte rejection paths quickly.
    let tight = Limits {
        max_bytes: 4096,
        max_depth: 4,
        max_elements: 16,
        max_attributes: 4,
    };
    for limits in [Limits::default(), tight] {
        bounded(16 * MIB, || {
            let result = usnm_alto::check(data, &limits);
            if data.len() > limits.max_bytes {
                assert_eq!(result, Err(AltoError::TooLarge));
            }
            if let Ok(page) = result {
                assert!(page.width.is_finite() && page.width > 0.0);
                assert!(page.height.is_finite() && page.height > 0.0);
                assert!(page.elements <= limits.max_elements);
            }
        });
    }
}

pub fn dead_letter(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    if let Ok(text) = std::str::from_utf8(data) {
        bounded(4 * MIB, || {
            if let Ok(letter) = decode_dead_letter(text) {
                assert!(letter.validate().is_ok());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_seeds_and_size_rejections() {
        for (target, run) in [
            ("read_node_part", node_part as fn(&[u8])),
            ("alto_check", alto),
            ("decode_dead_letter", dead_letter),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("corpus")
                .join(target);
            for entry in std::fs::read_dir(path).unwrap() {
                run(&std::fs::read(entry.unwrap().path()).unwrap());
            }
            run(&vec![b'x'; MAX_INPUT]);
        }
        let valid = usnm_overlay::read_node_part(PART.clone()).unwrap();
        assert_eq!(valid.len(), 1);
        let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
        for name in ["too-many-rows.parquet", "decompression-limit.parquet"] {
            let data = std::fs::read(corpus.join("read_node_part").join(name)).unwrap();
            assert!(usnm_overlay::read_node_part(Bytes::from(data)).is_err());
        }
        for name in [
            "v1.json",
            "v1.base64",
            "v2-invalid.json",
            "v2-invalid.base64",
            "v2-permanent.json",
            "v2-permanent.base64",
            "v2-unroutable.json",
            "v2-unroutable.base64",
            "v2-dequeue_count.json",
            "v2-dequeue_count.base64",
        ] {
            let text =
                std::fs::read_to_string(corpus.join("decode_dead_letter").join(name)).unwrap();
            let letter = decode_dead_letter(&text).unwrap();
            assert_eq!(decode_dead_letter(&letter.encode()).unwrap(), letter);
        }
        for (name, version) in [
            ("v2", usnm_alto::AltoVersion::V2),
            ("v3", usnm_alto::AltoVersion::V3),
            ("v4", usnm_alto::AltoVersion::V4),
        ] {
            let xml = std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("corpus/alto_check/{name}.xml")),
            )
            .unwrap();
            assert_eq!(
                usnm_alto::check(&xml, &Limits::default()).unwrap().version,
                version
            );
        }
    }
}
