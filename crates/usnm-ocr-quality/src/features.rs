//! Scan damage features from the page image, layout features from its
//! ALTO. No model: each is a fixed computation on a downscaled grey copy
//! of the page (integer box averaging, so the copy is a function of the
//! pixels alone) or on the ALTO's geometry.
//!
//! - `skew_deg`: the angle in -3..3 degrees, in 0.1 degree steps, at which
//!   the horizontal projection profile of the ink has the highest
//!   variance. Positive when text lines descend to the right in image
//!   coordinates.
//! - `blur`: variance of the 3x3 Laplacian; lower is blurrier.
//! - `contrast`: the 95th less the 5th percentile of grey.
//! - `noise`: mean absolute residual of a 3x3 median filter.
//! - `bleed_through`: the share of pixels in the middle third of grey
//!   between the ink mode and the background mode.
//! - `ink_density`: the share of pixels below the Otsu threshold.
use image::GrayImage;
use usnm_alto::Page;
pub use usnm_ocr_contract::quality::{ImageFeatures, LayoutFeatures};

/// Long side of the grey copy the features are computed on.
pub const MAX_SIDE: u32 = 1024;
/// Skew search: `-SKEW_STEPS..=SKEW_STEPS` tenths of a degree.
pub const SKEW_STEPS: i32 = 30;

/// Returns whether the host CPU supports wide SIMD (AVX2 on `x86_64`, NEON on `aarch64`).
#[inline]
pub fn has_wide_simd() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::arch::is_x86_feature_detected!("avx2")
    }
    #[cfg(target_arch = "aarch64")]
    {
        true
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// Returns the number of intra-page worker threads to use:
/// - `USNM_QUALITY_THREADS` if set (`>= 1`);
/// - otherwise, if the CPU does not support wide vectorization (`!has_wide_simd()`),
///   gracefully falls back to multicore execution (`available_parallelism().clamp(1, 4)`);
/// - otherwise `1` when wide SIMD is active (leaving outer BOINC slot parallelism in charge).
pub fn worker_threads() -> usize {
    if let Ok(val) = std::env::var("USNM_QUALITY_THREADS") {
        if let Ok(n) = val.parse::<usize>() {
            if n >= 1 {
                return n.min(16);
            }
        }
    }
    if !has_wide_simd() {
        return std::thread::available_parallelism()
            .map(|n| n.get().clamp(1, 4))
            .unwrap_or(1);
    }
    1
}

/// A grey copy with its long side at most [`MAX_SIDE`], each pixel the
/// mean of a `scale` by `scale` block (truncated to the pixels inside the
/// image). Returns the copy and the factor.
pub fn downscale(img: &GrayImage) -> (GrayImage, u32) {
    let (w, h) = img.dimensions();
    let scale = w.max(h).div_ceil(MAX_SIDE).max(1);
    if scale == 1 {
        return (img.clone(), 1);
    }
    let (ow, oh) = (w.div_ceil(scale), h.div_ceil(scale));
    let mut out = GrayImage::new(ow, oh);
    let raw = img.as_raw();
    let out_raw = out.as_mut();
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: guarded by runtime `is_x86_feature_detected!("avx2")`.
        unsafe {
            downscale_inner_avx2(raw, out_raw, w, h, ow, oh, scale);
        }
        return (out, scale);
    }
    downscale_inner_portable(raw, out_raw, w, h, ow, oh, scale);
    (out, scale)
}

#[inline(always)]
fn downscale_inner_portable(
    raw: &[u8],
    out_raw: &mut [u8],
    w: u32,
    h: u32,
    ow: u32,
    oh: u32,
    scale: u32,
) {
    let w_usize = w as usize;
    let ow_usize = ow as usize;
    let scale_usize = scale as usize;
    let mut row_sums = vec![0u64; ow_usize];
    let mut row_counts = vec![0u64; ow_usize];
    for oy in 0..oh as usize {
        row_sums.fill(0);
        row_counts.fill(0);
        let y0 = oy * scale_usize;
        let y1 = ((oy + 1) * scale_usize).min(h as usize);
        let dy_count = (y1 - y0) as u64;
        for (ox, count_slot) in row_counts.iter_mut().enumerate() {
            let x0 = ox * scale_usize;
            let x1 = ((ox + 1) * scale_usize).min(w_usize);
            *count_slot = dy_count * (x1 - x0) as u64;
        }
        for y in y0..y1 {
            let row = &raw[y * w_usize..(y + 1) * w_usize];
            for (ox, sum_slot) in row_sums.iter_mut().enumerate() {
                let x0 = ox * scale_usize;
                let x1 = ((ox + 1) * scale_usize).min(w_usize);
                let mut s = 0u32;
                for &px in &row[x0..x1] {
                    s += u32::from(px);
                }
                *sum_slot += u64::from(s);
            }
        }
        let out_row = &mut out_raw[oy * ow_usize..(oy + 1) * ow_usize];
        for ox in 0..ow_usize {
            out_row[ox] = (row_sums[ox] / row_counts[ox].max(1)) as u8;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[target_feature(enable = "avx2")]
unsafe fn downscale_inner_avx2(
    raw: &[u8],
    out_raw: &mut [u8],
    w: u32,
    h: u32,
    ow: u32,
    oh: u32,
    scale: u32,
) {
    downscale_inner_portable(raw, out_raw, w, h, ow, oh, scale);
}

pub fn histogram(img: &GrayImage) -> [u64; 256] {
    let mut h0 = [0u64; 256];
    let mut h1 = [0u64; 256];
    let mut h2 = [0u64; 256];
    let mut h3 = [0u64; 256];
    let raw = img.as_raw();
    let (chunks, rem) = raw.as_chunks::<4>();
    for &[c0, c1, c2, c3] in chunks {
        h0[usize::from(c0)] += 1;
        h1[usize::from(c1)] += 1;
        h2[usize::from(c2)] += 1;
        h3[usize::from(c3)] += 1;
    }
    for &b in rem {
        h0[usize::from(b)] += 1;
    }
    for i in 0..256 {
        h0[i] += h1[i] + h2[i] + h3[i];
    }
    h0
}

/// Otsu's threshold: the grey level that maximises between-class
/// variance; pixels below it are ink.
pub fn otsu(hist: &[u64; 256]) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, n)| i as f64 * *n as f64)
        .sum();
    let (mut w0, mut sum0) = (0f64, 0f64);
    let (mut best, mut at) = (f64::NEG_INFINITY, 128usize);
    for (t, n) in hist.iter().enumerate() {
        w0 += *n as f64;
        if w0 == 0.0 {
            continue;
        }
        let w1 = total as f64 - w0;
        if w1 == 0.0 {
            break;
        }
        sum0 += t as f64 * *n as f64;
        let m0 = sum0 / w0;
        let m1 = (sum_all - sum0) / w1;
        let between = w0 * w1 * (m0 - m1) * (m0 - m1);
        if between > best {
            best = between;
            at = t;
        }
    }
    at.min(254) as u8 + 1
}

fn percentile(hist: &[u64; 256], p: f64) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 0;
    }
    let want = (p * total as f64).ceil().max(1.0) as u64;
    let mut seen = 0;
    for (i, n) in hist.iter().enumerate() {
        seen += n;
        if seen >= want {
            return i as u8;
        }
    }
    255
}

fn skew(img: &GrayImage, threshold: u8) -> f64 {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return 0.0;
    }
    let raw = img.as_raw();
    let w_usize = w as usize;
    let h_usize = h as usize;
    let mut dark_xs: Vec<u16> = Vec::new();
    let mut row_bounds: Vec<(u32, usize, usize)> = Vec::with_capacity(h_usize);
    for y in 0..h_usize {
        let start = dark_xs.len();
        let row = &raw[y * w_usize..(y + 1) * w_usize];
        for (x, &px) in row.iter().enumerate() {
            if px < threshold {
                dark_xs.push(x as u16);
            }
        }
        if dark_xs.len() > start {
            row_bounds.push((y as u32, start, dark_xs.len()));
        }
    }
    if dark_xs.is_empty() {
        return 0.0;
    }
    let cx = f64::from(w) / 2.0;
    let margin =
        (f64::from(w) * (f64::from(SKEW_STEPS) / 10.0).to_radians().tan()).ceil() as usize + 2;
    let rows = h_usize + 2 * margin;
    let margin_i64 = margin as i64;
    let mut profile = vec![0u32; rows];
    let mut dx_t = vec![0.0f64; w_usize];
    let mut best = (f64::NEG_INFINITY, 0i32);
    let n = rows as f64;
    for step in -SKEW_STEPS..=SKEW_STEPS {
        let t = (f64::from(step) / 10.0).to_radians().tan();
        for (x, slot) in dx_t.iter_mut().enumerate() {
            *slot = (f64::from(x as u32) - cx) * t;
        }
        profile.fill(0);
        for &(y, start, end) in &row_bounds {
            let y_f64 = f64::from(y);
            for &x in &dark_xs[start..end] {
                let r = (y_f64 - dx_t[usize::from(x)]).round() as i64 + margin_i64;
                if let Some(cell) = usize::try_from(r).ok().and_then(|r| profile.get_mut(r)) {
                    *cell += 1;
                }
            }
        }
        let mean = profile.iter().map(|v| f64::from(*v)).sum::<f64>() / n;
        let var = profile
            .iter()
            .map(|v| {
                let d = f64::from(*v) - mean;
                d * d
            })
            .sum::<f64>()
            / n;
        // Ties go to the smaller angle, then the earlier one.
        if var > best.0 || (var == best.0 && step.abs() < best.1.abs()) {
            best = (var, step);
        }
    }
    f64::from(best.1) / 10.0
}

fn laplacian_variance(img: &GrayImage) -> f64 {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let w_usize = w as usize;
    let h_usize = h as usize;
    let raw = img.as_raw();
    let inner_w = w_usize - 2;
    let total = inner_w * (h_usize - 2);
    let mut vals = vec![0.0f64; total];
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: guarded by runtime `is_x86_feature_detected!("avx2")`.
        unsafe {
            laplacian_rows_avx2(raw, &mut vals, w_usize, h_usize, inner_w);
        }
    } else {
        laplacian_rows_portable(raw, &mut vals, w_usize, h_usize, inner_w);
    }
    #[cfg(not(target_arch = "x86_64"))]
    laplacian_rows_portable(raw, &mut vals, w_usize, h_usize, inner_w);
    let n = vals.len() as f64;
    let mean = vals.iter().sum::<f64>() / n;
    vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n
}

#[inline(always)]
fn laplacian_rows_portable(
    raw: &[u8],
    vals: &mut [f64],
    w_usize: usize,
    h_usize: usize,
    inner_w: usize,
) {
    for y in 1..h_usize - 1 {
        let prev = &raw[(y - 1) * w_usize..y * w_usize];
        let curr = &raw[y * w_usize..(y + 1) * w_usize];
        let next = &raw[(y + 1) * w_usize..(y + 2) * w_usize];
        let out_row = &mut vals[(y - 1) * inner_w..y * inner_w];
        for x in 1..w_usize - 1 {
            let lap = 4 * i32::from(curr[x])
                - i32::from(curr[x - 1])
                - i32::from(curr[x + 1])
                - i32::from(prev[x])
                - i32::from(next[x]);
            out_row[x - 1] = f64::from(lap);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[target_feature(enable = "avx2")]
unsafe fn laplacian_rows_avx2(
    raw: &[u8],
    vals: &mut [f64],
    w_usize: usize,
    h_usize: usize,
    inner_w: usize,
) {
    laplacian_rows_portable(raw, vals, w_usize, h_usize, inner_w);
}

#[inline(always)]
fn sort2_u8(a: u8, b: u8) -> (u8, u8) {
    (a.min(b), a.max(b))
}

#[inline(always)]
fn sort3_u8(a: u8, b: u8, c: u8) -> (u8, u8, u8) {
    let (a, b) = sort2_u8(a, b);
    let (b, c) = sort2_u8(b, c);
    let (a, b) = sort2_u8(a, b);
    (a, b, c)
}

#[inline(always)]
fn median3_u8(a: u8, b: u8, c: u8) -> u8 {
    let (a, b) = sort2_u8(a, b);
    let (_, b) = sort2_u8(a, b.min(c));
    b
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn median9_u8(a0: u8, a1: u8, a2: u8, b0: u8, b1: u8, b2: u8, c0: u8, c1: u8, c2: u8) -> u8 {
    let (lo0, mid0, hi0) = sort3_u8(a0, a1, a2);
    let (lo1, mid1, hi1) = sort3_u8(b0, b1, b2);
    let (lo2, mid2, hi2) = sort3_u8(c0, c1, c2);
    let max_lo = lo0.max(lo1).max(lo2);
    let min_hi = hi0.min(hi1).min(hi2);
    let med_mid = median3_u8(mid0, mid1, mid2);
    median3_u8(max_lo, med_mid, min_hi)
}

fn median_residual(img: &GrayImage) -> f64 {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let w_usize = w as usize;
    let h_usize = h as usize;
    let raw = img.as_raw();
    let n = ((w_usize - 2) * (h_usize - 2)) as u64;
    #[cfg(target_arch = "x86_64")]
    let sum = if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: guarded by runtime `is_x86_feature_detected!("avx2")`.
        unsafe { median_residual_sum_avx2(raw, w_usize, h_usize) }
    } else {
        median_residual_sum_portable(raw, w_usize, h_usize)
    };
    #[cfg(not(target_arch = "x86_64"))]
    let sum = median_residual_sum_portable(raw, w_usize, h_usize);
    sum as f64 / n.max(1) as f64
}

#[inline(always)]
fn median_residual_sum_portable(raw: &[u8], w_usize: usize, h_usize: usize) -> u64 {
    let mut sum = 0u64;
    for y in 1..h_usize - 1 {
        let r0 = &raw[(y - 1) * w_usize..y * w_usize];
        let r1 = &raw[y * w_usize..(y + 1) * w_usize];
        let r2 = &raw[(y + 1) * w_usize..(y + 2) * w_usize];
        let mut row_sum = 0u32;
        for x in 1..w_usize - 1 {
            let med = median9_u8(
                r0[x - 1],
                r0[x],
                r0[x + 1],
                r1[x - 1],
                r1[x],
                r1[x + 1],
                r2[x - 1],
                r2[x],
                r2[x + 1],
            );
            row_sum += u32::from(r1[x].abs_diff(med));
        }
        sum += u64::from(row_sum);
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[target_feature(enable = "avx2")]
unsafe fn median_residual_sum_avx2(raw: &[u8], w_usize: usize, h_usize: usize) -> u64 {
    median_residual_sum_portable(raw, w_usize, h_usize)
}

fn bleed_through(hist: &[u64; 256], threshold: u8) -> f64 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let t = usize::from(threshold);
    let mode = |range: std::ops::Range<usize>| -> usize {
        range
            .clone()
            .fold(range.start, |b, i| if hist[i] > hist[b] { i } else { b })
    };
    let ink = mode(0..t.max(1));
    let bg = mode(t.min(255)..256);
    if bg <= ink {
        return 0.0;
    }
    let span = bg - ink;
    let lo = ink + span / 3;
    let hi = ink + 2 * span / 3;
    let band: u64 = (lo..=hi).map(|i| hist[i]).sum();
    band as f64 / total as f64
}

/// The damage features of a page image.
pub fn image_features(img: &GrayImage) -> ImageFeatures {
    let (width, height) = img.dimensions();
    let (small, scale) = downscale(img);
    let hist = histogram(&small);
    let threshold = otsu(&hist);
    let total: u64 = hist.iter().sum();
    let ink: u64 = hist[..usize::from(threshold)].iter().sum();
    ImageFeatures {
        bleed_through: bleed_through(&hist, threshold),
        blur: laplacian_variance(&small),
        contrast: f64::from(percentile(&hist, 0.95)) - f64::from(percentile(&hist, 0.05)),
        height,
        ink_density: if total == 0 {
            0.0
        } else {
            ink as f64 / total as f64
        },
        noise: median_residual(&small),
        scale,
        skew_deg: skew(&small, threshold),
        threshold,
        width,
    }
}

fn median(v: &mut [f64]) -> Option<f64> {
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

/// Text blocks clustered by x-centre: a new column starts where the gap
/// to the previous centre is more than half the median block width.
pub fn columns(page: &Page) -> usize {
    let blocks: Vec<&usnm_alto::Block> = page.blocks.iter().filter(|b| b.kind.is_text()).collect();
    if blocks.is_empty() {
        return 0;
    }
    let mut widths: Vec<f64> = blocks.iter().map(|b| b.rect.width()).collect();
    let gap = (median(&mut widths).unwrap_or(0.0) / 2.0).max(1.0);
    let mut centres: Vec<f64> = blocks.iter().map(|b| b.rect.centre().0).collect();
    centres.sort_by(f64::total_cmp);
    let mut n = 1;
    for w in centres.windows(2) {
        if w[1] - w[0] > gap {
            n += 1;
        }
    }
    n
}

/// The layout features of a page's ALTO (in image pixels).
pub fn layout_features(page: &Page) -> LayoutFeatures {
    let (blocks, lines, words) = page.counts();
    let mut heights: Vec<f64> = page
        .blocks
        .iter()
        .filter(|b| b.kind.is_text())
        .flat_map(|b| &b.lines)
        .map(|l| l.rect.height())
        .collect();
    LayoutFeatures {
        blocks,
        columns: columns(page),
        line_height_px: median(&mut heights),
        lines,
        mean_word_conf: page.mean_conf(),
        words,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use usnm_alto::{Block, BlockKind, Granularity, Line, Rect, Word};

    /// A light page with dark text-like rows (short dashes of ink along
    /// each row) whose baselines follow `slope` (dy per dx).
    pub(crate) fn synthetic_page(w: u32, h: u32, slope: f64, bg: u8, ink: u8) -> GrayImage {
        let mut img = GrayImage::from_pixel(w, h, image::Luma([bg]));
        let cx = f64::from(w) / 2.0;
        let mut row = 60.0;
        while row < f64::from(h) - 60.0 {
            for x in 40..w - 40 {
                // Words: 7 px of ink, 3 px gap, as a crude line of type.
                if (x / 10) % 10 == 9 {
                    continue;
                }
                let y0 = row + (f64::from(x) - cx) * slope;
                for dy in 0..6 {
                    let y = y0.round() as i64 + dy;
                    if y >= 0 && y < i64::from(h) {
                        img.put_pixel(x, y as u32, image::Luma([ink]));
                    }
                }
            }
            row += 24.0;
        }
        img
    }

    fn box_blur(img: &GrayImage) -> GrayImage {
        let (w, h) = img.dimensions();
        let mut out = img.clone();
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let mut s = 0u32;
                for dy in 0..3 {
                    for dx in 0..3 {
                        s += u32::from(img.get_pixel(x + dx - 1, y + dy - 1).0[0]);
                    }
                }
                out.put_pixel(x, y, image::Luma([(s / 9) as u8]));
            }
        }
        out
    }

    #[test]
    fn skew_blur_contrast_on_a_rendered_page() {
        let slope = 1.5f64.to_radians().tan();
        let img = synthetic_page(800, 1000, slope, 235, 30);
        let f = image_features(&img);
        assert!((f.skew_deg - 1.5).abs() <= 0.3, "skew {}", f.skew_deg);
        assert!(f.contrast >= 200.0, "contrast {}", f.contrast);
        assert!(
            f.ink_density > 0.05 && f.ink_density < 0.5,
            "ink {}",
            f.ink_density
        );
        assert_eq!(f.scale, 1);
        let straight = image_features(&synthetic_page(800, 1000, 0.0, 235, 30));
        assert!(straight.skew_deg.abs() <= 0.1, "skew {}", straight.skew_deg);
        let blurred = image_features(&box_blur(&img));
        assert!(blurred.blur < f.blur, "{} !< {}", blurred.blur, f.blur);
        let other = synthetic_page(800, 1000, -slope, 235, 30);
        assert!((image_features(&other).skew_deg + 1.5).abs() <= 0.3);
        // Low contrast when ink and background are close.
        let faint = image_features(&synthetic_page(800, 1000, 0.0, 200, 150));
        assert!(faint.contrast < f.contrast);
        // A large page is downscaled by an integer factor and still reads.
        let big = synthetic_page(2600, 3000, slope, 235, 30);
        let fb = image_features(&big);
        assert_eq!(fb.scale, 3);
        assert_eq!((fb.width, fb.height), (2600, 3000));
        assert!((fb.skew_deg - 1.5).abs() <= 0.3, "skew {}", fb.skew_deg);
    }

    #[test]
    fn noise_and_bleed_move_the_right_way() {
        let clean = synthetic_page(400, 400, 0.0, 240, 20);
        let mut noisy = clean.clone();
        let mut s = 11u32;
        for p in noisy.pixels_mut() {
            s = s.wrapping_mul(1103515245).wrapping_add(12345);
            if s.is_multiple_of(7) {
                p.0[0] = p.0[0].saturating_sub(40);
            }
        }
        let (fc, fn_) = (image_features(&clean), image_features(&noisy));
        assert!(fn_.noise > fc.noise);
        let mut bled = clean.clone();
        for (x, y, p) in bled.enumerate_pixels_mut() {
            if p.0[0] == 240 && (x / 5 + y / 5).is_multiple_of(8) {
                p.0[0] = 130;
            }
        }
        assert!(image_features(&bled).bleed_through > fc.bleed_through);
        let empty = image_features(&GrayImage::new(0, 0));
        assert_eq!(empty.skew_deg, 0.0);
    }

    pub(crate) fn page_with_columns(cols: usize, lines_per_block: usize) -> Page {
        let (w, h) = (4000.0, 6000.0);
        let col_w = w / cols as f64;
        let mut blocks = Vec::new();
        for c in 0..cols {
            for b in 0..2 {
                let x0 = c as f64 * col_w + 40.0;
                let y0 = 200.0 + b as f64 * 2800.0;
                let mut lines = Vec::new();
                for l in 0..lines_per_block {
                    let ly = y0 + l as f64 * 40.0;
                    let words = (0..5)
                        .map(|k| Word {
                            rect: Rect::new(
                                x0 + k as f64 * 130.0,
                                ly,
                                x0 + k as f64 * 130.0 + 110.0,
                                ly + 30.0,
                            ),
                            text: format!("w{c}{b}{l}{k}"),
                            conf: Some(0.5 + 0.1 * (c % 3) as f64),
                        })
                        .collect();
                    lines.push(Line {
                        rect: Rect::new(x0, ly, x0 + col_w - 80.0, ly + 30.0),
                        words,
                        hyphen: None,
                    });
                }
                blocks.push(Block {
                    kind: BlockKind::Text,
                    rect: Rect::new(
                        x0,
                        y0,
                        x0 + col_w - 80.0,
                        y0 + lines_per_block as f64 * 40.0,
                    ),
                    lang: None,
                    lines,
                });
            }
        }
        Page {
            width: w,
            height: h,
            granularity: Granularity::Word,
            blocks,
        }
    }

    #[test]
    fn layout_counts_columns_and_lines() {
        let f = layout_features(&page_with_columns(4, 10));
        assert_eq!(f.columns, 4);
        assert_eq!(f.blocks, 8);
        assert_eq!(f.lines, 80);
        assert_eq!(f.words, 400);
        assert_eq!(f.line_height_px, Some(30.0));
        assert!(f.mean_word_conf.is_some());
        let empty = layout_features(&Page {
            width: 1.0,
            height: 1.0,
            granularity: Granularity::Page,
            blocks: vec![],
        });
        assert_eq!(empty.columns, 0);
        assert_eq!(empty.line_height_px, None);
    }
}
