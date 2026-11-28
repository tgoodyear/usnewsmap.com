#!/usr/bin/env python3
"""Chronicling Germany -> step 1 v2 reference pages, and the fresh-page selection (#2107).

Plan frozen before scoring (issue #2107, German fresh-page test comment). The adapter maps the PAGE-XML
of Chronicling Germany to the same reference rule that layout_columns/refs.py applies to Europeana rows
(biglam/europeana-newspapers-ground-truth), and writes the same reference-page shape:
{excluded, gutters, height, width, zones}.

Mapping, mirroring conv_europeana.py + refs.row_reference for Europeana:
  separator_vertical (SeparatorRegion)   one gutter of one segment [x0, x1, y0, y1], the polygon's bounding
                                         box, no gap cap (the Europeana rule applies none)
  paragraph (TextRegion)                 body-text zone (zones)
  heading, header, caption               text regions that are neither zones nor excluded (as in Europeana)
  inverted_text                          the same; Europeana has no equivalent, it is treated as a text
                                         region of another type (no zone, not excluded)
  table (TableRegion or structure type)  excluded (Europeana: every non-text region is excluded)
  image (ImageRegion, GraphicRegion, or structure type image/Image)   excluded
  separator_short, separator_horizontal  separators, give no gutter (Europeana: horizontal separators)
  any other or missing structure type    TextRegion: text of another type; SeparatorRegion: no gutter
The structure type is the `structure {type:...}` entry of the region's `custom` attribute.

Data: Chronicling Germany (Schultze, Kuebart et al., University of Bonn; DMLR v02-16, arXiv:2401.16845),
CC BY-NC 4.0, commit 66b50f53. Images are used for evaluation only and are not redistributed.

    cg_refs.py select --data-dir D/66b50f53 --out-dir OUT [--overlays N]
"""
import argparse
import hashlib
import json
import os
import re
import sys
import xml.etree.ElementTree as ET

PREFIX = "2107:cgfresh:"
N_PAGES = 300
IMAGE_TAGS = {"ImageRegion", "GraphicRegion"}
IMAGE_TYPES = {"image", "Image"}
TABLE_TYPES = {"table"}
TEXT_TYPES = {"paragraph", "heading", "header", "caption", "inverted_text"}


def key(pid):
    return hashlib.sha256((PREFIX + pid).encode()).hexdigest()


def _r(v):
    return round(float(v), 2)


def structure_type(el):
    m = re.search(r"structure\s*\{[^}]*?type:([^;}]*)", el.get("custom") or "")
    return m.group(1).strip() if m else None


def polygon_box(el):
    """Bounding box [x0, y0, x1, y1] of the region's own Coords, or None."""
    for c in el:
        if c.tag.endswith("}Coords") or c.tag == "Coords":
            pts = [tuple(float(v) for v in p.split(",")) for p in (c.get("points") or "").split() if "," in p]
            if len(pts) >= 2:
                xs, ys = [p[0] for p in pts], [p[1] for p in pts]
                return [min(xs), min(ys), max(xs), max(ys)]
    return None


def clamp(box, w, h):
    x0, y0, x1, y1 = box
    return [_r(max(0, min(x0, w))), _r(max(0, min(y0, h))), _r(max(0, min(x1, w))), _r(max(0, min(y1, h)))]


def parse_page(path):
    """Regions of one PAGE-XML as dicts {tag, type, box}, plus (width, height)."""
    root = ET.parse(path).getroot()
    page = next(e for e in root.iter() if e.tag.endswith("}Page") or e.tag == "Page")
    w, h = int(page.get("imageWidth")), int(page.get("imageHeight"))
    regs = []
    for e in page:
        tag = e.tag.split("}")[-1]
        if not tag.endswith("Region"):
            continue
        box = polygon_box(e)
        if box is None:
            regs.append({"tag": tag, "type": structure_type(e), "box": None})
            continue
        regs.append({"tag": tag, "type": structure_type(e), "box": clamp(box, w, h)})
    return regs, w, h


def classify(reg):
    """One of: vsep, hsep, zone, text, excluded, other."""
    tag, typ = reg["tag"], reg["type"]
    if tag == "SeparatorRegion":
        return "vsep" if typ == "separator_vertical" else "hsep"
    if tag in IMAGE_TAGS or typ in IMAGE_TYPES:
        return "excluded"
    if tag == "TableRegion" or typ in TABLE_TYPES:
        return "excluded"
    if tag == "TextRegion":
        return "zone" if typ == "paragraph" else "text"
    return "other"


def counts(regs):
    c = {"paragraph": 0, "separator_vertical": 0}
    for r in regs:
        if r["tag"] == "TextRegion" and r["type"] == "paragraph":
            c["paragraph"] += 1
        if r["tag"] == "SeparatorRegion" and r["type"] == "separator_vertical":
            c["separator_vertical"] += 1
    return c


def reference(regs, w, h):
    """{excluded, gutters, height, width, zones}: the Europeana rule on the mapped regions."""
    gut, exc, zones = [], [], []
    for r in regs:
        b = r["box"]
        if b is None:
            continue
        k = classify(r)
        if k == "vsep":
            gut.append([[b[0], b[2], b[1], b[3]]])
        elif k == "excluded":
            exc.append(list(b))
        elif k == "zone" and b[2] > b[0] and b[3] > b[1]:
            zones.append(list(b))
    return {"excluded": exc, "gutters": gut, "height": h, "width": w, "zones": zones}


def read_split(data_dir):
    sp = json.load(open(os.path.join(data_dir, "data", "split.json")))
    return set(sp.get("Test", []))


def overlay(img_path, ref, out_path, max_side=1600):
    from PIL import Image, ImageDraw
    Image.MAX_IMAGE_PIXELS = None
    im = Image.open(img_path).convert("RGB")
    s = max_side / max(im.size)
    im = im.resize((round(im.size[0] * s), round(im.size[1] * s)))
    over = Image.new("RGBA", im.size)
    d = ImageDraw.Draw(over)
    for b in ref["zones"]:
        d.rectangle([v * s for v in b], fill=(0, 120, 255, 45), outline=(0, 120, 255, 160))
    for b in ref["excluded"]:
        d.rectangle([v * s for v in b], fill=(255, 160, 0, 90), outline=(255, 160, 0, 255))
    for g in ref["gutters"]:
        for x0, x1, y0, y1 in g:
            d.rectangle([x0 * s - 1, y0 * s, x1 * s + 1, y1 * s], fill=(255, 0, 0, 200))
    Image.alpha_composite(im.convert("RGBA"), over).convert("RGB").save(out_path, quality=80)


def select(a):
    ann = os.path.join(a.data_dir, "data", "annotations")
    test = read_split(a.data_dir)
    stems = sorted(f[:-4] for f in os.listdir(ann) if f.endswith(".xml"))
    fresh = [p for p in stems if p not in test]
    order = sorted(fresh, key=lambda p: (key(p), p))
    chosen, refs, paths, parsed, qualifying = [], {}, {}, {}, 0
    for p in order:
        regs, w, h = parse_page(os.path.join(ann, p + ".xml"))
        c = counts(regs)
        if c["paragraph"] < 1 or c["separator_vertical"] < 1:
            continue
        qualifying += 1
        if len(chosen) < N_PAGES:
            chosen.append(p)
            refs[p] = reference(regs, w, h)
            paths[p] = os.path.abspath(os.path.join(a.data_dir, "data", "images", p + ".jpg"))
            parsed[p] = [c["paragraph"], c["separator_vertical"]]
    os.makedirs(a.out_dir, exist_ok=True)
    listing = "\n".join(chosen) + "\n"
    open(os.path.join(a.out_dir, "page-ids.txt"), "w").write(listing)
    digest = hashlib.sha256(listing.encode()).hexdigest()
    open(os.path.join(a.out_dir, "page-ids.sha256"), "w").write(digest + "\n")
    json.dump({"for_publication": False, "pages": refs, "set": "cg-fresh"}, open(os.path.join(a.out_dir, "refs.json"), "w"), sort_keys=True)
    json.dump(paths, open(os.path.join(a.out_dir, "paths.json"), "w"), sort_keys=True)
    info = {"non_test_pages": len(fresh), "qualifying": qualifying, "selected": len(chosen), "page_ids_sha256": digest}
    json.dump(info, open(os.path.join(a.out_dir, "selection.json"), "w"), indent=1, sort_keys=True)
    if a.overlays:
        od = os.path.join(a.out_dir, "overlays")
        os.makedirs(od, exist_ok=True)
        step = max(1, len(chosen) // a.overlays)
        for p in chosen[::step][:a.overlays]:
            overlay(paths[p], refs[p], os.path.join(od, p + ".jpg"))
    print(json.dumps(info, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    s = sp.add_parser("select")
    s.add_argument("--data-dir", required=True)
    s.add_argument("--out-dir", required=True)
    s.add_argument("--overlays", type=int, default=0)
    a = ap.parse_args(argv)
    return select(a)


if __name__ == "__main__":
    sys.exit(main())
