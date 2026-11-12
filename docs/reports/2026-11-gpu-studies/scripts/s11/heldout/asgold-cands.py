"""Candidate CA pages for each AS gold image from several word queries. Usage: --gold FILE --out FILE"""
import argparse, collections, json, re, urllib.parse, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--gold", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
d = json.load(open(a.gold))
by = collections.defaultdict(list)
for x in d["annotations"]: by[x["image_id"]].append(x)
COMMON = set("The This That There Then They When What Which With From Will Have Their These Those Some Many More Most Such Very After Before Where While Every Other Street Company County State States United America American January February March April June July August September October November December Monday Tuesday Wednesday Thursday Friday Saturday Sunday".split())
def q(s):
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://usnewsmap.com/api/v1/aggregate?" + urllib.parse.urlencode({"q": s}), headers={"User-Agent": "usnm-s9-heldout/1"}), timeout=30))
        t = r.get("total", {}); return t.get("hits", 0), t.get("first")
    except Exception: return None, None
out = {}
for im in d["images"]:
    arts = sorted([x for x in by[im["id"]] if len((x.get("text") or "")) > 200], key=lambda x: -len(x["text"]))
    c = collections.Counter()
    for art in arts[:8]:
        t = art["text"].replace("-\n", "")
        caps = [w for w, _ in collections.Counter(w for w in re.findall(r"(?<=[a-z,;] )([A-Z][a-z]{4,})", t) if w not in COMMON).most_common(3)]
        rare = sorted({w.lower() for w in re.findall(r"\b[a-z]{9,}\b", t)}, key=lambda w: (-len(w), w))[:3]
        for qs in {" ".join(caps), " ".join(caps[:2]), " ".join(rare)}:
            if len(qs.split()) < 2: continue
            hits, first = q(qs)
            if first and hits <= 20: c[first["doc_id"]] += 1
    out[im["file_name"]] = c.most_common(6)
    print(im["file_name"], c.most_common(6), flush=True)
json.dump(out, open(a.out, "w"), indent=1)
