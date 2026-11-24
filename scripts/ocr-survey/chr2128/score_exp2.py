#!/usr/bin/env python3
"""Experiments 2 and 3 of #2128 (Amendment 1 rules): silver CER, nameplate, and expert word error rate.

Every arm reads the same S0 crops (`read_lines.py`, OEM 1, PSM 13); this script only scores readings. It
writes aggregates only: no expert transcription text, and no reading of an expert-page line, leaves memory.

  regions  --classified S0.jsonl --reads chr=... --reads chr_phoenix=... --expert-dir DIR --out regions.json
      Locates the expert sentences on their two pages once, the same for every arm (see REGIONS below).
  score    --silver lines.jsonl --reads R2=... --reads R3=... --reads R4c=... --mismatch-tsv TSV
           --front-pages front-pages.txt --classified S0.jsonl --regions regions.json --expert-dir DIR
           --out exp2.json [--draws 2000] [--seed 2128]
  exp3     --found-a NAME=classified.jsonl --reads-a reads.jsonl --found-b NAME=classified.jsonl --reads-b reads.jsonl
           --regions regions.json --expert-dir DIR --out exp3.json [--draws 2000] [--seed 2128]

SILVER. CER over syllabary letters (U+13A0-U+13F5) as `cherokee_testset.py score` counts it: the letter edit
distance between the reference and the reading, summed over lines, over the reference letters. Lines in the
reference-mismatch TSV are anti-joined first, on (page, line id). The TSV's ids are LoC lines of test set v2's
pages, so the counts of rows, pages and lines it removes are reported.

INTERVALS. 2,000 draws (seed 2128) of pages, and separately of aligned blocks (`block_id`), with replacement,
the same draws for every arm; each draw recomputes each arm's pooled CER, and the difference of two arms. 95%
percentile intervals.

RULES (issue and Amendment 1). R3 passes against R2 if its CER is at least 10% lower (relative), both
intervals of R3 - R2 lie below 0, and it reads the nameplate exactly on no fewer front pages. R4c is compared
with R3 (5% lower) if R3 passes, otherwise with R2 under R3's rule (10%); it also needs both intervals below 0,
the nameplate not worse, and an expert WER no higher than its comparator's. The expert WER is reported for
every arm; an arm more than 2 points worse than R2 on it is flagged (not adopted without a further test).

NAMEPLATE. On each front page (seq-1) among the GHN test pages, the arm's readings of the page's S0 lines
called Cherokee are joined; the page counts if both ᏣᎳᎩ and ᏧᎴᎯᏌᏅᎯ are whole tokens (runs of letters and
digits that are all syllabary), as `chr_masthead.py` checks LoC's text.

EXPERT. The translators' files (cherokee-phoenix, web/scripts/fixtures/translations/<date>-*.txt), parsed as
translation-lib.mjs does: a header line, then groups of three non-empty lines whose first is the syllabary;
its words are whitespace tokens holding a letter or digit. A reference word is a token's syllabary letters
(small letters folded to capitals); a token with "(" or ")" (unread letters) or without syllabary is skipped.
The arm's words are its readings of the region's lines (below), in order, as the same letters. Words are
aligned one to one by edit distance over word sequences; a reference word is right when aligned to an equal
word. WER = reference words not right / reference words, so a word no line covers is an error; extra words
the arm reads are not counted. Intervals: 2,000 draws of sentences.

REGIONS. On each expert page, an S0 Cherokee line is an anchor when either fixed reading of phase 1 (stock
`chr` or `chr_phoenix`, R2) of it, with at least 8 letters, has at least 60% of its letters matched in a local
alignment (`cherokee_testset.local_align`) against the page's reference letters. Anchors are grouped into
columns (x centres within half the median anchor width of the group's mean); a group of at least 3 anchors
gives a rectangle, the anchors' bounding box. The region's lines are the lines whose box centre lies in a
rectangle; they are ordered rectangle by rectangle, left to right, and top to bottom inside each. Experiment 3
uses the same rectangles with S1's lines.
"""
import argparse
import collections
import json
import re
from pathlib import Path

import numpy as np

import common
import cherokee_testset as ct  # noqa: E402

NAMEPLATE = ("ᏣᎳᎩ", "ᏧᎴᎯᏌᏅᎯ")
EXPERT_PAGES = {"1829-02-25": "sn83020874/1829-02-25/ed-1/seq-3", "1832-03-17": "sn83020874/1832-03-17/ed-1/seq-1"}
WORD = re.compile(r"[^\W_]")


def letters(s):
    return "".join(ct.SYLLABARY.findall(s.upper()))


def parse_translation(text):
    """Sentences as lists of reference words (translation-lib.mjs's format; see EXPERT)."""
    lines = [l.strip() for l in text.splitlines() if l.strip()]
    lines = lines[1:]
    assert len(lines) % 3 == 0, "not groups of three lines"
    out = []
    for i in range(0, len(lines), 3):
        toks = [t for t in lines[i].split() if WORD.search(t)]
        out.append([letters(t) for t in toks if "(" not in t and ")" not in t and letters(t)])
    return out


def expert_sentences(expert_dir):
    out = {}
    for date, page in EXPERT_PAGES.items():
        files = sorted(Path(expert_dir).glob(f"web/scripts/fixtures/translations/{date}-*.txt"))
        assert len(files) == 1, f"{date}: {len(files)} files"
        out[page] = parse_translation(files[0].read_text(encoding="utf-8"))
    return out


def load_reads(specs):
    out = {}
    for s in specs:
        name, _, path = s.partition("=")
        out[name] = {r["line_id"]: r for r in common.read_jsonl(path)}
    return out


def centre_in(box, rect):
    cx, cy = (box[0] + box[2]) / 2, (box[1] + box[3]) / 2
    return rect[0] <= cx <= rect[2] and rect[1] <= cy <= rect[3]


def region_lines(rec, rects):
    """Line ids of `rec`'s Cherokee lines in the rectangles, in the region order."""
    out = []
    for rect in rects:
        inside = [l for l in rec["lines"] if l.get("chr") and centre_in(l["box"], rect)]
        out += [common.line_id(rec["page"], l["tid"]) for l in sorted(inside, key=lambda l: (l["box"][1], l["box"][0]))]
    return list(dict.fromkeys(out))


def align_words(ref, hyp):
    """For each reference word, whether a one-to-one edit-distance alignment pairs it with an equal word."""
    n, m = len(ref), len(hyp)
    d = np.zeros((n + 1, m + 1), np.int32)
    d[:, 0] = np.arange(n + 1)
    d[0, :] = np.arange(m + 1)
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i, j] = min(d[i - 1, j] + 1, d[i, j - 1] + 1, d[i - 1, j - 1] + (ref[i - 1] != hyp[j - 1]))
    right = [False] * n
    i, j = n, m
    while i > 0 and j > 0:
        if d[i, j] == d[i - 1, j - 1] + (ref[i - 1] != hyp[j - 1]):
            right[i - 1] = ref[i - 1] == hyp[j - 1]
            i, j = i - 1, j - 1
        elif d[i, j] == d[i - 1, j] + 1:
            i -= 1
        else:
            j -= 1
    return right


def expert_units(sentences, rec_by_page, reads, regions):
    """Per sentence: (reference words, errors), for one arm's readings."""
    units = []
    for page, sents in sentences.items():
        ids = region_lines(rec_by_page[page], regions[page]["rects"])
        hyp = [letters(w) for lid in ids for w in reads.get(lid, {}).get("text", "").split()]
        hyp = [w for w in hyp if w]
        ref = [w for s in sents for w in s]
        right = align_words(ref, hyp)
        k = 0
        for s in sents:
            units.append((len(s), len(s) - sum(right[k:k + len(s)])))
            k += len(s)
    return units


def boot(rng, n, draws):
    return rng.integers(0, n, size=(draws, n))


def ci(v):
    return [round(float(np.percentile(v, 2.5)), 4), round(float(np.percentile(v, 97.5)), 4)]


def cmd_regions(a):
    sentences = expert_sentences(a.expert_dir)
    reads = load_reads(a.reads)
    recs = {r["page"]: r for r in common.read_jsonl(a.classified) if r["page"] in sentences}
    out = {}
    for page, sents in sentences.items():
        E = "".join(w for s in sents for w in s)
        anchors = []
        for l in recs[page]["lines"]:
            if not l.get("chr"):
                continue
            lid = common.line_id(page, l["tid"])
            for rd in reads.values():
                L = letters(rd.get(lid, {}).get("text", ""))
                if len(L) < 8:
                    continue
                pairs = ct.local_align(L, E)
                matched = sum(1 for qi, ti in pairs if qi is not None and ti is not None and L[qi] == E[ti])
                if matched / len(L) >= 0.6:
                    anchors.append(l["box"])
                    break
        anchors.sort(key=lambda b: (b[0] + b[2]) / 2)
        groups = []
        for b in anchors:
            cx, w = (b[0] + b[2]) / 2, b[2] - b[0]
            g = next((g for g in groups if abs(cx - np.mean([(x[0] + x[2]) / 2 for x in g])) <=
                      0.5 * np.median([x[2] - x[0] for x in g])), None)
            (g.append(b) if g is not None else groups.append([b]))
        rects = [[min(b[0] for b in g), min(b[1] for b in g), max(b[2] for b in g), max(b[3] for b in g)]
                 for g in groups if len(g) >= 3]
        rects.sort(key=lambda r: r[0])
        ids = region_lines(recs[page], rects)
        out[page] = {"rects": rects, "anchors": len(anchors), "groups": [len(g) for g in groups],
                     "region_lines": len(ids), "reference_words": sum(len(s) for s in sents),
                     "sentences": len(sents), "page_width": recs[page]["width"],
                     "rect_x_share": [[round(r[0] / recs[page]["width"], 3), round(r[2] / recs[page]["width"], 3)] for r in rects]}
    json.dump(out, open(a.out, "w"), indent=1)
    print(json.dumps(out, indent=1))


def cmd_score(a):
    rng = np.random.default_rng(a.seed)
    reads = load_reads(a.reads)
    arms = list(reads)
    silver = common.read_jsonl(a.silver)
    # the reference-mismatch anti-join, on (page, line id)
    held, held_pages = set(), set()
    rows = 0
    for line in open(a.mismatch_tsv, encoding="utf-8").read().splitlines()[1:]:
        tid = line.split("\t")[0].split("/")[-1].split("~")[0]
        m = re.match(r"(sn\d+_\d{4}-\d\d-\d\d_ed-\d+_seq-\d+)_(.+)$", tid)
        rows += 1
        if m:
            page = m.group(1).replace("_", "/", 1).replace("_ed-", "/ed-").replace("_seq-", "/seq-")
            held.add((page, m.group(2)))
            held_pages.add(page)
    keep = [r for r in silver if (r["page"], r["line_id"].split("#")[1]) not in held]
    anti = {"tsv_rows": rows, "tsv_pages": len(held_pages), "tsv_pages_in_silver": len(held_pages & {r["page"] for r in silver}),
            "silver_lines_removed": len(silver) - len(keep), "silver_lines_scored": len(keep)}
    # per line: reference letters and each arm's errors
    ref = np.array([len(letters(r["reference"])) for r in keep], float)
    err = {n: np.array([ct.letter_edit_distance(letters(r["reference"]), letters(reads[n][r["line_id"]]["text"]))
                        for r in keep], float) for n in arms}
    missing = {n: sum(r["line_id"] not in reads[n] for r in keep) for n in arms}
    cer = {n: float(err[n].sum() / ref.sum()) for n in arms}
    out = {"silver": {"lines": len(keep), "pages": len({r["page"] for r in keep}), "blocks": len({r["block_id"] for r in keep}),
                      "reference_letters": int(ref.sum()), "missing_readings": missing, "anti_join": anti,
                      "cer": {n: round(cer[n], 4) for n in arms}, "ci95": {}, "diff": {}}}
    for unit in ("page", "block_id"):
        keys = sorted({r[unit] for r in keep})
        idx = {k: i for i, k in enumerate(keys)}
        u = np.array([idx[r[unit]] for r in keep])
        R = np.bincount(u, ref, len(keys))
        E = {n: np.bincount(u, err[n], len(keys)) for n in arms}
        d = boot(rng, len(keys), a.draws)
        bc = {n: E[n][d].sum(1) / R[d].sum(1) for n in arms}
        out["silver"]["ci95"][unit] = {n: ci(bc[n]) for n in arms}
        for x, y in (("R3", "R2"), ("R4c", "R3"), ("R4c", "R2")):
            out["silver"]["diff"].setdefault(f"{x}-{y}", {"point": round(cer[x] - cer[y], 4),
                                                         "relative": round((cer[x] - cer[y]) / cer[y], 4)})
            out["silver"]["diff"][f"{x}-{y}"][f"ci95_{unit}"] = ci(bc[x] - bc[y])
    # nameplate
    front = common.read_pages(a.front_pages)
    recs = {r["page"]: r for r in common.read_jsonl(a.classified)}
    np_ok = {}
    for n in arms:
        ok = 0
        for p in front:
            txt = " ".join(reads[n].get(common.line_id(p, l["tid"]), {}).get("text", "") for l in recs[p]["lines"] if l.get("chr"))
            toks = [t for t in re.findall(r"[^\W_]+", txt) if re.fullmatch("[Ꭰ-Ᏽ]+", t)]
            ok += all(w in toks for w in NAMEPLATE)
        np_ok[n] = ok
    out["nameplate"] = {"front_pages": len(front), "both_exact": np_ok, "share": {n: round(v / len(front), 4) for n, v in np_ok.items()}}
    # expert
    sentences = expert_sentences(a.expert_dir)
    regions = json.load(open(a.regions))
    units = {n: expert_units(sentences, recs, reads[n], regions) for n in arms}
    N = np.array([w for w, _ in units[arms[0]]], float)
    errs = {n: np.array([e for _, e in units[n]], float) for n in arms}
    d = boot(rng, len(N), a.draws)
    wer = {n: float(errs[n].sum() / N.sum()) for n in arms}
    bw = {n: errs[n][d].sum(1) / N[d].sum(1) for n in arms}
    out["expert"] = {"sentences": len(N), "reference_words": int(N.sum()), "wer": {n: round(wer[n], 4) for n in arms},
                     "ci95": {n: ci(bw[n]) for n in arms},
                     "diff": {f"{x}-{y}": {"point": round(wer[x] - wer[y], 4), "ci95": ci(bw[x] - bw[y])}
                              for x, y in (("R3", "R2"), ("R4c", "R3"), ("R4c", "R2"))}}
    # rules
    sd = out["silver"]["diff"]

    def silver_ok(x, y, rel):
        r = sd[f"{x}-{y}"]
        return {"relative_drop": r["relative"] <= -rel, "page_interval_below_0": r["ci95_page"][1] < 0,
                "block_interval_below_0": r["ci95_block_id"][1] < 0, "nameplate_not_worse": np_ok[x] >= np_ok[y]}
    r3 = silver_ok("R3", "R2", 0.10)
    r3["passes"] = all(r3.values())
    comp, rel = ("R3", 0.05) if r3["passes"] else ("R2", 0.10)
    r4 = silver_ok("R4c", comp, rel)
    r4["expert_wer_not_higher"] = wer["R4c"] <= wer[comp]
    r4["passes"] = all(r4.values())
    out["rules"] = {"R3_vs_R2": r3, f"R4c_vs_{comp}": r4,
                    "expert_more_than_2_points_worse_than_R2": {n: wer[n] - wer["R2"] > 0.02 for n in arms if n != "R2"}}
    adopted = "R4c" if r4["passes"] else ("R3" if r3["passes"] else "R2")
    if adopted != "R2" and out["rules"]["expert_more_than_2_points_worse_than_R2"][adopted]:
        out["rules"]["note"] = f"{adopted} passes but is over 2 points worse than R2 on the expert set: not adopted without a further test"
        adopted = "R2"
    out["adopted_reader"] = adopted
    json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps(out, indent=1, sort_keys=True))


def cmd_exp3(a):
    rng = np.random.default_rng(a.seed)
    sentences = expert_sentences(a.expert_dir)
    regions = json.load(open(a.regions))
    res, errs = {}, {}
    for spec, rpath in ((a.found_a, a.reads_a), (a.found_b, a.reads_b)):
        name, _, path = spec.partition("=")
        recs = {r["page"]: r for r in common.read_jsonl(path) if r["page"] in sentences}
        reads = {r["line_id"]: r for r in common.read_jsonl(rpath)}
        units = expert_units(sentences, recs, reads, regions)
        N = np.array([w for w, _ in units], float)
        errs[name] = np.array([e for _, e in units], float)
        res[name] = {"region_lines": {p: len(region_lines(recs[p], regions[p]["rects"])) for p in sentences}}
    names = list(errs)
    d = boot(rng, len(N), a.draws)
    bw = {n: errs[n][d].sum(1) / N[d].sum(1) for n in names}
    for n in names:
        res[n].update({"wer": round(float(errs[n].sum() / N.sum()), 4), "ci95": ci(bw[n])})
    out = {"sentences": len(N), "reference_words": int(N.sum()), "arms": res,
           "diff": {f"{names[0]}-{names[1]}": {"point": round(res[names[0]]["wer"] - res[names[1]]["wer"], 4),
                                                "ci95": ci(bw[names[0]] - bw[names[1]])}}, "descriptive_only": True}
    json.dump(out, open(a.out, "w"), indent=1, sort_keys=True)
    print(json.dumps(out, indent=1, sort_keys=True))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("regions")
    r.add_argument("--classified", required=True)
    r.add_argument("--reads", action="append", required=True)
    r.add_argument("--expert-dir", required=True)
    r.add_argument("--out", required=True)
    r.set_defaults(fn=cmd_regions)
    s = sub.add_parser("score")
    s.add_argument("--silver", required=True)
    s.add_argument("--reads", action="append", required=True, help="R2=..., R3=..., R4c=...")
    s.add_argument("--mismatch-tsv", required=True)
    s.add_argument("--front-pages", required=True)
    s.add_argument("--classified", required=True)
    s.add_argument("--regions", required=True)
    s.add_argument("--expert-dir", required=True)
    s.add_argument("--draws", type=int, default=2000)
    s.add_argument("--seed", type=int, default=2128)
    s.add_argument("--out", required=True)
    s.set_defaults(fn=cmd_score)
    e = sub.add_parser("exp3")
    e.add_argument("--found-a", required=True)
    e.add_argument("--reads-a", required=True)
    e.add_argument("--found-b", required=True)
    e.add_argument("--reads-b", required=True)
    e.add_argument("--regions", required=True)
    e.add_argument("--expert-dir", required=True)
    e.add_argument("--draws", type=int, default=2000)
    e.add_argument("--seed", type=int, default=2128)
    e.add_argument("--out", required=True)
    e.set_defaults(fn=cmd_exp3)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
