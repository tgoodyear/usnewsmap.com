//! `usnm-ingest pictures` (#1102): Newspaper Navigator's boxes into the curated
//! store, from synthetic files shaped like the dataset's per-kind configs. The
//! rows are made up; the shapes are the ones seen in the real cartoons config
//! (a missing frame, a reel of `print` or `no/reel`, a five-digit frame, an
//! upper-case LCCN, a `lccn` column of `no`). The files are Snappy-compressed
//! where the real ones are, which the reader needs the `snap` feature of
//! `parquet` for.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arrow_array::builder::{Decimal128Builder, Float64Builder, ListBuilder, StringBuilder};
use arrow_array::{ArrayRef, Float32Array, Int32Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::DataType;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use parquet::file::reader::{FileReader, SerializedFileReader};
use sha2::{Digest, Sha256};
use usnm_ingest::pictures::source::Source;
use usnm_ingest::pictures::{
    self, attempt_dir, finished, read_part, Options, Picture, Quality, PART_ROWS, PROVENANCE,
    SCHEMA_VERSION,
};
use usnm_store::{LocalStore, ObjectStore, StoreError};

use crate::common::sha256_file;

const REVISION: &str = "e2e585824cdd28d52755c481dca69ed0bfa992a1";
const BATCHES: &str = "https://chroniclingamerica.loc.gov/data/batches";
const OLD_IIIF: &str = "https://chroniclingamerica.loc.gov/iiif/2";

/// The pair LoC's 308 was checked on.
const OHI_OLD: &str = "https://chroniclingamerica.loc.gov/iiif/2/ohi_ingstad_ver01%2Fdata%2Fsn85026051%2F00296027029%2F1850072701%2F0037.jp2/pct:29.91,62.29,13.14,10.85/pct:100/0/default.jpg";
const OHI_NEW: &str = "https://tile.loc.gov/image-services/iiif/service:ndnp:ohi:batch_ohi_ingstad_ver01:data:sn85026051:00296027029:1850072701:0037/pct:29.91,62.29,13.14,10.85/pct:100/0/default.jpg";

fn at() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap()
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// One row of a source file.
#[derive(Clone)]
struct Src {
    lccn: String,
    pub_date: String,
    edition: i32,
    page_seq: Option<i64>,
    batch: String,
    boxed: Vec<f64>,
    score: Option<f64>,
    ocr: Vec<String>,
    page_url: Option<String>,
    iiif: Option<String>,
    predicted: String,
}

/// A row on a scan in the standard shape, `reel` as the path has it (`print`,
/// `no/reel`), `issue` as `YYYYMMDDEE`.
fn scan(batch: &str, lccn: &str, reel: &str, issue: &str, frame: &str) -> Src {
    Src {
        lccn: lccn.to_owned(),
        pub_date: format!("{}-{}-{}", &issue[..4], &issue[4..6], &issue[6..8]),
        edition: issue[8..].parse().unwrap(),
        page_seq: Some(frame.parse().unwrap()),
        batch: batch.to_owned(),
        boxed: vec![0.1, 0.2, 0.4, 0.6],
        score: Some(0.9),
        ocr: vec!["A".to_owned(), "caption".to_owned()],
        page_url: Some(format!(
            "{BATCHES}/{batch}/data/{lccn}/{reel}/{issue}/{frame}.jp2"
        )),
        iiif: Some(format!(
            "{OLD_IIIF}/{batch}%2Fdata%2F{lccn}%2F{}%2F{issue}%2F{frame}.jp2/pct:10,20,30,40/pct:100/0/default.jpg",
            reel.replace('/', "%2F")
        )),
        predicted: "synthetic".to_owned(),
    }
}

/// How a source file lays its numbers out.
#[derive(Clone, Copy)]
struct Shape {
    /// `Some((precision, scale))`: `box` as fixed-point decimals, as the
    /// per-kind configs have it; `None`: doubles, as the combined config.
    decimal: Option<(u8, i8)>,
    score_f32: bool,
    compression: Compression,
}

const SNAPPY_DECIMAL: Shape = Shape {
    decimal: Some((21, 20)),
    score_f32: false,
    compression: Compression::SNAPPY,
};

fn zstd() -> Compression {
    Compression::ZSTD(ZstdLevel::default())
}

fn encode(rows: &[Src], shape: Shape) -> Vec<u8> {
    let strings = |f: &dyn Fn(&Src) -> Option<&str>| {
        Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) as ArrayRef
    };
    let mut columns: Vec<(String, ArrayRef)> = Vec::new();
    let boxed: ArrayRef = match shape.decimal {
        Some((precision, scale)) => {
            let mut b = ListBuilder::new(
                Decimal128Builder::new()
                    .with_precision_and_scale(precision, scale)
                    .unwrap(),
            );
            for r in rows {
                for x in &r.boxed {
                    b.values()
                        .append_value((x * 10f64.powi(i32::from(scale))).round() as i128);
                }
                b.append(true);
            }
            Arc::new(b.finish())
        }
        None => {
            let mut b = ListBuilder::new(Float64Builder::new());
            for r in rows {
                b.values().append_slice(&r.boxed);
                b.append(true);
            }
            Arc::new(b.finish())
        }
    };
    let score: ArrayRef = if shape.score_f32 {
        Arc::new(Float32Array::from(
            rows.iter()
                .map(|r| r.score.map(|s| s as f32))
                .collect::<Vec<_>>(),
        ))
    } else {
        Arc::new(arrow_array::Float64Array::from(
            rows.iter().map(|r| r.score).collect::<Vec<_>>(),
        ))
    };
    let mut ocr = ListBuilder::new(StringBuilder::new());
    for r in rows {
        for w in &r.ocr {
            ocr.values().append_value(w);
        }
        ocr.append(true);
    }
    columns.extend([
        ("lccn".to_owned(), strings(&|r| Some(r.lccn.as_str()))),
        (
            "pub_date".to_owned(),
            strings(&|r| Some(r.pub_date.as_str())),
        ),
        (
            "edition_seq_num".to_owned(),
            Arc::new(Int32Array::from(
                rows.iter().map(|r| r.edition).collect::<Vec<_>>(),
            )) as ArrayRef,
        ),
        (
            "page_seq_num".to_owned(),
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r.page_seq).collect::<Vec<_>>(),
            )),
        ),
        ("batch".to_owned(), strings(&|r| Some(r.batch.as_str()))),
        ("box".to_owned(), boxed),
        ("score".to_owned(), score),
        ("ocr".to_owned(), Arc::new(ocr.finish())),
        (
            "place_of_publication".to_owned(),
            strings(&|_| Some("Nowhere")),
        ),
        ("page_url".to_owned(), strings(&|r| r.page_url.as_deref())),
        (
            "prediction_section_iiif_url".to_owned(),
            strings(&|r| r.iiif.as_deref()),
        ),
        (
            "predicted_type".to_owned(),
            strings(&|r| Some(r.predicted.as_str())),
        ),
    ]);
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let props = WriterProperties::builder()
        .set_compression(shape.compression)
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), batch.schema(), Some(props)).unwrap();
    w.write(&batch).unwrap();
    w.into_inner().unwrap()
}

/// `dir/{config}/{name}`.
fn put_file(dir: &Path, config: &str, name: &str, rows: &[Src], shape: Shape) {
    std::fs::create_dir_all(dir.join(config)).unwrap();
    std::fs::write(dir.join(config).join(name), encode(rows, shape)).unwrap();
}

/// The shapes of the real cartoons config, and a few more.
fn cartoon_rows() -> Vec<Src> {
    let ohi = scan(
        "ohi_ingstad_ver01",
        "sn85026051",
        "00296027029",
        "1850072701",
        "0037",
    );
    let ohi = Src {
        boxed: vec![0.2991, 0.6229, 0.4305, 0.7314],
        score: Some(0.9731),
        ocr: vec!["Mr.".to_owned(), "Ingstad".to_owned()],
        iiif: Some(OHI_OLD.to_owned()),
        ..ohi
    };

    // `wa_bittern_ver01` and `wa_auklet_ver01`: the `lccn` column is `no`, the
    // reel is `no/reel`, and the crop URL's image id ends at the issue.
    let frameless = |batch: &str, issue: &str, frame: &str| {
        let s = scan(batch, "sn84022793", "no/reel", issue, frame);
        Src {
            lccn: "no".to_owned(),
            iiif: Some(format!(
                "{OLD_IIIF}/{batch}%2Fdata%2Fsn84022793%2Fno%2Freel%2F{issue}.jp2/pct:10,20,30,40/pct:100/0/default.jpg"
            )),
            ..s
        }
    };
    // `mdu_bode_ver03`: reel `print`, five digits.
    let bode = scan(
        "mdu_bode_ver03",
        "sn83045462",
        "print",
        "1905063001",
        "10283",
    );
    // `vi_darwin_ver01`: an upper-case LCCN.
    let darwin = scan(
        "vi_darwin_ver01",
        "SN85038614",
        "00296027029",
        "1850072701",
        "0037",
    );
    // A page URL that is no scan, with a crop URL that is.
    let not_a_scan = Src {
        page_url: Some("https://example.test/not-a-scan".to_owned()),
        ..scan(
            "ohi_ingstad_ver01",
            "sn85026051",
            "00296027029",
            "1850072701",
            "0041",
        )
    };
    // A crop of another frame than the page URL's.
    let other_frame = Src {
        iiif: scan(
            "ohi_ingstad_ver01",
            "sn85026051",
            "00296027029",
            "1850072701",
            "0037",
        )
        .iiif,
        ..scan(
            "ohi_ingstad_ver01",
            "sn85026051",
            "00296027029",
            "1850072701",
            "0038",
        )
    };
    // A `page_seq_num` that is not the frame.
    let wrong_seq = Src {
        page_seq: Some(41),
        ..scan(
            "ohi_ingstad_ver01",
            "sn85026051",
            "00296027029",
            "1850072701",
            "0040",
        )
    };
    // Nothing usable.
    let empty = Src {
        lccn: String::new(),
        batch: String::new(),
        boxed: vec![0.1, 0.2, 0.3],
        score: None,
        ocr: vec![],
        page_url: None,
        iiif: None,
        page_seq: None,
        ..scan(
            "x_y_ver01",
            "sn85026051",
            "00296027029",
            "1850072701",
            "0039",
        )
    };
    vec![
        ohi,
        frameless("wa_bittern_ver01", "1873100901", "0368"),
        frameless("wa_auklet_ver01", "1873101001", "0369"),
        bode,
        darwin,
        not_a_scan,
        other_frame,
        wrong_seq,
        empty,
    ]
}

/// A dataset directory: the cartoons as above in a Snappy file, and a few
/// plain rows for each other kind, in the number layouts the other configs
/// have and with comics split over two files.
fn dataset(dir: &Path) {
    put_file(
        dir,
        "cartoons",
        "train-00000-of-00001.parquet",
        &cartoon_rows(),
        SNAPPY_DECIMAL,
    );
    let plain = |frame: &str| {
        scan(
            "dlc_abc_ver01",
            "sn83045462",
            "00280654021",
            "1890050301",
            frame,
        )
    };
    let comics = Shape {
        decimal: None,
        score_f32: true,
        compression: Compression::UNCOMPRESSED,
    };
    put_file(
        dir,
        "comics",
        "train-00000-of-00002.parquet",
        &[plain("0001"), plain("0002")],
        comics,
    );
    put_file(
        dir,
        "comics",
        "train-00001-of-00002.parquet",
        &[plain("0003")],
        comics,
    );
    put_file(
        dir,
        "illustrations",
        "train-00000-of-00001.parquet",
        &[plain("0004")],
        Shape {
            decimal: Some((22, 21)),
            score_f32: false,
            compression: zstd(),
        },
    );
    put_file(
        dir,
        "maps",
        "train-00000-of-00001.parquet",
        &[plain("0005")],
        Shape {
            decimal: Some((23, 22)),
            score_f32: false,
            compression: Compression::SNAPPY,
        },
    );
    put_file(
        dir,
        "photos",
        "train-00000-of-00001.parquet",
        &[plain("0006"), plain("0007")],
        Shape {
            decimal: None,
            score_f32: false,
            compression: Compression::SNAPPY,
        },
    );
}

fn options(source: Source) -> Options {
    Options {
        revision: REVISION.to_owned(),
        source,
        part_rows: PART_ROWS,
        // The cartoons fixture is mostly odd shapes.
        max_unplaced_pct: 100.0,
        commit: None,
    }
}

/// A store that records the order of its writes.
#[derive(Debug)]
struct Recording {
    inner: LocalStore,
    puts: Mutex<Vec<String>>,
}

impl Recording {
    fn new(root: &Path) -> Self {
        Self {
            inner: LocalStore::new(root),
            puts: Mutex::default(),
        }
    }
}

#[async_trait]
impl ObjectStore for Recording {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(path).await
    }
    async fn put_new(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<bool, StoreError> {
        self.puts.lock().unwrap().push(path.to_owned());
        self.inner.put_new(path, body, ct).await
    }
    async fn put(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<(), StoreError> {
        self.inner.put(path, body, ct).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.inner.list(prefix).await
    }
}

async fn part(store: &dyn ObjectStore, path: &str) -> Vec<Picture> {
    read_part(Bytes::from(store.get(path).await.unwrap().unwrap())).unwrap()
}

fn close(a: [f64; 4], b: [f64; 4]) {
    for (x, y) in a.iter().zip(&b) {
        assert!((x - y).abs() < 1e-12, "{a:?} != {b:?}");
    }
}

/// The five configs become a table per kind keyed by the scan, with the crop
/// URL on tile.loc.gov, every input hashed in the provenance, and the
/// provenance written after every part.
#[tokio::test]
async fn writes_a_table_per_kind_and_marks_the_attempt_finished_last() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let store = Recording::new(&dir.path().join("curated"));

    let report = pictures::run(&store, &options(Source::Dir(source.clone())), at())
        .await
        .unwrap();
    assert!(!report.already_written);
    assert_eq!(
        (report.revision.as_str(), report.attempt, report.rows),
        (REVISION, 1, 16)
    );
    assert_eq!(
        report
            .rows_by_kind
            .iter()
            .map(|(k, n)| (k.as_str(), *n))
            .collect::<Vec<_>>(),
        [
            ("cartoon", 9),
            ("comic", 3),
            ("illustration", 1),
            ("map", 1),
            ("photo", 2)
        ]
    );

    // The marker is the last write, and every part is under the attempt.
    let dir1 = attempt_dir(REVISION, 1);
    assert_eq!(
        dir1,
        format!("pictures/newspaper-navigator/{REVISION}/0001")
    );
    let puts = store.puts.lock().unwrap().clone();
    assert_eq!(puts.last().unwrap(), &format!("{dir1}/{PROVENANCE}"));
    assert_eq!(puts.len(), 7, "six parts and the marker: {puts:?}");
    assert!(puts.iter().all(|p| p.starts_with(&dir1)));

    let p = finished(&store, REVISION).await.unwrap().unwrap();
    assert_eq!(report.provenance, format!("{dir1}/{PROVENANCE}"));
    assert_eq!(
        (
            p.schema,
            p.source.as_str(),
            p.license.as_str(),
            p.revision.as_str(),
            p.attempt,
            p.rows
        ),
        (
            SCHEMA_VERSION,
            "biglam/newspaper-navigator",
            "cc0-1.0",
            REVISION,
            1,
            16
        )
    );
    assert_eq!(p.written_at, at());
    assert_eq!(p.commit, None);
    assert_eq!(
        p.kinds.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cartoon", "comic", "illustration", "map", "photo"]
    );

    // Every input file, in the order read, with its own sha256.
    let names: Vec<&str> = p.inputs.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(
        names,
        [
            "cartoons/train-00000-of-00001.parquet",
            "comics/train-00000-of-00002.parquet",
            "comics/train-00001-of-00002.parquet",
            "illustrations/train-00000-of-00001.parquet",
            "maps/train-00000-of-00001.parquet",
            "photos/train-00000-of-00001.parquet",
        ]
    );
    for input in &p.inputs {
        let file = source.join(&input.path);
        assert_eq!(input.sha256, sha256_file(&file), "{}", input.path);
        assert_eq!(input.bytes, std::fs::metadata(&file).unwrap().len());
    }
    assert_eq!(
        p.inputs.iter().map(|i| i.rows).collect::<Vec<_>>(),
        [9, 2, 1, 1, 1, 2]
    );

    // Every part is where the record says, with its own checksum.
    for (kind, summary) in &p.kinds {
        assert_eq!(
            summary.rows,
            summary.parts.iter().map(|x| x.rows).sum::<u64>()
        );
        for (i, part_ref) in summary.parts.iter().enumerate() {
            assert_eq!(part_ref.path, format!("{dir1}/{kind}-{i:04}.parquet"));
            let bytes = store.get(&part_ref.path).await.unwrap().unwrap();
            assert_eq!(bytes.len() as u64, part_ref.bytes);
            assert_eq!(
                usnm_ingest::source::hex(&Sha256::digest(&bytes)),
                part_ref.sha256
            );
        }
    }
    assert_eq!(
        p.kinds["comic"].parts.len(),
        2,
        "a part never spans two files"
    );

    // The cartoons: the pair LoC redirects, and each odd shape.
    let rows = part(&store, &p.kinds["cartoon"].parts[0].path).await;
    assert_eq!(rows.len(), 9);
    let ohi = &rows[0];
    assert_eq!(ohi.kind, "cartoon");
    assert_eq!(ohi.lccn.as_deref(), Some("sn85026051"));
    assert_eq!(ohi.date, Some(date(1850, 7, 27)));
    assert_eq!(ohi.edition, Some(1));
    assert_eq!(ohi.batch.as_deref(), Some("ohi_ingstad_ver01"));
    assert_eq!(ohi.reel.as_deref(), Some("00296027029"));
    assert_eq!(ohi.frame.as_deref(), Some("0037"));
    assert_eq!(ohi.score, Some(0.9731));
    close(ohi.bbox.unwrap(), [0.2991, 0.6229, 0.4305, 0.7314]);
    assert_eq!(ohi.caption_words, ["Mr.", "Ingstad"]);
    assert_eq!(ohi.predicted_type.as_deref(), Some("synthetic"));
    assert_eq!(
        ohi.page_url.as_deref(),
        Some("https://chroniclingamerica.loc.gov/data/batches/ohi_ingstad_ver01/data/sn85026051/00296027029/1850072701/0037.jp2")
    );
    assert_eq!(ohi.crop_url.as_deref(), Some(OHI_NEW));

    // No frame in the crop URL: placed by the page URL, `lccn` column ignored,
    // no crop URL guessed.
    for (row, batch, frame) in [
        (&rows[1], "wa_bittern_ver01", "0368"),
        (&rows[2], "wa_auklet_ver01", "0369"),
    ] {
        assert_eq!(row.lccn.as_deref(), Some("sn84022793"));
        assert_eq!(
            (row.reel.as_deref(), row.frame.as_deref()),
            (Some("no/reel"), Some(frame))
        );
        assert_eq!(row.batch.as_deref(), Some(batch));
        assert_eq!(row.crop_url, None);
    }
    // A five-digit frame on the reel `print`.
    let bode = &rows[3];
    assert_eq!(
        (bode.reel.as_deref(), bode.frame.as_deref()),
        (Some("print"), Some("10283"))
    );
    assert_eq!(bode.date, Some(date(1905, 6, 30)));
    assert!(bode
        .crop_url
        .as_deref()
        .unwrap()
        .contains(":batch_mdu_bode_ver03:data:sn83045462:print:1905063001:10283/pct:10,20,30,40/"));
    // An upper-case LCCN is joined on in lower case and kept as spelled in the crop.
    let darwin = &rows[4];
    assert_eq!(darwin.lccn.as_deref(), Some("sn85038614"));
    assert!(darwin
        .crop_url
        .as_deref()
        .unwrap()
        .contains(":data:SN85038614:00296027029:"));
    // A page URL that is no scan: no identity, the crop kept.
    let not_a_scan = &rows[5];
    assert_eq!(
        (&not_a_scan.lccn, &not_a_scan.date, &not_a_scan.frame),
        (&None, &None, &None)
    );
    assert_eq!(
        not_a_scan.page_url.as_deref(),
        Some("https://example.test/not-a-scan")
    );
    assert!(not_a_scan.crop_url.is_some());
    // A crop of another frame than the page's: no crop.
    assert_eq!(rows[6].frame.as_deref(), Some("0038"));
    assert_eq!(rows[6].crop_url, None);
    // Nothing usable: kept, with nulls.
    let empty = &rows[8];
    assert_eq!(empty.kind, "cartoon");
    assert_eq!(
        (
            &empty.lccn,
            &empty.batch,
            &empty.page_url,
            &empty.crop_url,
            empty.score,
            empty.bbox
        ),
        (&None, &None, &None, &None, None, None)
    );
    assert!(empty.caption_words.is_empty());

    // What did not fit, counted.
    let q = &p.kinds["cartoon"].quality;
    assert_eq!(
        *q,
        Quality {
            rows: 9,
            unplaced_rows: 5,
            batch_missing: 1,
            page_url_unplaced: 2,
            crop_url_missing: 1,
            crop_url_unplaced: 2,
            crop_url_mismatch: 1,
            box_unusable: 1,
            score_missing: 1,
            page_seq_num_missing: 1,
            frame_differs_from_page_seq_num: 1,
            without_crop_by_batch: [
                ("ohi_ingstad_ver01".to_owned(), 1),
                ("wa_auklet_ver01".to_owned(), 1),
                ("wa_bittern_ver01".to_owned(), 1),
            ]
            .into(),
        }
    );
    for kind in ["comic", "illustration", "map", "photo"] {
        let q = &p.kinds[kind].quality;
        assert_eq!(q.rows, p.kinds[kind].rows);
        assert_eq!(
            q.unplaced_rows + q.box_unusable + q.score_missing,
            0,
            "{kind}"
        );
    }
    assert_eq!(p.quality.rows, 16);
    assert_eq!(p.quality.unplaced_rows, 5);
    assert_eq!(p.quality.crop_url_unplaced, 2);
    assert_eq!(p.quality.without_crop_by_batch.len(), 3);
}

/// The fixtures are what they claim: the cartoons and photos are Snappy, the
/// comics are not, and `box` is a decimal of a different precision in each
/// per-kind file and a double in the combined layout.
#[test]
fn the_fixtures_are_snappy_and_lay_box_out_as_the_dataset_does() {
    let dir = tempfile::tempdir().unwrap();
    dataset(dir.path());
    let compression = |config: &str, name: &str| {
        let file = std::fs::File::open(dir.path().join(config).join(name)).unwrap();
        let reader = SerializedFileReader::new(file).unwrap();
        let meta = reader.metadata().row_group(0);
        (0..meta.num_columns())
            .map(|i| meta.column(i).compression())
            .collect::<Vec<_>>()
    };
    let cartoons = compression("cartoons", "train-00000-of-00001.parquet");
    assert!(
        cartoons.iter().all(|c| *c == Compression::SNAPPY),
        "{cartoons:?}"
    );
    assert!(compression("photos", "train-00000-of-00001.parquet")
        .iter()
        .all(|c| *c == Compression::SNAPPY));
    assert!(compression("comics", "train-00000-of-00002.parquet")
        .iter()
        .all(|c| *c == Compression::UNCOMPRESSED));

    let item_type = |config: &str, name: &str| {
        let file = std::fs::File::open(dir.path().join(config).join(name)).unwrap();
        let schema = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .schema()
            .clone();
        match schema.field_with_name("box").unwrap().data_type() {
            DataType::List(item) => item.data_type().clone(),
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(
        item_type("cartoons", "train-00000-of-00001.parquet"),
        DataType::Decimal128(21, 20)
    );
    assert_eq!(
        item_type("illustrations", "train-00000-of-00001.parquet"),
        DataType::Decimal128(22, 21)
    );
    assert_eq!(
        item_type("maps", "train-00000-of-00001.parquet"),
        DataType::Decimal128(23, 22)
    );
    assert_eq!(
        item_type("photos", "train-00000-of-00001.parquet"),
        DataType::Float64
    );
}

/// The same fractions come out of decimals of any precision, and of floats.
#[tokio::test]
async fn box_comes_out_the_same_from_every_layout() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let store = LocalStore::new(dir.path().join("curated"));
    pictures::run(&store, &options(Source::Dir(source)), at())
        .await
        .unwrap();
    let p = finished(&store, REVISION).await.unwrap().unwrap();
    for kind in ["comic", "illustration", "map", "photo"] {
        for summary in &p.kinds[kind].parts {
            for row in part(&store, &summary.path).await {
                close(row.bbox.unwrap(), [0.1, 0.2, 0.4, 0.6]);
                // The comics' scores went through a 32-bit float.
                assert!((row.score.unwrap() - 0.9).abs() < 1e-6, "{kind}");
            }
        }
    }
}

/// A revision with a finished attempt is left alone; one with an attempt of
/// another schema version, or only an unfinished attempt, gets a new one.
#[tokio::test]
async fn a_finished_revision_is_not_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let store = LocalStore::new(dir.path().join("curated"));
    let opts = options(Source::Dir(source.clone()));

    // An attempt that stopped before its marker: its parts stay, unlisted.
    let stray = format!("{}/cartoon-0000.parquet", attempt_dir(REVISION, 1));
    store
        .put_new(&stray, b"half".to_vec(), "application/octet-stream")
        .await
        .unwrap();
    assert!(finished(&store, REVISION).await.unwrap().is_none());

    let first = pictures::run(&store, &opts, at()).await.unwrap();
    assert_eq!(first.attempt, 2);
    assert_eq!(store.get(&stray).await.unwrap().unwrap(), b"half");
    let before = store.list("pictures").await.unwrap();

    // Run again: nothing read, nothing written (the source isn't even there).
    std::fs::remove_dir_all(&source).unwrap();
    let again = pictures::run(&store, &opts, at()).await.unwrap();
    assert!(again.already_written);
    assert_eq!((again.attempt, again.rows), (2, 16));
    assert_eq!(store.list("pictures").await.unwrap(), before);

    // Another revision is its own.
    let other = "0".repeat(40);
    assert!(finished(&store, &other).await.unwrap().is_none());

    // A marker of another schema version doesn't count.
    dataset(&source);
    let marker = format!("{}/{PROVENANCE}", attempt_dir(REVISION, 2));
    let mut old: serde_json::Value =
        serde_json::from_slice(&store.get(&marker).await.unwrap().unwrap()).unwrap();
    old["schema"] = serde_json::json!(SCHEMA_VERSION - 1);
    store
        .put(
            &marker,
            serde_json::to_vec(&old).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let third = pictures::run(&store, &opts, at()).await.unwrap();
    assert!(!third.already_written);
    assert_eq!(third.attempt, 3);
    assert_eq!(
        finished(&store, REVISION).await.unwrap().unwrap().attempt,
        3
    );
}

/// Parts hold at most the row limit, number on through a kind, and read back
/// in the order of the file.
#[tokio::test]
async fn parts_are_cut_at_the_row_limit() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    // Seven photos in one file.
    let frames = ["0010", "0011", "0012", "0013", "0014", "0015", "0016"];
    let rows: Vec<Src> = frames
        .iter()
        .map(|f| {
            scan(
                "dlc_abc_ver01",
                "sn83045462",
                "00280654021",
                "1890050301",
                f,
            )
        })
        .collect();
    put_file(
        &source,
        "photos",
        "train-00000-of-00001.parquet",
        &rows,
        SNAPPY_DECIMAL,
    );
    let store = LocalStore::new(dir.path().join("curated"));
    let opts = Options {
        part_rows: 3,
        ..options(Source::Dir(source))
    };
    pictures::run(&store, &opts, at()).await.unwrap();
    let p = finished(&store, REVISION).await.unwrap().unwrap();
    let photos = &p.kinds["photo"];
    assert_eq!(
        photos.parts.iter().map(|x| x.rows).collect::<Vec<_>>(),
        [3, 3, 1]
    );
    let mut read = Vec::new();
    for x in &photos.parts {
        read.extend(part(&store, &x.path).await);
    }
    let read: Vec<&str> = read.iter().map(|r| r.frame.as_deref().unwrap()).collect();
    assert_eq!(read, frames);
}

/// A run whose rows mostly don't fit the shapes read stops, marks nothing
/// finished and says why; a higher limit writes the attempt with its counts.
#[tokio::test]
async fn stops_when_the_rows_do_not_fit_the_shapes_read() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let store = LocalStore::new(dir.path().join("curated"));

    let strict = Options {
        max_unplaced_pct: 1.0,
        ..options(Source::Dir(source.clone()))
    };
    let err = format!(
        "{:#}",
        pictures::run(&store, &strict, at()).await.unwrap_err()
    );
    assert!(err.contains("5 of 9 `cartoons` rows"), "{err}");
    assert!(err.contains("--max-unplaced-pct"), "{err}");
    assert!(finished(&store, REVISION).await.unwrap().is_none());

    // The 5 of 9 pass at 56%, and not at 55%.
    let at_55 = Options {
        max_unplaced_pct: 55.0,
        ..strict.clone()
    };
    assert!(pictures::run(&store, &at_55, at()).await.is_err());
    let at_56 = Options {
        max_unplaced_pct: 56.0,
        ..strict
    };
    let done = pictures::run(&store, &at_56, at()).await.unwrap();
    // The two failed runs took attempts 1 and 2.
    assert_eq!(done.attempt, 3);
}

#[tokio::test]
async fn a_file_that_does_not_fit_fails_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStore::new(dir.path().join("curated"));

    // A missing config.
    let source = dir.path().join("source");
    dataset(&source);
    std::fs::remove_dir_all(source.join("maps")).unwrap();
    let err = format!(
        "{:#}",
        pictures::run(&store, &options(Source::Dir(source.clone())), at())
            .await
            .unwrap_err()
    );
    assert!(err.contains("maps"), "{err}");

    // A file that isn't Parquet.
    std::fs::create_dir_all(source.join("maps")).unwrap();
    std::fs::write(
        source.join("maps/train-00000-of-00001.parquet"),
        b"not parquet",
    )
    .unwrap();
    let err = format!(
        "{:#}",
        pictures::run(&store, &options(Source::Dir(source.clone())), at())
            .await
            .unwrap_err()
    );
    assert!(err.contains("maps/train-00000-of-00001.parquet"), "{err}");

    // A file without a column the rules read.
    let batch = RecordBatch::try_from_iter([(
        "lccn",
        Arc::new(StringArray::from(vec!["sn1"])) as ArrayRef,
    )])
    .unwrap();
    let mut w = ArrowWriter::try_new(Vec::new(), batch.schema(), None).unwrap();
    w.write(&batch).unwrap();
    std::fs::write(
        source.join("maps/train-00000-of-00001.parquet"),
        w.into_inner().unwrap(),
    )
    .unwrap();
    let err = format!(
        "{:#}",
        pictures::run(&store, &options(Source::Dir(source)), at())
            .await
            .unwrap_err()
    );
    assert!(
        err.contains("no `batch` column") || err.contains("no `box` column"),
        "{err}"
    );

    // None of them finished anything.
    assert!(finished(&store, REVISION).await.unwrap().is_none());
}

/// What the Hub serves for a dataset directory: the tree listing of a config
/// (with the LFS object ids and sizes the Hub gives, or a wrong one for the
/// files named in `corrupt`), and each file.
async fn hub(dataset_dir: PathBuf, corrupt: Vec<String>) -> String {
    use axum::extract::Path as AxPath;
    use axum::http::StatusCode;
    use axum::routing::get;
    let tree_dir = dataset_dir.clone();
    let app = axum::Router::new()
        .route(
            "/api/datasets/biglam/newspaper-navigator/tree/{revision}/{config}",
            get(move |AxPath((revision, config)): AxPath<(String, String)>| {
                let dir = tree_dir.clone();
                let corrupt = corrupt.clone();
                async move {
                    if revision != REVISION {
                        return (StatusCode::NOT_FOUND, axum::Json(serde_json::json!({"error": "no"})));
                    }
                    let mut entries = vec![serde_json::json!(
                        {"type": "file", "oid": "ab", "size": 5, "path": format!("{config}/README.md")}
                    )];
                    for e in std::fs::read_dir(dir.join(&config)).unwrap() {
                        let name = e.unwrap().file_name().into_string().unwrap();
                        let path = format!("{config}/{name}");
                        let file = dir.join(&path);
                        let oid = if corrupt.contains(&path) { "0".repeat(64) } else { sha256_file(&file) };
                        entries.push(serde_json::json!({
                            "type": "file", "oid": "cd", "size": 134, "path": path,
                            "lfs": {"oid": oid, "size": std::fs::metadata(&file).unwrap().len(), "pointerSize": 134}
                        }));
                    }
                    (StatusCode::OK, axum::Json(serde_json::Value::Array(entries)))
                }
            }),
        )
        .route(
            "/datasets/biglam/newspaper-navigator/resolve/{revision}/{config}/{file}",
            get(move |AxPath((revision, config, file)): AxPath<(String, String, String)>| {
                let dir = dataset_dir.clone();
                async move {
                    if revision != REVISION {
                        return (StatusCode::NOT_FOUND, Vec::new());
                    }
                    (StatusCode::OK, std::fs::read(dir.join(config).join(file)).unwrap())
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

/// Downloading from the Hub at the pinned revision gives the table a
/// directory gives, one file at a time, each deleted once read.
#[tokio::test]
async fn downloads_the_files_of_a_pinned_revision() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let base = hub(source.clone(), vec![]).await;
    let scratch = dir.path().join("work/pictures");

    let from_hub = LocalStore::new(dir.path().join("from-hub"));
    let opts = options(Source::Hub {
        base: base.clone(),
        scratch: scratch.clone(),
    });
    let report = pictures::run(&from_hub, &opts, at()).await.unwrap();
    assert_eq!(report.rows, 16);
    assert!(
        std::fs::read_dir(&scratch).unwrap().next().is_none(),
        "downloads are deleted"
    );

    let from_dir = LocalStore::new(dir.path().join("from-dir"));
    pictures::run(&from_dir, &options(Source::Dir(source)), at())
        .await
        .unwrap();
    let (a, b) = (
        finished(&from_hub, REVISION).await.unwrap().unwrap(),
        finished(&from_dir, REVISION).await.unwrap().unwrap(),
    );
    assert_eq!(a, b, "the same files give the same table");

    // A revision the Hub doesn't have.
    let other = Options {
        revision: "1".repeat(40),
        ..opts
    };
    let err = format!(
        "{:#}",
        pictures::run(&from_hub, &other, at()).await.unwrap_err()
    );
    assert!(err.contains("not found"), "{err}");
}

/// A download that isn't the file the Hub lists is not read.
#[tokio::test]
async fn a_download_that_differs_from_the_hubs_object_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    dataset(&source);
    let base = hub(source, vec!["maps/train-00000-of-00001.parquet".to_owned()]).await;
    let scratch = dir.path().join("work/pictures");
    let store = LocalStore::new(dir.path().join("curated"));
    let err = pictures::run(
        &store,
        &options(Source::Hub {
            base,
            scratch: scratch.clone(),
        }),
        at(),
    )
    .await
    .unwrap_err();
    let err = format!("{err:#}");
    assert!(
        err.contains("maps/train-00000-of-00001.parquet") && err.contains("sha256"),
        "{err}"
    );
    assert!(finished(&store, REVISION).await.unwrap().is_none());
    assert!(
        std::fs::read_dir(&scratch).unwrap().next().is_none(),
        "the refused file is deleted"
    );
}
