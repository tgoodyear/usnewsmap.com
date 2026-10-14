//! A page as the stored ALTO holds it (13 §13.8.2): blocks, lines and words
//! in pixels of one frame, in reading order.

use serde::{Deserialize, Serialize};

/// A box `[x0, y0, x1, y1]` from the top left, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub fn new(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        Self { x0, y0, x1, y1 }
    }

    /// From ALTO's `HPOS`, `VPOS`, `WIDTH` and `HEIGHT`.
    pub fn from_alto(hpos: f64, vpos: f64, width: f64, height: f64) -> Self {
        Self::new(hpos, vpos, hpos + width, vpos + height)
    }

    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.y1 - self.y0
    }

    pub fn centre(&self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }

    pub fn union(&self, o: &Rect) -> Rect {
        Rect::new(
            self.x0.min(o.x0),
            self.y0.min(o.y0),
            self.x1.max(o.x1),
            self.y1.max(o.y1),
        )
    }

    /// Scaled by `sx` and `sy`, then moved by `dx` and `dy`.
    pub fn map(&self, sx: f64, sy: f64, dx: f64, dy: f64) -> Rect {
        Rect::new(
            self.x0 * sx + dx,
            self.y0 * sy + dy,
            self.x1 * sx + dx,
            self.y1 * sy + dy,
        )
    }

    /// The union of `rects`, if there are any.
    pub fn union_all<'a>(rects: impl IntoIterator<Item = &'a Rect>) -> Option<Rect> {
        rects.into_iter().fold(None, |acc, r| match acc {
            None => Some(*r),
            Some(a) => Some(a.union(r)),
        })
    }
}

/// The finest level at which a reading has real geometry (13 §13.4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Granularity {
    Word,
    Line,
    Block,
    Page,
}

impl Granularity {
    pub fn as_str(self) -> &'static str {
        match self {
            Granularity::Word => "word",
            Granularity::Line => "line",
            Granularity::Block => "block",
            Granularity::Page => "page",
        }
    }
}

/// What a block holds (13 §13.4.5). The first four are text and become
/// TextBlocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockKind {
    Text,
    Heading,
    Caption,
    Table,
    Illustration,
    Separator,
}

impl BlockKind {
    pub fn is_text(self) -> bool {
        matches!(
            self,
            BlockKind::Text | BlockKind::Heading | BlockKind::Caption | BlockKind::Table
        )
    }
}

/// Reading direction of a block or line (13 §13.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Dir {
    #[default]
    #[serde(rename = "ltr")]
    Ltr,
    #[serde(rename = "rtl")]
    Rtl,
    /// Vertical lines read top to bottom, right to left.
    #[serde(rename = "ttb-rl")]
    TtbRl,
}

/// The direction a page's script reads in when the engine gives none:
/// right to left for Hebrew and Yiddish (and the Hebrew and Arabic
/// scripts), left to right otherwise. `language` is ISO 639-3 and
/// `script` ISO 15924.
pub fn default_dir(language: &str, script: Option<&str>) -> Dir {
    match (language, script) {
        (_, Some("Hebr" | "Arab")) | ("heb" | "yid", _) => Dir::Rtl,
        _ => Dir::Ltr,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub rect: Rect,
    pub text: String,
    /// 0 to 1.
    pub conf: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub rect: Rect,
    pub words: Vec<Word>,
    /// The line's last word continues on the next line; this is the
    /// hyphen printed at its end (ALTO's `HYP`), usually `-`.
    pub hyphen: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub kind: BlockKind,
    pub rect: Rect,
    /// ISO 639-3, when the engine gave the block its own language.
    pub lang: Option<String>,
    /// Empty for an illustration or a separator.
    pub lines: Vec<Line>,
}

/// One page in reading order, in pixels of a frame `width` by `height`.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub granularity: Granularity,
    pub blocks: Vec<Block>,
}

impl Page {
    /// The same page in a frame `width` by `height`: the engine read a
    /// scaled copy, and the stored ALTO is in the full image's pixels.
    pub fn rescale(&mut self, width: f64, height: f64) {
        let (sx, sy) = (width / self.width, height / self.height);
        for b in &mut self.blocks {
            b.rect = b.rect.map(sx, sy, 0.0, 0.0);
            for l in &mut b.lines {
                l.rect = l.rect.map(sx, sy, 0.0, 0.0);
                for w in &mut l.words {
                    w.rect = w.rect.map(sx, sy, 0.0, 0.0);
                }
            }
        }
        self.width = width;
        self.height = height;
    }

    /// A page whose only geometry is the page itself: one page-sized text
    /// block holding `text`, a line per line of it, for an engine that
    /// returns text alone (13 §13.4.4).
    pub fn text_only(width: f64, height: f64, text: &str) -> Page {
        let rect = Rect::new(0.0, 0.0, width, height);
        let block = crate::boxes::text_block(BlockKind::Text, rect, None, text);
        Page {
            width,
            height,
            granularity: Granularity::Page,
            blocks: block.into_iter().collect(),
        }
    }

    /// Counts of text blocks, lines and words.
    pub fn counts(&self) -> (usize, usize, usize) {
        let text = self.blocks.iter().filter(|b| b.kind.is_text());
        let (mut blocks, mut lines, mut words) = (0, 0, 0);
        for b in text {
            blocks += 1;
            lines += b.lines.len();
            words += b.lines.iter().map(|l| l.words.len()).sum::<usize>();
        }
        (blocks, lines, words)
    }

    /// The mean word confidence, over the words that have one.
    pub fn mean_conf(&self) -> Option<f64> {
        let confs: Vec<f64> = self
            .blocks
            .iter()
            .flat_map(|b| &b.lines)
            .flat_map(|l| &l.words)
            .filter_map(|w| w.conf)
            .collect();
        (!confs.is_empty()).then(|| confs.iter().sum::<f64>() / confs.len() as f64)
    }
}

/// The page's text in reading order (a port of the survey's
/// `altotext.py`): a line's words joined by spaces, with its hyphen at the
/// end, one line per line, and a blank line between text blocks.
/// Illustrations and separators hold no text. The worker normalizes this
/// with `normalize_ocr`, which rejoins the hyphenated words.
pub fn text(page: &Page) -> String {
    let mut blocks = Vec::new();
    for b in page.blocks.iter().filter(|b| b.kind.is_text()) {
        let mut lines = Vec::new();
        for l in &b.lines {
            let mut s = l
                .words
                .iter()
                .map(|w| w.text.as_str())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if s.is_empty() {
                continue;
            }
            if let Some(h) = &l.hyphen {
                s.push_str(h);
            }
            lines.push(s);
        }
        if !lines.is_empty() {
            blocks.push(lines.join("\n"));
        }
    }
    blocks.join("\n\n")
}
