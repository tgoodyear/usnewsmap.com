"""Query quoted headlines / first lines of articles. Usage: --gold FILE --files A,B --out FILE"""
import argparse, collections, json, re, urllib.parse, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--gold", required=True); ap.add_argument("--files", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
d = json.load(open(a.gold)); want = set(a.files.split(","))
cats = {c["id"]: c["name"] for c in d["categories"]}
by = collections.defaultdict(list)
for x in d["annotations"]: by[x["image_id"]].append(x)
def q(s):
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://usnewsmap.com/api/v1/aggregate?" + urllib.parse.urlencode({"q": s}), headers={"User-Agent": "usnm-s9-heldout/1"}), timeout=30))
        t = r.get("total", {}); return t.get("hits", 0), t.get("first")
    except Exception: return None, None
out = {}
for im in d["images"]:
    if im["file_name"] not in want: continue
    c = collections.Counter(); n = 0
    anns = [x for x in by[im["id"]] if (x.get("text") or "").strip()]
    for x in sorted(anns, key=lambda x: -len(x["text"]))[:25]:
        for line in x["text"].split("\n")[1:4]:
            ws = re.findall(r"[A-Za-z]{3,}", line)
            if len(ws) < 4: continue
            hits, first = q('"' + " ".join(ws[:4]) + '"'); n += 1
            if first and hits <= 40: c[first["doc_id"]] += 1
            break
    out[im["file_name"]] = c.most_common(6); print(im["file_name"], n, c.most_common(6), flush=True)
json.dump(out, open(a.out, "w"), indent=1)
