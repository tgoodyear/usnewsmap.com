//! The column repair (#925) on committed engine ALTO: a synthetic page that
//! Tesseract read across its gutters is repaired, a real Tesseract page with
//! the same fault is improved, and pages whose columns, tables and prose were
//! read correctly are left as they were.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use usnm_alto::{parse, split_columns, Dir, Page, ParseOptions};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The ALTO at `path` from the repository's root. The pass works in line
/// heights, so the dpi given for `inch1200` pages doesn't matter.
fn read(path: &str) -> Page {
    let opts = ParseOptions {
        dpi: Some(300.0),
        ..ParseOptions::default()
    };
    parse(&std::fs::read(root().join(path)).unwrap(), &opts).unwrap()
}

/// Lines whose words fall in more than one column, and how often the
/// segment changes from one word to the next in reading order. `seg` names a
/// word's segment from its centre; `None` leaves the word out.
fn measure(page: &Page, seg: impl Fn(f64, f64, f64) -> Option<&'static str>) -> (usize, usize) {
    let (mut cross, mut switches) = (0, 0);
    let mut last = None;
    for l in page.blocks.iter().flat_map(|b| &b.lines) {
        let segs: Vec<&str> = l
            .words
            .iter()
            .filter_map(|w| {
                let (cx, cy) = w.rect.centre();
                seg(cx, cy, w.rect.width())
            })
            .collect();
        let cols: Vec<&&str> = segs.iter().filter(|s| s.starts_with("col")).collect();
        if cols.windows(2).any(|p| p[0] != p[1]) {
            cross += 1;
        }
        for s in segs {
            if last.is_some_and(|l| l != s) {
                switches += 1;
            }
            last = Some(s);
        }
    }
    (cross, switches)
}

fn words(page: &Page) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for w in page
        .blocks
        .iter()
        .flat_map(|b| &b.lines)
        .flat_map(|l| &l.words)
    {
        *m.entry(format!("{} {:?}", w.text, w.rect)).or_default() += 1;
    }
    m
}

/// A synthetic page (random German words, no real text) in five 520 px
/// columns with 12 px gutters but one of 30 px, under a masthead and a
/// banner over the first three columns, rendered in DejaVu Serif and read by
/// Tesseract 5.5.0 (`deu`, PSM 3). Tesseract missed the narrow gutters: 87
/// of its 95 lines run across columns.
#[test]
fn a_page_read_across_narrow_gutters_is_read_by_column() {
    let page = read("crates/usnm-alto/tests/data/tesseract-synthetic-columns.xml");
    let seg = |cx: f64, cy: f64, _: f64| {
        Some(if cy < 170.0 {
            "mast"
        } else if cy < 240.0 && cx < 1660.0 {
            "banner"
        } else if cx < 586.0 {
            "col0"
        } else if cx < 1118.0 {
            "col1"
        } else if cx < 1659.0 {
            "col2"
        } else if cx < 2200.0 {
            "col3"
        } else {
            "col4"
        })
    };
    assert_eq!(measure(&page, seg), (87, 212));
    let (fixed, repair) = split_columns(&page, Dir::Ltr).unwrap();
    // Each segment read once, in order: masthead, banner, five columns.
    assert_eq!(measure(&fixed, seg), (0, 6));
    assert_eq!(repair.lines_split, 87);
    assert_eq!(words(&fixed), words(&page));
}

/// A real 1894 three-column page Tesseract read with column rules as `|`
/// and some lines run across them. Words fused with a rule (`New|`) keep
/// some lines crossing, but most are repaired.
#[test]
fn a_real_page_read_across_its_columns_is_improved() {
    let page = read("crates/usnm-ocr-worker/testdata/tesseract-agave-1894-06-23.xml");
    let seg = |cx: f64, cy: f64, w: f64| {
        (w >= 9.0).then_some(if cy < 540.0 {
            "head"
        } else if cx < 845.0 {
            "col1"
        } else if cx < 1590.0 {
            "col2"
        } else {
            "col3"
        })
    };
    let before = measure(&page, seg);
    let (fixed, _) = split_columns(&page, Dir::Ltr).unwrap();
    let after = measure(&fixed, seg);
    assert_eq!(before, (24, 57));
    assert!(after.0 <= 14 && after.1 <= 41, "{after:?}");
    assert_eq!(words(&fixed), words(&page));
}

/// Pages read correctly, among them ABBYY's tables and a page with a river
/// of spaces down a column, are left as they were.
#[test]
fn pages_read_correctly_are_left_alone() {
    for path in [
        "crates/usnm-alto/tests/data/tesseract-5.5.xml",
        "crates/usnm-alto/tests/data/ja-ocr.xml",
        "ocr/bench/serialset-pilot/source/alto/doc183-p3.xml",
        "ocr/bench/serialset-pilot/source/alto/doc42-p3.xml",
        "ocr/bench/serialset-pilot/source/alto/doc53-p4.xml",
        "ocr/bench/serialset-pilot/source/alto/doc53-p5.xml",
    ] {
        assert_eq!(split_columns(&read(path), Dir::Ltr), None, "{path}");
    }
}
