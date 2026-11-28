#!/usr/bin/env python3
"""Class maps, sha orders, IoU drop and YOLO label format for the layout training set (#2107).

Training plan: https://github.com/tgoodyear/usnewsmap.com/issues/2107 (frozen before training).
Target classes are the ten American Stories layout classes (Dell et al., arXiv:2308.12477), in the
order of the teacher model's metadata, which is also the YOLO class index.

Sources (training splits only):
  Teklia/Newspapers-finlam and Teklia/Newspapers-finlam-La-Liberte (MIT);
  biglam/loc_beyond_words (Lee et al., CIKM 2020, Library of Congress, CC0), where the labels are
  the American Stories model's predictions plus Beyond Words' crowd-verified boxes.
The detector is Ultralytics YOLOv8 (AGPL-3.0). Nothing here reads a test page.

The finlam and La Liberte maps below were set from each dataset's own class list (the
`zone_classes` names in its Hugging Face card), before any training. None means the class is not
labelled in the output: those zones are simply absent.
"""
import hashlib

NAMES = ["article", "author", "cartoon_or_advertisement", "headline", "image_caption", "masthead",
         "newspaper_header", "page_number", "photograph", "table"]
INDEX = {n: i for i, n in enumerate(NAMES)}

# finlam: 13 classes.
FINLAM_MAP = {
    "ARTICLE-TEXT": "article",
    "ARTICLE-TITLE": "headline",
    "ARTICLE-SUBTITLE": "headline",
    "ARTICLE-INSIDEHEADING": "headline",
    "SECTION-TITLE": "headline",
    "CAPTION": "image_caption",
    "AUTHOR": "author",
    "HEADER-TITLE": "masthead",
    "HEADER-TEXT": "newspaper_header",
    "ARTICLE-ILLUSTRATION": "cartoon_or_advertisement",
    "ADVERTISEMENT": "cartoon_or_advertisement",
    "ANNOUNCEMENT": "article",  # text notices: coordinator decision, 23 Nov
    "ARTICLE-TABLE": "table",
}
# La Liberte: 16 classes.
LIBERTE_MAP = {
    "TEXT": "article",
    "TITLE": "headline",
    "SUBTITLE": "headline",
    "INSIDEHEADING": "headline",
    "SECTION-TITLE": "headline",
    "CAPTION": "image_caption",
    "AUTHOR": "author",
    "HEADER-TITLE": "masthead",
    "HEADER-TEXT": "newspaper_header",
    "ILLUSTRATION": "cartoon_or_advertisement",
    "ADVERTISEMENT": "cartoon_or_advertisement",
    "ANNOUNCEMENT": "article",  # text notices: coordinator decision, 23 Nov
    "TABLE": "table",
    "ILLUSTRATEDTEXT": None,   # text with a picture inside: neither class is right
    "TABLECONTENT": None,      # sits inside TABLE zones; the table box carries it
    "ASIDE": None,             # side box of mixed content
}
# Beyond Words: the plan's map.
BEYOND_WORDS_MAP = {
    "Photograph": "photograph",
    "Illustration": "cartoon_or_advertisement",
    "Map": "cartoon_or_advertisement",
    "Comics/Cartoon": "cartoon_or_advertisement",
    "Editorial Cartoon": "cartoon_or_advertisement",
    "Advertisement": "cartoon_or_advertisement",
    "Headline": "headline",
}
MAPS = {
    "Teklia/Newspapers-finlam": FINLAM_MAP,
    "Teklia/Newspapers-finlam-La-Liberte": LIBERTE_MAP,
    "biglam/loc_beyond_words": BEYOND_WORDS_MAP,
}
SHORT = {"Teklia/Newspapers-finlam": "finlam", "Teklia/Newspapers-finlam-La-Liberte": "liberte",
         "biglam/loc_beyond_words": "beyondwords"}

TEACHER_CONF = 0.25
DROP_IOU = 0.3
SHA_PREFIX = "2107:tr:"
HOLDOUT_FRAC = 0.10


def sha_order(page_ids, prefix=SHA_PREFIX):
    """Page ids sorted by sha256(prefix + page_id), ties by id: the plan's order."""
    return sorted(page_ids, key=lambda p: (hashlib.sha256((prefix + str(p)).encode()).hexdigest(), str(p)))


def holdout_count(n):
    """Size of the validation holdout: 10% of n, rounded down."""
    return int(n * HOLDOUT_FRAC)


def split_in_order(ordered):
    """(train, holdout): the last 10% of an already sha-ordered list is the holdout."""
    k = holdout_count(len(ordered))
    return ordered[: len(ordered) - k], ordered[len(ordered) - k:]


def iou(a, b):
    ix = max(0.0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0.0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / union if union > 0 else 0.0


def drop_overlapping(teacher, verified, thr=DROP_IOU):
    """Drop teacher boxes of the same class that overlap a verified box at IoU > thr.

    Both are lists of (x0, y0, x1, y1, class_name).
    """
    out = []
    for t in teacher:
        if any(v[4] == t[4] and iou(t[:4], v[:4]) > thr for v in verified):
            continue
        out.append(t)
    return out


def yolo_line(cls_name, box, width, height, min_px=2.0):
    """One YOLO label line 'cls cx cy w h' (normalised), or None for a degenerate box."""
    x0, y0 = max(0.0, min(box[0], box[2])), max(0.0, min(box[1], box[3]))
    x1, y1 = min(float(width), max(box[0], box[2])), min(float(height), max(box[1], box[3]))
    if x1 - x0 < min_px or y1 - y0 < min_px:
        return None
    return "%d %.6f %.6f %.6f %.6f" % (INDEX[cls_name], (x0 + x1) / 2 / width, (y0 + y1) / 2 / height,
                                       (x1 - x0) / width, (y1 - y0) / height)


def mapped_boxes(row):
    """A refs row's regions as [(x0, y0, x1, y1, american_stories_class)], per the dataset's map."""
    m = MAPS[row["dataset"]]
    out = []
    for r in list(row.get("text_regions", [])) + list(row.get("nontext_regions", [])):
        if r["class"] not in m:
            raise KeyError("class %r of %s is not in its map" % (r["class"], row["dataset"]))
        cls = m[r["class"]]
        if cls is not None:
            out.append((*r["box"], cls))
    return out


def teacher_boxes(dets, conf=TEACHER_CONF):
    """Teacher output [x0, y0, x1, y1, conf, class_name] kept at confidence >= conf, as class boxes."""
    return [(d[0], d[1], d[2], d[3], d[5]) for d in dets if d[4] >= conf]
