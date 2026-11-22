"""A tiny synthetic gold set for the tests of #2103: made-up text, a drawn page, a hand-written ALTO.

No LoC or dataset content is in it. `make_set` writes the layout `loader.py` reads: a converted set
(manifest.jsonl, crops.jsonl, LICENSES/) and an ALTO folder (<stem>.xml, <stem>.jpg).
"""

from __future__ import annotations

from pathlib import Path

import paths  # noqa: F401
import gold

ALTO_SCALE = 2  # ALTO units per image pixel, so the tests cover the proportional mapping onto the image
LINE_PITCH = 60
INK_HEIGHT = 34

# (printed word, word LoC read, WC). The printed words are the truth; the third line ends in a hyphen.
LINES = [
    [("the", "the", 0.97), ("bad", "had", 0.91), ("dog", "dog", 0.95), ("ran", "ran", 0.96)],
    [("over", "ovcr", 0.55), ("the", "tbe", 0.62), ("hill", "hill", 0.94)],
    [("Clare-", "Clare-", 0.9), ("mont", "mont", 0.9), ("farm", "farm", 0.93)],
]
LICENCE = "CC BY-NC-SA 4.0"


def font(size: int = 28, path=None):
    """The face the page is printed in: Pillow's default sans, or the font file `path`."""
    from PIL import ImageFont

    return ImageFont.truetype(str(path), size) if path else ImageFont.load_default(size=size)


def make_set(root: Path, lines=LINES, page_size=(800, 260), licence: str = LICENCE, for_scoring: bool = True,
             scan_font=None) -> dict:
    """Write set/ and alto/ under root (the page printed in `scan_font`, a font file, or Pillow's sans) and return {"set_dir", "alto_dir", "page_size", "box", "transcription"}."""
    from PIL import Image, ImageDraw

    set_dir, alto_dir = root / "set", root / "alto"
    (set_dir / "LICENSES").mkdir(parents=True)
    alto_dir.mkdir(parents=True)
    (set_dir / "LICENSES" / "LICENSE").write_text("synthetic test data\n", encoding="utf-8")
    f = font(path=scan_font)
    img = Image.new("L", page_size, 255)
    draw = ImageDraw.Draw(img)
    strings, truth_lines = [], []
    for li, line in enumerate(lines):
        x, y = 30, 20 + li * LINE_PITCH
        parts, truth = [], []
        for printed, read, wc in line:
            w = int(f.getlength(printed))
            draw.text((x, y + INK_HEIGHT - 6), printed, font=f, fill=0, anchor="ls")
            parts.append(f'<String ID="s{li}_{len(parts)}" CONTENT="{read}" HPOS="{x * ALTO_SCALE}" VPOS="{y * ALTO_SCALE}" '
                         f'WIDTH="{w * ALTO_SCALE}" HEIGHT="{INK_HEIGHT * ALTO_SCALE}" WC="{wc}"/>')
            truth.append(printed)
            x += w + 14
        strings.append(f'<TextLine ID="l{li}">' + "".join(parts) + "</TextLine>")
        truth_lines.append(" ".join(truth))
    alto = ('<?xml version="1.0" encoding="UTF-8"?>\n<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout>'
            f'<Page WIDTH="{page_size[0] * ALTO_SCALE}" HEIGHT="{page_size[1] * ALTO_SCALE}"><PrintSpace>'
            f'<TextBlock ID="b0">{"".join(strings)}</TextBlock></PrintSpace></Page></Layout></alto>')
    (alto_dir / "page1.xml").write_text(alto, encoding="utf-8")
    img.convert("RGB").save(alto_dir / "page1.jpg", quality=95)
    box = [0, 0, page_size[0], page_size[1]]
    transcription = "\n".join(truth_lines)
    gold.write_jsonl(set_dir / "manifest.jsonl", [{
        "crop_id": "syn-001", "page_id": "synthetic-page-1", "lang": "eng", "box": box, "source_image": "page1.jpg",
        "source_license": licence, "for_scoring": for_scoring}])
    gold.write_jsonl(set_dir / "crops.jsonl", [{
        "crop_id": "syn-001", "lang": "eng", "transcription": transcription, "for_scoring": for_scoring}])
    return {"set_dir": set_dir, "alto_dir": alto_dir, "page_size": page_size, "box": box, "transcription": transcription}
