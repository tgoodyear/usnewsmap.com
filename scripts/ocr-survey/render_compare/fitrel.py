#!/usr/bin/env python3
"""Follow-up checks of #2103: does the look-alike swap vanish into the word-to-word baseline of the distances?

    fitrel.py fit   --words words.f.jsonl --alto-dir LOC --out words.fit.jsonl --summary fit.json
    fitrel.py check --words words.f.jsonl --fit words.fit.jsonl --alto-dir LOC --templates T.npz --out sanity.fit.json
                    [--rows-out swaps.rel.jsonl] [--jobs 6] [--limit N] [--seed 2103]
    fitrel.py all   --words words.f.jsonl --alto-dir LOC --templates T.npz --out distances.rel.jsonl [--jobs 6]

Check 1, layout fit. `fit` measures, per word, how well its ALTO box fits its ink. The padded frame around the box
is binarised as distances.py does (render.crop_frame, ink.binarise, ink.drop_specks) and three numbers are taken:

  (a) outside_share  of the connected ink (8-connected pieces) that touches the box, the share of its pixels outside the box
  (b) area_ratio     box area / area of the bounding box of the ink inside the box (ink.ink_rows by ink.ink_cols,
                     the span the renderer fits the word to); 1 for a box that hugs its ink, larger when loose
  (c) v_offset       |vertical centre of that ink span - vertical centre of the box| / box height

Fit score (`fit_score`, higher = a worse fit): each of the three is turned into its percentile rank among the words
of the file (ties share the mean rank) and the three ranks are averaged. It is scale-free, with no weights to tune.
A word with no ink in its box scores 1.0. Words are split into tertiles of the score (`tertile_cuts`): fit_tertile 0
is the best-fitting third, 2 the worst.

Check 2, relative score. For a text t, V(t) is every single substitution of one letter of t by its look-alike, in both
directions, using the pairs sanity.py uses (`sanity.lookalike_pairs`, lower-case Latin), de-duplicated and without t.
When V(t) has more than 20 members, 20 are drawn with random.Random("2103:" + t) from the sorted list (the same
draw every run, whatever the order of the words or PYTHONHASHSEED). rel_d(t) = d(t) - min over V(t) of d(v) for each
distance d, each v rendered into the same word box by the same renderer (page templates). With no variant that
renders (V(t) empty, or none gives a distance) rel_d = 0 and the word counts as `empty_v`.

`check` is sanity.py's swap run (same words, same swap draws) with, per word, the distances and rel of the original
and of the swapped text, each with variants generated from the scored text itself. It reports per fit tertile and
overall: the swap AUC (swapped vs original, unpaired) and the share of words whose score rose, for d and for rel_d.
`all` scores every word's OCR text the same way (the gate's step: only run it when a rel_d reaches AUC 0.75).
"""

from __future__ import annotations

import argparse
import json
import multiprocessing as mp
import random
import sys
import time
from pathlib import Path

import numpy as np

import paths  # noqa: F401
import compare
import distances
import fonts
import gold
import ink
import render
import sanity

METRICS = sanity.METRICS
CAP = 20
SEED = 2103
FIT_PARTS = ("outside_share", "area_ratio", "v_offset")


# ---------------------------------------------------------------- check 1: layout fit


def fit_from_mask(mask: np.ndarray, box: tuple[int, int, int, int]) -> dict:
    """The three fit numbers of a binarised frame and the box (x0, y0, w, h) in it. NaN each when the box holds no ink."""
    from scipy.ndimage import label

    bx, by, bw, bh = box
    inside = np.zeros_like(mask)
    inside[by: by + bh, bx: bx + bw] = mask[by: by + bh, bx: bx + bw]
    rows, cols = ink.ink_rows(inside), ink.ink_cols(inside)
    if rows is None or cols is None:
        return {"has_ink": False, "outside_share": float("nan"), "area_ratio": float("nan"), "v_offset": float("nan")}
    labels, _ = label(mask, structure=np.ones((3, 3), int))
    touching = np.isin(labels, np.unique(labels[inside]))
    touching &= labels > 0
    total = int(touching.sum())
    outside = total - int(touching[by: by + bh, bx: bx + bw].sum())
    area = max(1, (rows[1] - rows[0]) * (cols[1] - cols[0]))
    return {"has_ink": True, "outside_share": outside / total, "area_ratio": bw * bh / area,
            "v_offset": abs((rows[0] + rows[1]) / 2 - (by + bh / 2)) / bh}


def fit_metrics(page, box: list[float]) -> dict:
    """fit_from_mask for one word of a PIL page, binarised as distances.py does."""
    frame, _pad, fbox = render.crop_frame(page, box)
    return fit_from_mask(ink.drop_specks(ink.binarise(frame)), fbox)


def fit_score(parts: dict[str, list[float]]) -> np.ndarray:
    """Per word, the mean percentile rank (0..1) of the three fit numbers among the words; 1.0 for a word with no ink
    (NaN numbers). Higher = a worse fit."""
    from scipy.stats import rankdata

    n = len(next(iter(parts.values())))
    cols = []
    for k in FIT_PARTS:
        v = np.asarray(parts[k], float)
        ok = ~np.isnan(v)
        r = np.ones(n)
        if ok.any():
            r[ok] = rankdata(v[ok]) / ok.sum()
        cols.append(r)
    return np.mean(cols, axis=0)


def tertile_cuts(scores) -> tuple[float, float]:
    """The 1/3 and 2/3 quantiles of the fit scores."""
    a = np.asarray(scores, float)
    return float(np.quantile(a, 1 / 3)), float(np.quantile(a, 2 / 3))


def tertile_of(score: float, cuts: tuple[float, float]) -> int:
    """0 (best fit) for score <= cuts[0], 2 for score > cuts[1], else 1."""
    return 0 if score <= cuts[0] else 2 if score > cuts[1] else 1


def _word_key(r: dict):
    return (r["page_id"], r["crop_id"], r["word_index"])


def _fit_job(args):
    from PIL import Image

    alto_dir, stem, rows = args
    Image.MAX_IMAGE_PIXELS = None
    page = Image.open(Path(alto_dir) / f"{stem}.jpg").convert("L")
    return [(_word_key(r), fit_metrics(page, r["box"])) for r in rows]


def by_page(rows: list[dict], chunk: int = 150) -> list[tuple[str, list[dict]]]:
    """(stem, rows) tasks, a page's words in chunks so the pool stays busy."""
    pages: dict[str, list[dict]] = {}
    for r in rows:
        pages.setdefault(r["source_image"].rsplit(".", 1)[0], []).append(r)
    return [(s, rs[i: i + chunk]) for s, rs in sorted(pages.items()) for i in range(0, len(rs), chunk)]


def run_fit(rows: list[dict], alto_dir: Path, jobs: int) -> tuple[list[dict], dict]:
    tasks = [(str(alto_dir), s, rs) for s, rs in by_page(rows)]
    with mp.Pool(jobs) as pool:
        got = {k: m for chunk in pool.imap_unordered(_fit_job, tasks) for k, m in chunk}
    parts = {k: [got[_word_key(r)][k] for r in rows] for k in FIT_PARTS}
    score = fit_score(parts)
    cuts = tertile_cuts(score)
    out = []
    for r, s in zip(rows, score):
        m = got[_word_key(r)]
        out.append({"page_id": r["page_id"], "crop_id": r["crop_id"], "word_index": r["word_index"], "has_ink": m["has_ink"],
                    **{k: (None if np.isnan(m[k]) else m[k]) for k in FIT_PARTS}, "fit_score": float(s),
                    "fit_tertile": tertile_of(float(s), cuts)})
    tert = np.array([o["fit_tertile"] for o in out])
    summary = {"words": len(rows), "no_ink": sum(not o["has_ink"] for o in out), "cuts": {"t1": cuts[0], "t2": cuts[1]},
               "tertiles": {}}
    for t in range(3):
        sel = tert == t
        d = {"words": int(sel.sum())}
        for k in FIT_PARTS + ("fit_score",):
            v = np.array([np.nan if o[k] is None else o[k] for o in out])[sel]
            v = v[~np.isnan(v)]
            d[k] = {"median": float(np.median(v)), "q25": float(np.quantile(v, 0.25)), "q75": float(np.quantile(v, 0.75))} if len(v) else None
        summary["tertiles"][str(t)] = d
    return out, summary


# ---------------------------------------------------------------- check 2: relative score


def variants(text: str, pairs, cap: int = CAP, seed: int = SEED) -> list[str]:
    """V(text): every text that differs from it by one letter swapped for a look-alike partner (both directions of
    each pair; only letters that occur in a pair change), sorted, at most `cap` of them, drawn with
    random.Random(f"{seed}:{text}") when there are more. Deterministic."""
    partners: dict[str, set[str]] = {}
    for a, b, *_ in pairs:
        partners.setdefault(a, set()).add(b)
        partners.setdefault(b, set()).add(a)
    allv = sorted({text[:i] + p + text[i + 1:] for i, c in enumerate(text) for p in partners.get(c, ())} - {text})
    if len(allv) <= cap:
        return allv
    return sorted(random.Random(f"{seed}:{text}").sample(allv, cap))


class Scorer:
    """Distances of texts into one word box, cached by text, and rel_d over V(text)."""

    def __init__(self, page, box, font_path, wb, pairs, cap=CAP, seed=SEED):
        self.page, self.box, self.font_path, self.wb = page, box, font_path, wb
        self.pairs, self.cap, self.seed = pairs, cap, seed
        self.cache: dict[str, dict] = {}

    def dist(self, text: str) -> dict:
        if text not in self.cache:
            if not render.has_glyphs(text, self.font_path):
                self.cache[text] = {"status": "no_glyph"}
            else:
                self.cache[text] = distances.word_distances(text, self.page, self.box, self.font_path, self.wb)
        return self.cache[text]

    def score(self, text: str) -> dict:
        """{status, chamfer, pixel, hog_cos, rel_chamfer, ..., v_size, v_ok, empty_v}."""
        d = self.dist(text)
        out = {"status": d["status"], **{m: d.get(m) for m in METRICS}}
        v = variants(text, self.pairs, self.cap, self.seed)
        vd = [x for x in (self.dist(t) for t in v) if x["status"] == "ok"]
        out.update(v_size=len(v), v_ok=len(vd), empty_v=len(vd) == 0)
        for m in METRICS:
            out[f"rel_{m}"] = None if d["status"] != "ok" else (0.0 if not vd else d[m] - min(x[m] for x in vd))
        return out


_G: dict = {}


def _init(templates_path, font_path, pairs, cap, seed):
    import templates
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    _G.update(store=templates.TemplateStore.load(Path(templates_path)), font=font_path, pairs=pairs, cap=cap, seed=seed, stem=None)


def _scorer(stem, alto_dir, box):
    from PIL import Image

    if _G["stem"] != stem:
        _G["page"] = Image.open(Path(alto_dir) / f"{stem}.jpg").convert("L")
        _G["wb"] = _G["store"].renderer(stem, _G["font"])
        _G["stem"] = stem
    return Scorer(_G["page"], box, _G["font"], _G["wb"], _G["pairs"], _G["cap"], _G["seed"])


def _swap_job(args):
    alto_dir, stem, items = args
    out = []
    for r, swapped in items:
        sc = _scorer(stem, alto_dir, r["box"])
        out.append((_word_key(r), sc.score(r["ocr_word"]), sc.score(swapped)))
    return out


def _all_job(args):
    alto_dir, stem, rows = args
    return [(_word_key(r), _scorer(stem, alto_dir, r["box"]).score(r["ocr_word"])) for r in rows]


def swap_words(rows: list[dict], pairs, seed: int, limit: int | None = None) -> list[tuple[dict, str, tuple]]:
    """sanity.run's choice of words and swaps (the same rng draws): [(row, swapped text, (position, letter, partner, overlap))].
    Words the font cannot draw are skipped as there; the words whose distances do not come out ok are dropped later."""
    font = fonts.resolve("latin")[0]
    rng = random.Random(seed)
    right = sorted((r for r in rows if r["label"] == 1 and r["lang"] == "eng"), key=lambda r: (r["page_id"], r["crop_id"], r["word_index"]))
    out = []
    for r in right:
        if limit and len(out) >= limit:
            break
        sw = sanity.swap_word(r["ocr_word"], pairs, rng)
        if sw is None or not render.has_glyphs(sw[0], font.path):
            continue
        out.append((r, sw[0], sw[1]))
    return out


def tertile_report(sel: list[dict]) -> dict:
    """Swap AUC (swapped vs original, unpaired) and share of words whose score rose, for d and rel_d."""
    out = {"words": len(sel)}
    for pre in ("", "rel_"):
        for m in METRICS:
            o = np.array([x[f"{pre}{m}_original"] for x in sel], float)
            s = np.array([x[f"{pre}{m}_swapped"] for x in sel], float)
            if len(o) == 0:
                continue
            y = np.concatenate([np.zeros(len(o), int), np.ones(len(s), int)])
            out[f"{pre}{m}"] = {"auc": compare.auc(y, np.concatenate([o, s])), "share_rose": float((s > o).mean()),
                                "share_tied": float((s == o).mean()), "mean_original": float(o.mean()), "mean_swapped": float(s.mean())}
    return out


def run_check(rows, fit_rows, alto_dir, templates_path, jobs, seed=SEED, limit=None, cap=CAP):
    font = fonts.resolve("latin")[0]
    pairs = sanity.lookalike_pairs(font.path, 0.6)
    chosen = swap_words(rows, pairs, seed, limit)
    fit = {_word_key(r): r for r in fit_rows}
    tasks = []
    for stem, rs in by_page([r for r, _, _ in chosen], chunk=40):
        sw = {_word_key(r): s for r, s, _ in chosen}
        tasks.append((str(alto_dir), stem, [(r, sw[_word_key(r)]) for r in rs]))
    initargs = (str(templates_path), font.path, pairs, cap, seed)
    with mp.Pool(jobs, initializer=_init, initargs=initargs) as pool:
        got = {k: (a, b) for chunk in pool.imap_unordered(_swap_job, tasks) for k, a, b in chunk}
    out, skipped = [], 0
    for r, swapped, (pos, letter, partner, ov) in chosen:
        a, b = got[_word_key(r)]
        if a["status"] != "ok" or b["status"] != "ok":
            skipped += 1
            continue
        f = fit[_word_key(r)]
        row = {"page_id": r["page_id"], "crop_id": r["crop_id"], "word_index": r["word_index"], "word": r["ocr_word"],
               "swapped": swapped, "position": pos, "overlap": ov, "fit_score": f["fit_score"], "fit_tertile": f["fit_tertile"],
               "v_size_original": a["v_size"], "v_size_swapped": b["v_size"], "empty_v_original": a["empty_v"],
               "empty_v_swapped": b["empty_v"],
               "swapped_in_v_original": swapped in variants(r["ocr_word"], pairs, cap, seed),
               "original_in_v_swapped": r["ocr_word"] in variants(swapped, pairs, cap, seed)}
        for m in METRICS:
            for pre in ("", "rel_"):
                row[f"{pre}{m}_original"], row[f"{pre}{m}_swapped"] = a[f"{pre}{m}"], b[f"{pre}{m}"]
        out.append(row)
    report = {"sanity_check_only": True, "renderer": "templates", "seed": seed, "cap": cap, "pairs": len(pairs),
              "swapped_words": len(out), "skipped_words": skipped,
              "empty_v": {"original_texts": sum(x["empty_v_original"] for x in out), "swapped_texts": sum(x["empty_v_swapped"] for x in out),
                          "v_size_original_mean": float(np.mean([x["v_size_original"] for x in out])),
                          "capped_original": sum(x["v_size_original"] >= cap for x in out),
                          "swapped_in_v_original": sum(x["swapped_in_v_original"] for x in out),
                          "original_in_v_swapped": sum(x["original_in_v_swapped"] for x in out)},
              "all": tertile_report(out), "tertiles": {}}
    for t in range(3):
        report["tertiles"][str(t)] = tertile_report([x for x in out if x["fit_tertile"] == t])
    return report, out


def format_check(rep: dict) -> str:
    lines = [f"{rep['swapped_words']} swapped words ({rep['skipped_words']} skipped); empty V: {json.dumps(rep['empty_v'])}"]
    for name, t in [("all", rep["all"])] + [(f"tertile {k}", v) for k, v in rep["tertiles"].items()]:
        lines.append(f"-- {name}: {t['words']} words")
        for pre in ("", "rel_"):
            for m in METRICS:
                e = t.get(f"{pre}{m}")
                if e:
                    lines.append(f"   {pre}{m:<8} AUC {e['auc']:.3f}  rose {e['share_rose']:.1%}  tied {e['share_tied']:.1%}")
    return "\n".join(lines)


def run_all(rows, alto_dir, templates_path, jobs, seed=SEED, cap=CAP):
    font = fonts.resolve("latin")[0]
    pairs = sanity.lookalike_pairs(font.path, 0.6)
    eng = [r for r in rows if r["lang"] == "eng"]
    tasks = [(str(alto_dir), s, rs) for s, rs in by_page(eng, chunk=100)]
    with mp.Pool(jobs, initializer=_init, initargs=(str(templates_path), font.path, pairs, cap, seed)) as pool:
        got = {k: v for chunk in pool.imap_unordered(_all_job, tasks) for k, v in chunk}
    out = []
    for r in eng:
        s = got[_word_key(r)]
        rec = dict(r)
        rec.update({k: v for k, v in s.items()})
        rec["renderer"] = "templates"
        out.append(rec)
    return out


# ---------------------------------------------------------------- command line


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("cmd", choices=["fit", "check", "all"])
    ap.add_argument("--words", type=Path, required=True)
    ap.add_argument("--alto-dir", type=Path, required=True)
    ap.add_argument("--fit", type=Path, help="`fit` output, for check")
    ap.add_argument("--templates", type=Path)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--summary", type=Path)
    ap.add_argument("--rows-out", type=Path)
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--limit", type=int)
    ap.add_argument("--seed", type=int, default=SEED)
    a = ap.parse_args(argv)
    rows = gold.read_jsonl(a.words)
    t0 = time.time()
    a.out.parent.mkdir(parents=True, exist_ok=True)
    if a.cmd == "fit":
        out, summ = run_fit(rows, a.alto_dir, a.jobs)
        gold.write_jsonl(a.out, out)
        if a.summary:
            a.summary.write_text(json.dumps(summ, indent=1, sort_keys=True) + "\n", encoding="utf-8")
        print(json.dumps(summ, indent=1, sort_keys=True))
    elif a.cmd == "check":
        if not (a.fit and a.templates):
            ap.error("check needs --fit and --templates")
        rep, out = run_check(rows, gold.read_jsonl(a.fit), a.alto_dir, a.templates, a.jobs, a.seed, a.limit)
        rep["wall_s"] = time.time() - t0
        a.out.write_text(json.dumps(rep, indent=1, sort_keys=True) + "\n", encoding="utf-8")
        if a.rows_out:
            gold.write_jsonl(a.rows_out, out)
        print(format_check(rep))
    else:
        if not a.templates:
            ap.error("all needs --templates")
        if a.limit:
            rows = rows[: a.limit]
        out = run_all(rows, a.alto_dir, a.templates, a.jobs, a.seed)
        gold.write_jsonl(a.out, out)
        print(f"{len(out)} words, empty_v {sum(r['empty_v'] for r in out)}")
    print(f"wall_s={time.time() - t0:.1f}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
