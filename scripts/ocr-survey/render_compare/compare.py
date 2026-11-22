#!/usr/bin/env python3
"""Step 4 of #2103: does a render-vs-scan distance separate wrong words from right words better than what we have?

    compare.py --distances distances.parquet --out report.json [--qe-words qe.jsonl --qe-columns charlm_bpc,lm_oov]
               [--bootstrap 200] [--folds 5] [--low-damage-quantile 0.34] [--substitutions-only]

`--distances` is distances.py's output (loader.py's columns plus the distances; .parquet or JSON lines). Only rows
with `status` ok are used. The positive class is a wrong word (label 0); every signal is oriented so a higher
score means "more likely wrong". Per language (`lang`), the report holds AUC for

  render signals   chamfer, pixel, hog_cos (higher = further from the scan), and when the file has them rel_chamfer,
                   rel_pixel, rel_hog_cos (fitrel.py: the same distance less the least over the word's look-alike variants)
  ALTO WC          1 - wc (LoC's own word confidence)
  lex_oov          1 when the OCR word is not in the language's lexicon (english-words.txt, English only): a word-level
                   stand-in for qe-v1's `oov`, NOT qe-v1 itself (qe-v1's resources and its char 5-gram model score
                   readings, not words)
  qe columns       any numeric columns of `--qe-words` named by `--qe-columns` (rows keyed by crop_id and word_index,
                   higher = worse), such as per-word charlm_bpc from `usnm_core`'s qe example; skipped when not given
  combinations     a logistic regression on the standardised signals, scored out of fold by page (GroupKFold): `existing`
                   (WC and the lexicon/qe columns), `render` (the three distances) and `existing+render`

each on all words and split by: page damage (damage_v2's damage_rate of the page's LoC text, terciles over pages),
and real-word errors only (real_word_error words against right words). All signals of a language are compared on the
rows where every one of them has a value, so AUCs are over the same words. `gain` is existing+render minus the best
single existing signal, and minus the `existing` combination; the decision rule of #2103 reads the first, overall
and in the low-damage stratum (`rule`: the best render distance alone or existing+render, against the 0.05 margin).
`--bootstrap N` adds 95% intervals from N resamples of whole pages.

Nothing here decides anything: it reports. A tiny fixture only shows the script runs; the numbers are meaningless
until the full run (see the PR for its command).
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path

import numpy as np

import paths  # noqa: F401
import damage_v2
import gold
import loader

ABSOLUTE = ["chamfer", "pixel", "hog_cos"]
# fitrel.py's relative scores (d minus the least d among the text's single look-alike variants); present only in
# distance files made by `fitrel.py all`
RELATIVE = ["rel_chamfer", "rel_pixel", "rel_hog_cos"]
RENDER = ABSOLUTE + RELATIVE
MARGIN = 0.05
DAMAGE_STRATA = ("low", "mid", "high")
LEXICONS = {"eng": paths.ROOT / "crates" / "usnm-core" / "data" / "english-words.txt.zst"}


# ---------------------------------------------------------------- reading


def read_rows(path: Path) -> list[dict]:
    if path.suffix == ".parquet":
        import pyarrow.parquet as pq

        return pq.read_table(path).to_pylist()
    return gold.read_jsonl(path)


def merge_qe(rows: list[dict], qe_path: Path | None, columns: list[str]) -> list[str]:
    """Copy `columns` of the qe rows (keyed crop_id, word_index) into rows; return the names that were found."""
    if not qe_path or not columns:
        return []
    by = {(r["crop_id"], r["word_index"]): r for r in read_rows(qe_path)}
    found = []
    for c in columns:
        if any(c in r for r in by.values()):
            found.append(f"qe_{c}")
    for r in rows:
        q = by.get((r["crop_id"], r["word_index"]), {})
        for c in columns:
            if c in q:
                r[f"qe_{c}"] = q[c]
    return found


def add_lexicon_oov(rows: list[dict]) -> bool:
    """`lex_oov` for languages with a committed lexicon; True when any row got one."""
    read_zst = loader._zst_reader()
    lex: dict[str, frozenset] = {}
    got = False
    for r in rows:
        p = LEXICONS.get(r["lang"])
        if p is None:
            continue
        if r["lang"] not in lex:
            lex[r["lang"]] = frozenset(w for w in read_zst(p).split("\n") if w)
        word = damage_v2.token_info(r["ocr_word"])[0]
        if word is not None:
            r["lex_oov"] = 0.0 if word in lex[r["lang"]] else 1.0
            got = True
    return got


# ---------------------------------------------------------------- statistics


def auc(y: np.ndarray, s: np.ndarray) -> float | None:
    """P(a wrong word scores higher than a right one), ties half; None unless both classes are present."""
    from scipy.stats import rankdata

    pos = int(y.sum())
    neg = len(y) - pos
    if pos == 0 or neg == 0:
        return None
    ranks = rankdata(s)
    return float((ranks[y == 1].sum() - pos * (pos + 1) / 2) / (pos * neg))


def oof_logistic(X: np.ndarray, y: np.ndarray, groups: np.ndarray, folds: int) -> tuple[np.ndarray | None, str]:
    """Out-of-fold decision scores of a standardised logistic regression, folds split by page. With one page there is
    no honest split: the score is in-sample and says so."""
    from sklearn.linear_model import LogisticRegression
    from sklearn.model_selection import GroupKFold
    from sklearn.pipeline import make_pipeline
    from sklearn.preprocessing import StandardScaler

    if len(set(y.tolist())) < 2:
        return None, "one class"

    def model():
        return make_pipeline(StandardScaler(), LogisticRegression(max_iter=1000))

    n_groups = len(set(groups.tolist()))
    if n_groups < 2:
        return model().fit(X, y).decision_function(X), "in-sample (one page)"
    out = np.zeros(len(y))
    for train, test in GroupKFold(n_splits=min(folds, n_groups)).split(X, y, groups):
        if len(set(y[train].tolist())) < 2:
            return None, "a training fold has one class"
        out[test] = model().fit(X[train], y[train]).decision_function(X[test])
    return out, f"{min(folds, n_groups)}-fold by page"


def damage_strata(rows: list[dict], low_q: float) -> np.ndarray:
    """'low' | 'mid' | 'high' | '' per row, by the page's damage rate: pages at or below the `low_q` quantile of pages
    are low, at or above the (1 - low_q) quantile high. Rows of a page with no rate are ''."""
    page = {}
    for r in rows:
        if r.get("page_damage") is not None and not (isinstance(r["page_damage"], float) and math.isnan(r["page_damage"])):
            page[r["page_id"]] = r["page_damage"]
    if not page:
        return np.array([""] * len(rows))
    vals = np.array(list(page.values()))
    lo, hi = np.quantile(vals, low_q), np.quantile(vals, 1 - low_q)
    out = []
    for r in rows:
        d = page.get(r["page_id"])
        out.append("" if d is None else "low" if d <= lo else "high" if d >= hi else "mid")
    return np.array(out)


def split_masks(rows: list[dict], y: np.ndarray, low_q: float) -> dict[str, np.ndarray]:
    n = len(rows)
    strata = damage_strata(rows, low_q)
    rwe = np.array([bool(r.get("real_word_error")) for r in rows])
    masks = {"all": np.ones(n, bool)}
    for s in DAMAGE_STRATA:
        masks[f"damage_{s}"] = strata == s
    masks["real_word_errors"] = rwe | (y == 0)  # real-word errors against the right words
    masks["non_word_errors"] = (~rwe) | (y == 0)  # the other wrong words against the right words
    return masks


# ---------------------------------------------------------------- one language


def analyse_language(rows: list[dict], existing: list[str], folds: int, low_q: float, bootstrap: int, seed: int = 2103) -> dict:
    present = [s for s in RENDER + ["alto_wc"] + existing if any(r.get(s) is not None for r in rows)]
    unavailable = [s for s in RENDER + ["alto_wc"] + existing if s not in present]
    cols = {s: [r.get(s) for r in rows] for s in present}
    keep = np.array([all(cols[s][i] is not None and not (isinstance(cols[s][i], float) and math.isnan(cols[s][i]))
                         for s in present) for i in range(len(rows))])
    dropped = int((~keep).sum())
    rows = [r for r, k in zip(rows, keep) if k]
    n_wrong = sum(r["label"] == 0 for r in rows)
    info = {"words": len(rows), "wrong": n_wrong, "real_word_errors": sum(bool(r.get("real_word_error")) for r in rows),
            "pages": len({r["page_id"] for r in rows}), "dropped_missing_signal": dropped, "signals_unavailable": unavailable}
    if not rows or n_wrong in (0, len(rows)):
        return {**info, "note": "needs both right and wrong words", "results": {}}
    y = np.array([1 - r["label"] for r in rows])  # 1 = wrong
    groups = np.array([r["page_id"] for r in rows])
    scores = {s: np.array([float(r[s]) for r in rows]) for s in present}
    if "alto_wc" in scores:
        scores["alto_wc"] = 1.0 - scores["alto_wc"]  # a low confidence is the sign of a wrong word
    ex = [s for s in ["alto_wc"] + existing if s in scores]
    combos, cv_note = {}, {}
    for name, members in (("existing", ex), ("render", [s for s in RENDER if s in scores]),
                          ("existing+render", ex + [s for s in RENDER if s in scores])):
        if members and (name != "existing+render" or ex):
            X = np.column_stack([scores[m] for m in members])
            s, note = oof_logistic(X, y, groups, folds)
            if s is not None:
                scores[f"logistic:{name}"] = s
            cv_note[f"logistic:{name}"] = {"members": members, "cv": note}
    masks = split_masks(rows, y, low_q)

    results = {}
    for split, m in masks.items():
        results[split] = {"words": int(m.sum()), "wrong": int(y[m].sum()),
                          "auc": {k: auc(y[m], v[m]) for k, v in scores.items()}}
    for split, t in results.items():
        a = t["auc"]
        best_existing = max(((k, a[k]) for k in ex if a.get(k) is not None), key=lambda kv: kv[1], default=(None, None))
        best_render = max(((k, a[k]) for k in RENDER if a.get(k) is not None), key=lambda kv: kv[1], default=(None, None))
        t["best_existing_single"] = {"signal": best_existing[0], "auc": best_existing[1]}
        t["best_render_single"] = {"signal": best_render[0], "auc": best_render[1]}
        comb = a.get("logistic:existing+render")
        t["gain"] = {
            "render_alone_over_best_existing": _diff(best_render[1], best_existing[1]),
            "combined_over_best_existing": _diff(comb, best_existing[1]),
            "combined_over_existing_logistic": _diff(comb, a.get("logistic:existing")),
        }
        g = t["gain"]
        vals = [g["render_alone_over_best_existing"], g["combined_over_best_existing"]]
        t["rule"] = {"margin": MARGIN, "passes": any(v is not None and v >= MARGIN for v in vals)}
    if bootstrap:
        _bootstrap(results, scores, y, groups, masks, ex, bootstrap, seed)
    return {**info, "folds": cv_note, "results": results}


def _diff(a, b):
    return None if a is None or b is None else a - b


def _bootstrap(results, scores, y, groups, masks, ex, n_boot, seed):
    """95% intervals of each split's AUCs and gains from resampling whole pages (the logistic scores stay the
    out-of-fold ones; the models are not refit)."""
    rng = np.random.default_rng(seed)
    pages = np.array(sorted(set(groups.tolist())))
    by_page = {p: np.nonzero(groups == p)[0] for p in pages}
    draws = {split: {k: [] for k in list(scores) + ["gain:render_alone", "gain:combined"]} for split in masks}
    for _ in range(n_boot):
        pick = rng.choice(pages, size=len(pages), replace=True)
        idx = np.concatenate([by_page[p] for p in pick])
        for split, m in masks.items():
            sel = idx[m[idx]]
            yy = y[sel]
            a = {k: auc(yy, v[sel]) for k, v in scores.items()}
            for k, v in a.items():
                if v is not None:
                    draws[split][k].append(v)
            be = max((a[k] for k in ex if a.get(k) is not None), default=None)
            br = max((a[k] for k in RENDER if a.get(k) is not None), default=None)
            d1, d2 = _diff(br, be), _diff(a.get("logistic:existing+render"), be)
            if d1 is not None:
                draws[split]["gain:render_alone"].append(d1)
            if d2 is not None:
                draws[split]["gain:combined"].append(d2)
    for split, t in results.items():
        t["ci95"] = {k: [float(np.quantile(v, 0.025)), float(np.quantile(v, 0.975))] for k, v in draws[split].items() if len(v) >= 10}


# ---------------------------------------------------------------- run


def run(rows: list[dict], qe_words: Path | None = None, qe_columns: list[str] | None = None, folds: int = 5,
        low_q: float = 0.34, bootstrap: int = 0, substitutions_only: bool = False) -> dict:
    rows = [dict(r) for r in rows if r.get("status") == "ok"]
    if substitutions_only:
        rows = [r for r in rows if r["op"] in ("match", "sub")]
    existing = merge_qe(rows, qe_words, qe_columns or [])
    if add_lexicon_oov(rows):
        existing.append("lex_oov")
    for r in rows:
        r["alto_wc"] = r.get("wc")
    out = {"signals": {"render": RENDER, "existing": ["alto_wc"] + existing},
           "fonts": sorted({(r.get("font_family"), r.get("font_version"), r.get("font_sha256"), r.get("font_package")) for r in rows},
                           key=str),
           "settings": {"folds": folds, "low_damage_quantile": low_q, "bootstrap": bootstrap, "substitutions_only": substitutions_only},
           "languages": {}}
    out["fonts"] = [dict(zip(("family", "version", "sha256", "package"), f)) for f in out["fonts"]]
    out["renderer"] = sorted({r.get("renderer") or "font" for r in rows})
    if "templates" in out["renderer"]:
        use = {k: sum(r.get(k) or 0 for r in rows) for k in ("tpl_page_letters", "tpl_lccn_letters", "tpl_font_letters")}
        n = sum(use.values()) or 1
        out["template_usage"] = {**use, "fallback_share": (use["tpl_lccn_letters"] + use["tpl_font_letters"]) / n}
    for lang in sorted({r["lang"] for r in rows}):
        out["languages"][lang] = analyse_language([r for r in rows if r["lang"] == lang], [e for e in existing],
                                                  folds, low_q, bootstrap)
    return out


def format_report(rep: dict) -> str:
    def f(x):
        return "  -  " if x is None else f"{x:.3f}"

    lines = []
    for lang, L in rep["languages"].items():
        lines.append(f"== {lang}: {L['words']} words, {L['wrong']} wrong, {L['real_word_errors']} real-word errors, "
                     f"{L['pages']} pages; dropped for a missing signal: {L['dropped_missing_signal']}")
        if L["signals_unavailable"]:
            lines.append(f"   unavailable: {', '.join(L['signals_unavailable'])}")
        if not L["results"]:
            lines.append(f"   {L.get('note', 'no results')}")
            continue
        for split, t in L["results"].items():
            lines.append(f"-- {split}: {t['words']} words, {t['wrong']} wrong")
            for k, v in t["auc"].items():
                lines.append(f"     AUC {k:<26} {f(v)}")
            g = t["gain"]
            lines.append(f"     gain: render alone {f(g['render_alone_over_best_existing'])}, combined {f(g['combined_over_best_existing'])} "
                         f"over best existing ({t['best_existing_single']['signal']}); over the existing logistic "
                         f"{f(g['combined_over_existing_logistic'])}; rule (>= {MARGIN}): {'yes' if t['rule']['passes'] else 'no'}")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--distances", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True, help="the report (JSON)")
    ap.add_argument("--qe-words", type=Path, help="per-word qe signals, rows keyed crop_id and word_index")
    ap.add_argument("--qe-columns", default="", help="comma-separated numeric columns of --qe-words, higher = worse")
    ap.add_argument("--bootstrap", type=int, default=0, help="page-resampling draws for 95%% intervals")
    ap.add_argument("--folds", type=int, default=5)
    ap.add_argument("--low-damage-quantile", type=float, default=0.34)
    ap.add_argument("--substitutions-only", action="store_true", help="drop inserted OCR words from the wrong class")
    a = ap.parse_args(argv)
    rep = run(read_rows(a.distances), a.qe_words, [c for c in a.qe_columns.split(",") if c], a.folds,
              a.low_damage_quantile, a.bootstrap, a.substitutions_only)
    a.out.parent.mkdir(parents=True, exist_ok=True)
    a.out.write_text(json.dumps(rep, indent=1, sort_keys=True) + "\n", encoding="utf-8")
    print(format_report(rep))
    return 0


if __name__ == "__main__":
    sys.exit(main())
