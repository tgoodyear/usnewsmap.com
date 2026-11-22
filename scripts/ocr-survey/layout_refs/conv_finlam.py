#!/usr/bin/env python3
"""Finlam newspaper segmentation sets -> common layout-reference rows (#2107).

Datasets:
  Teklia/Newspapers-finlam, MIT, https://huggingface.co/datasets/Teklia/Newspapers-finlam
    721 pages (train 623, val 50, test 48), 149 titles, French 19th-20th century, page images
    resized to 2000 px high. Home page: https://finlam.projets.litislab.fr/
  Teklia/Newspapers-finlam-La-Liberte, MIT,
    https://huggingface.co/datasets/Teklia/Newspapers-finlam-La-Liberte
    8,836 pages of ONE title (La Liberte), 1925-28. Training-scale, single title. NOT the same
    scheme as finlam: 16 classes (TEXT, TITLE, SUBTITLE, INSIDEHEADING, SECTION-TITLE, CAPTION,
    AUTHOR, HEADER-*, ILLUSTRATION, ADVERTISEMENT, ANNOUNCEMENT, TABLE, TABLECONTENT,
    ILLUSTRATEDTEXT, ASIDE), columns zone_article_ids and zone_section_ids, no newspaper name or
    zone ids. TEXT plays the role of ARTICLE-TEXT; ILLUSTRATEDTEXT (text with a picture inside) is
    kept as text type "mixed" and ASIDE as "aside", neither feeds the gutters; TABLECONTENT is
    non-text type table_content (it sits inside TABLE zones).
  Pinned revisions are in each dataset's .source.json and in refs/README.md.

Class mapping (zone_classes -> row field, refgutters.py type):
  text_regions:     ARTICLE-TEXT article; ARTICLE-TITLE headline; ARTICLE-SUBTITLE,
                    ARTICLE-INSIDEHEADING, SECTION-TITLE heading; CAPTION caption; AUTHOR author
                    (no gutter); HEADER-TITLE, HEADER-TEXT header (excluded from gutters).
  nontext_regions:  ARTICLE-ILLUSTRATION picture; ADVERTISEMENT advertisement; ARTICLE-TABLE table;
                    ANNOUNCEMENT announcement (small notices; kept as non-text, drop it if you
                    treat notices as text).
  gutters:          refgutters.py chains over all text types in TEXT_TYPES (article, headline,
                    heading, caption), as for the de-rz references.
  column_gutters:   the same method over ARTICLE-TEXT zones only: the gaps between body-text zones.
Zone polygons are kept and converted to pixels (finlam stores percent of the page, 0..100;
La Liberte stores fractions, 0..1); boxes are the polygon bounding boxes. reading_order is the zone ids sorted
by zone_orders; articles group zones by article_id (page-local: ids repeat across pages only if the
source repeats them, so they are keyed per page here).

    conv_finlam.py --data-dir DIR --out-dir DIR --name finlam|liberte
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

CLASS_NAMES = [
    "HEADER-TITLE", "HEADER-TEXT", "ARTICLE-ILLUSTRATION", "ADVERTISEMENT", "ANNOUNCEMENT",
    "ARTICLE-TITLE", "ARTICLE-TEXT", "ARTICLE-SUBTITLE", "ARTICLE-INSIDEHEADING", "CAPTION",
    "AUTHOR", "ARTICLE-TABLE", "SECTION-TITLE",
]
TEXT_CLASS_TYPE = {
    "ARTICLE-TEXT": "article", "ARTICLE-TITLE": "headline", "ARTICLE-SUBTITLE": "heading",
    "ARTICLE-INSIDEHEADING": "heading", "SECTION-TITLE": "heading", "CAPTION": "caption",
    "AUTHOR": "author", "HEADER-TITLE": "header", "HEADER-TEXT": "header",
}
NONTEXT_CLASS_TYPE = {
    "ARTICLE-ILLUSTRATION": "picture", "ADVERTISEMENT": "advertisement",
    "ARTICLE-TABLE": "table", "ANNOUNCEMENT": "announcement",
}
DATASETS = {
    "finlam": {"hf": "Teklia/Newspapers-finlam", "dir": "Teklia__Newspapers-finlam",
               "dataset": "Teklia/Newspapers-finlam", "scale": "test-scale, 149 titles"},
    "liberte": {"hf": "Teklia/Newspapers-finlam-La-Liberte",
                "dir": "Teklia__Newspapers-finlam-La-Liberte",
                "dataset": "Teklia/Newspapers-finlam-La-Liberte",
                "scale": "training-scale, single title"},
}
TEXT_TYPES = ("article", "headline", "heading", "caption")
# La-Liberte uses a different 16-class scheme (and zone_article_ids / zone_section_ids columns).
LIB_CLASS_NAMES = [
    "HEADER-TITLE", "HEADER-TEXT", "SECTION-TITLE", "ILLUSTRATION", "ADVERTISEMENT", "ANNOUNCEMENT",
    "TITLE", "TEXT", "SUBTITLE", "INSIDEHEADING", "CAPTION", "AUTHOR", "TABLE", "ILLUSTRATEDTEXT",
    "TABLECONTENT", "ASIDE",
]
LIB_TEXT_CLASS_TYPE = {
    "TEXT": "article", "TITLE": "headline", "SUBTITLE": "heading", "INSIDEHEADING": "heading",
    "SECTION-TITLE": "heading", "CAPTION": "caption", "AUTHOR": "author", "HEADER-TITLE": "header",
    "HEADER-TEXT": "header", "ILLUSTRATEDTEXT": "mixed", "ASIDE": "aside",
}
LIB_NONTEXT_CLASS_TYPE = {
    "ILLUSTRATION": "picture", "ADVERTISEMENT": "advertisement", "TABLE": "table",
    "TABLECONTENT": "table_content", "ANNOUNCEMENT": "announcement",
}


def flatten_polygon(poly):
    """A zone polygon as a list of [x, y], tolerating one extra nesting level."""
    if not poly:
        return []
    if poly and poly[0] and isinstance(poly[0][0], (list, tuple)):
        poly = [p for ring in poly for p in ring]
    return [[float(p[0]), float(p[1])] for p in poly if len(p) >= 2]


def polygon_scale(peak, lib, width, height):
    """Multipliers (sx, sy) that turn source polygon units into pixels.

    finlam polygons are percent of the page (0..100); La-Liberte polygons are fractions (0..1).
    The unit is decided from the largest coordinate on the page; (1, 1) means already pixels.
    """
    if peak <= 0 or min(width, height) <= 100:
        return 1.0, 1.0
    if lib and peak <= 1.0 + 1e-6:
        return float(width), float(height)
    if not lib and peak <= 100.0 + 1e-6:
        return width / 100.0, height / 100.0
    return 1.0, 1.0


NAME_DATE = re.compile(r"^(.*?)\s+-\s+(\d{4})-(\d{2})-(\d{2})\b")


def parse_newspaper_name(name):
    """(title, date, year) from names like 'Paris-midi - 1932-01-25 (4729329)'; Nones if no date."""
    m = NAME_DATE.match(name or "")
    if not m:
        return (name or None), None, None
    return m.group(1), "%s-%s-%s" % m.groups()[1:], int(m.group(2))


def row_from_record(rec, split, image_path, width, height, dataset, licence="MIT", scale=None):
    """One page row from a finlam record (dict of the parquet columns minus the image)."""
    classes, polys = rec["zone_classes"], rec["zone_polygons"]
    lib = "Liberte" in dataset
    names = LIB_CLASS_NAMES if lib else CLASS_NAMES
    text_map = LIB_TEXT_CLASS_TYPE if lib else TEXT_CLASS_TYPE
    nontext_map = LIB_NONTEXT_CLASS_TYPE if lib else NONTEXT_CLASS_TYPE
    col_class = "TEXT" if lib else "ARTICLE-TEXT"
    orders = rec.get("zone_orders") or [None] * len(classes)
    arts = rec.get("zone_article_ids") or rec.get("article_id") or [None] * len(classes)
    sections = rec.get("zone_section_ids") or [None] * len(classes)
    texts = rec.get("zone_texts") or [""] * len(classes)
    text_regions, nontext_regions, problems = [], [], []
    order_of = {}
    # Units: La-Liberte polygons are fractions of the page (0..1), finlam polygons are percent of
    # the page (0..100). Detect from the data so a pixel-unit file would also pass through.
    allpts = [p for z in polys for p in flatten_polygon(z)]
    sx, sy = polygon_scale(max((max(p) for p in allpts), default=0.0), lib, width, height)
    for i, c in enumerate(classes):
        name = names[c] if isinstance(c, int) else c
        poly = flatten_polygon(polys[i])
        if (sx, sy) != (1.0, 1.0):
            poly = [[x * sx, y * sy] for x, y in poly]
        if len(poly) < 3:
            problems.append("zone %d has %d points" % (i, len(poly)))
            continue
        box = common.clamp_box(common.box_of_polygon(poly), width, height)
        if not (box[2] > box[0] and box[3] > box[1]):
            problems.append("zone %d degenerate box" % i)
            continue
        rid = "z%03d" % i
        art = arts[i] if arts[i] is not None and arts[i] >= 0 else None
        order = orders[i] if orders[i] is not None and orders[i] >= 0 else None
        order_of[rid] = order
        sec = sections[i] if sections[i] is not None and sections[i] >= 0 else None
        if name in text_map:
            text_regions.append(common.region(rid, name, box, poly, type=text_map[name], article=art,
                                              order=order, section=sec, n_chars=len(texts[i] or "")))
        elif name in nontext_map:
            nontext_regions.append(common.region(rid, name, box, poly, type=nontext_map[name],
                                                 article=art, order=order, section=sec))
        else:
            problems.append("zone %d unknown class %r" % (i, name))
    allr = text_regions + nontext_regions
    ordered = sorted((r for r in allr if r.get("order") is not None),
                     key=lambda r: (r["order"], r["id"]))
    articles = common.derive_articles(allr)
    gtext = [r["box"] for r in text_regions if r["type"] in TEXT_TYPES]
    gcol = [r["box"] for r in text_regions if r["class"] == col_class]
    title, date, year = parse_newspaper_name(rec.get("newspaper_name"))
    if lib:
        title = "La Liberte"  # single title; the card gives 1925-28 but no per-page date
    meta = {
        "date": date, "title": title, "year": year,
        "language": "fr" if "Liberte" in dataset else "fr (some en)",
        "newspaper_arkindex_id": rec.get("newspaper_arkindex_id"),
        "newspaper_name": rec.get("newspaper_name"),
        "page_arkindex_id": rec.get("page_arkindex_id"),
        "page_index": rec.get("page_index"),
        "problems": problems,
        "scale": scale,
    }
    return common.make_row(
        dataset, split, str(rec["page_arkindex_id"]), image_path, width, height, licence, True,
        text_regions=text_regions, nontext_regions=nontext_regions,
        reading_order=[r["id"] for r in ordered], articles=articles,
        gutters=common.derive_gutters(gtext, width, height),
        column_gutters=common.derive_gutters(gcol, width, height), meta=meta)


def split_of(filename):
    return os.path.basename(filename).split("-")[0]


def convert(data_dir, out_dir, name, limit=None, batch_size=4):
    import pyarrow.parquet as pq
    spec = DATASETS[name]
    files = sorted(f for f in os.listdir(os.path.join(data_dir, "data")) if f.endswith(".parquet"))
    by_split = {}
    for f in files:
        by_split.setdefault(split_of(f), []).append(f)
    import pyarrow.parquet as _pq
    present = set(_pq.ParquetFile(os.path.join(data_dir, "data", files[0])).schema_arrow.names)
    cols = [c for c in ["newspaper_name", "newspaper_arkindex_id", "page_arkindex_id", "page_index",
                        "page_image", "zone_arkindex_ids", "zone_polygons", "zone_texts",
                        "zone_classes", "zone_orders", "article_id", "zone_article_ids",
                        "zone_section_ids"] if c in present]
    summary = {}
    for split, fs in sorted(by_split.items()):
        def rows():
            for fn in fs:
                path = os.path.abspath(os.path.join(data_dir, "data", fn))
                pf = pq.ParquetFile(path)
                n = 0
                for batch in pf.iter_batches(batch_size=batch_size, columns=cols):
                    for rec in batch.to_pylist():
                        img = rec.pop("page_image")
                        w, h = common.image_size(img["bytes"])
                        yield row_from_record(rec, split, "%s#row=%d" % (path, n), w, h,
                                              spec["dataset"], scale=spec["scale"])
                        n += 1
                        if n % 200 == 0:
                            common.log("%s %s %s: %d rows" % (name, split, fn, n))
                        if limit and n >= limit:
                            return
        out = os.path.join(out_dir, "%s__%s.jsonl" % (spec["dir"], split))
        summary[split] = common.write_jsonl(out, rows())
        common.log("%s %s: wrote %d rows to %s" % (name, split, summary[split], out))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-dir", required=True, help="the downloaded dataset directory")
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--name", required=True, choices=sorted(DATASETS))
    ap.add_argument("--limit", type=int, help="rows per parquet file (smoke tests)")
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    print(convert(a.data_dir, a.out_dir, a.name, a.limit))
    return 0


if __name__ == "__main__":
    sys.exit(main())
