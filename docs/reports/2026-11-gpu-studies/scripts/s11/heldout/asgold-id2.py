"""Identify AS gold pages via proper nouns. Usage: --gold FILE --out FILE"""
import argparse, collections, json, re, urllib.parse, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--gold", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
d = json.load(open(a.gold))
by = collections.defaultdict(list)
for x in d["annotations"]: by[x["image_id"]].append(x)
COMMON = set("The This That There Then They When What Which With From Will Have Their These Those Some Many More Most Such Very After Before Where While Every Other Mrs Miss Street Company County State States United America American January February March April June July August September October November December Monday Tuesday Wednesday Thursday Friday Saturday Sunday".split())
def q(s):
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://usnewsmap.com/api/v1/aggregate?" + urllib.parse.urlencode({"q": s}), headers={"User-Agent": "usnm-s9-heldout/1"}), timeout=30))
        t = r.get("total", {}); return t.get("hits", 0), t.get("first")
    except Exception as e: return None, None
out = []
for im in d["images"]:
    arts = sorted([x for x in by[im["id"]] if len((x.get("text") or "")) > 300], key=lambda x: -len(x["text"]))
    votes = collections.Counter(); info = {}
    for art in arts[:6]:
        caps = collections.Counter(w for w in re.findall(r"(?<=[a-z,;] )([A-Z][a-z]{4,})", art["text"]) if w not in COMMON)
        names = [w for w, _ in caps.most_common(3)]
        if len(names) < 2: continue
        hits, first = q(" ".join(names))
        if first and hits <= 10:
            votes[first["doc_id"]] += 1; info[first["doc_id"]] = {k: first.get(k) for k in ("doc_id", "title", "date", "lccn", "seq")}
    best = votes.most_common(2)
    row = {"file": im["file_name"], "w": im["width"], "h": im["height"], "ca": info[best[0][0]] if best else None, "votes": dict(votes)}
    out.append(row); print(json.dumps(row), flush=True)
json.dump(out, open(a.out, "w"), indent=1)
