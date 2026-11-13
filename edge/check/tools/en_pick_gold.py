"""Make en_pick.py's --pages file from a gold set (en-op, en-as): crops with a human transcription, LoC's ALTO
and, when there is one, a Xiaomi reading of each crop.

  python3 en_pick_gold.py --set-dir sets/en-op --xiaomi xiaomi-local.jsonl --id-prefix en-op/ \
      --alto-dir data/overproof-ca/loc --out pages.jsonl                      # en-op: <stem>.xml and <stem>.jpg
  python3 en_pick_gold.py --set-dir en-as --xiaomi xiaomi-as.jsonl --provenance as-gold/provenance.json \
      --root as-gold --image-dir as-gold/gold_data/gold_images --out pages.jsonl   # en-as

--set-dir holds manifest.jsonl (crop id, page id, box, source image, page URL) and crops.jsonl (date, title,
transcription). --xiaomi is JSON lines {"id" or "crop_id", "text"}; --id-prefix is put before a crop id to find its row
(en-op's rows are `en-op/<crop id>`). Crops with no Xiaomi row are left out. The truth is the crop's transcription.
A page's damage rate is LoC's own: the share of its words in the crops that differ from the truth, since
the gold sets have no dictionary rate; it is only a band for the dry run.
"""
import argparse
import json
import os
import re

from difflib import SequenceMatcher

import en_pick  # noqa: E402  (run from this folder, or with it on sys.path)
from en_pick import read_jsonl


def page_key(row: dict) -> str:
    """lccn/date/ed-N/seq-N from the page URL, or the page id."""
    m = re.search(r"/resource/(sn\d+)/(\d{4}-\d{2}-\d{2})/ed-(\d+)/\?sp=(\d+)", row.get("page_url") or "")
    if m:
        return f"{m.group(1)}/{m.group(2)}/ed-{m.group(3)}/seq-{m.group(4)}"
    return row["page_id"]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--set-dir", required=True)
    ap.add_argument("--xiaomi", required=True)
    ap.add_argument("--id-prefix", default="")
    ap.add_argument("--alto-dir", help="<stem>.xml and <stem>.jpg per page, stem = the source image's name without .jpg")
    ap.add_argument("--provenance", help="as-gold provenance.json: rows {file, alto}")
    ap.add_argument("--root", help="folder the provenance's `alto` paths are relative to")
    ap.add_argument("--image-dir", help="page images by their source file name (with --provenance)")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    manifest = {r["crop_id"]: r for r in read_jsonl(os.path.join(a.set_dir, "manifest.jsonl"))}
    crops = {r["crop_id"]: r for r in read_jsonl(os.path.join(a.set_dir, "crops.jsonl"))}
    xiaomi = {(r.get("crop_id") or r["id"]): r["text"] for r in read_jsonl(a.xiaomi)}
    prov = {}
    if a.provenance:
        prov = {r["file"]: r["alto"] for r in json.load(open(a.provenance))}
    pages = {}
    for cid, m in manifest.items():
        text = xiaomi.get(a.id_prefix + cid)
        if text is None:
            continue
        src = m["source_image"]
        if a.provenance:
            alto, image = os.path.join(a.root, prov[src]), os.path.join(a.image_dir, src)
        else:
            stem = src.rsplit(".", 1)[0]
            alto, image = os.path.join(a.alto_dir, stem + ".xml"), os.path.join(a.alto_dir, stem + ".jpg")
        key = page_key(m)
        c = crops[cid]
        page = pages.setdefault(key, {"page": key, "source": m.get("page_url"), "date": c["date"],
                                      "label": f"{c.get('title', '').strip()}, {c['date']}", "alto": alto, "image": image,
                                      "crops": []})
        page["crops"].append({"id": cid, "box": m["box"], "xiaomi": text, "truth": c["transcription"]})
    from PIL import Image
    for page in pages.values():
        with Image.open(page["image"]) as im:
            size = im.size
        words = en_pick.parse_alto(open(page["alto"], "rb").read(), size)
        wrong = total = 0
        for c in page["crops"]:
            loc = [en_pick.bt.clean(w["text"]) for w in en_pick.in_crop(words, c["box"])]
            truth = en_pick.bt.clean(c["truth"]).split()
            hit = sum(b.size for b in SequenceMatcher(None, loc, truth, autojunk=False).get_matching_blocks())
            wrong, total = wrong + len(loc) - hit, total + len(loc)
        page["damage_rate"] = round(wrong / total, 4) if total else None
    with open(a.out, "w", encoding="utf-8") as f:
        for page in pages.values():
            f.write(json.dumps(page, ensure_ascii=False) + "\n")
    print(f"{len(pages)} pages, {sum(len(p['crops']) for p in pages.values())} crops")


if __name__ == "__main__":
    main()
