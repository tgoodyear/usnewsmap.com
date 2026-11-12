//! A bank of a newspaper's own printed glyphs (#1620, #1666).
//!
//! LoC's ALTO gives every word a box on the scan. A word whose reading we
//! trust (for the *Cherokee Phoenix*: a form of the 1860 New Testament, or
//! one [`usnm_core::cherokee_type::fix_word`] reads again) is cut from the
//! scan, split into as many glyphs as it has letters ([`split_word`]), and
//! each glyph is labelled with its letter. The split can be wrong, so
//! [`clean`] keeps a glyph only when its nearest labelled neighbours on other
//! pages agree with its label.
//!
//! The bank feeds the check site's example pictures, synthetic training
//! lines and a traced font for the Cherokee model (#1621), and per-word
//! script detection (#1666).

use image::GrayImage;

/// The side of a glyph's feature image, in pixels.
pub const FEATURE_SIDE: usize = 24;

/// A glyph box in a crop: `[x0, y0, x1, y1]`, end-exclusive.
pub type GlyphBox = [u32; 4];

/// Otsu's threshold of a grayscale image: ink is at or below it.
pub fn otsu(img: &GrayImage) -> u8 {
    let mut hist = [0f64; 256];
    for p in img.pixels() {
        hist[usize::from(p.0[0])] += 1.0;
    }
    let total: f64 = hist.iter().sum();
    let sum_all: f64 = hist.iter().enumerate().map(|(i, h)| i as f64 * h).sum();
    let (mut w0, mut s0, mut best, mut thr) = (0.0, 0.0, -1.0, 128u8);
    for (t, h) in hist.iter().enumerate() {
        w0 += h;
        s0 += t as f64 * h;
        let w1 = total - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let between = w0 * w1 * (s0 / w0 - (sum_all - s0) / w1).powi(2);
        if between > best {
            best = between;
            thr = t as u8;
        }
    }
    thr
}

/// An ink mask, row by row.
pub struct Ink {
    pub width: u32,
    pub height: u32,
    pub on: Vec<bool>,
}

impl Ink {
    pub fn of(img: &GrayImage) -> Self {
        let t = otsu(img);
        Self {
            width: img.width(),
            height: img.height(),
            on: img.pixels().map(|p| p.0[0] <= t).collect(),
        }
    }

    fn at(&self, x: u32, y: u32) -> bool {
        self.on[(y * self.width + x) as usize]
    }

    /// Ink pixels in column `x` between rows `y0` and `y1`.
    fn column(&self, x: u32, y0: u32, y1: u32) -> u32 {
        (y0..y1).filter(|&y| self.at(x, y)).count() as u32
    }
}

/// `[x0, y0, x1, y1, area]` of each 8-connected ink component.
pub fn components(ink: &Ink) -> Vec<[u32; 5]> {
    let (w, h) = (ink.width as i64, ink.height as i64);
    let mut seen = vec![false; ink.on.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..ink.on.len() {
        if !ink.on[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1, mut area) = (u32::MAX, u32::MAX, 0, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = ((i as i64 % w), (i as i64 / w));
            x0 = x0.min(x as u32);
            y0 = y0.min(y as u32);
            x1 = x1.max(x as u32 + 1);
            y1 = y1.max(y as u32 + 1);
            area += 1;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx >= w || ny >= h {
                        continue;
                    }
                    let j = (ny * w + nx) as usize;
                    if ink.on[j] && !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
        }
        out.push([x0, y0, x1, y1, area]);
    }
    out
}

/// The crop a word is cut from: its columns `wx0..wx1` widened by 15% of the line's height
/// (`ly1 - ly0`, at least 2 px) on each side, and the line's rows `ly0..ly1` widened the same.
/// All in scan pixels, clamped to a scan of `width` by `height`. `None` for a crop of 2 px or less.
pub fn word_crop(
    wx0: f64,
    wx1: f64,
    ly0: f64,
    ly1: f64,
    width: u32,
    height: u32,
) -> Option<[u32; 4]> {
    let pad = (0.15 * (ly1 - ly0)).max(2.0);
    let clamp = |v: f64, hi: u32| v.max(0.0).min(f64::from(hi)) as u32;
    let (x0, x1) = (clamp(wx0 - pad, width), clamp(wx1 + pad, width));
    let (y0, y1) = (clamp(ly0 - pad, height), clamp(ly1 + pad, height));
    (x1 > x0 + 2 && y1 > y0 + 2).then_some([x0, y0, x1, y1])
}

/// The boxes of `n` glyphs in a word crop, left to right, or `None` when
/// the split isn't plausible. Components are kept when they sit in the
/// word's text band; ones stacked over each other merge; punctuation and
/// flat marks drop out. Then the narrowest gaps merge, or the widest
/// glyphs are cut at their thinnest column, until there are `n`. A split
/// whose widths vary too much is refused.
pub fn split_word(ink: &Ink, n: usize) -> Option<Vec<GlyphBox>> {
    if n == 0 {
        return None;
    }
    let comps: Vec<[u32; 5]> = components(ink).into_iter().filter(|c| c[4] >= 10).collect();
    let mut heights: Vec<u32> = comps
        .iter()
        .filter(|c| c[4] >= 30)
        .map(|c| c[3] - c[1])
        .collect();
    heights.sort_unstable();
    let body = heights
        .get(heights.len() / 2)
        .copied()
        .unwrap_or(ink.height / 2)
        .max(1) as f64;
    let centre = ink.height as f64 / 2.0;
    let mut comps: Vec<[u32; 5]> = comps
        .into_iter()
        .filter(|c| ((c[1] + c[3]) as f64 / 2.0 - centre).abs() < 0.75 * body + 4.0)
        .collect();
    comps.sort_by_key(|c| c[0]);
    let mut boxes: Vec<GlyphBox> = Vec::new();
    for c in comps {
        if let Some(m) = boxes.last_mut() {
            let overlap = m[2].min(c[2]) as f64 - m[0].max(c[0]) as f64;
            if overlap > 0.5 * ((m[2] - m[0]).min(c[2] - c[0])) as f64 {
                *m = [
                    m[0].min(c[0]),
                    m[1].min(c[1]),
                    m[2].max(c[2]),
                    m[3].max(c[3]),
                ];
                continue;
            }
        }
        boxes.push([c[0], c[1], c[2], c[3]]);
    }
    boxes.retain(|b| {
        let (bw, bh) = ((b[2] - b[0]) as f64, (b[3] - b[1]) as f64);
        bh >= 0.45 * body && bh >= 0.35 * bw
    });
    if boxes.is_empty() {
        return None;
    }
    while boxes.len() > n {
        let i = (0..boxes.len() - 1)
            .min_by_key(|&i| boxes[i + 1][0] as i64 - boxes[i][2] as i64)
            .expect("two boxes");
        let b = boxes.remove(i + 1);
        let a = boxes[i];
        boxes[i] = [
            a[0].min(b[0]),
            a[1].min(b[1]),
            a[2].max(b[2]),
            a[3].max(b[3]),
        ];
    }
    while boxes.len() < n {
        let i = (0..boxes.len())
            .max_by_key(|&i| boxes[i][2] - boxes[i][0])
            .expect("a box");
        let [x0, y0, x1, y1] = boxes[i];
        if x1 - x0 < 8 {
            return None;
        }
        let (lo, hi) = (x0 + (x1 - x0) / 4, x0 + 3 * (x1 - x0) / 4);
        let cut = (lo..hi)
            .min_by_key(|&x| ink.column(x, y0, y1))
            .expect("a column");
        boxes.splice(i..=i, [[x0, y0, cut, y1], [cut, y0, x1, y1]]);
    }
    let mut widths: Vec<u32> = boxes.iter().map(|b| b[2] - b[0]).collect();
    widths.sort_unstable();
    let med = widths[widths.len() / 2] as f64;
    if n > 1 && (widths[widths.len() - 1] as f64 > 2.0 * med || (widths[0] as f64) < 0.35 * med) {
        return None;
    }
    Some(boxes)
}

/// A glyph's ink, centred in a square and scaled to [`FEATURE_SIDE`],
/// as a unit vector, for cosine similarity.
pub fn feature(ink: &Ink, b: GlyphBox) -> Vec<f32> {
    let (w, h) = (b[2] - b[0], b[3] - b[1]);
    let side = w.max(h).max(1);
    let mut square = GrayImage::new(side, side);
    let (ox, oy) = ((side - w) / 2, (side - h) / 2);
    for y in 0..h {
        for x in 0..w {
            if ink.at(b[0] + x, b[1] + y) {
                square.put_pixel(ox + x, oy + y, image::Luma([255]));
            }
        }
    }
    let small = image::imageops::resize(
        &square,
        FEATURE_SIDE as u32,
        FEATURE_SIDE as u32,
        image::imageops::FilterType::Triangle,
    );
    let mut v: Vec<f32> = small.pixels().map(|p| f32::from(p.0[0])).collect();
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// For each glyph, the share of its `k` nearest neighbours' similarity
/// (from pages other than its own) that falls on its own class. `features`
/// holds unit vectors of equal length; `classes` and `pages` index them.
/// Only glyphs with `voters[t]` set count as neighbours. Runs on `threads` threads.
pub fn agreement(
    features: &[Vec<f32>],
    classes: &[u32],
    pages: &[u32],
    voters: &[bool],
    k: usize,
    threads: usize,
) -> Vec<f32> {
    let n = features.len();
    let mut out = vec![0f32; n];
    if k == 0 {
        return out; // no neighbours, no agreement
    }
    let chunk = n.div_ceil(threads.max(1)).max(1);
    std::thread::scope(|s| {
        for (part, slot) in out.chunks_mut(chunk).enumerate() {
            s.spawn(move || {
                let mut top: Vec<(f32, u32)> = Vec::with_capacity(k + 1);
                for (j, o) in slot.iter_mut().enumerate() {
                    let i = part * chunk + j;
                    top.clear();
                    let fi = &features[i];
                    for (t, ft) in features.iter().enumerate() {
                        if !voters[t] || pages[t] == pages[i] {
                            continue;
                        }
                        let sim = dot(fi, ft);
                        if sim <= 0.0 || (top.len() == k && sim <= top[k - 1].0) {
                            continue;
                        }
                        let at = top.partition_point(|&(s, _)| s >= sim);
                        top.insert(at, (sim, classes[t]));
                        top.truncate(k);
                    }
                    let total: f32 = top.iter().map(|t| t.0).sum();
                    let own: f32 = top.iter().filter(|t| t.1 == classes[i]).map(|t| t.0).sum();
                    *o = if total > 0.0 { own / total } else { 0.0 };
                }
            });
        }
    });
    out
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    let mut acc = [0f32; 8];
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] += x[l] * y[l];
        }
    }
    acc.iter().sum::<f32>() + ra.iter().zip(rb).map(|(x, y)| x * y).sum::<f32>()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white crop with black rectangles at `rects`.
    fn crop(w: u32, h: u32, rects: &[[u32; 4]]) -> GrayImage {
        let mut img = GrayImage::from_pixel(w, h, image::Luma([230]));
        for r in rects {
            for y in r[1]..r[3] {
                for x in r[0]..r[2] {
                    img.put_pixel(x, y, image::Luma([20]));
                }
            }
        }
        img
    }

    #[test]
    fn word_crops_are_padded_and_clamped() {
        // A 40 px line pads by 6 px; a 10 px line by the 2 px minimum.
        assert_eq!(
            word_crop(100.0, 200.0, 50.0, 90.0, 1000, 1000),
            Some([94, 44, 206, 96])
        );
        assert_eq!(
            word_crop(100.0, 200.0, 50.0, 60.0, 1000, 1000),
            Some([98, 48, 202, 62])
        );
        assert_eq!(
            word_crop(-5.0, 20.0, 0.0, 40.0, 1000, 30),
            Some([0, 0, 26, 30])
        );
        assert_eq!(
            word_crop(100.0, 101.0, 50.0, 51.0, 1000, 1000),
            Some([98, 48, 103, 53])
        );
        assert_eq!(word_crop(990.0, 995.0, 50.0, 51.0, 990, 1000), None);
    }

    #[test]
    fn three_glyphs_split_three_ways() {
        let img = crop(100, 40, &[[5, 8, 25, 32], [35, 8, 55, 32], [65, 8, 85, 32]]);
        let ink = Ink::of(&img);
        let boxes = split_word(&ink, 3).unwrap();
        assert_eq!(boxes, [[5, 8, 25, 32], [35, 8, 55, 32], [65, 8, 85, 32]]);
        // Two glyphs printed touching are cut apart at their thinnest column.
        let img = crop(
            100,
            40,
            &[[5, 8, 25, 32], [25, 18, 27, 20], [27, 8, 47, 32]],
        );
        let boxes = split_word(&Ink::of(&img), 2).unwrap();
        assert_eq!(boxes.len(), 2);
        assert!(boxes[0][2] >= 24 && boxes[0][2] <= 28, "{boxes:?}");
        // A wildly uneven split is refused.
        let img = crop(140, 40, &[[5, 8, 105, 32], [115, 8, 130, 32]]);
        assert!(split_word(&Ink::of(&img), 2).is_none());
    }

    #[test]
    fn features_are_unit_vectors_and_neighbours_vote() {
        let a = Ink::of(&crop(30, 30, &[[5, 5, 25, 12]]));
        let b = Ink::of(&crop(30, 30, &[[12, 5, 18, 25]]));
        let fa = feature(&a, [5, 5, 25, 12]);
        let fb = feature(&b, [12, 5, 18, 25]);
        assert!((dot(&fa, &fa) - 1.0).abs() < 1e-4);
        // Two shapes, two classes, three pages each: each agrees with its own.
        let feats = [fa.clone(), fa.clone(), fa, fb.clone(), fb.clone(), fb];
        let classes = [0, 0, 0, 1, 1, 1];
        let pages = [0, 1, 2, 0, 1, 2];
        let agree = agreement(&feats, &classes, &pages, &[true; 6], 2, 2);
        assert!(agree.iter().all(|&g| g > 0.99), "{agree:?}");
    }

    #[test]
    fn non_voters_never_vote() {
        let a = Ink::of(&crop(30, 30, &[[5, 5, 25, 12]]));
        let fa = feature(&a, [5, 5, 25, 12]);
        // Glyph 0 (class 0) has two identical neighbours: one labelled 0 on a page that may
        // vote and one labelled 1 on a page that may not. Only the first counts.
        let feats = [fa.clone(), fa.clone(), fa];
        let agree = agreement(&feats, &[0, 0, 1], &[0, 1, 2], &[true, true, false], 2, 1);
        assert!(agree[0] > 0.99, "{agree:?}");
        // The non-voter is still judged by the others.
        assert!(agree[2] < 0.01, "{agree:?}");
    }
}
