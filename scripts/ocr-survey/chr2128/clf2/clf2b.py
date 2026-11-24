"""Classifier v2 rerun (#2128 Amendment 3.2, comment 7134230004): a clean calibration set, a fresh
not-held gate sample and its blinded sheets. Classifier, t = 0.97 and scorer unchanged.

  clf2b.py cal    --silver silver/lines.jsonl --aligned-ghn r4-candidates/lines.jsonl --first-key key.jsonl
                  --aligned v2/aligned.jsonl --alto-root X --loc-images L --wordlist W
                  --inventory inventory.jsonl --ghn-root R --out cal.jsonl
  clf2b.py draw   --summary S --contrast C --exclude-pages F [--exclude-pages ...] --cal cal.jsonl --out-dir D
  clf2b.py sheets --sample sample.jsonl --cal cal.jsonl --out-dir D

Cherokee calibration: NT-aligned lines only, each with >= 8 syllabary letters in its aligned reference:
first the GHN silver lines and the 51 aligned GHN training lines, then the first check's 200 test set v2
Cherokee calibration lines. Within each source, lines are ranked by sha256("2128:clf2b-cal:"+line_id),
GHN before v2; the first 60 lookalikes (>= half of the reference's syllabary letters among Amendment 2.1's
23) are taken, then the rest of the 200 in the same order. GHN lines are cut from the GHN scan at their
S0 box. Latin: LoC lines on test set v2 pages outside language=chr blocks, every 3+-letter word in the
word list (clf_sheets.english_line), >= 8 letters, ranked by the same key; first 200.
Line ids: GHN `ghn:<doc_id>:<tid>`; LoC `loc:<doc_id>:<ALTO TextLine ID>`.
"""
import argparse
import collections
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import clf2  # noqa: E402

cs = clf2.cs
sha, jsonl, file_sha = cs.sha, cs.jsonl, cs.file_sha
KEY = "2128:clf2b-cal:"
MIN_SYL = 8


def look_share(ref):
    syl = cs.SYLLABARY.findall(ref)
    return len(syl), (sum(c in cs.LOOKALIKE for c in syl) / len(syl) if syl else 0.0)


def cmd_cal(a):
    inv = {r["id"]: r for r in jsonl(a.inventory)}
    ghn, stats = [], collections.Counter()
    for src, path in (("ghn_silver", a.silver), ("ghn_aligned_training", a.aligned_ghn)):
        for r in jsonl(path):
            stats[src] += 1
            n, sh = look_share(r["reference"])
            if n < MIN_SYL:
                stats[src + "_under_8"] += 1
                continue
            assert r["reference_source"] == "nt-1860-aligned"
            doc = r["page"].replace("/", "_")
            e = inv[r["page"]]
            ghn.append({"source": "calibration", "cal_source": src, "page_id": "ghn:" + doc, "box_id": r["tid"],
                        "line_id": f"ghn:{doc}:{r['tid']}", "known_script": "cherokee", "lookalike": sh >= 0.5,
                        "lookalike_share": sh, "syllabary_letters": n, "img": str(Path(a.ghn_root) / e["file"]),
                        "img_sha256": e["sha256"], "box": r["box"], "pad": True})
    assert len({c["line_id"] for c in ghn}) == len(ghn)
    aligned = {"loc:" + r["page"].replace("/", "_") + ":" + r["line_id"]: r for r in jsonl(a.aligned)}
    v2 = []
    for k in jsonl(a.first_key):
        if k.get("source") != "calibration" or k["known_script"] != "cherokee":
            continue
        stats["v2_first_check"] += 1
        r = aligned[k["line_id"]]
        n, sh = look_share(r["reference"])
        if n < MIN_SYL:
            stats["v2_first_check_under_8"] += 1
            continue
        x0, y0, x1, y1 = k["crop_box_px"]
        P = cs.PAD
        v2.append({"source": "calibration", "cal_source": "testset_v2_first_check", "page_id": k["page_id"],
                   "box_id": k["box_id"], "line_id": k["line_id"], "known_script": "cherokee",
                   "lookalike": sh >= 0.5, "lookalike_share": sh, "syllabary_letters": n,
                   "img": str(Path(a.loc_images) / (k["page_id"].split(":", 1)[1] + ".jpg")),
                   "box": [x0 + P, y0 + P, x1 - P, y1 - P], "pad": True})
    order = sorted(ghn, key=lambda c: sha(KEY + c["line_id"])) + sorted(v2, key=lambda c: sha(KEY + c["line_id"]))
    look = [c for c in order if c["lookalike"]][:cs.MIN_LOOKALIKE]
    taken = {c["line_id"] for c in look}
    chr_ = look + [c for c in order if c["line_id"] not in taken][:cs.N_CAL_CHR - len(look)]
    assert len(chr_) == 200 and len(look) >= 60, (len(chr_), len(look))

    vocab = {w.strip().lower() for w in open(a.wordlist, encoding="utf-8") if w.strip()}
    lat, sizes = [], {}
    for page in sorted({r["page"] for r in aligned.values()}):
        lccn, date, ed, seq = page.split("/")
        y, m, d = date.split("-")
        lines, size = cs.alto_lines(Path(a.alto_root) / lccn / y / m / d / ed / seq / "ocr.xml")
        pid = "loc:" + page.replace("/", "_")
        img = Path(a.loc_images) / (page.replace("/", "_") + ".jpg")
        for ln in lines:
            if ln["block_lang"] == "chr" or not cs.english_line(ln["words"], vocab):
                continue
            if sum(ch.isalpha() for ch in " ".join(ln["words"])) < MIN_SYL:
                continue
            if img not in sizes:
                from PIL import Image
                with Image.open(img) as im:
                    sizes[img] = im.size
            lat.append({"source": "calibration", "cal_source": "testset_v2_alto_english", "page_id": pid,
                        "box_id": ln["line_id"], "line_id": pid + ":" + ln["line_id"], "known_script": "latin",
                        "lookalike": False, "lookalike_share": None, "img": str(img),
                        "box": list(cs.alto_to_px(ln["box"], size, sizes[img])), "pad": True})
    stats["latin_candidates"] = len(lat)
    lat = sorted(lat, key=lambda c: sha(KEY + c["line_id"]))[:cs.N_CAL_LAT]
    assert len(lat) == 200
    rows = chr_ + lat
    fd = os.open(a.out, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    comp = collections.Counter((c["cal_source"], c["lookalike"]) for c in rows)
    stats = dict(stats)
    stats.update({"ghn_eligible": len(ghn), "ghn_lookalike_eligible": sum(c["lookalike"] for c in ghn),
                  "v2_eligible": len(v2), "v2_lookalike_eligible": sum(c["lookalike"] for c in v2),
                  "composition": {f"{s}{' lookalike' if l else ''}": n for (s, l), n in sorted(comp.items())},
                  "cal_ghn_pages": sorted({c["page_id"] for c in rows if c["page_id"].startswith("ghn:")})})
    print(json.dumps(stats, indent=1))


def cmd_draw(a):
    summary = jsonl(a.summary)
    excl = set()
    for f in a.exclude_pages:
        excl |= {l.strip() for l in open(f) if l.strip()}
    cal_pages = {r["page_id"] for r in jsonl(a.cal) if r["page_id"].startswith("ghn:")}
    excl |= cal_pages
    pl = [r for r in summary if r["boxes"] >= 1 and r["doc_id"] not in excl]
    contrast = {r["page_id"]: r["contrast"] for r in jsonl(a.contrast)}
    nh = sorted((r["doc_id"] for r in pl if not r["held"]), key=lambda p: sha(a.key + p))[:23]
    pages = sorted(nh)
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    (out / "gate-pages.txt").write_text("".join(p + "\n" for p in pages))
    res = {"pool": len(pl), "pool_not_held": sum(not r["held"] for r in pl), "excluded": len(excl),
           "cal_pages_excluded": sorted(cal_pages), "gate_pages_sha256": file_sha(out / "gate-pages.txt"),
           "strata": {p: ["not_held"] for p in pages}, "added": [], "contrast": {p: contrast.get(p) for p in pages}}
    (out / "gate-pages.sha256").write_text(f"{res['gate_pages_sha256']}  gate-pages.txt\n")
    (out / "gate-pages.json").write_text(json.dumps(res, indent=1) + "\n")
    print(json.dumps({k: res[k] for k in ("pool", "pool_not_held", "excluded", "cal_pages_excluded", "gate_pages_sha256")}, indent=1))


def cmd_sheets(a):
    from PIL import Image
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    sample, cal = jsonl(a.sample), jsonl(a.cal)
    items = []
    for r in sample:
        items.append((sha(a.shuffle_key + r["page_id"] + ":" + r["box_id"]),
                      {k: v for k, v in r.items() if k != "scan"}, Path(r["scan"]), tuple(r["box"])))
    for c in cal:
        if c.get("img_sha256"):
            assert file_sha(c["img"]) == c["img_sha256"], c["img"]
        row = {k: v for k, v in c.items() if k not in ("img", "img_sha256", "pad", "box")}
        items.append((sha(a.shuffle_key + c["page_id"] + ":" + c["box_id"]), row, Path(c["img"]), tuple(c["box"])))
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
        cid = f"B{i + 1:05d}"
        crops.append((cid, crop))
        keyrows.append({"crop_id": cid, **t[1], "crop_box_px": padded, "crop_size": list(crop.size),
                        "sheet": f"sheet-{i // cs.PER_SHEET + 1:03d}", "row": i % cs.PER_SHEET + 1})
    n = clf2.render(crops, out)
    kp = out / "key.jsonl"
    fd = os.open(kp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        for r in keyrows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.chmod(kp, 0o600)
    man = {"issue": "#2128 Amendment 3.2, gate sheets", "sample_sha256": file_sha(a.sample), "cal_sha256": file_sha(a.cal),
           "shuffle_key": a.shuffle_key,
           "crops": {"total": len(crops), "ghn_called": sum(r["called"] for r in sample),
                     "ghn_sampled_not_called": sum(not r["called"] for r in sample), "calibration": len(cal)},
           "sheets": {"count": n, "per_sheet": cs.PER_SHEET}}
    (out / "manifest.json").write_text(json.dumps(man, indent=1) + "\n")
    print(json.dumps(man, indent=1))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    spec = {"cal": ("--silver", "--aligned-ghn", "--first-key", "--aligned", "--alto-root", "--loc-images",
                    "--wordlist", "--inventory", "--ghn-root", "--out"),
            "draw": ("--summary", "--contrast", "--cal", "--out-dir"),
            "sheets": ("--sample", "--cal", "--out-dir")}
    for name, args in spec.items():
        s = sub.add_parser(name)
        for k in args:
            s.add_argument(k, required=True)
        if name == "draw":
            s.add_argument("--exclude-pages", action="append", required=True)
            s.add_argument("--key", default="2128:clf2b:")
        if name == "sheets":
            s.add_argument("--shuffle-key", default="2128:clf2b-shuffle:")
    a = ap.parse_args(argv)
    globals()["cmd_" + a.cmd](a)


if __name__ == "__main__":
    main()
