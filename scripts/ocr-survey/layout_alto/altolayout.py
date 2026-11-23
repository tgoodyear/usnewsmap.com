#!/usr/bin/env python3
"""ALTO TextLine / String / TextBlock boxes in image pixels (#2107, step 2).

Method sources: step 1 of #2107 (banded column finder, #2112, after jscancella/NYTribuneOCRExperiments
findText_usingSums.py, GPL-3.0, reimplemented); #2103 (render-vs-scan word tables and loader);
Beyond Words (Library of Congress, CC0, biglam/loc_beyond_words@6c7f5fb3); American Stories
(Dell et al., arXiv:2308.12477).

The ALTO is read with xml.etree.ElementTree. Coordinates are mapped to the page image by
proportion (image size / ALTO Page WIDTH and HEIGHT), as #2103's loader does. Lines are numbered
in document order over every `TextLine` element (the same numbering as #2103's `line` column).
A line's box is its own HPOS, VPOS, WIDTH, HEIGHT attributes when all four are numbers, else the
union of its `String` boxes; a line with neither has box None. A TextBlock's box is its own
attributes, else the union of its lines' boxes.
"""
import io
import xml.etree.ElementTree as ET


def _local(tag):
    return tag.rsplit("}", 1)[-1]


def _num(el, *keys):
    try:
        return [float(el.get(k)) for k in keys]
    except (TypeError, ValueError):
        return None


def _union(boxes):
    boxes = [b for b in boxes if b]
    if not boxes:
        return None
    return [min(b[0] for b in boxes), min(b[1] for b in boxes), max(b[2] for b in boxes), max(b[3] for b in boxes)]


def page_dims(xml):
    """(WIDTH, HEIGHT) of the first Page element, or None. Reads only as far as the Page start tag."""
    try:
        for _, el in ET.iterparse(io.BytesIO(xml), events=("start",)):
            if _local(el.tag) == "Page":
                d = _num(el, "WIDTH", "HEIGHT")
                return tuple(d) if d else None
    except ET.ParseError:
        return None
    return None


def parse(xml, image_size):
    """Parse ALTO bytes. Returns {"lines": [...], "blocks": [...], "alto_size": (w, h)}, boxes [x0, y0, x1, y1]
    in image pixels.

    line: {"idx": document-order index, "box": box or None, "block": block index or None,
           "words": [{"text", "box", "wc"}]}
    block: {"idx", "box"}
    """
    root = ET.fromstring(xml)
    page = next((e for e in root.iter() if _local(e.tag) == "Page"), None)
    d = _num(page, "WIDTH", "HEIGHT") if page is not None else None
    if not d or d[0] <= 0 or d[1] <= 0:
        raise ValueError("ALTO has no usable Page WIDTH/HEIGHT")
    sx, sy = image_size[0] / d[0], image_size[1] / d[1]

    def box_of(el):
        v = _num(el, "HPOS", "VPOS", "WIDTH", "HEIGHT")
        if v is None:
            return None
        return [v[0] * sx, v[1] * sy, (v[0] + v[2]) * sx, (v[1] + v[3]) * sy]

    line_els = [e for e in root.iter() if _local(e.tag) == "TextLine"]
    index = {id(e): i for i, e in enumerate(line_els)}
    block_of = {}
    blocks = []
    for b in (e for e in root.iter() if _local(e.tag) == "TextBlock"):
        bi = len(blocks)
        blocks.append({"idx": bi, "box": box_of(b), "_lines": []})
        for ln in b.iter():
            if id(ln) in index:
                block_of[id(ln)] = bi
    lines = []
    for e in line_els:
        words = []
        for el in e:
            if _local(el.tag) == "String" and el.get("CONTENT"):
                bx = box_of(el)
                if bx is None:
                    continue
                wc = el.get("WC")
                try:
                    wc = float(wc) if wc is not None else None
                except ValueError:
                    wc = None
                if wc is not None and not 0 <= wc <= 1:
                    wc = None
                words.append({"text": el.get("CONTENT"), "box": bx, "wc": wc})
        bx = box_of(e) or _union([w["box"] for w in words])
        bi = block_of.get(id(e))
        lines.append({"idx": index[id(e)], "box": bx, "block": bi, "words": words})
        if bi is not None:
            blocks[bi]["_lines"].append(bx)
    for b in blocks:
        if b["box"] is None:
            b["box"] = _union(b["_lines"])
        del b["_lines"]
    return {"lines": lines, "blocks": blocks, "alto_size": tuple(d)}
