#!/usr/bin/env python3
"""Scorer for the segment-first OCR test (#2122). Plan frozen in the issue; nothing here is tuned.

    score2122.py truth --pages pages.json --overproof-dir D --asgold-dir D --out truth.json
    score2122.py score --pages pages.json --truth truth.json --run-dir D --out results.json

Truth: Overproof dataset 3 (HIPE-OCRepair v0.9, CC BY-NC-SA 4.0): each article's `ground_truth` lines (HIPE's
line-end ¬ read as -), for the pages in pages.json. en-asgold (American Stories gold, Apache-2.0 text): each
transcribed region of regions.jsonl (article, headline, caption) is a truth article; raw/refs.jsonl's `ref` is
exactly these regions joined by blank lines, checked on load.

Normalisation, the same for every arm and the truth: Unicode NFKC, then lowercase; a hyphen (-, U+2010, U+00AD,
¬) at a line end, after a letter or digit, joins its word to the next line's; remaining soft hyphens are dropped;
U+2018, U+2019 and U+02BC are read as apostrophes; every other character of a Unicode punctuation category (P*)
becomes a space; an apostrophe stays only between two letters or digits; whitespace is collapsed. Symbols (S*,
such as $ and &) are not punctuation and stay.

Measures, per arm:
  trigram / unigram recall: truth word n-grams counted with multiplicity, pooled over a page's articles and
    clipped by the arm's count on that page (an n-gram of the page text matches at most as many truth n-grams
    as it occurs), pooled over pages: sum matched / sum truth. N-grams do not cross article boundaries.
  article CER: each article (normalised characters) is aligned to the best-matching substring of the arm's
    normalised page text (edlib, infix "HW" mode: the whole article against any substring); CER = edits /
    article length. Reported per article, pooled (sum edits / sum length) and as the median.
  lines read across a gutter: output lines with a word centre left of a finder gutter segment and one right of
    it, the line's vertical centre in the segment's y range.
  intervals: page bootstrap, 1,000 draws, seed 2107, the same draws for every arm (paired), 2.5/97.5 percentiles.
"""
import argparse
import hashlib
import json
import os
import re
import sys
import unicodedata
from collections import Counter

import numpy as np

SEED, DRAWS = 2107, 1000
HYPHENS = "-‐­¬"
APOS = {"'", "‘", "’", "ʼ"}
LINE_END_HYPHEN = re.compile(r"(?<=\w)[" + HYPHENS + r"][ \t]*\r?\n\s*(?=\w)")


# ---------------------------------------------------------------- normalisation and counts


def normalise(text):
    t = unicodedata.normalize("NFKC", text).lower()
    t = LINE_END_HYPHEN.sub("", t)
    t = t.replace("­", "")
    chars = []
    for ch in t:
        if ch in APOS:
            chars.append("'")
        elif unicodedata.category(ch).startswith("P"):
            chars.append(" ")
        else:
            chars.append(ch)
    out = []
    for i, ch in enumerate(chars):
        if ch == "'" and not (0 < i < len(chars) - 1 and chars[i - 1].isalnum() and chars[i + 1].isalnum()):
            out.append(" ")
        else:
            out.append(ch)
    return " ".join("".join(out).split())


def tokens(text):
    return normalise(text).split()


def ngrams(toks, n):
    return Counter(tuple(toks[i:i + n]) for i in range(len(toks) - n + 1))


def clipped_matches(truth, hyp):
    return sum(min(c, hyp.get(g, 0)) for g, c in truth.items())


# ---------------------------------------------------------------- article CER


def _codes(a, b):
    """Both strings as bytes over one alphabet (edlib takes at most 256 symbols); rare extras share code 255."""
    freq = Counter(a) + Counter(b)
    ranked = [c for c, _ in sorted(freq.items(), key=lambda kv: (-kv[1], kv[0]))]
    code = {c: min(i, 255) for i, c in enumerate(ranked)}
    return bytes(code[c] for c in a), bytes(code[c] for c in b)


def article_edits(article, page):
    """Edits of the best infix alignment of the whole article against the page text (both normalised)."""
    if not article:
        return 0
    if not page:
        return len(article)
    import edlib
    q, t = _codes(article, page)
    return edlib.align(q, t, mode="HW", task="distance")["editDistance"]


# ---------------------------------------------------------------- gutters


def crosses(line, segments):
    """A line (word boxes) has word centres on both sides of a gutter segment at its vertical centre."""
    if len(line) < 2:
        return False
    yc = (min(b[1] for b in line) + max(b[3] for b in line)) / 2.0
    xs = [(b[0] + b[2]) / 2.0 for b in line]
    for x0, x1, y0, y1 in segments:
        if y0 <= yc < y1 and min(xs) < x0 and max(xs) > x1:
            return True
    return False


# ---------------------------------------------------------------- truth


def read_jsonl(path):
    with open(path, encoding="utf-8") as f:
        return [json.loads(ln) for ln in f if ln.strip()]


CA_DOC_ID = re.compile(r"^(?P<lccn>sn\d+)-\d+-(?P<reel>[0-9A-Za-z]+)-(?P<folder>\d{8}\d{1,2})-(?P<file>\d{4})$")


def overproof_key(doc_id):
    """gold.py ca_page()'s key: LCCN-reel-folder-file, the stem of the page in loc/."""
    m = CA_DOC_ID.match(doc_id)
    return f"{m['lccn']}-{m['reel']}-{m['folder']}-{m['file']}" if m else None


def hipe_lines(d, key):
    unit = d[key]["transcription_unit"]
    return [unit[a:b].replace("¬", "-") for a, b in d[key]["line_offsets"]]


def load_truth(pages, overproof_dir, asgold_dir):
    want = {p["truth_key"]: pid for pid, p in pages.items() if p["set"] == "overproof"}
    out = {pid: {"articles": [], "set": p["set"]} for pid, p in pages.items()}
    seen = set()
    hipe = os.path.join(overproof_dir, "hipe")
    for f in sorted(os.listdir(hipe)):
        if not f.endswith(".jsonl"):
            continue
        for d in read_jsonl(os.path.join(hipe, f)):
            m = d["document_metadata"]
            if "/dataset3/" not in m.get("primary_dataset_filename", ""):
                continue
            key = overproof_key(m["document_id"])
            if key not in want:
                continue
            if m["document_id"] in seen:
                raise SystemExit(f"Overproof article {m['document_id']} appears twice")
            seen.add(m["document_id"])
            out[want[key]]["articles"].append({"id": m["document_id"], "text": "\n".join(hipe_lines(d, "ground_truth"))})
    want = {p["truth_key"]: pid for pid, p in pages.items() if p["set"] == "asgold"}
    raw = os.path.join(asgold_dir, "raw")
    refs = {r["id"]: r["ref"] for r in read_jsonl(os.path.join(raw, "refs.jsonl"))}
    regions = sorted(read_jsonl(os.path.join(raw, "regions.jsonl")), key=lambda r: (r["id"], r["order"]))
    for tk, pid in want.items():
        rs = [r for r in regions if r["id"] == tk and r.get("transcribed") and r["text"].strip()]
        if "\n\n".join(r["text"] for r in rs) != refs[tk]:
            raise SystemExit(f"{tk}: regions do not join to refs.jsonl's ref")
        out[pid]["articles"] = [{"id": f"{tk}/{r['region']}", "text": r["text"]} for r in rs]
    for pid, t in out.items():
        if not t["articles"]:
            raise SystemExit(f"{pid}: no truth articles")
    return out


def cmd_truth(a):
    pages = json.load(open(a.pages))
    truth = load_truth(pages, a.overproof_dir, a.asgold_dir)
    with open(a.out, "w") as f:
        json.dump(truth, f, sort_keys=True)
    print(json.dumps({"articles": sum(len(t["articles"]) for t in truth.values()), "pages": len(truth),
                      "truth_sha256": hashlib.sha256(open(a.out, "rb").read()).hexdigest()}, sort_keys=True))
    return 0


# ---------------------------------------------------------------- scoring


def page_counts(articles, text):
    """[tri matched, tri total, uni matched, uni total] for one page and arm, and per-article CER rows."""
    toks = tokens(text)
    hyp3, hyp1 = ngrams(toks, 3), ngrams(toks, 1)
    t3, t1 = Counter(), Counter()
    for art in articles:
        at = tokens(art["text"])
        t3 += ngrams(at, 3)
        t1 += ngrams(at, 1)
    page_norm = normalise(text)
    cer = []
    for art in articles:
        an = normalise(art["text"])
        cer.append({"chars": len(an), "edits": article_edits(an, page_norm), "id": art["id"]})
    return [clipped_matches(t3, hyp3), sum(t3.values()), clipped_matches(t1, hyp1), sum(t1.values())], cer


def interval(values):
    lo, hi = np.percentile(values, [2.5, 97.5])
    return [float(lo), float(hi)]


def bootstrap(counts, arms):
    """counts[arm]: array pages x [tri m, tri n, uni m, uni n, cer edits, cer chars]. Paired page draws."""
    rng = np.random.default_rng(SEED)
    n = len(next(iter(counts.values())))
    stats = {}
    for _ in range(DRAWS):
        i = rng.integers(0, n, n)
        for arm in arms:
            s = counts[arm][i].sum(axis=0)
            stats.setdefault(arm, []).append((s[0] / s[1], s[2] / s[3], s[4] / s[5]))
    out = {}
    for arm in arms:
        v = np.array(stats[arm])
        out[arm] = {"cer_pooled": interval(v[:, 2]), "trigram_recall": interval(v[:, 0]), "unigram_recall": interval(v[:, 1])}
    for a, b in (("C", "W"), ("C", "L"), ("W", "L")):
        if a in arms and b in arms:
            va, vb = np.array(stats[a]), np.array(stats[b])
            out[f"{a}-{b}"] = {"cer_pooled": interval(va[:, 2] - vb[:, 2]), "trigram_recall": interval(va[:, 0] - vb[:, 0]),
                               "unigram_recall": interval(va[:, 1] - vb[:, 1])}
    return out


def score(pages, truth, run_dir, arms=("C", "L", "W")):
    ids = sorted(pages)
    if sorted(truth) != ids:
        raise SystemExit("truth and pages differ")
    counts = {arm: [] for arm in arms}
    per_page, articles, cross = {}, {arm: [] for arm in arms}, {}
    for pid in ids:
        g = json.load(open(os.path.join(run_dir, "gutters", pid + ".json")))
        segs = [s for gt in g["gutters"] for s in gt]
        per_page[pid] = {}
        for arm in arms:
            res = json.load(open(os.path.join(run_dir, arm, pid + ".json")))
            c, cer = page_counts(truth[pid]["articles"], res["text"])
            nx = sum(crosses(ln, segs) for ln in res["lines"])
            counts[arm].append(c + [sum(r["edits"] for r in cer), sum(r["chars"] for r in cer)])
            articles[arm] += [dict(r, page=pid) for r in cer]
            cross.setdefault(arm, [0, 0])
            cross[arm][0] += nx
            cross[arm][1] += len(res["lines"])
            per_page[pid][arm] = {"errors": res.get("errors", []), "lines": len(res["lines"]), "lines_across_gutter": nx, "trigram": c[:2], "unigram": c[2:]}
    arr = {arm: np.array(v, dtype=np.float64) for arm, v in counts.items()}
    res = {"arms": {}, "pages": len(ids), "per_page": per_page,
           "settings": {"bootstrap_draws": DRAWS, "seed": SEED}}
    for arm in arms:
        s = arr[arm].sum(axis=0)
        cers = [r["edits"] / r["chars"] for r in articles[arm] if r["chars"]]
        res["arms"][arm] = {"article_cer_median": float(np.median(cers)), "article_cer_pooled": float(s[4] / s[5]),
                            "articles": articles[arm], "engine_errors": sum(len(per_page[p][arm]["errors"]) for p in ids),
                            "lines": cross[arm][1], "lines_across_gutter": cross[arm][0],
                            "trigram_recall": float(s[0] / s[1]), "trigrams": int(s[1]),
                            "unigram_recall": float(s[2] / s[3]), "unigrams": int(s[3])}
    res["intervals"] = bootstrap(arr, arms)
    A = res["arms"]
    if "C" in A and "W" in A:
        d3 = A["C"]["trigram_recall"] - A["W"]["trigram_recall"]
        d1 = A["C"]["unigram_recall"] - A["W"]["unigram_recall"]
        r1 = {"trigram_C_minus_W": d3, "trigram_C_minus_W_ge_0.02": bool(d3 >= 0.02),
              "trigram_interval_above_0": bool(res["intervals"]["C-W"]["trigram_recall"][0] > 0),
              "unigram_C_minus_W": d1, "unigram_C_minus_W_ge_-0.005": bool(d1 >= -0.005)}
        r1["met"] = bool(r1["trigram_C_minus_W_ge_0.02"] and r1["trigram_interval_above_0"] and r1["unigram_C_minus_W_ge_-0.005"])
        res["rule_1"] = r1
    if "C" in A and "L" in A:
        d = A["C"]["trigram_recall"] - A["L"]["trigram_recall"]
        r2 = {"trigram_C_minus_L": d, "trigram_C_minus_L_ge_0.02": bool(d >= 0.02),
              "trigram_interval_above_0": bool(res["intervals"]["C-L"]["trigram_recall"][0] > 0)}
        r2["met"] = bool(r2["trigram_C_minus_L_ge_0.02"] and r2["trigram_interval_above_0"])
        res["rule_2"] = r2
    return res


def cmd_score(a):
    pages = json.load(open(a.pages))
    truth = json.load(open(a.truth))
    res = score(pages, truth, a.run_dir)
    res["inputs"] = {"pages_sha256": hashlib.sha256(open(a.pages, "rb").read()).hexdigest(),
                     "truth_sha256": hashlib.sha256(open(a.truth, "rb").read()).hexdigest()}
    with open(a.out, "w") as f:
        json.dump(res, f, indent=1, sort_keys=True)
    summary = {k: v for k, v in res.items() if k not in ("per_page", "arms")}
    summary["arms"] = {arm: {k: v for k, v in r.items() if k != "articles"} for arm, r in res["arms"].items()}
    print(json.dumps(summary, indent=1, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sp = ap.add_subparsers(dest="cmd", required=True)
    t = sp.add_parser("truth")
    t.add_argument("--pages", required=True)
    t.add_argument("--overproof-dir", required=True)
    t.add_argument("--asgold-dir", required=True)
    t.add_argument("--out", required=True)
    s = sp.add_parser("score")
    s.add_argument("--pages", required=True)
    s.add_argument("--truth", required=True)
    s.add_argument("--run-dir", required=True)
    s.add_argument("--out", required=True)
    a = ap.parse_args(argv)
    return cmd_truth(a) if a.cmd == "truth" else cmd_score(a)


if __name__ == "__main__":
    sys.exit(main())
