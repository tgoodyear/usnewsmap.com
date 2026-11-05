//! Column repair (#925): lines an engine read across a narrow gutter, cut at
//! the gutter, and the page read column by column.
//!
//! Tesseract's page layout (PSM 3) separates columns at white space and
//! rules. Where a gutter is narrow (about half a line height or less) and has
//! no rule, it can miss it and make one block whose lines run across two or
//! more columns, so the page text reads a line of one column, then the
//! same line of the next. The stored ALTO and the text hold exactly what the
//! engine read, and an XY cut of whole blocks ([`crate::order`]) can't help,
//! because the lines themselves cross the gutter.
//!
//! [`split_columns`] finds such gutters from the geometry alone. A gutter is
//! an x-range where most of a block's lines have no word, which lines on both
//! sides cross with only a narrow gap there (a merged pair of justified column
//! lines), with line ends aligned on both sides of it, and which no body-size
//! line prints across. The columns it separates must be at least four line
//! heights wide and hold two words on each side, so a table's narrow cells or
//! a river in one column aren't read as columns.
//! Lines of display type (a masthead or headline set larger) may cross it
//! and stay whole. Each line that crosses a gutter is cut there, its words
//! going to the side their centre is on; then the run of blocks that held
//! cut lines is read column by column, top to bottom within a column, by an
//! XY cut that takes a page-wide row (a headline) before the columns under
//! it.
//!
//! Every word is kept, with its box, text and confidence; only the lines and
//! blocks around it change. A word the engine itself read across the gutter
//! (two words fused into one token) can't be divided and goes to the side its
//! centre is on. A page without a gutter is returned as it was.

use std::collections::BTreeMap;

use crate::model::{Block, Dir, Granularity, Line, Page, Rect};

/// Words narrower than this many line heights (a rule read as `|`, stray
/// punctuation) don't count as printing in a line.
const THIN: f64 = 0.2;
/// The narrowest gutter, in line heights: the width of the window the
/// evidence for one is counted in.
const MIN_GAP: f64 = 0.2;
/// How far around a block's lines, in line heights, other lines count as
/// evidence, and how far a gutter reaches past the last line that showed it.
const BAND: f64 = 8.0;
/// The evidence a gutter needs: lines that cross it with a gap there, plus
/// line ends aligned on both sides of it.
const SUPPORT: usize = 3;
/// Lines that cross a gutter with a gap there, at least: a lone merged line
/// isn't enough.
const MIN_STRADDLERS: usize = 2;
/// A line whose words are more than this many times the usual height is
/// display type (a masthead, a headline) and may cross a gutter.
const DISPLAY: f64 = 1.5;
/// How many supporting lines one body line printed across a gutter
/// outweighs.
const RATIO: usize = 4;
/// The widest gap, in line heights, a line crossing a gutter may have there:
/// two justified column lines read as one leave a narrow gap, a table row or
/// a tab stop a wide one.
const MAX_GAP: f64 = 2.0;
/// How near the window, in line heights, a line's end must be to count as
/// aligned with a gutter.
const EDGE: f64 = 0.5;
/// The narrowest column, in line heights: the median width of the words on
/// each side of a gutter in the lines that cross it.
const MIN_COL: f64 = 4.0;
/// The fewest words on each side of a gutter, in the median line crossing it.
const MIN_WORDS: f64 = 2.0;

/// What [`split_columns`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Repair {
    /// Gutters found.
    pub gutters: usize,
    /// Lines cut at a gutter.
    pub lines_split: usize,
    /// Text blocks that became more than one block.
    pub blocks_split: usize,
}

/// `page` with lines that cross a narrow gutter cut at it, and the blocks
/// around them read column by column, or `None` if no line crosses one.
///
/// Only a left-to-right page with word boxes is repaired: a right-to-left or
/// vertical page, or one without word boxes, is returned as `None`.
/// Illustrations and separators keep their place.
pub fn split_columns(page: &Page, dir: Dir) -> Option<(Page, Repair)> {
    if dir != Dir::Ltr || page.granularity != Granularity::Word {
        return None;
    }
    // The text blocks, in order: the sequence the pass works on.
    let seq: Vec<usize> = (0..page.blocks.len())
        .filter(|&i| page.blocks[i].kind.is_text())
        .collect();
    let blocks: Vec<&Block> = seq.iter().map(|&i| &page.blocks[i]).collect();
    let h = median(
        blocks
            .iter()
            .flat_map(|b| &b.lines)
            .filter(|l| !l.words.is_empty())
            .map(|l| l.rect.height())
            .collect(),
    )?;
    if !(h.is_finite() && h > 0.0) {
        return None;
    }
    // Each text line's geometry, and where it is in `info` by block and line.
    let mut info = Vec::new();
    let mut by_line: Vec<Vec<Option<usize>>> = Vec::with_capacity(blocks.len());
    for b in &blocks {
        let mut ids = Vec::with_capacity(b.lines.len());
        for (li, l) in b.lines.iter().enumerate() {
            ids.push(Info::of(li, l, h).map(|o| {
                info.push(o);
                info.len() - 1
            }));
        }
        by_line.push(ids);
    }
    let wh = median(info.iter().map(|o| o.wh).collect())?;
    let mine =
        |bi: usize| -> Vec<&Info> { by_line[bi].iter().flatten().map(|&i| &info[i]).collect() };

    let mut found = Vec::new();
    for bi in 0..blocks.len() {
        let mine = mine(bi);
        if mine.is_empty() {
            continue;
        }
        let k = mine.len() / 3;
        let occs: Vec<&[(f64, f64)]> = mine.iter().map(|o| o.occ.as_slice()).collect();
        for (c, d) in candidates(&occs, k, MIN_GAP * h) {
            found.extend(confirm(&info, &mine, c, d, h));
        }
    }
    let gutters = guard(merge(found), &info, h);
    if gutters.is_empty() {
        return None;
    }

    let mut lines_split = 0;
    let mut split = vec![false; blocks.len()];
    let mut per_block: Vec<Vec<Unit>> = Vec::with_capacity(blocks.len());
    for (bi, b) in blocks.iter().enumerate() {
        let mine = mine(bi);
        let gs = applied(&gutters, &mine, wh);
        if gs.is_empty() {
            let rect = Rect::union_all(
                b.lines
                    .iter()
                    .flat_map(|l| &l.words)
                    .filter(|w| !thin(&w.rect, h))
                    .map(|w| &w.rect),
            )
            .unwrap_or(b.rect);
            per_block.push(vec![Unit {
                rect,
                block: (*b).clone(),
            }]);
            continue;
        }
        let mut mids: Vec<f64> = gs.iter().map(|g| g.mid).collect();
        mids.sort_by(f64::total_cmp);
        let mut bands: Vec<Band> = Vec::new();
        for (li, l) in b.lines.iter().enumerate() {
            if l.words.is_empty() {
                continue;
            }
            let pieces = match by_line[bi][li].map(|i| &info[i]) {
                None => vec![l.clone()],
                Some(o) => {
                    let pieces = cut_line(l, li, o, &gs, wh);
                    if pieces.len() > 1 {
                        lines_split += 1;
                        split[bi] = true;
                    }
                    pieces
                }
            };
            for p in pieces {
                let core = core(&p, &gs, h);
                let (x0, x1) = (core.x0, core.x1);
                if gs.iter().any(|g| x0 < g.lo && x1 > g.hi) {
                    match bands.last_mut() {
                        Some(Band::Span(ps)) => ps.push((p, core)),
                        _ => bands.push(Band::Span(vec![(p, core)])),
                    }
                } else {
                    let cx = (x0 + x1) / 2.0;
                    let bin = mids.iter().filter(|&&m| cx > m).count();
                    if !matches!(bands.last(), Some(Band::Bins(_))) {
                        bands.push(Band::Bins(BTreeMap::new()));
                    }
                    if let Some(Band::Bins(bins)) = bands.last_mut() {
                        bins.entry(bin).or_default().push((p, core));
                    }
                }
            }
        }
        let groups = bands.into_iter().flat_map(|band| match band {
            Band::Span(ps) => vec![ps],
            Band::Bins(bins) => bins.into_values().collect(),
        });
        let mut units: Vec<Unit> = groups
            .filter_map(|ps| {
                let rect = Rect::union_all(ps.iter().map(|(_, c)| c))?;
                let lines: Vec<Line> = ps.into_iter().map(|(l, _)| l).collect();
                let outer = Rect::union_all(lines.iter().map(|l| &l.rect))?;
                Some(Unit {
                    rect,
                    block: Block {
                        kind: b.kind,
                        rect: outer,
                        lang: b.lang.clone(),
                        lines,
                    },
                })
            })
            .collect();
        if !split[bi] && units.len() == 1 {
            units[0].block = (*b).clone();
        }
        per_block.push(units);
    }
    if lines_split == 0 {
        return None;
    }

    let runs = runs(&per_block, &split, h);
    let mut run_of = vec![None; blocks.len()];
    for (r, &(lo, hi)) in runs.iter().enumerate() {
        for slot in &mut run_of[lo..=hi] {
            *slot = Some(r);
        }
    }
    let blocks_split = per_block.iter().filter(|u| u.len() > 1).count();
    let mut per_block: Vec<Option<Vec<Unit>>> = per_block.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(page.blocks.len() + lines_split);
    let mut next = 0;
    for b in &page.blocks {
        if !b.kind.is_text() {
            out.push(b.clone());
            continue;
        }
        let j = next;
        next += 1;
        match run_of[j] {
            None => out.extend(per_block[j].take().into_iter().flatten().map(|u| u.block)),
            Some(r) if runs[r].0 == j => {
                let (lo, hi) = runs[r];
                let units: Vec<Unit> = (lo..=hi)
                    .flat_map(|k| per_block[k].take().into_iter().flatten())
                    .collect();
                let rects: Vec<Rect> = units.iter().map(|u| u.rect).collect();
                let mut units: Vec<Option<Unit>> = units.into_iter().map(Some).collect();
                out.extend(
                    xy_cut(&rects)
                        .into_iter()
                        .filter_map(|i| units[i].take().map(|u| u.block)),
                );
            }
            Some(_) => {}
        }
    }
    Some((
        Page {
            blocks: out,
            ..page.clone()
        },
        Repair {
            gutters: gutters.len(),
            lines_split,
            blocks_split,
        },
    ))
}

/// A line's geometry as the pass reads it.
struct Info {
    line: usize,
    /// Where the line prints: its words' x-extents (thin ones left out),
    /// merged where they touch.
    occ: Vec<(f64, f64)>,
    /// From the first word's left to the last word's right.
    ext: (f64, f64),
    cy: f64,
    /// The median height of the line's words.
    wh: f64,
    /// The words' boxes, thin ones left out.
    words: Vec<Rect>,
}

impl Info {
    fn of(line: usize, l: &Line, h: f64) -> Option<Info> {
        let words: Vec<Rect> = l
            .words
            .iter()
            .map(|w| w.rect)
            .filter(|r| !thin(r, h))
            .collect();
        let mut iv: Vec<(f64, f64)> = words.iter().map(|r| (r.x0, r.x1)).collect();
        iv.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        let mut occ: Vec<(f64, f64)> = Vec::new();
        for (a, b) in iv {
            match occ.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => occ.push((a, b)),
            }
        }
        let ext = (occ.first()?.0, occ.last()?.1);
        Some(Info {
            line,
            occ,
            ext,
            cy: l.rect.centre().1,
            wh: median(words.iter().map(|r| r.height()).collect())?,
            words,
        })
    }

    fn covers(&self, a: f64, b: f64) -> bool {
        self.occ.iter().any(|&(x0, x1)| x0 < b && x1 > a)
    }

    fn straddles(&self, a: f64, b: f64) -> bool {
        self.ext.0 < a && self.ext.1 > b
    }

    /// The width of the gap in the line around `[a, b]`.
    fn gap_at(&self, a: f64, b: f64) -> f64 {
        let left = self
            .occ
            .iter()
            .filter(|o| o.1 <= a)
            .map(|o| o.1)
            .max_by(f64::total_cmp);
        let right = self
            .occ
            .iter()
            .filter(|o| o.0 >= b)
            .map(|o| o.0)
            .min_by(f64::total_cmp);
        match (left, right) {
            (Some(l), Some(r)) => r - l,
            _ => f64::INFINITY,
        }
    }
}

#[derive(Debug, Clone)]
struct Gutter {
    score: i64,
    lo: f64,
    hi: f64,
    /// The lines it applies to, by their centre.
    y0: f64,
    y1: f64,
}

impl Gutter {
    fn mid(&self) -> f64 {
        (self.lo + self.hi) / 2.0
    }

    fn holds(&self, y: f64) -> bool {
        self.y0 <= y && y <= self.y1
    }
}

/// A gutter as one block takes it.
struct Applied {
    lo: f64,
    hi: f64,
    mid: f64,
    y0: f64,
    y1: f64,
    /// The block's first and last line that showed it, if any did.
    members: Option<(usize, usize)>,
}

/// Lines, each with the box of the words that place it, read together.
enum Band {
    /// Lines across a gutter, kept together.
    Span(Vec<(Line, Rect)>),
    /// Lines between gutters, by column.
    Bins(BTreeMap<usize, Vec<(Line, Rect)>>),
}

/// A block of the result, with the box it is ordered by.
struct Unit {
    rect: Rect,
    block: Block,
}

fn thin(r: &Rect, h: f64) -> bool {
    r.width() < THIN * h
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// The maximal x-ranges at least `min_gap` wide that at most `k` of `occs`
/// print in, strictly inside their extent.
fn candidates(occs: &[&[(f64, f64)]], k: usize, min_gap: f64) -> Vec<(f64, f64)> {
    let mut ev: Vec<(f64, i64)> = occs
        .iter()
        .flat_map(|o| o.iter().flat_map(|&(x0, x1)| [(x0, 1), (x1, -1)]))
        .collect();
    ev.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let (Some(&(lo, _)), Some(&(hi, _))) = (ev.first(), ev.last()) else {
        return Vec::new();
    };
    let mut out: Vec<(f64, f64)> = Vec::new();
    let (mut cov, mut x) = (0i64, lo);
    for &(pos, d) in &ev {
        if pos > x && cov <= k as i64 && lo < x && pos < hi {
            // `x` is the previous event's position, copied, so a range that
            // goes on from the last one starts exactly at its end.
            match out.last_mut() {
                Some(last) if last.1 == x => last.1 = pos,
                _ => out.push((x, pos)),
            }
        }
        cov += d;
        x = pos;
    }
    out.retain(|&(a, b)| b - a >= min_gap);
    out
}

/// The best gutter in `[c, d]`, a candidate of the block whose lines are
/// `mine`, by the evidence of the lines around it.
fn confirm(info: &[Info], mine: &[&Info], c: f64, d: f64, h: f64) -> Option<Gutter> {
    let w = MIN_GAP * h;
    let y0 = mine.iter().map(|o| o.cy).min_by(f64::total_cmp)? - BAND * h;
    let y1 = mine.iter().map(|o| o.cy).max_by(f64::total_cmp)? + BAND * h;
    let band: Vec<&Info> = info.iter().filter(|o| y0 <= o.cy && o.cy <= y1).collect();
    let ctx: Vec<&Info> = band
        .iter()
        .copied()
        .filter(|o| o.ext.0 < d && o.ext.1 > c)
        .collect();
    let hc = median(ctx.iter().map(|o| o.wh).collect())?;
    let mut starts = vec![c, d - w];
    for o in &ctx {
        for &(x0, x1) in &o.occ {
            starts.extend([x1, x0 - w].into_iter().filter(|&x| c <= x && x <= d - w));
        }
    }
    starts.sort_by(f64::total_cmp);
    starts.dedup();
    // (score, start, first and last centre of the lines that showed it)
    let mut scored: Vec<(i64, f64, f64, f64)> = Vec::new();
    for a in starts {
        let b = a + w;
        let st: Vec<&Info> = ctx.iter().copied().filter(|o| o.straddles(a, b)).collect();
        let free: Vec<&Info> = st
            .iter()
            .copied()
            .filter(|o| !o.covers(a, b) && o.gap_at(a, b) <= MAX_GAP * h)
            .collect();
        let s = free.len();
        let blocked = st
            .iter()
            .filter(|o| o.covers(a, b) && o.wh <= DISPLAY * hc)
            .count();
        let e = if s >= 1 {
            let left = band
                .iter()
                .filter(|o| a - EDGE * h <= o.ext.1 && o.ext.1 <= a + w / 2.0)
                .count();
            let right = band
                .iter()
                .filter(|o| b - w / 2.0 <= o.ext.0 && o.ext.0 <= b + EDGE * h)
                .count();
            left.min(right)
        } else {
            0
        };
        let support = s + e;
        if s >= MIN_STRADDLERS && support >= SUPPORT && blocked * RATIO <= support {
            let fy0 = free.iter().map(|o| o.cy).min_by(f64::total_cmp)?;
            let fy1 = free.iter().map(|o| o.cy).max_by(f64::total_cmp)?;
            scored.push((support as i64 - (RATIO * blocked) as i64, a, fy0, fy1));
        }
    }
    let top = scored.iter().map(|s| s.0).max()?;
    // The best windows next to each other: the gutter is their middle.
    let mut run: Vec<&(i64, f64, f64, f64)> = Vec::new();
    for s in &scored {
        if s.0 == top {
            if run.last().is_some_and(|last| s.1 - last.1 > w) {
                break;
            }
            run.push(s);
        } else if !run.is_empty() {
            break;
        }
    }
    let (first, last) = (run.first()?, run.last()?);
    let mid = (first.1 + last.1 + w) / 2.0;
    Some(Gutter {
        score: top,
        lo: mid - w / 2.0,
        hi: mid + w / 2.0,
        y0: run.iter().map(|s| s.2).min_by(f64::total_cmp)? - BAND * h,
        y1: run.iter().map(|s| s.3).max_by(f64::total_cmp)? + BAND * h,
    })
}

/// Gutters that overlap in x and y, directly or through others, as one: the
/// union of their lines, at the best one's place.
fn merge(mut found: Vec<Gutter>) -> Vec<Gutter> {
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    found.sort_by(|a, b| a.lo.total_cmp(&b.lo));
    let n = found.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (&found[i], &found[j]);
            if a.lo < b.hi && a.hi > b.lo && a.y0 < b.y1 && a.y1 > b.y0 {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri.max(rj)] = ri.min(rj);
            }
        }
    }
    let mut out: Vec<Gutter> = Vec::new();
    let mut at: BTreeMap<usize, usize> = BTreeMap::new();
    for (i, g) in found.into_iter().enumerate() {
        let r = root(&mut parent, i);
        match at.get(&r) {
            Some(&k) => {
                let o = &mut out[k];
                o.y0 = o.y0.min(g.y0);
                o.y1 = o.y1.max(g.y1);
                if g.score > o.score {
                    (o.lo, o.hi, o.score) = (g.lo, g.hi, g.score);
                }
            }
            None => {
                at.insert(r, out.len());
                out.push(g);
            }
        }
    }
    out
}

/// The gutters between real columns: in the lines that cross one with a gap
/// there, the words on each side (up to the next gutter) are a column wide
/// and more than a cell or two.
fn guard(gutters: Vec<Gutter>, info: &[Info], h: f64) -> Vec<Gutter> {
    let keep: Vec<bool> = gutters
        .iter()
        .enumerate()
        .map(|(gi, g)| {
            let m = g.mid();
            let (mut widths, mut counts) = (Vec::new(), Vec::new());
            for o in info {
                if !(g.holds(o.cy) && o.straddles(g.lo, g.hi) && !o.covers(g.lo, g.hi)) {
                    continue;
                }
                let others = gutters
                    .iter()
                    .enumerate()
                    .filter(|&(oi, n)| {
                        oi != gi && n.holds(o.cy) && o.ext.0 < n.mid() && n.mid() < o.ext.1
                    })
                    .map(|(_, n)| n.mid());
                let left = others
                    .clone()
                    .filter(|&n| n < m)
                    .fold(f64::NEG_INFINITY, f64::max);
                let right = others.filter(|&n| n > m).fold(f64::INFINITY, f64::min);
                let side = |from: f64, to: f64| -> Vec<&Rect> {
                    o.words
                        .iter()
                        .filter(|r| {
                            let cx = r.centre().0;
                            from < cx && cx < to
                        })
                        .collect()
                };
                let (lw, rw) = (side(left, m), side(m, right));
                let span = |ws: &[&Rect]| {
                    let x0 = ws.iter().map(|r| r.x0).fold(f64::INFINITY, f64::min);
                    let x1 = ws.iter().map(|r| r.x1).fold(f64::NEG_INFINITY, f64::max);
                    x1 - x0
                };
                if lw.is_empty() || rw.is_empty() {
                    continue;
                }
                widths.push(span(&lw).min(span(&rw)));
                counts.push(lw.len().min(rw.len()) as f64);
            }
            median(widths).is_some_and(|w| w >= MIN_COL * h)
                && median(counts).is_some_and(|n| n >= MIN_WORDS)
        })
        .collect();
    gutters
        .into_iter()
        .zip(keep)
        .filter_map(|(g, k)| k.then_some(g))
        .collect()
}

/// The gutters a block takes: those that most of its body lines crossing
/// them do with a gap there, and those its lines lie on both sides of with
/// none crossing.
fn applied(gutters: &[Gutter], mine: &[&Info], wh: f64) -> Vec<Applied> {
    let mut out = Vec::new();
    for g in gutters {
        let m = g.mid();
        let inband: Vec<&Info> = mine.iter().copied().filter(|o| g.holds(o.cy)).collect();
        let st: Vec<&Info> = inband
            .iter()
            .copied()
            .filter(|o| o.ext.0 < m && m < o.ext.1)
            .collect();
        let members: Vec<usize> = st
            .iter()
            .filter(|o| o.wh <= DISPLAY * wh && !o.occ.iter().any(|&(x0, x1)| x0 < m && m < x1))
            .map(|o| o.line)
            .collect();
        let span = members
            .iter()
            .min()
            .copied()
            .zip(members.iter().max().copied());
        let takes = if span.is_some() {
            2 * members.len() >= st.len()
        } else {
            st.is_empty()
                && inband.iter().any(|o| o.ext.1 <= m)
                && inband.iter().any(|o| o.ext.0 >= m)
        };
        if takes {
            out.push(Applied {
                lo: g.lo,
                hi: g.hi,
                mid: m,
                y0: g.y0,
                y1: g.y1,
                members: span,
            });
        }
    }
    out
}

/// Line `li` of a block, `o` its geometry, cut at the gutters it crosses:
/// those its block's lines showed around it, and any for a body-size line.
/// Each word goes to the side its centre is on; the hyphen stays with the
/// line's last word.
fn cut_line(l: &Line, li: usize, o: &Info, gs: &[Applied], wh: f64) -> Vec<Line> {
    let mut cuts: Vec<f64> = gs
        .iter()
        .filter(|g| {
            (g.members.is_some_and(|(a, b)| a <= li && li <= b) || o.wh <= DISPLAY * wh)
                && g.y0 <= o.cy
                && o.cy <= g.y1
                && o.ext.0 < g.mid
                && g.mid < o.ext.1
        })
        .map(|g| g.mid)
        .collect();
    cuts.sort_by(f64::total_cmp);
    let mut parts: Vec<Vec<usize>> = vec![Vec::new(); cuts.len() + 1];
    for (i, w) in l.words.iter().enumerate() {
        let cx = w.rect.centre().0;
        parts[cuts.iter().filter(|&&m| cx > m).count()].push(i);
    }
    parts.retain(|p| !p.is_empty());
    if parts.len() <= 1 {
        return vec![l.clone()];
    }
    let last = l.words.len() - 1;
    parts
        .into_iter()
        .filter_map(|p| {
            let words: Vec<_> = p.iter().map(|&i| l.words[i].clone()).collect();
            Some(Line {
                rect: Rect::union_all(words.iter().map(|w| &w.rect))?,
                hyphen: if p.contains(&last) {
                    l.hyphen.clone()
                } else {
                    None
                },
                words,
            })
        })
        .collect()
}

/// The box that places a line among the columns: its words, less thin ones
/// and any word printed across a gutter (all of them if that leaves none).
fn core(l: &Line, gs: &[Applied], h: f64) -> Rect {
    let kept = l
        .words
        .iter()
        .map(|w| &w.rect)
        .filter(|r| !thin(r, h) && !gs.iter().any(|g| r.x0 < g.lo && r.x1 > g.hi));
    Rect::union_all(kept)
        .or_else(|| Rect::union_all(l.words.iter().map(|w| &w.rect)))
        .unwrap_or(l.rect)
}

/// The runs of blocks read again: each starts at a block with a cut line
/// and takes the blocks after it that had one or lie within its columns,
/// and the blocks before it within them that no other run took.
fn runs(per_block: &[Vec<Unit>], split: &[bool], h: f64) -> Vec<(usize, usize)> {
    let rect = |j: usize| Rect::union_all(per_block[j].iter().map(|u| &u.rect));
    let n = per_block.len();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        let Some(mut bx) = rect(i).filter(|_| split[i]) else {
            i += 1;
            continue;
        };
        let inside =
            |j: usize, bx: &Rect| rect(j).is_some_and(|r| r.x0 >= bx.x0 - h && r.x1 <= bx.x1 + h);
        let (mut lo, mut hi) = (i, i);
        while hi + 1 < n && (split[hi + 1] || inside(hi + 1, &bx)) {
            hi += 1;
            if let Some(r) = rect(hi) {
                bx = bx.union(&r);
            }
        }
        // Runs are found in order, so the blocks before `i` no run took are
        // those after the last run's end.
        let free = runs.last().map_or(0, |r| r.1 + 1);
        while lo > free && !split[lo - 1] && inside(lo - 1, &bx) {
            lo -= 1;
        }
        runs.push((lo, hi));
        i = hi + 1;
    }
    runs
}

/// The order to read the units of a run in: a cut between columns first,
/// then the top row alone (a headline over its columns) before the rest,
/// and boxes no cut separates top to bottom.
fn xy_cut(rects: &[Rect]) -> Vec<usize> {
    let mut out = Vec::with_capacity(rects.len());
    cut(rects, (0..rects.len()).collect(), &mut out);
    out
}

fn cut(rects: &[Rect], ids: Vec<usize>, out: &mut Vec<usize>) {
    if ids.len() <= 1 {
        out.extend(ids);
        return;
    }
    let cols = groups(rects, &ids, |r| (r.x0, r.x1));
    if cols.len() > 1 {
        for g in cols {
            cut(rects, g, out);
        }
        return;
    }
    let mut rows = groups(rects, &ids, |r| (r.y0, r.y1)).into_iter();
    if rows.len() > 1 {
        if let Some(first) = rows.next() {
            cut(rects, first, out);
        }
        cut(rects, rows.flatten().collect(), out);
        return;
    }
    let mut ids = ids;
    ids.sort_by(|&a, &b| {
        rects[a]
            .y0
            .total_cmp(&rects[b].y0)
            .then(rects[a].x0.total_cmp(&rects[b].x0))
    });
    out.extend(ids);
}

/// `ids` in groups separated by gaps in their projection on one axis.
fn groups(rects: &[Rect], ids: &[usize], span: impl Fn(&Rect) -> (f64, f64)) -> Vec<Vec<usize>> {
    let mut sorted = ids.to_vec();
    sorted.sort_by(|&a, &b| span(&rects[a]).0.total_cmp(&span(&rects[b]).0));
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut end = f64::NEG_INFINITY;
    for i in sorted {
        let (s, e) = span(&rects[i]);
        match out.last_mut() {
            Some(g) if s < end => g.push(i),
            _ => out.push(vec![i]),
        }
        end = end.max(e);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{text, BlockKind, Word};

    /// Line pitch and word height of the body type, in pixels.
    const PITCH: f64 = 30.0;
    const BODY: f64 = 22.0;
    const WIDTHS: [f64; 7] = [64.0, 88.0, 40.0, 112.0, 72.0, 96.0, 52.0];

    struct Layout {
        cols: usize,
        col_w: f64,
        gutter: f64,
        rows: usize,
        top: f64,
    }

    impl Layout {
        fn x0(&self, col: usize) -> f64 {
            60.0 + col as f64 * (self.col_w + self.gutter)
        }

        /// Row `row` of column `col`, justified to the column's width.
        fn words(&self, col: usize, row: usize) -> Vec<Word> {
            let mut widths = Vec::new();
            for k in 0.. {
                let w = WIDTHS[(col * 5 + row * 3 + k) % WIDTHS.len()];
                let used: f64 = widths.iter().sum::<f64>() + w + 12.0 * widths.len() as f64;
                if used > self.col_w {
                    break;
                }
                widths.push(w);
            }
            assert!(
                widths.len() >= 2,
                "a {} px column holds one word",
                self.col_w
            );
            let gap = (self.col_w - widths.iter().sum::<f64>()) / (widths.len() - 1) as f64;
            let y0 = self.top + row as f64 * PITCH;
            let mut x = self.x0(col);
            widths
                .iter()
                .enumerate()
                .map(|(i, &w)| {
                    let word = word(x, y0, x + w, BODY, &format!("c{col}r{row}w{i}"));
                    x += w + gap;
                    word
                })
                .collect()
        }

        /// The page as Tesseract reads it when it misses the gutters: one
        /// block whose lines each hold a row of every column.
        fn merged(&self) -> Block {
            block(
                (0..self.rows)
                    .map(|r| line((0..self.cols).flat_map(|c| self.words(c, r)).collect()))
                    .collect(),
            )
        }

        /// The text read column by column.
        fn expected(&self) -> String {
            (0..self.cols)
                .map(|c| {
                    (0..self.rows)
                        .map(|r| words_text(&self.words(c, r)))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }
    }

    fn word(x0: f64, y0: f64, x1: f64, h: f64, t: &str) -> Word {
        Word {
            rect: Rect::new(x0, y0, x1, y0 + h),
            text: t.into(),
            conf: Some(0.9),
        }
    }

    fn words_text(ws: &[Word]) -> String {
        ws.iter()
            .map(|w| w.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn line(words: Vec<Word>) -> Line {
        let r = Rect::union_all(words.iter().map(|w| &w.rect)).unwrap();
        Line {
            rect: Rect::new(r.x0, r.y0 - 2.0, r.x1, r.y1 + 2.0),
            words,
            hyphen: None,
        }
    }

    fn block(lines: Vec<Line>) -> Block {
        Block {
            kind: BlockKind::Text,
            rect: Rect::union_all(lines.iter().map(|l| &l.rect)).unwrap(),
            lang: None,
            lines,
        }
    }

    fn page(blocks: Vec<Block>) -> Page {
        Page {
            width: 3000.0,
            height: 2000.0,
            granularity: Granularity::Word,
            blocks,
        }
    }

    /// Every word with its box and confidence, in a fixed order.
    fn word_set(p: &Page) -> Vec<String> {
        let mut v: Vec<String> = p
            .blocks
            .iter()
            .flat_map(|b| &b.lines)
            .flat_map(|l| &l.words)
            .map(|w| format!("{} {:?} {:?}", w.text, w.rect, w.conf))
            .collect();
        v.sort();
        v
    }

    fn layout() -> Layout {
        Layout {
            cols: 3,
            col_w: 520.0,
            gutter: 14.0,
            rows: 20,
            top: 160.0,
        }
    }

    #[test]
    fn merged_columns_are_read_one_after_another() {
        let l = layout();
        let sep = Block {
            kind: BlockKind::Separator,
            rect: Rect::new(60.0, 150.0, 1700.0, 152.0),
            lang: None,
            lines: Vec::new(),
        };
        let p = page(vec![sep.clone(), l.merged()]);
        assert_ne!(text(&p), l.expected());
        let (fixed, repair) = split_columns(&p, Dir::Ltr).unwrap();
        assert_eq!(text(&fixed), l.expected());
        assert_eq!(
            repair,
            Repair {
                gutters: 2,
                lines_split: 20,
                blocks_split: 1
            }
        );
        assert_eq!(word_set(&fixed), word_set(&p));
        assert_eq!(fixed.blocks[0], sep);
        assert_eq!(fixed.blocks.len(), 4);
    }

    #[test]
    fn a_headline_across_the_columns_stays_whole_and_first() {
        let l = layout();
        let head = line(vec![
            word(300.0, 90.0, 700.0, 46.0, "Grosse"),
            word(740.0, 90.0, 1400.0, 46.0, "Versammlung"),
        ]);
        let mut b = l.merged();
        b.lines.insert(0, head);
        let (fixed, _) = split_columns(&page(vec![b]), Dir::Ltr).unwrap();
        assert_eq!(
            text(&fixed),
            format!("Grosse Versammlung\n\n{}", l.expected())
        );
    }

    #[test]
    fn columns_already_apart_are_left_alone() {
        let l = layout();
        let cols = (0..l.cols)
            .map(|c| block((0..l.rows).map(|r| line(l.words(c, r))).collect()))
            .collect();
        assert_eq!(split_columns(&page(cols), Dir::Ltr), None);
    }

    #[test]
    fn a_river_in_one_column_is_not_a_gutter() {
        let l = Layout {
            cols: 1,
            ..layout()
        };
        // Two thirds of the lines have a space at the same place.
        let lines = (0..30)
            .map(|r| {
                if r % 3 == 0 {
                    line(l.words(0, r))
                } else {
                    let y = l.top + r as f64 * PITCH;
                    line(vec![
                        word(60.0, y, 300.0, BODY, "links"),
                        word(316.0, y, 580.0, BODY, "rechts"),
                    ])
                }
            })
            .collect();
        assert_eq!(split_columns(&page(vec![block(lines)]), Dir::Ltr), None);
    }

    #[test]
    fn a_table_of_narrow_cells_is_not_columns() {
        let lines = (0..20)
            .map(|r| {
                let y = 100.0 + r as f64 * PITCH;
                line(
                    (0..6)
                        .map(|c| {
                            let x = 60.0 + c as f64 * 100.0;
                            word(x, y, x + 60.0, BODY, &format!("{}", 1890 + r + c))
                        })
                        .collect(),
                )
            })
            .collect();
        assert_eq!(split_columns(&page(vec![block(lines)]), Dir::Ltr), None);
    }

    #[test]
    fn a_word_read_across_the_gutter_goes_where_its_centre_is() {
        let l = layout();
        let mut b = l.merged();
        let row = &mut b.lines[4].words;
        let left = row.iter().position(|w| w.rect.x1 > 575.0).unwrap();
        let (a, z) = (row[left].rect, row[left + 1].rect);
        // The last word of column 0 and the first of column 1 as one token,
        // most of it in column 0.
        let fused = word(a.x0, a.y0, z.x1 - 40.0, BODY, "fused");
        row.splice(left..left + 2, [fused]);
        let p = page(vec![b]);
        let (fixed, _) = split_columns(&p, Dir::Ltr).unwrap();
        assert_eq!(word_set(&fixed), word_set(&p));
        let t = text(&fixed);
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[4].ends_with(" fused"), "{}", lines[4]);
        // Column 1's row 4, after column 0 and the blank line between them.
        let next = lines[l.rows + 1 + 4];
        assert!(next.starts_with("c1r4w1 "), "{next}");
    }

    #[test]
    fn the_hyphen_stays_with_the_last_word() {
        let l = layout();
        let mut b = l.merged();
        b.lines[2].hyphen = Some("-".into());
        let (fixed, _) = split_columns(&page(vec![b]), Dir::Ltr).unwrap();
        let hyphenated: Vec<&Line> = fixed
            .blocks
            .iter()
            .flat_map(|b| &b.lines)
            .filter(|l| l.hyphen.is_some())
            .collect();
        assert_eq!(hyphenated.len(), 1);
        assert!(hyphenated[0].words[0].text.starts_with("c2r2"));
    }

    #[test]
    fn only_left_to_right_pages_with_word_boxes_are_repaired() {
        let p = page(vec![layout().merged()]);
        assert_eq!(split_columns(&p, Dir::Rtl), None);
        assert_eq!(split_columns(&p, Dir::TtbRl), None);
        let lines = Page {
            granularity: Granularity::Line,
            ..p
        };
        assert_eq!(split_columns(&lines, Dir::Ltr), None);
    }

    /// Gutters that overlap only through another are one gutter, at the
    /// best one's place, over all their lines.
    #[test]
    fn a_chain_of_overlapping_gutters_is_one() {
        let g = |lo: f64, score: i64, y0: f64, y1: f64| Gutter {
            score,
            lo,
            hi: lo + 6.0,
            y0,
            y1,
        };
        let merged = merge(vec![
            g(108.0, 3, 0.0, 50.0),
            g(100.0, 5, 40.0, 90.0),
            g(104.0, 4, 20.0, 60.0),
        ]);
        assert_eq!(merged.len(), 1, "{merged:?}");
        let m = &merged[0];
        assert_eq!((m.lo, m.score, m.y0, m.y1), (100.0, 5, 0.0, 90.0));
    }
}
