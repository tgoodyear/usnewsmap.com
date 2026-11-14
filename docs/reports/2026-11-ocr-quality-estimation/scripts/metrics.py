#!/usr/bin/env python3
"""Reference-free quality measures for every reading, one language at a time.

    metrics.py --rows rows/truth.jsonl --kind truth --langs eng,fra --res DIR --confusions confusions.json \
        --yi-list wordlists/yi.txt --out-dir metrics [--max-crops 6000 --seed 1]

Per reading: words, in-lexicon words of 3+ letters (the omission guard's count), lexical hit rate (wordfreq or Hunspell), out-of-lexicon rate, near-miss rates
(an out-of-lexicon word one edit, or two for words of 7+ letters, from a lexicon word: any edit, or
only edits among the commonest OCR confusions measured on the OTHER languages' truth sets of the same
script), garbage-token rate (Taghva-style rules), script-mix rate, character 5-gram cross-entropy
(bits per character) from a model trained on Project Gutenberg books (optionally topped up from, or
replaced by, Wikisource; --lm-sources) plus a wordfreq sample, and for
truth rows the CER (folded and diplomatic). Unlabelled rows: at most --max-crops crops per language,
whole pages drawn at random with --seed.
"""
import argparse, json, os, random, glob, time
from collections import Counter, defaultdict
from qe_lib import (LANGS, Lexicon, NearMiss, CharLM, fold, words, garbage_flags, script_of, lm_text,
                    gutenberg_body, cer, edit_ops)

ap = argparse.ArgumentParser()
ap.add_argument("--rows", required=True)
ap.add_argument("--kind", choices=["truth", "unlabelled"], required=True)
ap.add_argument("--langs", required=True)
ap.add_argument("--res", required=True)
ap.add_argument("--confusions", required=True)
ap.add_argument("--yi-list", required=True)
ap.add_argument("--out-dir", required=True)
ap.add_argument("--max-crops", type=int, default=6000)
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--lm-chars", type=int, default=3_000_000)
ap.add_argument("--lm-sources", choices=["gutenberg", "gutenberg+wikisource", "wikisource"], default="gutenberg",
                help="period text for the character model: Gutenberg books, those topped up to --lm-chars from Wikisource, or Wikisource alone")
ap.add_argument("--conf-mass", default="0.3,0.5,0.8", help="error-mass cut-offs for the high-confusion edit sets")
a = ap.parse_args()
os.makedirs(a.out_dir, exist_ok=True)
conf = json.load(open(a.confusions, encoding="utf-8"))


def high_confusions(lang, mass):
    """Edits covering `mass` of all errors on the other languages' truth sets of the same script."""
    script = LANGS[lang][3]
    c = Counter()
    for l2, m in conf.items():
        if l2 == lang or LANGS[l2][3] != script:
            continue
        for rd, v in m.items():
            c.update(v["ops"])
    tot = sum(c.values())
    keep, acc = set(), 0
    for k, n in c.most_common():
        if tot == 0 or acc >= mass * tot:
            break
        e, g = k.split("\t")
        if e == " " or g == " ":
            continue
        keep.add((e, g)); acc += n
    return keep


def train_lm(lang, lex):
    lm = CharLM(5)
    parts = []
    g = LANGS[lang][2]
    if g and a.lm_sources != "wikisource":
        for p in sorted(glob.glob(os.path.join(a.res, "gutenberg", g, "*.txt"))):
            parts.append(lm_text(gutenberg_body(open(p, encoding="utf-8", errors="replace").read()), lang))
    text = " ".join(parts)[: a.lm_chars]
    ws = os.path.join(a.res, "wikisource", f"{g or 'yi'}.txt")
    if a.lm_sources != "gutenberg" and len(text) < a.lm_chars and os.path.exists(ws):
        text = (text + " " + lm_text(open(ws, encoding="utf-8").read(), lang))[: a.lm_chars]
    # A frequency-weighted sample of the word list, so every language has in-vocabulary coverage.
    rnd = random.Random(7)
    top = sorted(lex.freq.items(), key=lambda kv: -kv[1])[:30000]
    bag = []
    for w, p in top:
        bag += [w] * max(1, round(p * 300_000))
    rnd.shuffle(bag)
    lm.train(text + " " + " ".join(bag)[: max(500_000, a.lm_chars - len(text))])
    return lm, len(text)


rows = [json.loads(l) for l in open(a.rows, encoding="utf-8")]
for lang in a.langs.split(","):
    t0 = time.time()
    rs = [r for r in rows if r["lang"] == lang]
    if a.kind == "unlabelled":
        pages = defaultdict(list)
        for r in rs:
            pages[r["page"]].append(r)
        order = sorted(pages)
        random.Random(a.seed).shuffle(order)
        pick, n = [], 0
        for p in order:
            if n >= a.max_crops:
                break
            pick += pages[p]; n += len(pages[p]) // 2
        rs = pick
    if not rs:
        continue
    lex = Lexicon(lang, a.res, a.yi_list)
    toks = {}
    allw = set()
    for r in rs:
        w = [fold(x, lang) for x in words(r["hyp"])]
        toks[id(r)] = w
        allw.update(w)
    lex.prime(allw)
    nm = NearMiss(lex)
    masses = [float(x) for x in a.conf_mass.split(",")]
    his = [high_confusions(lang, m_) for m_ in masses]
    lm, gut = train_lm(lang, lex)
    t1 = time.time()
    cache = {}

    def near(t):
        if t not in cache:
            any_, conf_ = False, [False] * len(his)
            if len(t) >= 3:
                for w, d in nm.candidates(t).items():
                    any_ = True
                    ops = edit_ops(w, t)
                    for j, hi in enumerate(his):
                        if hi and all(op in hi for op in ops):
                            conf_[j] = True
            cache[t] = (any_, conf_)
        return cache[t]

    out = open(os.path.join(a.out_dir, f"{a.kind}-{lang}.jsonl"), "w", encoding="utf-8")
    for r in rs:
        w = toks[id(r)]
        n = len(w)
        known = sum(lex.known(x) for x in w)
        known3 = sum(1 for x in w if len(x) >= 3 and lex.known(x))  # one- and two-letter "words" are often junk
        nm_any = 0
        nm_conf = [0] * len(his)
        for x in w:
            if not lex.known(x):
                p, q = near(x)
                nm_any += p
                for j, b in enumerate(q):
                    nm_conf[j] += b
        ws = r["hyp"].split()
        gflags = [garbage_flags(x, lang) for x in ws]
        garbage = sum(1 for f in gflags if f)
        mixed = 0
        for x in words(r["hyp"]):
            sc = {script_of(c) for c in x if c.isalpha()}
            mixed += len(sc) > 1
        lt = lm_text(r["hyp"], lang)
        lp, nc = lm.logprob(lt) if lt else (0.0, 0)
        m = {k: r[k] for k in r if k not in ("ref", "hyp")}
        m.update({
            "chars": len(lt), "words": n, "ws_tokens": len(ws), "known3": known3,
            "lex_hit": known / n if n else None,
            "oov": 1 - known / n if n else None,
            "nearmiss_any": nm_any / n if n else None,
            **{f"nearmiss_conf{int(m_ * 100)}": (nm_conf[j] / n if n else None) for j, m_ in enumerate(masses)},
            "novel": (n - known - nm_any) / n if n else None,
            "garbage": garbage / len(ws) if ws else None,
            "script_mix": mixed / n if n else None,
            "charlm_bpc": -lp / nc if nc else None,
            "garbage_rules": dict(Counter(f for s in gflags for f in s)),
        })
        if lang == "srp":
            letters = [script_of(c) for c in r["hyp"] if c.isalpha()]
            m["srp_cyrillic_share"] = (letters.count("Cyrillic") / len(letters)) if letters else None
        if a.kind == "truth":
            m["cer"] = cer(r["ref"], r["hyp"], True)
            m["cer_raw"] = cer(r["ref"], r["hyp"], False)
            m["ref_chars"] = len(lm_text(r["ref"], lang))
        out.write(json.dumps(m, ensure_ascii=False) + "\n")
    out.close()
    print(f"{lang}: {len(rs)} readings, {len(allw)} word types, high confusions {[len(h) for h in his]}, period-text chars {gut}, "
          f"setup {t1 - t0:.0f}s, scoring {time.time() - t1:.0f}s", flush=True)
