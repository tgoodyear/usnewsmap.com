"""Score engines against references checked by hand on the glyph-check page (#1620)."""
import argparse, json, re, collections
SY = re.compile("[Ꭰ-Ᏽ]")
p = argparse.ArgumentParser()
p.add_argument("--answers", required=True); p.add_argument("--engine", action="append", required=True, help="name=path.jsonl")
p.add_argument("--out", required=True)
a = p.parse_args()
def letters(s): return "".join(SY.findall(s))
def ed(x, y):
    prev = list(range(len(y) + 1))
    for i, cx in enumerate(x, 1):
        cur = [i]
        for j, cy in enumerate(y, 1): cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (cx != cy)))
        prev = cur
    return prev[-1]
eng = {}
for spec in a.engine:
    name, path = spec.split("=", 1)
    eng[name] = {r["crop"]: letters(r["text"]) for r in map(json.loads, open(path, encoding="utf-8"))}
rows = [json.loads(l) for l in open(a.answers, encoding="utf-8")]
checked, stats = [], collections.Counter()
for r in rows:
    words = [w for w in r["loc"].split() if SY.search(w)]
    dec = {int(d["id"].rsplit("~", 1)[1]): d for d in r["decisions"]}
    parts, ok = [], True
    for wi, w in enumerate(words):
        d = dec.get(wi)
        if d is None: parts.append(letters(w)); continue
        c = d.get("choice")
        if c == "a": parts.append(d["a"])
        elif c == "b": parts.append(d["b"])
        else: ok = False
    stats[r["tier"], ok] += 1
    if ok: checked.append({"crop": r["crop"], "tier": r["tier"], "checked": "".join(parts), "nt": letters(r["reference"])})
res = {"lines_total": len(rows), "lines_fully_checked": len(checked), "by_tier": {f"{t}/{'checked' if o else 'open'}": n for (t, o), n in sorted(stats.items())}}
def cer(get, subset):
    e = n = 0
    for c in subset: e += ed(get(c), c["checked"]); n += len(c["checked"])
    return {"cer": round(e / n, 4), "letters": n, "lines": len(subset)}
for label, subset in [("all", checked), ("silver", [c for c in checked if c["tier"] == "silver"])]:
    res[label] = {"nt_reference": cer(lambda c: c["nt"], subset)}
    for name, m in eng.items():
        sub = [c for c in subset if c["crop"] in m]
        res[label][name] = cer(lambda c, m=m: m[c["crop"]], sub)
json.dump(checked, open(a.out, "w", encoding="utf-8"), ensure_ascii=False, indent=0)
print(json.dumps(res, indent=1))
