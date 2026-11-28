#!/usr/bin/env python3
"""Colorado newspaper layout YOLO set -> common layout-reference rows (#2107).

Dataset: NealCaren/co-newspaper-layout-yolo, https://huggingface.co/datasets/NealCaren/co-newspaper-layout-yolo
  1,110 Colorado Historic Newspapers pages (US English), DocLayout-YOLO labels
  (class cx cy w h, normalised) for ADVERTISEMENT, ARTICLE, ARTICLE+ILLUSTRATION, ILLUSTRATION,
  TITLE_SECTION, TITLE_SECTION+ILLUSTRATION, with train.txt and val.txt lists.
  The dataset card states NO LICENCE. PRIVATE TUNING ONLY: every row is for_publication=false; its
  images, crops and per-page numbers must not go into a repo, PR, issue or any public place.
  Only aggregate counts may be reported.

Mapping:
  text_regions     ARTICLE (type article), TITLE_SECTION (headline), and the two mixed classes
                   ARTICLE+ILLUSTRATION / TITLE_SECTION+ILLUSTRATION (type mixed: text with a picture
                   inside the box, flagged contains_illustration; no gutter from them).
  nontext_regions  ADVERTISEMENT (advertisement) and ILLUSTRATION (picture). Mixed boxes are not in
                   the non-text mask because only part of each is a picture.
  gutters          refgutters.py chains over ARTICLE and TITLE_SECTION boxes.
  column_gutters   the same over ARTICLE boxes only.
  articles         one entry per ARTICLE or ARTICLE+ILLUSTRATION box (the labels have no article
                   grouping and no reading order, so reading_order is empty).
Page names look like ADS19100425_1_1 (title code, date yyyymmdd, then two numbers); meta keeps them
parsed (title_code, date, year, part_a, part_b). They are NOT confirmed as edition and sequence.

    conv_coyolo.py --data-dir DIR --out-dir DIR
"""
import argparse
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402

DATASET = "NealCaren/co-newspaper-layout-yolo"
DIRNAME = "NealCaren__co-newspaper-layout-yolo"
LICENCE = "unstated on the dataset card (private research use only; do not redistribute)"
CLASS_NAMES = ["ADVERTISEMENT", "ARTICLE", "ARTICLE+ILLUSTRATION", "ILLUSTRATION", "TITLE_SECTION",
               "TITLE_SECTION+ILLUSTRATION"]
TEXT_TYPE = {"ARTICLE": "article", "TITLE_SECTION": "headline", "ARTICLE+ILLUSTRATION": "mixed",
             "TITLE_SECTION+ILLUSTRATION": "mixed"}
NONTEXT_TYPE = {"ADVERTISEMENT": "advertisement", "ILLUSTRATION": "picture"}
NAME = re.compile(r"^([A-Za-z]+)(\d{8})(?:-(\d+))?_(\d+)_(\d+)$")


def parse_name(stem):
    m = NAME.match(stem)
    if not m:
        return {}
    d = m.group(2)
    return {"date": "%s-%s-%s" % (d[:4], d[4:6], d[6:]), "part_a": int(m.group(4)),
            "part_b": int(m.group(5)), "suffix": m.group(3), "title_code": m.group(1),
            "year": int(d[:4])}


def parse_labels(text):
    """Yield (class_id, cx, cy, w, h) per well-formed line; bad lines are returned separately."""
    good, bad = [], []
    for ln in text.splitlines():
        p = ln.split()
        if not p:
            continue
        try:
            if len(p) != 5:
                raise ValueError
            good.append((int(p[0]), float(p[1]), float(p[2]), float(p[3]), float(p[4])))
        except ValueError:
            bad.append(ln[:40])
    return good, bad


def row_from_labels(stem, split, image_path, width, height, label_text):
    labels, bad = parse_labels(label_text)
    problems = ["unparseable label line %r" % b for b in bad]
    text_regions, nontext = [], []
    for i, (c, cx, cy, w, h) in enumerate(labels):
        if not 0 <= c < len(CLASS_NAMES):
            problems.append("class %d out of range" % c)
            continue
        box = common.clamp_box([(cx - w / 2) * width, (cy - h / 2) * height,
                                (cx + w / 2) * width, (cy + h / 2) * height], width, height)
        if not (box[2] > box[0] and box[3] > box[1]):
            problems.append("box %d degenerate" % i)
            continue
        name, rid = CLASS_NAMES[c], "b%03d" % i
        if name in TEXT_TYPE:
            extra = {"contains_illustration": True} if "ILLUSTRATION" in name else {}
            text_regions.append(common.region(rid, name, box, type=TEXT_TYPE[name], **extra))
        else:
            nontext.append(common.region(rid, name, box, type=NONTEXT_TYPE[name]))
    arts = [{"box": r["box"], "id": r["id"], "regions": [r["id"]]}
            for r in text_regions if r["class"] in ("ARTICLE", "ARTICLE+ILLUSTRATION")]
    gt = [r["box"] for r in text_regions if r["type"] in ("article", "headline")]
    gc = [r["box"] for r in text_regions if r["type"] == "article"]
    meta = dict(parse_name(stem))
    meta.update({"language": "en", "problems": problems})
    return common.make_row(
        DATASET, split, stem, image_path, width, height, LICENCE, False,
        text_regions=text_regions, nontext_regions=nontext, articles=arts,
        gutters=common.derive_gutters(gt, width, height),
        column_gutters=common.derive_gutters(gc, width, height), meta=meta)


def read_list(path):
    if not os.path.exists(path):
        return None
    with open(path) as f:
        return {os.path.splitext(os.path.basename(ln.strip()))[0] for ln in f if ln.strip()}


def convert(data_dir, out_dir, limit=None):
    from PIL import Image
    root = os.path.join(data_dir, "newspaper")
    lists = {s: read_list(os.path.join(root, s + ".txt")) for s in ("train", "val")}
    if lists["train"] is None and lists["val"] is None:
        # the yaml says the lists sit next to the dataset root; fall back to the data dir
        lists = {s: read_list(os.path.join(data_dir, s + ".txt")) for s in ("train", "val")}
    stems = sorted(os.path.splitext(f)[0] for f in os.listdir(os.path.join(root, "images"))
                   if f.lower().endswith((".jpg", ".jpeg", ".png")))
    if limit:
        stems = stems[:limit]
    listed = (lists["train"] or set()) | (lists["val"] or set())
    unlabelled = [s for s in stems if s not in listed and not os.path.exists(os.path.join(root, "labels", s + ".txt"))]
    if unlabelled:
        common.log("coyolo: skipping %d images that are in neither list and have no label file" % len(unlabelled))
    stems = [s for s in stems if s not in set(unlabelled)]
    by_split = {}
    for i, s in enumerate(stems):
        sp = "train" if s in (lists["train"] or ()) else "val" if s in (lists["val"] or ()) else "unlisted"
        by_split.setdefault(sp, []).append(s)
        if (i + 1) % 200 == 0:
            common.log("coyolo: scanned %d names" % (i + 1))
    summary = {}
    for split, ss in sorted(by_split.items()):
        def rows():
            for n, s in enumerate(ss):
                ip = os.path.join(root, "images")
                ipath = next(os.path.join(ip, s + e) for e in (".jpg", ".jpeg", ".png")
                             if os.path.exists(os.path.join(ip, s + e)))
                with Image.open(ipath) as im:
                    w, h = im.size
                lp = os.path.join(root, "labels", s + ".txt")
                txt = open(lp).read() if os.path.exists(lp) else ""
                r = row_from_labels(s, split, os.path.abspath(ipath), w, h, txt)
                if not os.path.exists(lp):
                    r["meta"]["problems"].append("no label file")
                yield r
                if (n + 1) % 200 == 0:
                    common.log("coyolo %s: %d rows" % (split, n + 1))
        out = os.path.join(out_dir, "%s__%s.jsonl" % (DIRNAME, split))
        summary[split] = common.write_jsonl(out, rows())
        common.log("coyolo %s: wrote %d rows" % (split, summary[split]))
    return summary


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--limit", type=int)
    a = ap.parse_args(argv)
    os.makedirs(a.out_dir, exist_ok=True)
    print(convert(a.data_dir, a.out_dir, a.limit))
    return 0


if __name__ == "__main__":
    sys.exit(main())
