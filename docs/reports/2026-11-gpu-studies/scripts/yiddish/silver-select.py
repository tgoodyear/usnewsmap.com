# Silver-set selection, read-only: 100 Yiddish pages from qwen-nonenglish-v1, 20 per LoC title by
# sha256("yid-silver-202611:"+doc) rank, then one crop per page by sha256(doc+":"+crop) rank.
import json, glob, hashlib, collections, sys
root = sys.argv[1]
h = lambda s: hashlib.sha256(s.encode()).hexdigest()
by = collections.defaultdict(list)
for f in glob.glob(root + "/*/qwen.json"):
    j = json.load(open(f))
    if j["language"] != "yid" or j.get("pages"):
        continue
    by[j["doc_id"].split("_")[0]].append((h("yid-silver-202611:" + j["doc_id"]), j))
for t in sorted(by):
    for _, j in sorted(by[t], key=lambda x: x[0])[:20]:
        c = min(j["crops"], key=lambda c: h(j["doc_id"] + ":" + c["id"]))
        print(json.dumps({"doc": j["doc_id"], "title": t, "crop": c["id"], "box": c["box"], "stratum": c["stratum"], "source_job": j["id"]}))
