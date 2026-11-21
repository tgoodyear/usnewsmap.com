"""Assemble a frozen held-out page-level OCR set.

Sources:
  asgold  American Stories gold (COCO JSON + page JPEGs), --src holds gold_hand_transcription.json and NNNN.jpg
  rz      Reichsanzeiger-GT (PAGE XML in --src/data/reichsanzeiger-1820-1939/GT-PAGE, images in --images)
  cg      Chronicling Germany (--src/annotations/*.xml, --src/images/*.jpg, --src/meta/split.json)

Writes to --out: images/ (original bytes, unchanged), refs.jsonl ({id,page,source,lang,image,ref}),
regions.jsonl (per-region boxes/polygons and text, with a transcribed flag), selection.tsv
(rank order by sha256(--salt + ":" + page)), excluded.tsv.

Usage: python3 -I assemble.py --source asgold|rz|cg --src DIR --out DIR --set NAME --salt STR
         [--images DIR] [--exclude FILE] [--ca-map FILE] [--split NAME] [--take N]
"""
import argparse
import hashlib
import json
import os
import shutil
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import page_text  # noqa: E402  (our own module, next to this script)


def jpeg_size(path):
    with open(path, "rb") as f:
        if f.read(2) != b"\xff\xd8":
            raise ValueError("not a JPEG: " + path)
        while True:
            b = f.read(1)
            while b and b != b"\xff":
                b = f.read(1)
            while b == b"\xff":
                b = f.read(1)
            if not b:
                raise ValueError("no SOF in " + path)
            marker = b[0]
            if marker in (0xD8, 0x01) or 0xD0 <= marker <= 0xD7:
                continue
            (length,) = struct.unpack(">H", f.read(2))
            if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                _, h, w = struct.unpack(">BHH", f.read(5))
                return w, h
            f.seek(length - 2, 1)


def image_kind(path):
    with open(path, "rb") as f:
        head = f.read(3)
    if head[:2] == b"\xff\xd8":
        return ".jpg"
    if head[:2] in (b"P5", b"P6"):
        return ".ppm" if head[:2] == b"P6" else ".pgm"
    if head[:3] == b"\x89PN":
        return ".png"
    raise ValueError("unknown image type: " + path)


def ppm_size(path):
    with open(path, "rb") as f:
        data = f.read(256)
    tokens, i = [], 2
    while len(tokens) < 2:
        while data[i:i + 1].isspace():
            i += 1
        if data[i:i + 1] == b"#":
            while data[i:i + 1] not in (b"\n", b""):
                i += 1
            continue
        j = i
        while not data[j:j + 1].isspace():
            j += 1
        tokens.append(int(data[i:j]))
        i = j
    return tokens[0], tokens[1]


def image_size(path):
    return jpeg_size(path) if image_kind(path) == ".jpg" else ppm_size(path)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def bbox_of_points(points):
    xs, ys = [], []
    for p in points.split():
        x, y = p.split(",")
        xs.append(int(float(x)))
        ys.append(int(float(y)))
    return [min(xs), min(ys), max(xs), max(ys)] if xs else None


def load_asgold(src, ca_map):
    d = json.load(open(os.path.join(src, "gold_hand_transcription.json")))
    cats = {c["id"]: c["name"] for c in d["categories"]}
    ca = {}
    if ca_map:
        for row in json.load(open(ca_map)):
            ca[row["file"]] = row
    pages = []
    for im in d["images"]:
        anns = [a for a in d["annotations"] if a["image_id"] == im["id"]]
        regs = []
        for a in anns:
            x, y, w, h = a["bbox"]
            text = (a.get("text") or "").rstrip("\n")
            regs.append({"region": f"coco-{a['id']}", "type": cats[a["category_id"]],
                         "box": [int(x), int(y), int(x + w), int(y + h)],
                         "polygon": a.get("segmentation"), "text": text, "transcribed": bool(text.strip())})
        # COCO carries no reading order: columns left to right (bins of 6% page width), then top to bottom.
        W = im["width"]
        regs.sort(key=lambda r: (int(r["box"][0] // (W * 0.06)), r["box"][1]))
        stem = os.path.splitext(im["file_name"])[0]
        info = ca.get(im["file_name"]) or {}
        pages.append({"page": f"americanstories-gold-{stem}", "image_src": os.path.join(src, im["file_name"]),
                      "w": im["width"], "h": im["height"], "lang": "eng", "regions": regs,
                      "status": None, "ca": info.get("ca"), "ca_check": info.get("check")})
    return pages


def load_page_xml(xml_path, image_path, page_id, lang):
    meta, regs = page_text.parse(xml_path)
    out = []
    for r in regs:
        lines = [t for t in r["lines"]]
        text = "\n".join(lines).strip("\n")
        if not text.strip() and r.get("region_text"):
            text = r["region_text"]
        out.append({"region": r["id"], "type": r["type"], "box": bbox_of_points(r["coords"]) if r["coords"] else None,
                    "polygon": r["coords"], "text": text, "transcribed": bool(text.strip())})
    return {"page": page_id, "image_src": image_path, "w": meta["w"], "h": meta["h"], "lang": lang,
            "regions": out, "status": meta.get("status")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True, choices=["asgold", "rz", "cg"])
    ap.add_argument("--src", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--set", required=True)
    ap.add_argument("--salt", required=True)
    ap.add_argument("--images")
    ap.add_argument("--exclude", help="file: one page id per line, with a reason after a tab")
    ap.add_argument("--ca-map")
    ap.add_argument("--mark", help="file: page id<TAB>CHURRO-DS split (train/dev/test) for pages in CHURRO-DS")
    ap.add_argument("--split", default="Test")
    ap.add_argument("--take", type=int, default=0, help="keep the first N by rank (0 = all)")
    a = ap.parse_args()

    if a.source == "asgold":
        pages = load_asgold(a.src, a.ca_map)
    elif a.source == "rz":
        gt = os.path.join(a.src, "data", "reichsanzeiger-1820-1939", "GT-PAGE")
        pages = []
        for x in sorted(os.listdir(gt)):
            stem = x[:-4]
            img = os.path.join(a.images, stem + ".jpg")
            if not os.path.exists(img):
                continue
            pages.append(load_page_xml(os.path.join(gt, x), img, f"reichsanzeiger-{stem}", "deu"))
    else:
        stems = json.load(open(os.path.join(a.src, "meta", "split.json")))[a.split]
        pages = [load_page_xml(os.path.join(a.src, "annotations", s + ".xml"), os.path.join(a.src, "images", s + ".jpg"),
                               f"chronicling-germany-{s}", "deu") for s in sorted(stems)]

    excluded = {}
    if a.exclude:
        for line in open(a.exclude):
            line = line.rstrip("\n")
            if line:
                pid, _, why = line.partition("\t")
                excluded[pid] = why or "excluded"

    marks = {}
    if a.mark:
        for line in open(a.mark):
            pid, _, sp = line.rstrip("\n").partition("\t")
            if pid:
                marks[pid] = sp
    for p in pages:
        p["churro"] = marks.get(p["page"], "-")
        p["sha"] = hashlib.sha256(f"{a.salt}:{p['page']}".encode()).hexdigest()
    pages.sort(key=lambda p: p["sha"])
    kept, dropped = [], []
    for p in pages:
        if p["page"] in excluded:
            dropped.append((p, excluded[p["page"]]))
        elif not any(r["transcribed"] for r in p["regions"]):
            dropped.append((p, "no transcribed text"))
        else:
            kept.append(p)
    if a.take:
        for p in kept[a.take:]:
            dropped.append((p, f"beyond --take {a.take}"))
        kept = kept[:a.take]

    os.makedirs(os.path.join(a.out, "images"), exist_ok=True)
    with open(os.path.join(a.out, "refs.jsonl"), "w") as refs, \
            open(os.path.join(a.out, "regions.jsonl"), "w") as regf, \
            open(os.path.join(a.out, "selection.tsv"), "w") as sel:
        clean = []
        sel.write("rank\tsha256_key\tid\tpage\timage_sha256\twidth\theight\tregions\ttranscribed_regions\tref_chars\tstatus\tca_page\tchurro_ds_split\n")
        for rank, p in enumerate(kept, 1):
            ext = image_kind(p["image_src"])
            sid = f"s9-{a.set}-{rank:03d}"
            img_rel = f"images/{sid}{ext}"
            shutil.copyfile(p["image_src"], os.path.join(a.out, img_rel))
            w, h = image_size(os.path.join(a.out, img_rel))
            if (w, h) != (p["w"], p["h"]):
                print(f"warning: {p['page']}: image {w}x{h}, annotation {p['w']}x{p['h']}", file=sys.stderr)
            tr = [r for r in p["regions"] if r["transcribed"]]
            ref = "\n\n".join(r["text"] for r in tr)
            refs.write(json.dumps({"id": sid, "page": p["page"], "source": a.source if a.source != "asgold" else "americanstories-gold",
                                   "lang": p["lang"], "image": img_rel, "ref": ref}, ensure_ascii=False) + "\n")
            for order, r in enumerate(p["regions"]):
                regf.write(json.dumps({"id": sid, "page": p["page"], "order": order, **r}, ensure_ascii=False) + "\n")
            ca = p.get("ca") or {}
            sel.write("\t".join(str(v) for v in [rank, p["sha"], sid, p["page"], sha256_file(os.path.join(a.out, img_rel)),
                                                 w, h, len(p["regions"]), len(tr), len(ref), p.get("status") or "",
                                                 ca.get("doc_id", ""), p["churro"]]) + "\n")
            if p["churro"] != "train":
                clean.append(sid)
    with open(os.path.join(a.out, "subset-not-in-churro-train.txt"), "w") as f:
        f.write("".join(i + "\n" for i in clean))
    with open(os.path.join(a.out, "excluded.tsv"), "w") as ex:
        ex.write("page\tsha256_key\treason\n")
        for p, why in dropped:
            ex.write(f"{p['page']}\t{p['sha']}\t{why}\n")
    print(f"{a.set}: kept {len(kept)}, excluded {len(dropped)}, not in CHURRO train {len(clean)}")


if __name__ == "__main__":
    main()
