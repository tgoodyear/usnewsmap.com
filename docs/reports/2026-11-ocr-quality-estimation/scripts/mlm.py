#!/usr/bin/env python3
"""Masked-LM pseudo-perplexity of readings with XLM-RoBERTa base on CPU (Salazar et al. 2020).

    mlm.py --rows rows/truth.jsonl --model DIR --out mlm-truth.jsonl --per-lang 50 [--langs ...] [--seed 1] [--threads 3]

A reading's first 128 subword tokens are scored. Instead of one forward pass per token, positions are
masked in 6 interleaved groups (every 6th token), so each token is predicted once with 1/6 of its
neighbours also masked; the score is the mean negative log-probability (nats) of the true tokens.
A sample of --per-lang units per language is scored, with every reading of each sampled unit.
"""
import argparse, json, math, random, time
from collections import defaultdict
import torch
from transformers import AutoTokenizer, AutoModelForMaskedLM
from qe_lib import norm_text, WS

ap = argparse.ArgumentParser()
ap.add_argument("--rows", required=True)
ap.add_argument("--model", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--per-lang", type=int, default=50)
ap.add_argument("--langs", default=None)
ap.add_argument("--readers", default=None, help="comma-separated readers to keep")
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--threads", type=int, default=3)
ap.add_argument("--max-tokens", type=int, default=128)
ap.add_argument("--groups", type=int, default=6)
a = ap.parse_args()
torch.set_num_threads(a.threads)
tok = AutoTokenizer.from_pretrained(a.model)
model = AutoModelForMaskedLM.from_pretrained(a.model).eval()

rows = [json.loads(l) for l in open(a.rows, encoding="utf-8")]
keep_r = set(a.readers.split(",")) if a.readers else None
units = defaultdict(list)
for r in rows:
    if (a.langs is None or r["lang"] in a.langs.split(",")) and (keep_r is None or r["reader"] in keep_r):
        units[(r["lang"], r["unit"])].append(r)
bylang = defaultdict(list)
for (lang, u) in sorted(units):
    bylang[lang].append(u)
rnd = random.Random(a.seed)
out = open(a.out, "w", encoding="utf-8")
for lang, us in sorted(bylang.items()):
    rnd.shuffle(us)
    t0 = time.time(); n = 0
    for u in us[: a.per_lang]:
        for r in units[(lang, u)]:
            text = WS.sub(" ", norm_text(r["hyp"])).strip()
            ids = tok(text, add_special_tokens=False)["input_ids"][: a.max_tokens]
            if not ids:
                out.write(json.dumps({"lang": lang, "unit": u, "reader": r["reader"], "tokens": 0, "pll_per_token": None}) + "\n")
                continue
            seq = torch.tensor([tok.cls_token_id] + ids + [tok.sep_token_id])
            batch, targets = [], []
            for g in range(a.groups):
                pos = [i + 1 for i in range(len(ids)) if i % a.groups == g]
                if not pos:
                    continue
                s = seq.clone(); s[pos] = tok.mask_token_id
                batch.append(s); targets.append(pos)
            with torch.no_grad():
                logits = model(torch.stack(batch)).logits
            lp = torch.log_softmax(logits, dim=-1)
            nll = 0.0
            for b, pos in enumerate(targets):
                nll -= float(lp[b, pos, seq[pos]].sum())
            out.write(json.dumps({"lang": lang, "unit": u, "reader": r["reader"], "tokens": len(ids), "pll_per_token": nll / len(ids)}) + "\n")
            n += 1
        out.flush()
    print(f"{lang}: {n} readings in {time.time() - t0:.0f}s", flush=True)
