"""Many cheap queries per remaining AS gold image; collect every first-hit doc. Usage: --gold FILE --files A,B --out FILE"""
import argparse, collections, json, re, urllib.parse, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--gold", required=True); ap.add_argument("--files", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
d = json.load(open(a.gold))
want = set(a.files.split(","))
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
    arts = sorted([x for x in by[im["id"]] if len((x.get("text") or "")) > 150], key=lambda x: -len(x["text"]))[:12]
    c = collections.Counter()
    for art in arts:
        t = art["text"].replace("-\n", "")
        words = re.findall(r"\b[A-Za-z]{6,}\b", t)
        freq = collections.Counter(w.lower() for w in words)
        rare = [w for w in sorted(freq, key=lambda w: (-len(w), w)) if freq[w] == 1][:8]
        for k in range(0, 6, 2):
            qs = " ".join(rare[k:k+2])
            if len(qs.split()) < 2: continue
            hits, first = q(qs)
            if first and hits <= 40: c[first["doc_id"]] += 1
    out[im["file_name"]] = c.most_common(10)
    print(im["file_name"], c.most_common(10), flush=True)
json.dump(out, open(a.out, "w"), indent=1)
