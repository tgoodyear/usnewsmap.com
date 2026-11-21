"""Find CA page per AS gold image: AND of proper nouns from two different articles. Usage: --gold FILE --out FILE"""
import argparse, collections, itertools, json, re, urllib.parse, urllib.request
ap = argparse.ArgumentParser(); ap.add_argument("--gold", required=True); ap.add_argument("--out", required=True); a = ap.parse_args()
d = json.load(open(a.gold))
by = collections.defaultdict(list)
for x in d["annotations"]: by[x["image_id"]].append(x)
COMMON = set("The This That There Then They When What Which With From Will Have Their These Those Some Many More Most Such Very After Before Where While Every Other Street Company County State States United America American January February March April June July August September October November December Monday Tuesday Wednesday Thursday Friday Saturday Sunday Christmas Washington Congress President Government".split())
def q(s):
    try:
        r = json.load(urllib.request.urlopen(urllib.request.Request("https://usnewsmap.com/api/v1/aggregate?" + urllib.parse.urlencode({"q": s}), headers={"User-Agent": "usnm-s9-heldout/1"}), timeout=30))
        t = r.get("total", {}); return t.get("hits", 0), t.get("first")
    except Exception: return None, None
out = {}
for im in d["images"]:
    arts = sorted([x for x in by[im["id"]] if len((x.get("text") or "")) > 150], key=lambda x: -len(x["text"]))[:8]
    names = []
    for art in arts:
        t = art["text"].replace("-\n", "")
        c = collections.Counter(w for w in re.findall(r"\b([A-Z][a-z]{4,})\b", t) if w not in COMMON)
        names.append([w for w, _ in c.most_common(2)])
    votes = collections.Counter()
    for (i, A), (j, B) in itertools.islice(itertools.combinations(enumerate(names), 2), 14):
        if not A or not B or A[0] == B[0]: continue
        hits, first = q(A[0] + " " + B[0])
        if first and hits <= 30: votes[first["doc_id"]] += 1
    out[im["file_name"]] = votes.most_common(4)
    print(im["file_name"], votes.most_common(4), flush=True)
json.dump(out, open(a.out, "w"), indent=1)
