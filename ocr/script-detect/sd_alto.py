"""LoC ALTO reading shared by the script-detection tools (#1666).

A page's words come out in reading order with their pixel boxes on the scan, the line and text block
they sit in, and LoC's reading. Pages are named by their slug, `lccn/yyyy-mm-dd/ed-N/seq-N`.
"""
from __future__ import annotations

import glob
import json
import os
import re
import xml.etree.ElementTree as ET

SYL = re.compile("[Ꭰ-Ᏽ]")
LATIN = re.compile("[A-Za-z]")


def tag(e):
    return e.tag.rsplit("}", 1)[-1]


def alto_pages(alto_dir):
    """(slug, path) for every ocr.xml under an LoC batch tree."""
    for p in sorted(glob.glob(os.path.join(alto_dir, "sn*/*/*/*/ed-*/seq-*/ocr.xml"))):
        lccn, y, mo, d, ed, seq = p.split(os.sep)[-7:-1]
        yield f"{lccn}/{y}-{mo}-{d}/{ed}/{seq}", p


def excluded_pages(manifest_path):
    """The frozen test set's pages, never used for training."""
    m = json.load(open(manifest_path, encoding="utf-8"))
    return {p for s in m["sets"].values() if isinstance(s, dict) and isinstance(s.get("pages"), list)
            for p in s["pages"]}


def image_path(images_dirs, slug):
    name = slug.replace("/", "_") + ".jpg"
    for d in images_dirs:
        p = os.path.join(d, name)
        if os.path.exists(p):
            return p
    return None


def syllabary_word(w):
    return bool(SYL.search(w))


def read_page(path, image_width):
    """Blocks of lines of words, each word a dict with its pixel box and LoC's text."""
    root = ET.parse(path).getroot()
    width = next(float(e.get("WIDTH")) for e in root.iter() if tag(e) == "Page")
    s = image_width / width

    def box(e):
        x, y = float(e.get("HPOS")) * s, float(e.get("VPOS")) * s
        return [int(x), int(y), int(x + float(e.get("WIDTH")) * s), int(y + float(e.get("HEIGHT")) * s)]

    blocks = []
    for b in (e for e in root.iter() if tag(e) == "TextBlock"):
        lines = []
        for ln in (e for e in b.iter() if tag(e) == "TextLine"):
            words = [{"id": w.get("ID"), "text": w.get("CONTENT", ""), "box": box(w)}
                     for w in ln if tag(w) == "String"]
            if words:
                lines.append({"id": ln.get("ID"), "box": box(ln), "words": words})
        if lines:
            blocks.append({"id": b.get("ID"), "box": box(b), "lines": lines})
    return blocks


def syllabary_line(line):
    """A line of Cherokee text: two or more words LoC read with syllabary letters (#1666's count)."""
    return sum(1 for w in line["words"] if syllabary_word(w["text"])) >= 2
