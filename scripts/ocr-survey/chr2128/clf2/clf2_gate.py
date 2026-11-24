"""Line classifier v2, stage 2 (#2128 Amendment 3): calibration of the tuning labels, the threshold t,
the gate sample size, the gate draw, new calibration lines and the gate contact sheets.

  clf2_gate.py tune     --key dev-key.jsonl --labels-a A --labels-b B --labels-c C --out tune.json
  clf2_gate.py simulate --key first-key.jsonl --labels-a A --labels-b B --labels-c C --out sim.json
  clf2_gate.py draw     --summary S --contrast contrast.jsonl --excluded excluded-pages.txt
                        --tuning tuning-pages.txt --n N --out-dir DIR
  clf2_gate.py sample   --scored scored.jsonl --pages gate-pages.json --t T --out sample.jsonl
  clf2_gate.py sheets   --sample sample.jsonl --aligned aligned.jsonl --first-key first-key.jsonl
                        --dev-key dev-key.jsonl --phoenix-pages P --alto-root X --loc-images L
                        --wordlist W --out-dir DIR

Labels and estimators are clf_score.py's (307d960a): final label A where A = B, else the majority of
A, B, C, else X; C and M are Cherokee, L Latin, X left out; HT recall with weight 1 for every box at or
above the cut; Latin share of calls unweighted. Calibration thresholds 0.97, 0.95, 0.03 (Amendment 2.1).
"""
import argparse
import collections
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "clf"))
import clf2  # noqa: E402
import clf_score as sc  # noqa: E402

cs = clf2.cs
sha, jsonl, file_sha = cs.sha, cs.jsonl, cs.file_sha
TRUTH = {"C": "chr", "M": "chr", "L": "lat"}


def labelled(key, a):
    A, B, C = sc.read_tsv(a.labels_a), sc.read_tsv(a.labels_b), sc.read_tsv(a.labels_c)
    assert {k["crop_id"] for k in key} == set(A) == set(B), "label files don't cover the key"
    return A, B, C, {k["crop_id"]: sc.final(A, B, C, k["crop_id"]) for k in key}


def calibration(key, A, B):
    cal = [k for k in key if k.get("known_script")]
    out = {}
    for name, L in (("A", A), ("B", B)):
        def acc(rows):
            return sum((L[k["crop_id"]] in ("C", "M")) == (k["known_script"] == "cherokee") and L[k["crop_id"]] != "X"
                       for k in rows) / len(rows)
        look = [k for k in cal if k["lookalike"]]
        ct = sum(L[k["crop_id"]] == "X" for k in cal) / len(cal)
        r = {"overall": acc(cal), "lookalike": acc(look), "cant_tell": ct, "n": len(cal), "n_lookalike": len(look)}
        r["pass"] = r["overall"] >= 0.97 and r["lookalike"] >= 0.95 and ct <= 0.03
        out[name] = r
    return out


def cmd_tune(a):
    key = jsonl(a.key)
    A, B, C, F = labelled(key, a)
    res = {"key_sha256": file_sha(a.key), "calibration": calibration(key, A, B)}
    ghn = [k for k in key if k["source"] == "ghn"]
    res["final_labels"] = dict(collections.Counter(F[k["crop_id"]] for k in ghn))
    rows = [(k["prob"], TRUTH.get(F[k["crop_id"]]), k["weight"]) for k in ghn]
    grid = []
    # Boxes at or above 0.05 are all on the sheets, so t runs from 0.05 (below it calls would be sampled).
    for t in np.round(np.arange(0.05, 1.0, 0.01), 2):
        m = sc.measures([{"truth": tr or "x", "w": w, "called": p >= t} for p, tr, w in rows])
        grid.append({"t": float(t), **m, "calls": int(sum(p >= t for p, _, _ in rows))})
    ok = [g for g in grid if g["latin_share"] is not None and g["latin_share"] <= 0.03]
    # Highest recall; among ties, the highest t (fewest calls).
    best = max(ok, key=lambda g: (g["recall"], g["t"])) if ok else None
    res.update({"grid": grid, "t": best and best["t"], "at_t": best,
                "rule": "max HT recall subject to latin_share <= 0.03, t in 0.05..0.99 step 0.01, ties -> highest t"})
    json.dump(res, open(a.out, "w"), indent=1)
    print(json.dumps({k: res[k] for k in ("calibration", "final_labels", "t", "at_t")}, indent=1))


def cmd_simulate(a):
    """Sample size for the not-held stratum (Amendment 3). Page structure from the first check's not-held
    pages: per page, the HT-estimated numbers of Cherokee and Latin boxes (final labels, key weights).
    Each simulated classifier calls a Cherokee box with probability 0.93 and a Latin box with the
    probability that makes the Latin share of calls 0.03 over those pages. A simulated sample draws n
    pages with replacement, the calls, and 150 not-called boxes per page (or all); the estimates get the
    amendment 2.1 bootstrap (pages, then boxes within called / not called), 95% percentile bounds."""
    key = jsonl(a.key)
    _, _, _, F = labelled(key, a)
    pages = collections.defaultdict(lambda: [0.0, 0.0])
    for k in key:
        if k.get("source") == "ghn" and "not_held" in k["strata"]:
            t = TRUTH.get(F[k["crop_id"]])
            w = 1.0 if k["called"] else float(k["weight"])
            if t:
                pages[k["page_id"]][t == "lat"] += w
    pop = np.array([[round(c), round(l)] for c, l in pages.values()], int)
    R, LS = 0.93, 0.03
    q = LS / (1 - LS) * R * pop[:, 0].sum() / pop[:, 1].sum()
    rng = np.random.default_rng(2128)
    sims, draws, n_nc = a.sims, a.draws, 150
    res = {"pages": len(pop), "population": pop.tolist(), "latin_call_prob": q, "sims": sims, "draws": draws,
           "sizes": {}}
    chosen = None
    for n in range(5, 41):
        ok = 0
        for _ in range(sims):
            idx = rng.integers(0, len(pop), n)
            Nc, Nl = pop[idx, 0], pop[idx, 1]
            cc = rng.binomial(Nc, R)                     # Cherokee called
            lc = rng.binomial(Nl, q)                     # Latin called
            ncc, ncl = Nc - cc, Nl - lc                  # not called
            N_nc = ncc + ncl
            m = np.minimum(N_nc, n_nc)
            sc_ = rng.hypergeometric(np.maximum(ncc, 0), np.maximum(ncl, 0), np.maximum(m, 0)) if N_nc.sum() else np.zeros(n, int)
            w = np.where(m > 0, N_nc / np.maximum(m, 1), 0.0)
            # bootstrap: pages, then boxes within called and sampled not-called
            bi = rng.integers(0, n, (draws, n))
            C_, L_, M_, S_, W_ = cc[bi], lc[bi], m[bi], sc_[bi], w[bi]
            calls = C_ + L_
            bl = rng.binomial(calls, np.where(calls > 0, L_ / np.maximum(calls, 1), 0))
            bs = rng.binomial(M_, np.where(M_ > 0, S_ / np.maximum(M_, 1), 0))
            bc = calls - bl
            rec = bc.sum(1) / np.maximum(bc.sum(1) + (W_ * bs).sum(1), 1e-9)
            lsh = bl.sum(1) / np.maximum(calls.sum(1), 1)
            lo, hi = np.quantile(rec, 0.025), np.quantile(lsh, 0.975)
            ok += bool(lo >= 0.80 and hi <= 0.10)
        res["sizes"][n] = ok / sims
        print(f"n={n}: power {ok / sims:.3f}", file=sys.stderr, flush=True)
        if ok / sims >= 0.80:
            chosen = n
            break
    res["n_not_held"] = chosen if chosen else 40
    res["capped"] = chosen is None
    json.dump(res, open(a.out, "w"), indent=1)
    print(json.dumps({k: res[k] for k in ("pages", "latin_call_prob", "sizes", "n_not_held", "capped")}, indent=1))


def cmd_draw(a):
    summary = jsonl(a.summary)
    excl = {l.strip() for l in open(a.excluded) if l.strip()} | {l.strip() for l in open(a.tuning) if l.strip()}
    pl = [r for r in summary if r["boxes"] >= 1 and r["doc_id"] not in excl]
    contrast = {r["page_id"]: r["contrast"] for r in jsonl(a.contrast)}
    held = {r["doc_id"]: bool(r["held"]) for r in pl}
    assert set(held) <= set(contrast)
    ranked = sorted(held, key=lambda p: (contrast[p], p))
    n_faded = len(ranked) // 4
    faded = set(ranked[:n_faded])
    order = sorted(held, key=lambda p: sha("2128:clf2:" + p))
    n = a.n
    nh = [p for p in order if not held[p]][:n]
    hd = [p for p in order if held[p]][:n]
    base = nh + hd
    want_f = min(n, n_faded)
    added = []
    nf = sum(p in faded for p in base)
    for p in order:
        if nf + len(added) >= want_f:
            break
        if p in faded and p not in base:
            added.append(p)
    pages = sorted(base + added)
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    (out / "gate-pages.txt").write_text("".join(p + "\n" for p in pages))
    strata = {p: [s for s, c in (("not_held", p in nh), ("held", p in hd), ("faded", p in faded)) if c] for p in pages}
    res = {"pool": len(pl), "pool_held": sum(held.values()), "n": n, "faded_rule": f"lowest floor(N/4) = {n_faded} pool pages "
           f"by (contrast, page_id); cut {contrast[ranked[n_faded - 1]]:.3f}", "not_held": len(nh), "held": len(hd),
           "faded_in_base": nf, "added_faded": len(added), "faded_total": nf + len(added),
           "gate_pages_sha256": file_sha(out / "gate-pages.txt"), "strata": strata,
           "added": added, "contrast": {p: contrast[p] for p in pages}}
    (out / "gate-pages.sha256").write_text(f"{res['gate_pages_sha256']}  gate-pages.txt\n")
    (out / "gate-pages.json").write_text(json.dumps(res, indent=1) + "\n")
    print(json.dumps({k: v for k, v in res.items() if k not in ("strata", "contrast")}, indent=1))


def cmd_sample(a):
    """Every box with prob >= t, plus 150 of the rest by sha256('2128:clf2-box:'+page_id+':'+box_id), weight N/n.
    Boxes with no p_chr (no ink) can't be scored or called, so they count as not called and can be sampled."""
    gp = json.load(open(a.pages))
    scored = collections.defaultdict(list)
    for r in jsonl(a.scored):
        scored[r["page_id"]].append(r)
    noink = collections.defaultdict(list)
    for r in jsonl(a.devset_all):
        noink[r["page_id"]].append(r)
    out, stats = [], {}
    for p, strata in gp["strata"].items():
        rows = scored[p] + noink.get(p, [])
        called = [r for r in rows if r.get("prob") is not None and r["prob"] >= a.t]
        other = sorted((r for r in rows if not (r.get("prob") is not None and r["prob"] >= a.t)),
                       key=lambda r: sha("2128:clf2-box:" + p + ":" + r["box_id"]))
        chosen = other[:150]
        N_o, n_o = len(other), len(chosen)
        stats[p] = {"boxes": len(rows), "called": len(called), "N_notcalled": N_o, "n_sampled": n_o,
                    "no_ink": len(noink.get(p, []))}
        for r, sel in [(r, "called") for r in called] + [(r, "sampled_not_called") for r in chosen]:
            out.append({"source": "ghn", "page_id": p, "box_id": r["box_id"], "box": r["box"], "scan": r["scan"],
                        "held_flag": r["held_flag"], "strata": strata, "added_faded": p in gp["added"],
                        "contrast": gp["contrast"][p], "prob": r.get("prob"), "x": r.get("x"), "p_chr": r.get("p_chr"),
                        "called": sel == "called", "selection": sel, "weight": 1.0 if sel == "called" else N_o / n_o,
                        "n_boxes": len(rows), "n_called": len(called), "N_notcalled": N_o, "n_sampled": n_o})
    clf2.cs_write(a.out, out)
    print(json.dumps({"t": a.t, "pages": len(stats), "called": sum(s["called"] for s in stats.values()),
                      "sampled_not_called": sum(s["n_sampled"] for s in stats.values()),
                      "no_ink": sum(s["no_ink"] for s in stats.values())}))


def cmd_sheets(a):
    from PIL import Image
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    sample = jsonl(a.sample)
    if a.stratum:  # Amendment 3.1: only the pages of one stratum are labelled
        sample = [r for r in sample if a.stratum in r["strata"]]
    used = {r["line_id"] for f in (a.first_key, a.dev_key) for r in jsonl(f) if r.get("source") == "calibration"}
    vocab = {w.strip().lower() for w in open(a.wordlist, encoding="utf-8") if w.strip()}
    aligned = jsonl(a.aligned)
    v2_pages = sorted({r["page"] for r in aligned})
    have_img = {p.stem for p in Path(a.loc_images).glob("*.jpg")}
    # Cherokee: test set v2 lines aligned to the NT (drop in null, wording, gap_run), not used before.
    chr_c = []
    for r in aligned:
        if r["drop"] not in cs.CAL_TIERS:
            continue
        pid = "loc:" + r["page"].replace("/", "_")
        ref = cs.SYLLABARY.findall(r["reference"])
        sh = sum(c in cs.LOOKALIKE for c in ref) / len(ref) if ref else 0.0
        c = {"page": r["page"], "page_id": pid, "box_id": r["line_id"], "line_id": pid + ":" + r["line_id"],
             "alto_box": r["box"], "alto_page_size": r["alto_page_size"], "lookalike_share": sh,
             "lookalike": sh >= 0.5, "cal_source": "testset_v2_aligned"}
        if c["line_id"] not in used:
            chr_c.append(c)
    # Lookalikes: the v2 pool has none left, so lookalike lines also come from LoC's ALTO on the Phoenix pages
    # (lines whose LoC text is >= 0.8 syllabary by letters; lookalike share over its syllabary letters).
    loc_look = []
    for r in jsonl(a.phoenix_pages):
        page = r["page"]
        if page.replace("/", "_") not in have_img:
            continue
        lccn, date, ed, seq = page.split("/")
        y, m, d = date.split("-")
        lines, size = cs.alto_lines(Path(a.alto_root) / lccn / y / m / d / ed / seq / "ocr.xml")
        pid = "loc:" + page.replace("/", "_")
        for ln in lines:
            text = " ".join(ln["words"])
            letters = [ch for ch in text if ch.isalpha()]
            syl = cs.SYLLABARY.findall(text)
            if not letters or len(syl) / len(letters) < 0.8:
                continue
            sh = sum(ch in cs.LOOKALIKE for ch in syl) / len(syl)
            lid = pid + ":" + ln["line_id"]
            if sh >= 0.5 and lid not in used:
                loc_look.append({"page": page, "page_id": pid, "box_id": ln["line_id"], "line_id": lid,
                                 "alto_box": ln["box"], "alto_page_size": list(size), "lookalike_share": sh,
                                 "lookalike": True, "cal_source": "loc_alto_syllabary"})
    k = lambda c: sha("2128:clf2-cal:" + c["line_id"])
    v2_look = sorted((c for c in chr_c if c["lookalike"]), key=k)
    look = (v2_look + sorted(loc_look, key=k))[:cs.MIN_LOOKALIKE]
    taken = {c["line_id"] for c in look}
    cal_chr = look + sorted((c for c in chr_c if c["line_id"] not in taken), key=k)[:cs.N_CAL_CHR - len(look)]
    lat_c = []
    for page in v2_pages:
        lccn, date, ed, seq = page.split("/")
        y, m, d = date.split("-")
        lines, size = cs.alto_lines(Path(a.alto_root) / lccn / y / m / d / ed / seq / "ocr.xml")
        pid = "loc:" + page.replace("/", "_")
        for ln in lines:
            lid = pid + ":" + ln["line_id"]
            if ln["block_lang"] != "chr" and lid not in used and cs.english_line(ln["words"], vocab):
                lat_c.append({"page": page, "page_id": pid, "box_id": ln["line_id"], "line_id": lid,
                              "alto_box": ln["box"], "alto_page_size": list(size)})
    cal_lat = sorted(lat_c, key=k)[:cs.N_CAL_LAT]
    assert len(cal_chr) == 200 and len(cal_lat) == 200 and len(look) >= 60

    items = []
    for r in sample:
        row = {kk: v for kk, v in r.items() if kk != "scan"}
        items.append((sha(a.shuffle_key + r["page_id"] + ":" + r["box_id"]), row, Path(r["scan"]), tuple(r["box"])))
    sizes = {}
    for script, group in (("cherokee", cal_chr), ("latin", cal_lat)):
        for c in group:
            img = Path(a.loc_images) / (c["page"].replace("/", "_") + ".jpg")
            if img not in sizes:
                with Image.open(img) as im:
                    sizes[img] = im.size
            px = cs.alto_to_px(c["alto_box"], c["alto_page_size"], sizes[img])
            row = {"source": "calibration", "page_id": c["page_id"], "box_id": c["box_id"], "line_id": c["line_id"],
                   "known_script": script, "lookalike": c.get("lookalike", False),
                   "lookalike_share": c.get("lookalike_share"), "cal_source": c.get("cal_source", "testset_v2_alto")}
            items.append((sha(a.shuffle_key + c["page_id"] + ":" + c["box_id"]), row, img, px))
    items.sort(key=lambda t: t[0])
    by_img = collections.defaultdict(list)
    for i, t in enumerate(items):
        by_img[t[2]].append(i)
    cut = {}
    for img, idx in by_img.items():
        with Image.open(img) as im0:
            im = im0.convert("L")
        for i in idx:
            cut[i] = cs.crop_scaled(im, *items[i][3])
    crops, keyrows = [], []
    for i, t in enumerate(items):
        crop, padded = cut[i]
        cid = f"G{i + 1:05d}"
        crops.append((cid, crop))
        keyrows.append({"crop_id": cid, **t[1], "crop_box_px": padded, "crop_size": list(crop.size),
                        "sheet": f"sheet-{i // cs.PER_SHEET + 1:03d}", "row": i % cs.PER_SHEET + 1})
    n = clf2.render(crops, out)
    import os
    kp = out / "key.jsonl"
    fd = os.open(kp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        for r in keyrows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.chmod(kp, 0o600)
    man = {"issue": "#2128 Amendment 3 / 3.1, gate sheets", "sample_sha256": file_sha(a.sample),
           "stratum": a.stratum, "pages": len({r["page_id"] for r in sample}), "shuffle_key": a.shuffle_key,
           "crops": {"total": len(crops), "ghn_called": sum(r["called"] for r in sample),
                     "ghn_sampled_not_called": sum(not r["called"] for r in sample),
                     "cal_cherokee": len(cal_chr), "cal_lookalike": len(look),
                     "cal_lookalike_from_v2": sum(c["cal_source"] == "testset_v2_aligned" for c in look),
                     "cal_lookalike_from_loc_alto": sum(c["cal_source"] == "loc_alto_syllabary" for c in look),
                     "cal_latin": len(cal_lat)},
           "cal_candidates": {"v2_chr_unused": len(chr_c), "v2_lookalike_unused": len(v2_look),
                              "loc_alto_lookalike": len(loc_look), "latin_unused": len(lat_c)},
           "excluded_cal_line_ids": len(used), "sheets": {"count": n, "per_sheet": cs.PER_SHEET}}
    (out / "manifest.json").write_text(json.dumps(man, indent=1) + "\n")
    print(json.dumps(man, indent=1))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    lab = ("--key", "--labels-a", "--labels-b", "--labels-c", "--out")
    spec = {"tune": lab, "simulate": lab,
            "draw": ("--summary", "--contrast", "--excluded", "--tuning", "--out-dir"),
            "sample": ("--scored", "--devset-all", "--pages", "--out"),
            "sheets": ("--sample", "--aligned", "--first-key", "--dev-key", "--phoenix-pages", "--alto-root",
                       "--loc-images", "--wordlist", "--out-dir")}
    for name, args in spec.items():
        s = sub.add_parser(name)
        for k in args:
            s.add_argument(k, required=True)
        if name == "simulate":
            s.add_argument("--sims", type=int, default=1000)
            s.add_argument("--draws", type=int, default=1000)
        if name == "draw":
            s.add_argument("--n", type=int, required=True)
        if name == "sheets":
            s.add_argument("--stratum", help="keep only the sample pages in this stratum (e.g. not_held)")
            s.add_argument("--shuffle-key", default="2128:clf2-gate-shuffle:")
        if name == "sample":
            s.add_argument("--t", type=float, required=True)
    a = ap.parse_args(argv)
    globals()["cmd_" + a.cmd](a)


if __name__ == "__main__":
    main()
