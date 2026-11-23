#!/usr/bin/env python3
"""Test A of #2107 step 2: do ALTO-vs-column signals add to lex_oov and raw WC for wrong words?

Method sources: step 1 of #2107 (#2112; after jscancella/NYTribuneOCRExperiments findText_usingSums.py,
GPL-3.0, reimplemented); #2103 (word tables, labels, lex_oov, the WC and filter methods, frozen at 53af36d07);
Beyond Words (Library of Congress, CC0, biglam/loc_beyond_words@6c7f5fb3); American Stories (Dell et al.,
arXiv:2308.12477).

    testa.py --set overproof,WORDS.jsonl,ALTO_DIR --set asgold,WORDS.jsonl,ALTO_DIR --gutters banded.json
             --render-compare-dir DIR --testb-json testb.json --out testa.json [--as-gutters as.json]
             [--bootstrap 200] [--seed 2107] [--folds 5]

WORDS are #2103's filtered word tables (words.f.jsonl, loader.py rows after filter_words.py defaults); ALTO_DIR holds
`<stem>.xml` per page (stem = the row's `source_image` without its extension). --render-compare-dir is a checkout of
#2103's code (compare.py at 53af36d07): its add_lexicon_oov, oof_logistic, auc, split_masks and damage_strata are used
unchanged. Gutters come from columns.py (frozen banded defaults) on the same page images; the word's and line's boxes are
in the image's pixels (ALTO mapped by proportion).

Per word: word_cross (the word's box crosses a gutter), on_cross_line (the word's line, `line` = document-order TextLine
index, crosses). Per page: cross_share. Rows lacking lex_oov or WC are dropped, as in #2103 (all models share the rows).
  baseline   logistic(lex_oov, 1 - WC)         raw WC, no within-page rank
  candidate  baseline + word_cross + on_cross_line + cross_share
Five-fold by page (compare.oof_logistic: GroupKFold), pooled over the sets; the interval on the AUC gain (candidate
minus baseline) is the 2.5 and 97.5 percentiles of --bootstrap page resamples (seed --seed), the out-of-fold scores kept.

Decision (the plan's): the gain is at least 0.02 with the interval's lower end above 0, and test B passed. If test B did
not pass, the output holds no test A result. The 200-wrong-word floor counts the wrong words of the word tables on
crossing lines, pooled over the pages; below it, test A is descriptive only. Reported, not deciding: damage strata,
real-word errors, and the same with --as-gutters (American Stories).
"""
import argparse
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "layout_columns"))
import altolayout  # noqa: E402
import signals  # noqa: E402

GAIN_BAR = 0.02
WRONG_FLOOR = 200
LOW_Q = 0.34


def read_jsonl(path):
    with open(path) as f:
        return [json.loads(ln) for ln in f if ln.strip()]


def add_layout(rows, alto_dir, gut):
    """Add word_cross, on_cross_line, cross_share to rows of one set (mutates). Returns page counts."""
    by_page = {}
    for r in rows:
        by_page.setdefault(r["page_id"], []).append(r)
    mismatch = 0
    stats = {}
    for n, (pid, rs) in enumerate(sorted(by_page.items()), 1):
        g = gut[pid]
        size = (g["width"], g["height"])
        stem = rs[0]["source_image"].rsplit(".", 1)[0]
        with open(os.path.join(alto_dir, stem + ".xml"), "rb") as f:
            parsed = altolayout.parse(f.read(), size)
        segs = signals.segments(g["gutters"])
        flags = signals.line_flags(parsed["lines"], segs, size[0])
        boxes = {ln["idx"]: ln["box"] for ln in parsed["lines"]}
        share = signals.cross_share_of(list(boxes.values()), segs, size[0])
        for r in rs:
            x, y, w, h = r["box"]
            wb = [x, y, x + w, y + h]
            r["word_cross"] = 1.0 if signals.box_crosses(wb, segs, size[0]) else 0.0
            r["on_cross_line"] = 1.0 if flags.get(r["line"], False) else 0.0
            r["cross_share"] = share if share is not None else 0.0
            lb = boxes.get(r["line"])
            cx, cy = x + w / 2.0, y + h / 2.0
            if lb is None or not (lb[0] - 2 <= cx <= lb[2] + 2 and lb[1] - 2 <= cy <= lb[3] + 2):
                mismatch += 1
        stats[pid] = {"gutters": len(g["gutters"]), "lines": len(boxes), "cross_share": share,
                      "crossing_lines": int(sum(flags.values()))}
        print(f"test A pages {n}/{len(by_page)}", flush=True)
    return stats, mismatch


def bootstrap_gain(scores_b, scores_c, y, groups, masks, n_boot, seed):
    from compare import auc
    rng = np.random.default_rng(seed)
    pages = np.array(sorted(set(groups.tolist())))
    by_page = {p: np.nonzero(groups == p)[0] for p in pages}
    draws = {k: [] for k in masks}
    for _ in range(n_boot):
        pick = rng.choice(pages, size=len(pages), replace=True)
        idx = np.concatenate([by_page[p] for p in pick])
        for k, m in masks.items():
            sel = idx[m[idx]]
            a, b = auc(y[sel], scores_b[sel]), auc(y[sel], scores_c[sel])
            if a is not None and b is not None:
                draws[k].append(b - a)
    return {k: ([float(np.quantile(v, 0.025)), float(np.quantile(v, 0.975))] if len(v) >= 10 else None) for k, v in draws.items()}


def analyse(rows, folds, n_boot, seed):
    import compare
    keep = [r for r in rows if r.get("lex_oov") is not None and r.get("wc") is not None]
    y = np.array([1 - r["label"] for r in keep])
    groups = np.array([r["page_id"] for r in keep])
    xb = np.column_stack([[r["lex_oov"] for r in keep], [1.0 - r["wc"] for r in keep]]).astype(float)
    xc = np.column_stack([xb] + [np.array([r[k] for r in keep], dtype=float) for k in ("word_cross", "on_cross_line", "cross_share")])
    sb, nb = compare.oof_logistic(xb, y, groups, folds)
    sc, nc = compare.oof_logistic(xc, y, groups, folds)
    masks = compare.split_masks(keep, y, LOW_Q)
    out = {"rows_analysed": len(keep), "rows_dropped_missing_signal": len(rows) - len(keep), "wrong": int(y.sum()),
           "pages": len(set(groups.tolist())), "cv": {"baseline": nb, "candidate": nc}, "splits": {}}
    cis = bootstrap_gain(sb, sc, y, groups, masks, n_boot, seed) if n_boot else {}
    for k, m in masks.items():
        a, b = compare.auc(y[m], sb[m]), compare.auc(y[m], sc[m])
        out["splits"][k] = {"words": int(m.sum()), "wrong": int(y[m].sum()), "auc_baseline": a, "auc_candidate": b,
                            "gain": None if a is None or b is None else b - a, "gain_ci95": cis.get(k)}
    return out


def crossing_counts(rows):
    wrong = [r for r in rows if r["label"] == 0]
    return {"words": len(rows), "wrong": len(wrong),
            "words_on_cross_line": int(sum(r["on_cross_line"] for r in rows)),
            "words_crossing": int(sum(r["word_cross"] for r in rows)),
            "wrong_on_cross_line": int(sum(r["on_cross_line"] for r in wrong)),
            "wrong_word_crossing": int(sum(r["word_cross"] for r in wrong)),
            "wrong_on_cross_line_in_analysis_rows": int(sum(r["on_cross_line"] for r in wrong if r.get("lex_oov") is not None and r.get("wc") is not None))}


def run_variant(sets, gut, folds, n_boot, seed):
    rows, page_stats, mism = [], {}, 0
    for name, wpath, adir in sets:
        rs = read_jsonl(wpath)
        st, mm = add_layout(rs, adir, gut)
        mism += mm
        page_stats.update(st)
        for r in rs:
            r["set"] = name
        rows += rs
    import compare
    compare.add_lexicon_oov(rows)
    per_set = {name: crossing_counts([r for r in rows if r["set"] == name]) for name, _, _ in sets}
    res = analyse(rows, folds, n_boot, seed)
    res["crossing"] = crossing_counts(rows)
    res["crossing_by_set"] = per_set
    res["line_box_mismatches"] = mism
    res["pages_detail"] = page_stats
    return res


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--set", action="append", required=True, help="NAME,WORDS.jsonl,ALTO_DIR (repeat)")
    ap.add_argument("--gutters", required=True)
    ap.add_argument("--as-gutters")
    ap.add_argument("--render-compare-dir", required=True)
    ap.add_argument("--testb-json", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--bootstrap", type=int, default=200)
    ap.add_argument("--seed", type=int, default=2107)
    ap.add_argument("--folds", type=int, default=5)
    a = ap.parse_args(argv)
    sys.path.insert(0, a.render_compare_dir)
    sets = [tuple(s.split(",", 2)) for s in a.set]
    tb = json.load(open(a.testb_json))["banded"]["test_b"]
    out = {"test_b": {"auc": tb["auc"], "passes": tb["passes"]}}
    if not tb["passes"]:
        out["reported"] = False
        out["note"] = "test B failed: test A is not reported as a result (the signal is broken)"
    else:
        out["reported"] = True
        gut = json.load(open(a.gutters))
        res = run_variant(sets, gut, a.folds, a.bootstrap, a.seed)
        allp = res["splits"]["all"]
        lo = allp["gain_ci95"][0] if allp["gain_ci95"] else None
        floor = res["crossing"]["wrong_on_cross_line"]
        res["decision"] = {"gain": allp["gain"], "gain_bar": GAIN_BAR, "gain_ci95": allp["gain_ci95"],
                           "gain_meets_bar": bool(allp["gain"] is not None and allp["gain"] >= GAIN_BAR),
                           "ci_lower_above_0": bool(lo is not None and lo > 0),
                           "test_b_passes": True,
                           "wrong_words_on_crossing_lines": floor, "floor": WRONG_FLOOR,
                           "descriptive_only": bool(floor < WRONG_FLOOR)}
        d = res["decision"]
        d["add_layout_signals"] = bool(d["gain_meets_bar"] and d["ci_lower_above_0"] and not d["descriptive_only"])
        out["banded"] = res
        if a.as_gutters:
            out["american_stories"] = run_variant(sets, json.load(open(a.as_gutters)), a.folds, a.bootstrap, a.seed)
    with open(a.out, "w") as f:
        json.dump(out, f, sort_keys=True, indent=1)
    if out["reported"]:
        d = out["banded"]["decision"]
        s = out["banded"]["splits"]["all"]
        print(f"test A AUC baseline {s['auc_baseline']:.4f} candidate {s['auc_candidate']:.4f} gain {d['gain']:.4f} ci {d['gain_ci95']}; "
              f"wrong on crossing lines {d['wrong_words_on_crossing_lines']} (floor {WRONG_FLOOR})")
    else:
        print(out["note"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
