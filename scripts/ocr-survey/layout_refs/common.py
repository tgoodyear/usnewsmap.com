"""Shared row schema, writer and gutter derivation for the layout-reference converters (#2107).

Every converter turns one dataset into rows of the same shape (see make_row) and writes them as
JSONL with sorted keys. Gutters come from refgutters.py, COPIED from #2112 (branch
layout-columns-2107, not merged to main yet): pair_gutters / merge_gutters build the chain format,
one gutter = a list of segments [x0, x1, y0, y1].

Row fields:
  articles          [{"box", "id", "regions": [region ids]}]
  column_gutters    gutters from the cleanest text class only (finlam ARTICLE-TEXT, CO ARTICLE, ...)
  dataset, split, page_id, image_path (a file, or "<parquet>#row=N" when the image is embedded),
  width, height, licence, for_publication
  gutters           refgutters.py chains from every text region (headlines, headings, captions too)
  meta              dataset-specific extras (title, date, language, page identity, flags)
  nontext_regions   [{"box", "class", "id", "polygon"?}] pictures, ads, tables, headers
  reading_order     region ids in reading order (text and non-text)
  separators        [{"box", "class", "id", "orientation", "polygon"?}] ruling lines
  text_regions      [{"box", "class", "id", "type", "article"?, "order"?, "polygon"?}]
Boxes are [x0, y0, x1, y1] in the pixel frame of the page image as stored.

Datasets: see each converter's docstring and refs/README.md for URLs, revisions and licences.
"""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import refgutters  # noqa: E402  (copied from #2112)

ROW_KEYS = (
    "articles", "column_gutters", "dataset", "for_publication", "gutters", "height", "image_path",
    "licence", "meta", "nontext_regions", "page_id", "reading_order", "separators", "split",
    "text_regions", "width",
)


def _r(v):
    return round(float(v), 2)


def box_of_polygon(poly):
    xs = [p[0] for p in poly]
    ys = [p[1] for p in poly]
    return [_r(min(xs)), _r(min(ys)), _r(max(xs)), _r(max(ys))]


def clean_polygon(poly):
    return [[_r(p[0]), _r(p[1])] for p in poly]


def clamp_box(box, width, height):
    x0, y0, x1, y1 = box
    return [_r(max(0, min(x0, width))), _r(max(0, min(y0, height))),
            _r(max(0, min(x1, width))), _r(max(0, min(y1, height)))]


def union_box(boxes):
    return [_r(min(b[0] for b in boxes)), _r(min(b[1] for b in boxes)),
            _r(max(b[2] for b in boxes)), _r(max(b[3] for b in boxes))]


def region(rid, cls, box, polygon=None, **extra):
    d = {"box": [_r(v) for v in box], "class": cls, "id": rid}
    if polygon:
        d["polygon"] = clean_polygon(polygon)
    for k, v in extra.items():
        if v is not None:
            d[k] = v
    return d


def derive_gutters(boxes, width, height):
    """Chains of [x0, x1, y0, y1] segments from side-by-side boxes (refgutters.py method)."""
    boxes = [tuple(b) for b in boxes if b[2] > b[0] and b[3] > b[1]]
    if len(boxes) < 2 or width <= 0 or height <= 0:
        return []
    chains = refgutters.merge_gutters(refgutters.pair_gutters(boxes), width, height)
    return [[[_r(v) for v in s] for s in c] for c in chains]


def derive_articles(text_regions, key="article"):
    """Group region ids by their article key, skipping regions without one."""
    groups = {}
    for r in text_regions:
        a = r.get(key)
        if a is None:
            continue
        groups.setdefault(a, []).append(r)
    out = []
    for a in sorted(groups, key=lambda v: (str(type(v)), v)):
        rs = groups[a]
        out.append({"box": union_box([r["box"] for r in rs]), "id": a,
                    "regions": [r["id"] for r in rs]})
    return out


def make_row(dataset, split, page_id, image_path, width, height, licence, for_publication,
             text_regions=(), nontext_regions=(), separators=(), reading_order=(), articles=(),
             gutters=(), column_gutters=(), meta=None):
    return {
        "articles": list(articles), "column_gutters": list(column_gutters), "dataset": dataset,
        "for_publication": bool(for_publication), "gutters": list(gutters), "height": height,
        "image_path": image_path, "licence": licence, "meta": dict(meta or {}),
        "nontext_regions": list(nontext_regions), "page_id": page_id,
        "reading_order": list(reading_order), "separators": list(separators), "split": split,
        "text_regions": list(text_regions), "width": width,
    }


def check_row(row):
    """Return a list of problems (empty if the row is well formed)."""
    bad = []
    if set(row) != set(ROW_KEYS):
        bad.append("keys:%s" % sorted(set(row) ^ set(ROW_KEYS)))
    w, h = row.get("width"), row.get("height")
    if not (isinstance(w, int) and isinstance(h, int) and w > 0 and h > 0):
        bad.append("size")
        return bad
    for k in ("text_regions", "nontext_regions", "separators"):
        for r in row[k]:
            x0, y0, x1, y1 = r["box"]
            if k != "separators" and not (x1 > x0 and y1 > y0):
                bad.append("%s:degenerate" % k)
            if x0 < -1 or y0 < -1 or x1 > w + 1 or y1 > h + 1:
                bad.append("%s:outside" % k)
    ids = [r["id"] for k in ("text_regions", "nontext_regions", "separators") for r in row[k]]
    if len(ids) != len(set(ids)):
        bad.append("duplicate_ids")
    idset = set(ids)
    for rid in row["reading_order"]:
        if rid not in idset:
            bad.append("reading_order:unknown_id")
            break
    return bad


def write_jsonl(path, rows):
    """Write rows with sorted keys, one per line. Returns the row count."""
    n = 0
    tmp = path + ".part"
    with open(tmp, "w") as f:
        for row in rows:
            f.write(json.dumps(row, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n")
            n += 1
    os.replace(tmp, path)
    return n


def read_jsonl(path):
    with open(path) as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def image_size(data):
    """(width, height) of encoded image bytes without decoding the pixels."""
    import io

    from PIL import Image
    with Image.open(io.BytesIO(data)) as im:
        return im.size
