//! A rendered line of capitals as a binary PGM, for tests that run the real
//! Tesseract and for `check-engine`, without a checked-in image; [`png`]
//! encodes one as the contract's PNG.

use image::ImageEncoder;

/// 5×7 glyphs for the letters the tests use.
fn glyph(c: char) -> [&'static str; 7] {
    match c {
        'D' => [
            "####.", "#...#", "#...#", "#...#", "#...#", "#...#", "####.",
        ],
        'E' => [
            "#####", "#....", "#....", "####.", "#....", "#....", "#####",
        ],
        'H' => [
            "#...#", "#...#", "#...#", "#####", "#...#", "#...#", "#...#",
        ],
        'L' => [
            "#....", "#....", "#....", "#....", "#....", "#....", "#####",
        ],
        'O' => [
            ".###.", "#...#", "#...#", "#...#", "#...#", "#...#", ".###.",
        ],
        'R' => [
            "####.", "#...#", "#...#", "####.", "#.#..", "#..#.", "#...#",
        ],
        'W' => [
            "#...#", "#...#", "#...#", "#.#.#", "#.#.#", "##.##", "#...#",
        ],
        _ => [
            ".....", ".....", ".....", ".....", ".....", ".....", ".....",
        ],
    }
}

/// Font pixels per glyph pixel, the margin round the text, and the
/// rows of font pixels from one line's top to the next's.
const SCALE: usize = 6;
const MARGIN: usize = 40;
const PITCH: usize = 10;

/// `text` in black on white, each font pixel `SCALE` pixels square.
pub fn text_image(text: &str) -> Vec<u8> {
    lines_image(&[text])
}

/// `lines` one below the other, as [`text_image`] draws one.
pub fn lines_image(lines: &[&str]) -> Vec<u8> {
    let (w, h) = lines_size(lines);
    let mut px = vec![255u8; w * h];
    for (n, text) in lines.iter().enumerate() {
        for (i, c) in text.chars().enumerate() {
            for (row, line) in glyph(c).iter().enumerate() {
                for (col, b) in line.bytes().enumerate() {
                    if b != b'#' {
                        continue;
                    }
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            let x = MARGIN + (i * 6 + col) * SCALE + dx;
                            let y = MARGIN + (n * PITCH + row) * SCALE + dy;
                            px[y * w + x] = 0;
                        }
                    }
                }
            }
        }
    }
    pgm(w, h, px)
}

/// A white page `w` by `h`.
pub fn blank_image(w: usize, h: usize) -> Vec<u8> {
    pgm(w, h, vec![255u8; w * h])
}

/// The size [`lines_image`] draws `lines` at.
pub fn lines_size(lines: &[&str]) -> (usize, usize) {
    let cols = lines.iter().map(|t| t.chars().count()).max().unwrap_or(0) * 6;
    let rows = (lines.len().max(1) - 1) * PITCH + 7;
    (cols * SCALE + 2 * MARGIN, rows * SCALE + 2 * MARGIN)
}

/// The ink of each word of [`text_image`]'s `text`, as `(word, [x0, y0,
/// x1, y1])` in pixels.
pub fn word_boxes(text: &str) -> Vec<(String, [f64; 4])> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.chars().chain(std::iter::once(' ')).enumerate() {
        match (c == ' ', start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                let word: String = text.chars().skip(s).take(i - s).collect();
                let x0 = MARGIN + s * 6 * SCALE;
                let x1 = MARGIN + (i * 6 - 1) * SCALE;
                out.push((
                    word,
                    [
                        x0 as f64,
                        MARGIN as f64,
                        x1 as f64,
                        (MARGIN + 7 * SCALE) as f64,
                    ],
                ));
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// A PGM from this module as a PNG (8-bit grey), the contract's input
/// format (13 §13.4.1). Deterministic: the same image encodes to the same
/// bytes.
pub fn png(pgm: &[u8]) -> Vec<u8> {
    let (w, h) = pgm_size(pgm);
    let px = &pgm[pgm.len() - w * h..];
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::NoFilter,
    )
    .write_image(px, w as u32, h as u32, image::ExtendedColorType::L8)
    .expect("a PNG in memory");
    out
}

/// The width and height in a PGM header from [`pgm`].
fn pgm_size(pgm: &[u8]) -> (usize, usize) {
    let head = std::str::from_utf8(&pgm[..pgm.len().min(32)])
        .unwrap_or_else(|e| std::str::from_utf8(&pgm[..e.valid_up_to()]).expect("valid up to"));
    let mut nums = head[2..]
        .split_ascii_whitespace()
        .map(|t| t.parse::<usize>().expect("a PGM from this module"));
    (
        nums.next().expect("a width"),
        nums.next().expect("a height"),
    )
}

fn pgm(w: usize, h: usize, px: Vec<u8>) -> Vec<u8> {
    let mut out = format!("P5\n{w} {h}\n255\n").into_bytes();
    out.extend_from_slice(&px);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_holds_the_same_pixels() {
        let pgm = text_image("HELLO");
        let (w, h) = lines_size(&["HELLO"]);
        let png = png(&pgm);
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(png, super::png(&pgm), "deterministic");
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .unwrap()
            .into_luma8();
        assert_eq!((img.width() as usize, img.height() as usize), (w, h));
        assert_eq!(img.as_raw().as_slice(), &pgm[pgm.len() - w * h..]);
    }

    #[test]
    fn word_boxes_cover_the_ink() {
        let img = text_image("HE LO");
        let (w, _) = lines_size(&["HE LO"]);
        let body = &img[img.len() - w * lines_size(&["HE LO"]).1..];
        let dark = |x: usize, y: usize| body[y * w + x] == 0;
        let boxes = word_boxes("HE LO");
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].0, "HE");
        assert_eq!(boxes[1].0, "LO");
        for (_, [x0, y0, x1, y1]) in &boxes {
            let (x0, y0, x1, y1) = (*x0 as usize, *y0 as usize, *x1 as usize, *y1 as usize);
            // Ink on the box's left, top and bottom edges, none outside it.
            assert!((y0..y1).any(|y| dark(x0, y)));
            assert!((x0..x1).any(|x| dark(x, y0)));
            assert!((x0..x1).any(|x| dark(x, y1 - 1)));
            assert!((y0..y1).all(|y| !dark(x0 - 1, y) && !dark(x1, y)));
        }
        // The text line is unchanged by the multi-line drawing.
        assert_eq!(lines_image(&["HE LO"]), img);
    }
}
