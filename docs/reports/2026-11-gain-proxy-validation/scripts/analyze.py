#!/usr/bin/env python3
"""Relates the damage-rate drop to the CER drop on the frozen #968 sets (#1415 gain-proxy validation).

    analyze.py --dir /tank/usnm-lake/968-dev2-20261112 --rows rows-out.jsonl --out OUT_DIR [--boot 1000] [--seed 202611]

--rows is proxy-rows' output for prep_rows.py's input. Standard library only.

Units. A "crop" row is one reader's reading of one crop. A "page" row sums the head and near counts and the
edits of a page's crops, which is the scale the campaign decides on (a page, then a cell's mean over pages).
Pairs. For every pair of readers (Tesseract PSM 3 against 6 is left out: one engine), the one with the higher pooled CER on the language's crops is the baseline
(the stand-in for LoC), the other the re-read. drop = baseline - re-read, for the proxy and for the CER. A crop
where either reader returned no text is left out of that pair (an empty reading has no rate).
Limits are 95% percentile bootstraps resampling whole source pages, all pairs of a page together.
"""
import argparse, collections, itertools, json, math, random
from pathlib import Path

READERS = ["tess3", "tess6", "xiaomi-local", "qwen8b-local", "loc"]
LANG_NAME = {"eng": "English", "deu": "German", "fra": "French", "fin": "Finnish", "swe": "Swedish"}
GROUPS = {"eng": ["en-op"], "deu": ["de-nzz", "de-rz81"], "fra": ["new-newseye-bnf", "fr-52"],
          "fin": ["new-newseye-nlf-fi", "fi-52"], "swe": ["new-newseye-nlf-sv", "sv-52"]}
T = 0.02          # the stop rule's threshold
CER_STEP = 0.01   # a CER drop worth having: one point
CHUNK_HEAD = 100  # head-word tokens in the reference per chunk
GRID = [0.005, 0.01, 0.02, 0.03, 0.05, 0.075, 0.10]


def rate_damage(c):
    d = c["near"] + c["head"]
    return c["near"] / d if d else None


def rate_unknown(c):
    return 1 - c["known"] / c["words"] if c["words"] else None


PROXIES = {"damage": rate_damage, "unknown": rate_unknown}


def ranks(xs):
    order = sorted(range(len(xs)), key=xs.__getitem__)
    r = [0.0] * len(xs)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and xs[order[j + 1]] == xs[order[i]]:
            j += 1
        for k in range(i, j + 1):
            r[order[k]] = (i + j) / 2 + 1
        i = j + 1
    return r


def pearson(a, b):
    n = len(a)
    if n < 3:
        return None
    ma, mb = sum(a) / n, sum(b) / n
    sa = sum((x - ma) ** 2 for x in a)
    sb = sum((y - mb) ** 2 for y in b)
    if sa == 0 or sb == 0:
        return None
    return sum((x - ma) * (y - mb) for x, y in zip(a, b)) / math.sqrt(sa * sb)


def spearman(a, b):
    return pearson(ranks(a), ranks(b)) if len(a) >= 3 else None


def pct(xs, q):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    k = (len(xs) - 1) * q
    lo, hi = int(math.floor(k)), int(math.ceil(k))
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def origin_slope(x, y):
    sx = sum(v * v for v in x)
    return sum(a * b for a, b in zip(x, y)) / sx if sx else None


def stats(units):
    """units: list of (drop_proxy, drop_cer, page). Point statistics."""
    x = [u[0] for u in units]
    y = [u[1] for u in units]
    up = [u for u in units if u[0] >= T]
    s = {"n": len(units), "rho": spearman(x, y), "slope": origin_slope(x, y)}
    s["implied_cer_at_t"] = None if s["slope"] is None else s["slope"] * T
    s["n_up"] = len(up)
    s["up_cer_improves"] = sum(u[1] > 0 for u in up) / len(up) if up else None
    s["up_cer_step"] = sum(u[1] >= CER_STEP for u in up) / len(up) if up else None
    s["up_median_cer"] = pct([u[1] for u in up], 0.5)
    rest = [u for u in units if u[0] < T]
    big = [u for u in units if u[1] >= CER_STEP]
    s["n_big_cer"] = len(big)
    s["recall_big_cer"] = sum(u[0] >= T for u in big) / len(big) if big else None
    dn = [u for u in units if u[0] <= -T]
    s["n_down"] = len(dn)
    s["down_cer_worsens"] = sum(u[1] < 0 for u in dn) / len(dn) if dn else None
    s["missed_gain"] = sum(u[1] >= CER_STEP for u in rest) / len(rest) if rest else None
    return s


def boot_ci(units, fn, B, rng):
    by = collections.defaultdict(list)
    for u in units:
        by[u[2]].append(u)
    pages = list(by)
    vals = []
    for _ in range(B):
        samp = []
        for p in (rng.choice(pages) for _ in pages):
            samp.extend(by[p])
        vals.append(fn(samp))
    return pct(vals, 0.025), pct(vals, 0.975)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", type=Path, required=True)
    ap.add_argument("--rows", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--boot", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=202611)
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    meta = {}
    for l in a.dir.joinpath("refs.jsonl").read_text(encoding="utf-8").splitlines():
        if l.strip():
            r = json.loads(l)
            meta[r["id"]] = r
    data = collections.defaultdict(dict)  # crop id -> reader -> counts
    for l in a.rows.read_text(encoding="utf-8").splitlines():
        r = json.loads(l)
        rd, cid = r["id"].split("|", 1)
        data[cid][rd] = r
    out = {}
    rng = random.Random(a.seed)
    for lang, sets in GROUPS.items():
        crops = [c for c in data if meta[c]["source"] in sets]
        res = {"sets": sets, "crops": len(crops), "pages": len({meta[c]["page"] for c in crops})}
        # per reader descriptives
        desc = {}
        for rd in READERS:
            cs = [c for c in crops if rd in data[c]]
            ne = [c for c in cs if data[c][rd]["hyp_words"]]
            rc = sum(data[c][rd]["ref_chars"] for c in cs)
            hh = sum(data[c][rd]["hyp_head"] for c in ne)
            hn = sum(data[c][rd]["hyp_near"] for c in ne)
            rh = sum(data[c][rd]["ref_head"] for c in ne)
            rn = sum(data[c][rd]["ref_near"] for c in ne)
            ed = sum(data[c][rd]["char_edits"] for c in cs)
            fe = sum(data[c][rd]["folded_edits"] for c in cs)
            hw = sum(data[c][rd]["hyp_words"] for c in ne)
            hk = sum(data[c][rd]["hyp_known"] for c in ne)
            if not cs:
                continue
            desc[rd] = {"crops": len(cs), "empty": len(cs) - len(ne), "cer": ed / rc if rc else None,
                        "folded_cer": fe / rc if rc else None,
                        "damage": hn / (hn + hh) if hn + hh else None,
                        "ref_damage_same_crops": rn / (rn + rh) if rn + rh else None,
                        "unknown": 1 - hk / hw if hw else None, "head_per_crop": hh / len(ne) if ne else None}
        res["readers"] = desc
        res["ref_head_median_crop"] = pct([data[c][READERS[0]]["ref_head"] for c in crops if READERS[0] in data[c]], 0.5)
        hp = collections.defaultdict(int)
        for c in crops:
            hp[meta[c]["page"]] += data[c][READERS[0]]["ref_head"]
        res["ref_head_median_page"] = pct(list(hp.values()), 0.5)
        res["ref_head_lt5_crops"] = sum(data[c][READERS[0]]["ref_head"] < 5 for c in crops) / len(crops)
        res["ref_head_lt10_page"] = sum(v < 10 for v in hp.values()) / len(hp)
        # Chunks: a set's crops in page order, cut where the reference has CHUNK_HEAD head-word tokens, so a
        # unit has about the head-word count of a real page (the sampled "pages" hold two or three crops).
        chunk_of = {}
        for s_ in sets:
            acc, k = 0, 0
            for c in sorted((c for c in crops if meta[c]["source"] == s_), key=lambda c: (meta[c]["page"], c)):
                chunk_of[c] = f"{s_}#{k}"
                acc += data[c][READERS[0]]["ref_head"]
                if acc >= CHUNK_HEAD:
                    acc, k = 0, k + 1
        present = [r for r in READERS if r in desc]
        pairs = []
        for r1, r2 in itertools.combinations(present, 2):
            if r1.startswith("tess") and r2.startswith("tess"):
                continue  # PSM 3 and 6 are one engine: a pair with no difference to relate
            both = [c for c in crops if r1 in data[c] and r2 in data[c]]
            c1 = sum(data[c][r1]["char_edits"] for c in both)
            c2 = sum(data[c][r2]["char_edits"] for c in both)
            pairs.append((r1, r2) if c1 >= c2 else (r2, r1))
        res["pairs"] = [f"{b}>{n}" for b, n in pairs]
        for proxy, fn in PROXIES.items():
            for level, target in itertools.product(("crop", "page", "chunk"), ("cer", "folded")):
                ekey = "char_edits" if target == "cer" else "folded_edits"
                units, by_pair = [], {}
                for b, n in pairs:
                    cs = [c for c in crops if b in data[c] and n in data[c] and data[c][b]["hyp_words"] and data[c][n]["hyp_words"]]
                    if level == "crop":
                        grp = {c: [c] for c in cs}
                    elif level == "chunk":
                        grp = collections.defaultdict(list)
                        for c in cs:
                            grp[chunk_of[c]].append(c)
                    else:
                        grp = collections.defaultdict(list)
                        for c in cs:
                            grp[meta[c]["page"]].append(c)
                    pu = []
                    for key, members in grp.items():
                        agg = {}
                        for who in (b, n):
                            s = collections.Counter()
                            for c in members:
                                r = data[c][who]
                                s["head"] += r["hyp_head"]; s["near"] += r["hyp_near"]; s["known"] += r["hyp_known"]
                                s["words"] += r["hyp_words"]; s["edits"] += r[ekey]; s["chars"] += r["ref_chars"]
                            agg[who] = s
                        pb, pn = fn(agg[b]), fn(agg[n])
                        if pb is None or pn is None or not agg[b]["chars"]:
                            continue
                        pu.append((pb - pn, (agg[b]["edits"] - agg[n]["edits"]) / agg[b]["chars"], (meta[members[0]]["page"] if level != "chunk" else chunk_of[members[0]]), key))
                    by_pair[f"{b}>{n}"] = pu
                    units.extend(pu)
                block = {"point": stats(units)}
                if proxy == "damage" or level != "crop":
                    block["rho_ci"] = boot_ci(units, lambda u: spearman([v[0] for v in u], [v[1] for v in u]), a.boot if level != "crop" else max(a.boot // 4, 100), rng)
                    block["implied_ci"] = boot_ci(units, lambda u: (lambda s: None if s is None else s * T)(origin_slope([v[0] for v in u], [v[1] for v in u])), a.boot if level != "crop" else max(a.boot // 4, 100), rng)
                block["by_pair"] = {k: stats(v) for k, v in by_pair.items()}
                if level != "crop":
                    block["sd_drop_proxy"] = math.sqrt(sum(u[0] ** 2 for u in units) / len(units)) if units else None
                    block["grid"] = {}
                    for t in GRID:
                        up = [u for u in units if u[0] >= t]
                        block["grid"][t] = {"n": len(up), "cer_improves": sum(u[1] > 0 for u in up) / len(up) if up else None,
                                            "cer_step": sum(u[1] >= CER_STEP for u in up) / len(up) if up else None,
                                            "median_cer": pct([u[1] for u in up], 0.5)}
                res[f"{proxy}_{level}_{target}"] = block
        # set-level (a cell): pooled proxy drop vs pooled CER drop, per set and pair
        cells = []
        for s in sets:
            cs = [c for c in crops if meta[c]["source"] == s]
            for b, n in pairs:
                m = [c for c in cs if b in data[c] and n in data[c] and data[c][b]["hyp_words"] and data[c][n]["hyp_words"]]
                if len(m) < 5:
                    continue
                def pool(who):
                    s_ = collections.Counter()
                    for c in m:
                        r = data[c][who]
                        for k, kk in (("head", "hyp_head"), ("near", "hyp_near"), ("known", "hyp_known"), ("words", "hyp_words"), ("edits", "char_edits"), ("chars", "ref_chars"), ("fed", "folded_edits")):
                            s_[k] += r[kk]
                    return s_
                pb, pn = pool(b), pool(n)
                cells.append({"set": s, "pair": f"{b}>{n}", "crops": len(m),
                              "d_damage": rate_damage(pb) - rate_damage(pn), "d_unknown": rate_unknown(pb) - rate_unknown(pn),
                              "d_cer": (pb["edits"] - pn["edits"]) / pb["chars"], "d_cer_folded": (pb["fed"] - pn["fed"]) / pb["chars"]})
        res["cells"] = cells
        out[lang] = res
    (a.out / "results.json").write_text(json.dumps(out, indent=1, sort_keys=True), encoding="utf-8")
    print("written", a.out / "results.json")


if __name__ == "__main__":
    main()
