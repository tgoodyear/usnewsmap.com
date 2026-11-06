//! Reading order (13 §13.4.5): a column-aware XY cut on block boxes for
//! engines that give none, and the order of lines and words by direction.

use std::cmp::Ordering;

use crate::model::{Dir, Line, Page, Rect, Word};

/// The order to read `rects` in, as indices: a recursive XY cut that tries
/// a cut between columns first, so the lines of a 6 to 8 column page aren't
/// read across the columns, then a cut between rows (a headline over its
/// columns). Columns run left to right, or right to left for `rtl` and
/// `ttb-rl`; rows run top to bottom. Boxes that no cut separates are read
/// top to bottom, then along the direction.
pub fn xy_cut(rects: &[Rect], dir: Dir) -> Vec<usize> {
    let mut out = Vec::with_capacity(rects.len());
    let ids: Vec<usize> = (0..rects.len()).collect();
    cut(rects, ids, dir, &mut out);
    out
}

fn cut(rects: &[Rect], ids: Vec<usize>, dir: Dir, out: &mut Vec<usize>) {
    if ids.len() <= 1 {
        out.extend(ids);
        return;
    }
    let mut cols = split(rects, &ids, |r| (r.x0, r.x1));
    if cols.len() > 1 {
        if dir != Dir::Ltr {
            cols.reverse();
        }
        for g in cols {
            cut(rects, g, dir, out);
        }
        return;
    }
    let rows = split(rects, &ids, |r| (r.y0, r.y1));
    if rows.len() > 1 {
        for g in rows {
            cut(rects, g, dir, out);
        }
        return;
    }
    let mut ids = ids;
    ids.sort_by(|&a, &b| {
        let (ra, rb) = (&rects[a], &rects[b]);
        let along = match dir {
            Dir::Ltr => ra.x0.total_cmp(&rb.x0),
            Dir::Rtl | Dir::TtbRl => rb.x1.total_cmp(&ra.x1),
        };
        ra.y0.total_cmp(&rb.y0).then(along)
    });
    out.extend(ids);
}

/// `ids` in groups separated by gaps in their projection on one axis, in
/// increasing order along it.
fn split(rects: &[Rect], ids: &[usize], span: impl Fn(&Rect) -> (f64, f64)) -> Vec<Vec<usize>> {
    let mut sorted = ids.to_vec();
    sorted.sort_by(|&a, &b| span(&rects[a]).0.total_cmp(&span(&rects[b]).0));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut end = f64::NEG_INFINITY;
    for i in sorted {
        let (s, e) = span(&rects[i]);
        match groups.last_mut() {
            Some(g) if s < end => g.push(i),
            _ => groups.push(vec![i]),
        }
        end = end.max(e);
    }
    groups
}

/// `a` against `b` along the direction: left to right, right to left, or
/// top to bottom for vertical lines.
fn along(a: &Rect, b: &Rect, dir: Dir) -> Ordering {
    match dir {
        Dir::Ltr => a.x0.total_cmp(&b.x0),
        Dir::Rtl => b.x1.total_cmp(&a.x1),
        Dir::TtbRl => a.y0.total_cmp(&b.y0),
    }
}

/// Words along the line's direction.
pub fn sort_words(words: &mut [Word], dir: Dir) {
    words.sort_by(|a, b| along(&a.rect, &b.rect, dir));
}

/// The most lines one row holds. A longer run starts another row, which
/// bounds the work on a malformed block; no printed row has this many
/// segments.
const MAX_ROW: usize = 256;

/// Lines across the block: rows top to bottom, or right to left for
/// vertical lines, and the lines of a row along the direction. A reader
/// that splits a printed line into segments, as PP-OCR's detector does,
/// gives them tops a pixel or two apart, so the tops alone would order them
/// by that jitter. A line joins the row before it only when it shares the
/// row with every line in it (`same_row`), so neither a chain of overlaps
/// nor a tall line beside a column can make a column one row. Stacked lines
/// keep the order of their tops.
pub fn sort_lines(lines: &mut [Line], dir: Dir) {
    match dir {
        Dir::Ltr | Dir::Rtl => lines.sort_by(|a, b| a.rect.y0.total_cmp(&b.rect.y0)),
        Dir::TtbRl => lines.sort_by(|a, b| b.rect.x1.total_cmp(&a.rect.x1)),
    }
    let mut start = 0;
    for i in 1..=lines.len() {
        let joins = i < lines.len()
            && i - start < MAX_ROW
            && lines[start..i]
                .iter()
                .all(|m| same_row(&m.rect, &lines[i].rect, dir));
        if !joins {
            lines[start..i].sort_by(|a, b| along(&a.rect, &b.rect, dir));
            start = i;
        }
    }
}

/// Whether two lines are parts of one row (one column of vertical lines):
/// their extents across the lines overlap by more than half the smaller
/// one, and along the lines by no more than half the shorter one, since
/// lines stacked in a column overlap there. A box that isn't finite or has
/// no area shares no row.
fn same_row(a: &Rect, b: &Rect, dir: Dir) -> bool {
    let sound = |r: &Rect| {
        [r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) && r.x1 > r.x0 && r.y1 > r.y0
    };
    if !(sound(a) && sound(b)) {
        return false;
    }
    let (y, x) = ((a.y0, a.y1, b.y0, b.y1), (a.x0, a.x1, b.x0, b.x1));
    let (across, lengthwise) = match dir {
        Dir::Ltr | Dir::Rtl => (y, x),
        Dir::TtbRl => (x, y),
    };
    let (shared, half) = overlap(across);
    let (stacked, half_length) = overlap(lengthwise);
    shared > half && stacked <= half_length
}

/// How far two extents `(a0, a1, b0, b1)` overlap (negative for a gap),
/// and half the shorter one.
fn overlap((a0, a1, b0, b1): (f64, f64, f64, f64)) -> (f64, f64) {
    (a1.min(b1) - a0.max(b0), (a1 - a0).min(b1 - b0) / 2.0)
}

/// Puts `page`'s blocks in XY-cut order.
pub fn order_blocks(page: &mut Page, dir: Dir) {
    let rects: Vec<Rect> = page.blocks.iter().map(|b| b.rect).collect();
    let order = xy_cut(&rects, dir);
    let mut old: Vec<Option<crate::model::Block>> = page.blocks.drain(..).map(Some).collect();
    page.blocks = order.into_iter().filter_map(|i| old[i].take()).collect();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
        Rect::new(x0, y0, x1, y1)
    }

    #[test]
    fn reads_columns_down_then_across() {
        // A masthead, then three columns of two blocks each, given in
        // detection order (row by row).
        let rects = [
            r(0.0, 0.0, 300.0, 50.0),
            r(0.0, 100.0, 90.0, 200.0),
            r(100.0, 100.0, 190.0, 180.0),
            r(200.0, 100.0, 300.0, 150.0),
            r(0.0, 210.0, 90.0, 300.0),
            r(100.0, 190.0, 190.0, 300.0),
            r(200.0, 160.0, 300.0, 300.0),
        ];
        assert_eq!(xy_cut(&rects, Dir::Ltr), vec![0, 1, 4, 2, 5, 3, 6]);
        assert_eq!(xy_cut(&rects, Dir::Rtl), vec![0, 3, 6, 2, 5, 1, 4]);
        assert_eq!(xy_cut(&rects, Dir::TtbRl), vec![0, 3, 6, 2, 5, 1, 4]);
    }

    #[test]
    fn a_headline_across_two_columns() {
        let rects = [
            r(0.0, 0.0, 90.0, 100.0),
            r(100.0, 0.0, 190.0, 100.0),
            r(0.0, 110.0, 190.0, 130.0),
            r(0.0, 140.0, 90.0, 300.0),
            r(100.0, 140.0, 190.0, 300.0),
            r(200.0, 0.0, 300.0, 300.0),
        ];
        assert_eq!(xy_cut(&rects, Dir::Ltr), vec![0, 1, 2, 3, 4, 5]);
        // Overlapping boxes: top to bottom.
        let rects = [r(0.0, 50.0, 100.0, 90.0), r(10.0, 0.0, 90.0, 60.0)];
        assert_eq!(xy_cut(&rects, Dir::Ltr), vec![1, 0]);
        assert!(xy_cut(&[], Dir::Ltr).is_empty());
    }

    fn line(text: &str, x0: f64, y0: f64, x1: f64, y1: f64) -> Line {
        let rect = r(x0, y0, x1, y1);
        Line {
            rect,
            words: vec![Word {
                rect,
                text: text.into(),
                conf: None,
            }],
            hyphen: None,
        }
    }

    fn sorted(mut lines: Vec<Line>, dir: Dir) -> Vec<String> {
        sort_lines(&mut lines, dir);
        lines.into_iter().map(|l| l.words[0].text.clone()).collect()
    }

    #[test]
    fn reads_a_split_row_left_to_right() {
        // One printed line in two segments, the right one a pixel higher,
        // then the next line.
        let lines = vec![
            line("next", 0.0, 125.0, 90.0, 145.0),
            line("right", 110.0, 100.0, 200.0, 120.0),
            line("left", 0.0, 101.0, 100.0, 121.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["left", "right", "next"]);
    }

    #[test]
    fn reads_a_split_row_right_to_left() {
        let lines = vec![
            line("next", 100.0, 125.0, 200.0, 145.0),
            line("left", 0.0, 100.0, 100.0, 120.0),
            line("right", 110.0, 101.0, 200.0, 121.0),
        ];
        assert_eq!(sorted(lines, Dir::Rtl), ["right", "left", "next"]);
        // A vertical line in two segments, the lower one a pixel further
        // right, then the line to its left.
        let lines = vec![
            line("next", 70.0, 0.0, 90.0, 200.0),
            line("top", 101.0, 0.0, 121.0, 100.0),
            line("bottom", 100.0, 110.0, 122.0, 200.0),
        ];
        assert_eq!(sorted(lines, Dir::TtbRl), ["top", "bottom", "next"]);
    }

    #[test]
    fn keeps_stacked_lines_top_to_bottom() {
        // Indented lines whose padded boxes overlap, the last two by more
        // than half a line.
        let lines = vec![
            line("c", 10.0, 30.0, 200.0, 50.0),
            line("a", 20.0, 0.0, 200.0, 20.0),
            line("d", 0.0, 38.0, 200.0, 58.0),
            line("b", 0.0, 15.0, 200.0, 35.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["a", "b", "c", "d"]);
        // A tall line beside the column overlaps every line in it, but
        // joins only the first row: the column's lines share no row.
        let lines = vec![
            line("tall", 210.0, 0.0, 300.0, 70.0),
            line("a", 20.0, 0.0, 200.0, 20.0),
            line("b", 0.0, 25.0, 200.0, 45.0),
            line("c", 10.0, 50.0, 200.0, 70.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["a", "tall", "b", "c"]);
    }

    #[test]
    fn a_near_overlap_stays_two_rows() {
        // Side by side, the left line lower by just over half its height.
        let lines = vec![
            line("low", 0.0, 10.5, 100.0, 30.5),
            line("high", 110.0, 0.0, 200.0, 20.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["high", "low"]);
        // Exactly half isn't more than half.
        let lines = vec![
            line("low", 0.0, 10.0, 100.0, 30.0),
            line("high", 110.0, 0.0, 200.0, 20.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["high", "low"]);
        let lines = vec![
            line("low", 0.0, 9.5, 100.0, 29.5),
            line("high", 110.0, 0.0, 200.0, 20.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["low", "high"]);
    }

    #[test]
    fn degenerate_boxes_keep_their_top_order() {
        let lines = vec![
            line("nan", 0.0, f64::NAN, 100.0, 20.0),
            line("flat", 110.0, 5.0, 200.0, 5.0),
            line("line", 0.0, 5.0, 100.0, 25.0),
            line("infinite", 110.0, 4.0, f64::INFINITY, 24.0),
        ];
        assert_eq!(sorted(lines, Dir::Ltr), ["infinite", "flat", "line", "nan"]);
    }
}
