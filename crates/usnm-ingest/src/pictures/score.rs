//! Scores predicted picture regions against Beyond Words annotations (#1102,
//! 04 §4.11): `usnm-ingest pictures-score`, a report that changes no store.
//!
//! The gold set is the Beyond Words COCO file (`train_80_percent.json` or
//! `val_20_percent.json` of `biglam/loc_beyond_words`, the same files
//! `scripts/pictures/beyond_words.py` reads): `images` with `id`, `width` and
//! `height`; `annotations` with `image_id`, `category_id`, `bbox` and
//! `iscrowd`; `categories` with `id` and `name`. A `bbox` is `[x, y, width,
//! height]` in pixels of its image. Only the five picture categories count
//! (`Photograph`, `Illustration`, `Map`, `Comics/Cartoon`, `Editorial
//! Cartoon`); `Headline`, `Advertisement` and crowd annotations are left out
//! and counted.
//!
//! Two kinds of prediction are read, each keyed to a page by the image `id`:
//!
//! - ALTO: the `Illustration` blocks of `{id}.xml` in a directory, as the
//!   stored ALTO has them (`usnm_alto`), in pixels of the page's own frame.
//! - Newspaper Navigator: a JSON Lines file whose lines have `page` (an
//!   image `id`), `box` (`[x0, y0, x1, y1]` as fractions of the page, the
//!   dataset's own convention) and optionally `score`. Other fields are
//!   ignored. Mapping a scan to an image `id` is the caller's job; this
//!   module reads no scan identity.
//!
//! Every box is turned into fractions of its page and clipped to the page.
//! Predictions are matched to annotations one to one, the highest score first
//! (ALTO has no score, so the order of the file's blocks), each taking the
//! unmatched annotation it overlaps most, if that overlap's IoU is at least
//! the threshold. Class is not compared: ALTO has no picture classes.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use usnm_alto::{BlockKind, ParseOptions};

/// The Beyond Words classes that are pictures, by name.
pub const PICTURE_CLASSES: [&str; 5] = [
    "Photograph",
    "Illustration",
    "Map",
    "Comics/Cartoon",
    "Editorial Cartoon",
];

/// The largest annotations file read (the published ones are a few MB).
const MAX_ANNOTATIONS_BYTES: u64 = 64 << 20;

/// The default IoU a prediction must reach to match an annotation.
pub const DEFAULT_IOU: f64 = 0.5;

/// A box as fractions of its page, `x0 < x1` and `y0 < y1`, inside 0 to 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Region {
    /// The box clipped to the page, or `None` if a value is not finite or
    /// nothing of it is left.
    pub fn clipped(x0: f64, y0: f64, x1: f64, y1: f64) -> Option<Self> {
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return None;
        }
        let c = |v: f64| v.clamp(0.0, 1.0);
        let r = Region {
            x0: c(x0),
            y0: c(y0),
            x1: c(x1),
            y1: c(y1),
        };
        (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
    }

    fn area(&self) -> f64 {
        (self.x1 - self.x0) * (self.y1 - self.y0)
    }

    /// Intersection over union, 0 to 1.
    pub fn iou(&self, other: &Region) -> f64 {
        let w = (self.x1.min(other.x1) - self.x0.max(other.x0)).max(0.0);
        let h = (self.y1.min(other.y1) - self.y0.max(other.y0)).max(0.0);
        let inter = w * h;
        let union = self.area() + other.area() - inter;
        if union > 0.0 {
            inter / union
        } else {
            0.0
        }
    }
}

/// An image id from the annotations, as the key predictions use. COCO ids are
/// integers; a string is accepted if it is plain (letters, digits, `-`, `_`),
/// since it names a file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(untagged)]
enum RawId {
    Int(i64),
    Str(String),
}

impl RawId {
    fn key(&self) -> anyhow::Result<String> {
        match self {
            RawId::Int(n) => Ok(n.to_string()),
            RawId::Str(s)
                if !s.is_empty()
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') =>
            {
                Ok(s.clone())
            }
            RawId::Str(s) => bail!("image id `{s}` is not letters, digits, `-` or `_`"),
        }
    }
}

#[derive(Deserialize)]
struct CocoImage {
    id: RawId,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
struct CocoAnnotation {
    image_id: RawId,
    category_id: i64,
    bbox: [f64; 4],
    #[serde(default)]
    iscrowd: i64,
}

#[derive(Deserialize)]
struct CocoCategory {
    id: i64,
    name: String,
}

#[derive(Deserialize)]
struct Coco {
    images: Vec<CocoImage>,
    annotations: Vec<CocoAnnotation>,
    categories: Vec<CocoCategory>,
}

/// What was read from the annotations and what was left out.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct AnnotationCounts {
    pub images: usize,
    pub images_with_pictures: usize,
    pub picture_annotations: usize,
    /// `Headline`, `Advertisement` or any other class that is not a picture.
    pub other_class_annotations: usize,
    pub crowd_annotations: usize,
}

/// The verified picture boxes of each image, by image id.
#[derive(Debug, Default)]
pub struct Gold {
    pub pages: BTreeMap<String, Vec<Region>>,
    pub counts: AnnotationCounts,
}

impl Gold {
    /// Read a Beyond Words COCO document.
    pub fn parse(json: &[u8]) -> anyhow::Result<Gold> {
        let coco: Coco = serde_json::from_slice(json).context("reading the annotations JSON")?;
        let classes: HashMap<i64, &str> = coco
            .categories
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect();
        let mut sizes: HashMap<String, (f64, f64)> = HashMap::new();
        let mut gold = Gold::default();
        for image in &coco.images {
            let key = image.id.key()?;
            if !(image.width.is_finite()
                && image.height.is_finite()
                && image.width > 0.0
                && image.height > 0.0)
            {
                bail!("image {key} has no positive width and height");
            }
            if sizes
                .insert(key.clone(), (image.width, image.height))
                .is_some()
            {
                bail!("image id {key} appears twice");
            }
            gold.pages.insert(key, Vec::new());
        }
        gold.counts.images = gold.pages.len();
        for a in &coco.annotations {
            let key = a.image_id.key()?;
            let Some(&(width, height)) = sizes.get(&key) else {
                bail!("an annotation names image {key}, which is not in `images`");
            };
            let Some(class) = classes.get(&a.category_id) else {
                bail!(
                    "an annotation on image {key} has unknown category {}",
                    a.category_id
                );
            };
            if !PICTURE_CLASSES.contains(class) {
                gold.counts.other_class_annotations += 1;
                continue;
            }
            if a.iscrowd != 0 {
                gold.counts.crowd_annotations += 1;
                continue;
            }
            let [x, y, w, h] = a.bbox;
            let Some(region) =
                Region::clipped(x / width, y / height, (x + w) / width, (y + h) / height)
            else {
                bail!("a {class} annotation on image {key} has an unusable box");
            };
            gold.pages.entry(key).or_default().push(region);
            gold.counts.picture_annotations += 1;
        }
        gold.counts.images_with_pictures = gold.pages.values().filter(|v| !v.is_empty()).count();
        Ok(gold)
    }

    /// Read the annotations file `path`.
    pub fn read(path: &Path) -> anyhow::Result<Gold> {
        let len = std::fs::metadata(path)
            .with_context(|| format!("reading `{}`", path.display()))?
            .len();
        if len > MAX_ANNOTATIONS_BYTES {
            bail!(
                "`{}` is {len} bytes, over the {MAX_ANNOTATIONS_BYTES} limit",
                path.display()
            );
        }
        let bytes = std::fs::read(path).with_context(|| format!("reading `{}`", path.display()))?;
        Gold::parse(&bytes).with_context(|| format!("in `{}`", path.display()))
    }
}

/// A predicted region and its score (1.0 where the source has none).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Prediction {
    pub region: Region,
    pub score: f64,
}

/// What reading one source's predictions found.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct InputCounts {
    /// Annotated pages the source has input for: an ALTO file, or a line.
    pub pages_with_input: usize,
    /// Annotated pages it has none for. Their annotations count as missed,
    /// but nothing says the source found no pictures there.
    pub pages_without_input: usize,
    pub predictions_read: usize,
    pub predictions_below_min_score: usize,
    /// Boxes with a value that is not a number, or nothing left inside the page.
    pub predictions_unusable: usize,
    /// Lines for pages that are not in the annotations (Navigator only).
    pub predictions_off_annotated_pages: usize,
}

/// One source's predictions for the annotated pages.
#[derive(Debug, Default)]
pub struct Predictions {
    pub pages: BTreeMap<String, Vec<Prediction>>,
    pub counts: InputCounts,
}

/// Read the `Illustration` blocks of `{id}.xml` in `dir` for each annotated page.
pub fn read_alto(dir: &Path, gold: &Gold) -> anyhow::Result<Predictions> {
    let mut out = Predictions::default();
    for key in gold.pages.keys() {
        let path = dir.join(format!("{key}.xml"));
        let xml = match std::fs::read(&path) {
            Ok(xml) => xml,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.counts.pages_without_input += 1;
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("reading `{}`", path.display())),
        };
        let page = usnm_alto::parse(&xml, &ParseOptions::default())
            .with_context(|| format!("reading `{}`", path.display()))?;
        let mut found = Vec::new();
        for block in page
            .blocks
            .iter()
            .filter(|b| b.kind == BlockKind::Illustration)
        {
            out.counts.predictions_read += 1;
            let r = block.rect;
            match Region::clipped(
                r.x0 / page.width,
                r.y0 / page.height,
                r.x1 / page.width,
                r.y1 / page.height,
            ) {
                Some(region) => found.push(Prediction { region, score: 1.0 }),
                None => out.counts.predictions_unusable += 1,
            }
        }
        out.counts.pages_with_input += 1;
        out.pages.insert(key.clone(), found);
    }
    Ok(out)
}

/// A line of the Navigator file.
#[derive(Deserialize)]
struct NavigatorLine {
    page: RawId,
    #[serde(rename = "box")]
    region: Option<Vec<f64>>,
    score: Option<f64>,
}

/// Read Newspaper Navigator boxes from the JSON Lines file `path`, keeping
/// those with a score of at least `min_score` (a line with no score always
/// counts as 1) on annotated pages.
pub fn read_navigator(path: &Path, gold: &Gold, min_score: f64) -> anyhow::Result<Predictions> {
    let file = File::open(path).with_context(|| format!("reading `{}`", path.display()))?;
    let mut out = Predictions::default();
    for (n, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| format!("reading `{}`", path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let at = || format!("line {} of `{}`", n + 1, path.display());
        let l: NavigatorLine = serde_json::from_str(&line).with_context(at)?;
        let key = l.page.key().with_context(at)?;
        if !gold.pages.contains_key(&key) {
            out.counts.predictions_off_annotated_pages += 1;
            continue;
        }
        out.counts.predictions_read += 1;
        let found = out.pages.entry(key).or_default();
        let score = l.score.unwrap_or(1.0);
        if !score.is_finite() {
            out.counts.predictions_unusable += 1;
            continue;
        }
        if score < min_score {
            out.counts.predictions_below_min_score += 1;
            continue;
        }
        match l.region.as_deref() {
            Some(&[x0, y0, x1, y1]) => match Region::clipped(x0, y0, x1, y1) {
                Some(region) => found.push(Prediction { region, score }),
                None => out.counts.predictions_unusable += 1,
            },
            _ => out.counts.predictions_unusable += 1,
        }
    }
    out.counts.pages_with_input = out.pages.len();
    out.counts.pages_without_input = gold.pages.len() - out.pages.len();
    Ok(out)
}

/// How many predictions match annotations, one to one at `iou` or more.
pub fn matches(gold: &[Region], predictions: &[Prediction], iou: f64) -> usize {
    let mut order: Vec<usize> = (0..predictions.len()).collect();
    // Highest score first; equal scores keep their order in the input.
    order.sort_by(|&a, &b| predictions[b].score.total_cmp(&predictions[a].score));
    let mut used = vec![false; gold.len()];
    let mut matched = 0;
    for i in order {
        let best = gold
            .iter()
            .enumerate()
            .filter(|(g, _)| !used[*g])
            .map(|(g, r)| (g, r.iou(&predictions[i].region)))
            .filter(|(_, v)| *v >= iou)
            .max_by(|a, b| a.1.total_cmp(&b.1).then(b.0.cmp(&a.0)));
        if let Some((g, _)) = best {
            used[g] = true;
            matched += 1;
        }
    }
    matched
}

/// Counts and ratios for one page or for all pages.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Tally {
    pub annotated: usize,
    pub predicted: usize,
    pub matched: usize,
    pub unmatched_predictions: usize,
    pub unmatched_annotations: usize,
    /// `matched / predicted`; `None` with no predictions.
    pub precision: Option<f64>,
    /// `matched / annotated`; `None` with no annotations.
    pub recall: Option<f64>,
}

impl Tally {
    fn new(annotated: usize, predicted: usize, matched: usize) -> Tally {
        let ratio = |n: usize, d: usize| (d > 0).then(|| n as f64 / d as f64);
        Tally {
            annotated,
            predicted,
            matched,
            unmatched_predictions: predicted - matched,
            unmatched_annotations: annotated - matched,
            precision: ratio(matched, predicted),
            recall: ratio(matched, annotated),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageScore {
    /// Whether the source had input for the page at all.
    pub has_input: bool,
    #[serde(flatten)]
    pub tally: Tally,
}

/// One source's scores.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceScore {
    pub input: InputCounts,
    /// All annotated pages, including those the source has no input for, so
    /// recall counts them as missed.
    pub total: Tally,
    /// Only the pages the source has input for.
    pub on_pages_with_input: Tally,
    pub pages: BTreeMap<String, PageScore>,
}

/// Score `predictions` against `gold` at `iou`.
pub fn score_source(gold: &Gold, predictions: &Predictions, iou: f64) -> SourceScore {
    let mut pages = BTreeMap::new();
    let (mut all, mut with) = ([0usize; 3], [0usize; 3]);
    for (key, regions) in &gold.pages {
        let found = predictions.pages.get(key);
        let preds = found.map(Vec::as_slice).unwrap_or(&[]);
        let matched = matches(regions, preds, iou);
        for (sums, on) in [(&mut all, true), (&mut with, found.is_some())] {
            if on {
                sums[0] += regions.len();
                sums[1] += preds.len();
                sums[2] += matched;
            }
        }
        pages.insert(
            key.clone(),
            PageScore {
                has_input: found.is_some(),
                tally: Tally::new(regions.len(), preds.len(), matched),
            },
        );
    }
    SourceScore {
        input: predictions.counts.clone(),
        total: Tally::new(all[0], all[1], all[2]),
        on_pages_with_input: Tally::new(with[0], with[1], with[2]),
        pages,
    }
}

/// What `pictures-score` writes.
#[derive(Debug, Serialize)]
pub struct ScoreReport {
    pub iou_threshold: f64,
    /// Newspaper Navigator boxes below this score are not scored.
    pub navigator_min_score: f64,
    pub matching: &'static str,
    pub annotations: AnnotationCounts,
    /// By source: `alto`, `newspaper-navigator`.
    pub sources: BTreeMap<&'static str, SourceScore>,
}

/// The scores of the sources given, against `gold`.
pub fn report(
    gold: &Gold,
    alto: Option<&Predictions>,
    navigator: Option<&Predictions>,
    iou: f64,
    min_score: f64,
) -> ScoreReport {
    let mut sources = BTreeMap::new();
    if let Some(p) = alto {
        sources.insert("alto", score_source(gold, p, iou));
    }
    if let Some(p) = navigator {
        sources.insert("newspaper-navigator", score_source(gold, p, iou));
    }
    ScoreReport {
        iou_threshold: iou,
        navigator_min_score: min_score,
        matching: "one to one, highest score first, class not compared",
        annotations: gold.counts.clone(),
        sources,
    }
}

/// Create the report file; it must not exist.
pub fn create_report(path: &Path) -> anyhow::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("creating `{}`", path.display()))
}

/// Write `report` as JSON to `file`, which [`create_report`] made at `path`.
pub fn write_report(file: &mut File, path: &Path, report: &ScoreReport) -> anyhow::Result<()> {
    let mut write = || -> anyhow::Result<()> {
        serde_json::to_writer_pretty(&mut *file, report)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    };
    write().with_context(|| format!("writing `{}`", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COCO: &str = r#"{
      "images": [
        {"id": 1, "width": 1000, "height": 2000},
        {"id": 2, "width": 500, "height": 500},
        {"id": 3, "width": 500, "height": 500}
      ],
      "categories": [
        {"id": 0, "name": "Photograph"}, {"id": 1, "name": "Illustration"},
        {"id": 2, "name": "Map"}, {"id": 3, "name": "Comics/Cartoon"},
        {"id": 4, "name": "Editorial Cartoon"}, {"id": 5, "name": "Headline"},
        {"id": 6, "name": "Advertisement"}
      ],
      "annotations": [
        {"id": 1, "image_id": 1, "category_id": 0, "bbox": [100, 200, 300, 400], "iscrowd": 0},
        {"id": 2, "image_id": 1, "category_id": 1, "bbox": [600, 1000, 300, 600], "iscrowd": 0},
        {"id": 3, "image_id": 1, "category_id": 5, "bbox": [0, 0, 100, 100], "iscrowd": 0},
        {"id": 4, "image_id": 2, "category_id": 2, "bbox": [0, 0, 250, 250], "iscrowd": 0},
        {"id": 5, "image_id": 2, "category_id": 6, "bbox": [0, 0, 500, 500], "iscrowd": 0},
        {"id": 6, "image_id": 2, "category_id": 0, "bbox": [250, 250, 100, 100], "iscrowd": 1},
        {"id": 7, "image_id": 2, "category_id": 3, "bbox": [400, 400, 300, 300]}
      ]
    }"#;

    fn region(x0: f64, y0: f64, x1: f64, y1: f64) -> Region {
        Region::clipped(x0, y0, x1, y1).unwrap()
    }

    #[test]
    fn iou_of_boxes() {
        let a = region(0.0, 0.0, 0.5, 0.5);
        assert_eq!(a.iou(&a), 1.0);
        assert_eq!(a.iou(&region(0.5, 0.5, 1.0, 1.0)), 0.0);
        // Half of a is inside b: 0.125 / (0.25 + 0.25 - 0.125).
        let b = region(0.25, 0.0, 0.75, 0.5);
        assert!((a.iou(&b) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn clipping_and_unusable_boxes() {
        assert_eq!(
            Region::clipped(-0.5, 0.0, 0.5, 1.5),
            Some(region(0.0, 0.0, 0.5, 1.0))
        );
        assert_eq!(
            Region::clipped(1.2, 0.0, 1.5, 1.0),
            None,
            "outside the page"
        );
        assert_eq!(Region::clipped(0.5, 0.5, 0.5, 0.9), None, "no width");
        assert_eq!(Region::clipped(f64::NAN, 0.0, 1.0, 1.0), None);
    }

    #[test]
    fn reads_picture_annotations_only() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        assert_eq!(
            gold.counts,
            AnnotationCounts {
                images: 3,
                images_with_pictures: 2,
                picture_annotations: 4,
                other_class_annotations: 2,
                crowd_annotations: 1,
            }
        );
        assert_eq!(
            gold.pages["1"],
            vec![region(0.1, 0.1, 0.4, 0.3), region(0.6, 0.5, 0.9, 0.8)]
        );
        assert_eq!(gold.pages["2"].len(), 2, "the missing iscrowd means 0");
        assert!(gold.pages["3"].is_empty());
    }

    #[test]
    fn refuses_bad_annotations() {
        let swap = |from: &str, to: &str| COCO.replacen(from, to, 1);
        for bad in [
            swap(
                r#""image_id": 1, "category_id": 0"#,
                r#""image_id": 9, "category_id": 0"#,
            ),
            swap(
                r#""category_id": 0, "bbox": [100"#,
                r#""category_id": 42, "bbox": [100"#,
            ),
            swap(r#"[100, 200, 300, 400]"#, r#"[100, 200, 0, 400]"#),
            swap(r#"{"id": 2, "width": 500"#, r#"{"id": 1, "width": 500"#),
            swap(
                r#"{"id": 3, "width": 500, "height": 500}"#,
                r#"{"id": "../x", "width": 500, "height": 500}"#,
            ),
            swap(r#""width": 1000"#, r#""width": 0"#),
        ] {
            assert!(Gold::parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    fn p(score: f64, r: Region) -> Prediction {
        Prediction { region: r, score }
    }

    #[test]
    fn matching_is_one_to_one_and_best_score_first() {
        let g = [region(0.0, 0.0, 0.5, 0.5)];
        let exact = region(0.0, 0.0, 0.5, 0.5);
        // Both predictions overlap the one annotation well; only one may take it.
        assert_eq!(matches(&g, &[p(0.9, exact), p(0.8, exact)], 0.5), 1);
        // A weak box that overlaps at IoU 0.4 does not match at 0.5, does at 0.4.
        let weak = region(0.0, 0.0, 0.5, 0.3125);
        assert_eq!(matches(&g, &[p(0.9, weak)], 0.5), 1, "IoU 0.625");
        let weaker = region(0.0, 0.0, 0.5, 0.2);
        assert_eq!(matches(&g, &[p(0.9, weaker)], 0.5), 0, "IoU 0.4");
        assert_eq!(matches(&g, &[p(0.9, weaker)], 0.4), 1);
        // The higher-scored box wins the annotation; the other is unmatched
        // even though it would fit better.
        let g2 = [region(0.0, 0.0, 0.5, 0.5), region(0.5, 0.5, 1.0, 1.0)];
        let both = [p(0.9, region(0.0, 0.0, 0.5, 0.4)), p(0.5, exact)];
        assert_eq!(matches(&g2, &both, 0.5), 1);
        assert_eq!(matches(&[], &both, 0.5), 0);
        assert_eq!(matches(&g2, &[], 0.5), 0);
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn alto(blocks: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
  <Description><MeasurementUnit>pixel</MeasurementUnit></Description>
  <Layout><Page ID="p" WIDTH="1000" HEIGHT="2000" PHYSICAL_IMG_NR="1"><PrintSpace>
    <TextBlock ID="t" HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="100"/>
    {blocks}
  </PrintSpace></Page></Layout>
</alto>"#
        )
    }

    #[test]
    fn scores_alto_illustrations() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        // Image 1: one illustration on the photograph, one stray; a text block is ignored.
        write(
            dir.path(),
            "1.xml",
            &alto(
                r#"<Illustration ID="a" HPOS="100" VPOS="200" WIDTH="300" HEIGHT="400"/>
                   <Illustration ID="b" HPOS="0" VPOS="1500" WIDTH="100" HEIGHT="100"/>
                   <Illustration ID="c" HPOS="2000" VPOS="0" WIDTH="100" HEIGHT="100"/>"#,
            ),
        );
        // Image 2 has no file; image 3 has a file with no illustration.
        write(dir.path(), "3.xml", &alto(""));
        let found = read_alto(dir.path(), &gold).unwrap();
        assert_eq!(
            found.counts,
            InputCounts {
                pages_with_input: 2,
                pages_without_input: 1,
                predictions_read: 3,
                predictions_below_min_score: 0,
                predictions_unusable: 1,
                predictions_off_annotated_pages: 0,
            }
        );
        let s = score_source(&gold, &found, 0.5);
        let one = &s.pages["1"];
        assert!(one.has_input);
        assert_eq!(
            (one.tally.annotated, one.tally.predicted, one.tally.matched),
            (2, 2, 1)
        );
        assert_eq!(one.tally.precision, Some(0.5));
        assert_eq!(one.tally.recall, Some(0.5));
        let two = &s.pages["2"];
        assert!(!two.has_input);
        assert_eq!(two.tally.precision, None);
        assert_eq!(two.tally.recall, Some(0.0));
        assert_eq!(s.pages["3"].tally.recall, None, "no annotations");
        // All pages: 4 annotated, 2 predicted, 1 matched. With input: 2 annotated.
        assert_eq!(
            (s.total.annotated, s.total.predicted, s.total.matched),
            (4, 2, 1)
        );
        assert_eq!(s.total.recall, Some(0.25));
        assert_eq!(s.on_pages_with_input.annotated, 2);
        assert_eq!(s.on_pages_with_input.recall, Some(0.5));
    }

    #[test]
    fn alto_that_is_not_alto_stops_the_run() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "1.xml", "<nope/>");
        let err = format!("{:#}", read_alto(dir.path(), &gold).unwrap_err());
        assert!(err.contains("1.xml"), "{err}");
    }

    #[test]
    fn scores_navigator_boxes_with_a_score_floor() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nn.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"page": 1, "box": [0.1, 0.1, 0.4, 0.3], "score": 0.97, "kind": "photo"}"#,
                "\n",
                r#"{"page": "1", "box": [0.0, 0.9, 0.1, 1.0], "score": 0.3}"#,
                "\n",
                "\n",
                r#"{"page": 2, "box": [0.0, 0.0, 0.5, 0.5], "score": 0.8}"#,
                "\n",
                r#"{"page": 2, "box": [0.0, 0.0, 0.5], "score": 0.8}"#,
                "\n",
                r#"{"page": 99, "box": [0.0, 0.0, 0.5, 0.5], "score": 0.8}"#,
                "\n",
            ),
        )
        .unwrap();
        let all = read_navigator(&path, &gold, 0.0).unwrap();
        assert_eq!(all.counts.predictions_read, 4);
        assert_eq!(all.counts.predictions_unusable, 1);
        assert_eq!(all.counts.predictions_off_annotated_pages, 1);
        assert_eq!(all.counts.pages_with_input, 2);
        assert_eq!(all.counts.pages_without_input, 1);
        let s = score_source(&gold, &all, 0.5);
        assert_eq!((s.total.predicted, s.total.matched), (3, 2));
        assert_eq!(s.pages["1"].tally.precision, Some(0.5));

        let floor = read_navigator(&path, &gold, 0.5).unwrap();
        assert_eq!(floor.counts.predictions_below_min_score, 1);
        let s = score_source(&gold, &floor, 0.5);
        assert_eq!((s.total.predicted, s.total.matched), (2, 2));
        assert_eq!(s.pages["1"].tally.precision, Some(1.0));
        assert_eq!(s.total.recall, Some(0.5));
    }

    #[test]
    fn bad_navigator_line_names_its_place() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nn.jsonl");
        std::fs::write(&path, "{\"page\": 1, \"box\": [0,0,1,1]}\nnot json\n").unwrap();
        let err = format!("{:#}", read_navigator(&path, &gold, 0.0).unwrap_err());
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn report_states_its_settings_and_refuses_an_existing_file() {
        let gold = Gold::parse(COCO.as_bytes()).unwrap();
        let none = Predictions::default();
        let r = report(&gold, Some(&none), None, 0.5, 0.0);
        assert_eq!(r.iou_threshold, 0.5);
        assert_eq!(r.sources.keys().copied().collect::<Vec<_>>(), ["alto"]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("score.json");
        let mut f = create_report(&path).unwrap();
        write_report(&mut f, &path, &r).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["iou_threshold"], 0.5);
        assert_eq!(v["sources"]["alto"]["total"]["annotated"], 4);
        assert!(create_report(&path).is_err());
    }
}
