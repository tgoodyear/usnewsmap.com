//! Regenerate the synthetic corpus; run from fuzz/ with cargo run --example seed.
use std::path::Path;
use usnm_ocr_contract::message::{Cause, DeadLetter, OcrMessage, Reason};
use usnm_overlay::{write_part, OverlayRow};

fn seed(target: &str, name: &str, data: impl AsRef<[u8]>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus")
        .join(target);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), data).unwrap();
}

fn main() {
    // usnm-overlay's reads_a_node_part_strictly / page_headers_are_read_without_decoding.
    let row = OverlayRow {
        key: "sn83045462/1890-05-03/ed-2/seq-7".parse().unwrap(),
        batch: "batch_dlc_test_ver01".into(),
        ocr_source: "usnm-tesseract".into(),
        ocr_engine: "tesseract-5.5.1/deu_latf+deu".into(),
        loc_text: "bulk".into(),
        text: "Die Zeitung erscheint jeden Sonnabend in Lincoln.".into(),
        image_url: "https://tile.loc.gov/image-services/iiif/service:ndnp:x".into(),
        ocred_at: chrono::DateTime::from_timestamp(1_791_000_000, 123_000).unwrap(),
    };
    seed(
        "read_node_part",
        "valid.parquet",
        write_part(std::slice::from_ref(&row)).unwrap(),
    );
    seed(
        "read_node_part",
        "too-many-rows.parquet",
        write_part(&vec![row.clone(); 17]).unwrap(),
    );
    let mut big = row;
    big.text = "x".repeat(5 * 1024 * 1024);
    seed(
        "read_node_part",
        "decompression-limit.parquet",
        write_part(&[big]).unwrap(),
    );
    seed(
        "read_node_part",
        "negative-size.header",
        [0x15, 0, 0x15, 1, 0x15, 0, 0],
    );
    let mut header = vec![0x15, 0, 0x15];
    let mut v = 200_000_000u64;
    while v >= 0x80 {
        header.push(v as u8 | 0x80);
        v >>= 7;
    }
    header.push(v as u8);
    header.extend([0x15, 8, 0, 1, 2, 3, 4]);
    seed("read_node_part", "oversized-page.header", header);
    seed(
        "read_node_part",
        "huge-map.header",
        [
            0x15, 0, 0x15, 8, 0x15, 8, 0xfb, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x3f,
            0x11, 1, 1, 0,
        ],
    );

    // usnm-alto's V3 fixture, also used with the v2/v4 namespace.
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#" xmlns:xlink="http://www.w3.org/1999/xlink">
  <Description><MeasurementUnit>pixel</MeasurementUnit>
    <sourceImageInformation><fileName>https://tile.loc.gov/a?b&amp;c</fileName></sourceImageInformation>
  </Description>
  <Layout><Page ID="p1" WIDTH="5000" HEIGHT="7000" PHYSICAL_IMG_NR="1">
    <PrintSpace><TextBlock ID="b1"><TextLine ID="l1">
      <String CONTENT="Zeitung" HPOS="1" VPOS="2" WIDTH="3" HEIGHT="4"/><SP/>
      <String CONTENT="&#228;&lt;" />
    </TextLine></TextBlock></PrintSpace>
  </Page></Layout>
</alto>
"#;
    for version in [2, 3, 4] {
        seed(
            "alto_check",
            &format!("v{version}.xml"),
            xml.replace("ns-v3#", &format!("ns-v{version}#")),
        );
    }
    seed(
        "alto_check",
        "doctype.xml",
        xml.replace(
            "<alto ",
            "<!DOCTYPE alto [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;\">]>\n<alto ",
        ),
    );
    seed(
        "alto_check",
        "undeclared-entity.xml",
        xml.replace("Zeitung", "&b;"),
    );
    seed(
        "alto_check",
        "malformed.xml",
        xml.replace("</TextLine>", ""),
    );
    seed(
        "alto_check",
        "prefixed.xml",
        r#"<a:alto xmlns:a="http://www.loc.gov/standards/alto/ns-v4#"><a:Layout><a:Page WIDTH="1.5" HEIGHT="2"/></a:Layout></a:alto>"#,
    );

    // usnm-ocr-contract message::tests::{sample,dead}.
    let message = OcrMessage {
        v: 1,
        page: "sn83045462/1890-05-03/ed-1/seq-7".into(),
        batch: "nbu_gage".into(),
        language: "deu".into(),
        reason: Reason::Bulk,
        score: Some(0.62),
        queued_at: "2026-10-10T12:00:00Z".into(),
    };
    for cause in [
        Cause::Invalid,
        Cause::Permanent,
        Cause::Unroutable,
        Cause::DequeueCount,
    ] {
        let mut letter = DeadLetter::new(
            "ocr-bulk",
            "m1",
            (cause != Cause::Invalid).then_some(&message),
            cause,
            (cause == Cause::Invalid).then_some("bad_json"),
            (cause == Cause::Unroutable).then(|| vec!["tesseract-5.5".into()]),
            "x\ny",
            6,
            "node1",
        );
        letter.at = "2026-10-10T12:00:00Z".into();
        letter.validate().unwrap();
        seed(
            "decode_dead_letter",
            &format!("v2-{}.json", cause.as_str()),
            serde_json::to_vec(&letter).unwrap(),
        );
        seed(
            "decode_dead_letter",
            &format!("v2-{}.base64", cause.as_str()),
            letter.encode(),
        );
        if cause == Cause::Permanent {
            letter.v = 1;
            letter.cause = None;
            seed(
                "decode_dead_letter",
                "v1.json",
                serde_json::to_vec(&letter).unwrap(),
            );
            seed("decode_dead_letter", "v1.base64", letter.encode());
        }
    }
    seed("decode_dead_letter", "bad-json.txt", b"not base64!");
    seed(
        "decode_dead_letter",
        "unknown-field.json",
        br#"{"v":2,"q":"synthetic"}"#,
    );
}
