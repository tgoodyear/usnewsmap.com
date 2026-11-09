#!/usr/bin/env python3
"""Example pictures for check.usnewsmap.com from the glyph bank (#1620).

For each letter with at least --per kept glyphs, the best-agreeing ones from different pages, as
data URIs in the glyph-check data file's `examples` shape; letters the bank lacks keep the old file's
examples. Ꮎ gets word-initial glyphs (the block G this paper prints at the start of a word) first,
then one from inside a word. Writes a new data file and a contact sheet to look at before loading.

    bank_examples.py --bank bank-rs --data data.json --out data-v2.json --sheet examples-v2.png
"""
import argparse, base64, collections, json, os
from PIL import Image

ap = argparse.ArgumentParser()
ap.add_argument("--bank", required=True); ap.add_argument("--data", required=True)
ap.add_argument("--out", required=True); ap.add_argument("--sheet", required=True)
ap.add_argument("--per", type=int, default=3)
a = ap.parse_args()
by = collections.defaultdict(list)
for line in open(os.path.join(a.bank, "index.jsonl"), encoding="utf-8"):
    r = json.loads(line)
    if r["kept"]:
        key = "Ꮎ^" if r["letter"] == "Ꮎ" and r["initial"] else r["letter"]
        by[key].append(r)
def pick(rows, n):
    rows = sorted(rows, key=lambda r: -r["agree"]); seen, out = set(), []
    for r in rows:
        if r["page"] not in seen:
            out.append(r); seen.add(r["page"])
        if len(out) == n: break
    return out
path = lambda r: os.path.join(a.bank, "glyphs", f"{ord(r['letter']):04x}", r["id"] + ".png")
uri = lambda r: "data:image/png;base64," + base64.b64encode(open(path(r), "rb").read()).decode()
data = json.load(open(a.data, encoding="utf-8"))
old = data["examples"]; new = {}; source = {}
for code in range(0x13A0, 0x13F5):
    ch = chr(code)
    rows = (pick(by["Ꮎ^"], 2) + pick(by["Ꮎ"], 1)) if ch == "Ꮎ" else pick(by[ch], a.per)
    if len(rows) >= a.per:
        new[ch] = [uri(r) for r in rows]; source[ch] = rows
    elif ch in old:
        new[ch] = old[ch]
data["examples"] = new
json.dump(data, open(a.out, "w", encoding="utf-8"), ensure_ascii=False)
tile, cols = 48, 12
letters = sorted(source)
sheet = Image.new("L", (cols * tile * 3 // 1, ((len(letters) + cols - 1) // cols) * tile), 255)
for i, ch in enumerate(letters):
    for j, r in enumerate(source[ch]):
        im = Image.open(path(r)).convert("L"); im.thumbnail((tile - 4, tile - 4))
        sheet.paste(im, (((i % cols) * 3 + j) * tile + 2, (i // cols) * tile + 2))
sheet.save(a.sheet)
print(json.dumps({"from_bank": len(source), "kept_old": len([c for c in new if c not in source]),
                  "letters": len(new), "order": "".join(letters)}, ensure_ascii=False))
