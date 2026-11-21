"""Find the Chronicling America page behind each American Stories gold image.

Queries the public usnewsmap.com aggregate API with rare words from each page's
longest transcribed articles; a page is identified when two different articles
point to the same CA page. Usage: --gold FILE --out FILE
"""
import argparse
import collections
import json
import re
import urllib.parse
import urllib.request


def query(q):
    url = "https://usnewsmap.com/api/v1/aggregate?" + urllib.parse.urlencode({"q": q})
    req = urllib.request.Request(url, headers={"User-Agent": "usnm-s9-heldout/1"})
    try:
        return json.load(urllib.request.urlopen(req, timeout=25))
    except Exception as e:  # network or API error: no answer for this query
        return {"error": str(e)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--gold", required=True)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    d = json.load(open(a.gold))
    cats = {c["id"]: c["name"] for c in d["categories"]}
    by = collections.defaultdict(list)
    for x in d["annotations"]:
        by[x["image_id"]].append(x)
    res = []
    for im in d["images"]:
        anns = by[im["id"]]
        cnt = collections.Counter(cats[x["category_id"]] for x in anns)
        arts = sorted([x for x in anns if len((x.get("text") or "").strip()) > 300], key=lambda x: -len(x["text"]))
        votes = collections.Counter()
        info = {}
        tries = []
        for art in arts[:5]:
            words = sorted({w.lower() for w in re.findall(r"[A-Za-z]{7,}", art["text"])}, key=lambda w: (-len(w), w))
            q = " ".join(words[2:6])
            r = query(q)
            t = r.get("total", {})
            first = t.get("first")
            tries.append({"q": q, "hits": t.get("hits"), "doc": first and first.get("doc_id")})
            if first and t.get("hits", 0) <= 5:
                votes[first["doc_id"]] += 1
                info[first["doc_id"]] = {k: first.get(k) for k in ("doc_id", "title", "date", "lccn", "edition", "seq")}
                info[first["doc_id"]]["viewer"] = first.get("links", {}).get("viewer")
        best = votes.most_common(1)
        row = {"file": im["file_name"], "w": im["width"], "h": im["height"], "regions": dict(cnt),
               "chars": sum(len(x.get("text") or "") for x in anns),
               "ca": info[best[0][0]] if best else None, "votes": best[0][1] if best else 0, "tries": tries}
        res.append(row)
        print(json.dumps({k: row[k] for k in ("file", "ca", "votes")}), flush=True)
    json.dump(res, open(a.out, "w"), indent=1)


if __name__ == "__main__":
    main()
