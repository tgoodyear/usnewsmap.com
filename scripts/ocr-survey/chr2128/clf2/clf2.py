"""Line classifier v2 for #2128 (Amendment 3, comment 7132048425): stage 1 (features, fit, tuning sheets).

  clf2.py fitset  --s1 exp1/s1.jsonl --classified s1-classified.jsonl --truth loc-truth.jsonl
                  --pages layout-test-pages.txt --scans DIR --out fitset.jsonl
  clf2.py pages   --summary line-boxes/summary.jsonl --first-check sample-pages.txt --pilot pilot.txt
                  --testset-manifest v2/manifest.json --out-dir DIR
  clf2.py devset  --pages tuning-pages.txt --boxes-dir line-boxes --found found-s1.jsonl --summary S
                  --inventory inventory.jsonl --ghn-root DIR --out devset.jsonl
  clf2.py read    --items fitset.jsonl|devset.jsonl --tessdata DIR --eng-tessdata DIR --out reads.jsonl --workers N
  clf2.py fit     --items fitset.jsonl --reads reads.jsonl --chr-text corpus.txt --phoenix-pages phoenix-pages.jsonl
                  --alto-root DIR --wordlist W --out-dir DIR
  clf2.py score   --model model.json --items devset.jsonl --reads reads.jsonl --chr-text ... --phoenix-pages ...
                  --alto-root ... --wordlist ... --out scored.jsonl
  clf2.py sheets  --scored scored.jsonl --pages tuning-pages.txt --first-key clf-check/key.jsonl
                  --loc-images DIR --out-dir DIR

Items: one JSON line per box, {"item_id", "page_id", "scan", "box", "p_chr", "rel_h", ...}; `fitset` adds the
LoC label. `read` reads each box (read_lines.py's crop: the box padded 12 px sideways and 8 px up and down,
grey) with chr_phoenix and with eng, Tesseract --oem 1 --psm 13 --dpi 300, and keeps each reading's text and
mean word confidence. The character models are rebuilt from their texts each time (`fit`, `score`); the
model file records the texts' sha256 and `score` checks them.

Features (Amendment 3): ll_chr, ll_eng (mean log-probability per character of the reading under a
Witten-Bell character 5-gram model; an empty reading gets the model's log-probability of an unseen
character), mean word confidence of each reading / 100 (0 when empty), syllabary share of the chr_phoenix
reading's letters (0 when it has none), box height / the page's median box height, and p_chr.
"""
import argparse
import hashlib
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
from collections import defaultdict
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.path.insert(0, str(HERE.parent / "clf"))
import clf_sheets as cs  # noqa: E402
from score_layout import iou_matrix  # noqa: E402

Image.MAX_IMAGE_PIXELS = None
ORDER = 5
PAD_X, PAD_Y = 12, 8
FEATURES = ["ll_chr", "ll_eng", "conf_chr", "conf_eng", "syl_share", "rel_h", "p_chr"]
SYL = cs.SYLLABARY
N_HELD = N_NOT_HELD = 4
N_OTHER = 150
P_MIN = 0.05
sha, file_sha, jsonl = cs.sha, cs.file_sha, cs.jsonl


def norm(text):
    return " ".join(text.split())


# ---------------------------------------------------------------- Witten-Bell character n-gram model
class WittenBell:
    """Interpolated Witten-Bell character n-gram model. Each training line is padded with ORDER-1 start
    symbols; there is no end symbol. The order-0 base is uniform over the training characters plus one
    unseen-character class."""

    BOS = "\x02"

    def __init__(self, lines, order=ORDER):
        self.n = order
        self.count = [defaultdict(lambda: defaultdict(int)) for _ in range(order)]  # [k][context of length k][c]
        chars = 0
        for line in lines:
            s = self.BOS * (order - 1) + line
            for i in range(order - 1, len(s)):
                c = s[i]
                for k in range(order):
                    self.count[k][s[i - k:i]][c] += 1
                chars += 1
        self.chars = chars
        self.vocab = set(self.count[0][""])
        self.tot = [{h: sum(d.values()) for h, d in ck.items()} for ck in self.count]
        self.typ = [{h: len(d) for h, d in ck.items()} for ck in self.count]
        self.unk_lp = math.log(self.prob("\x00", ""))

    def prob(self, c, hist):
        p = 1.0 / (len(self.vocab) + 1)
        for k in range(self.n):
            if k > len(hist):
                break
            h = hist[len(hist) - k:] if k else ""
            t = self.tot[k].get(h)
            if not t:
                break
            p = (self.count[k][h].get(c, 0) + self.typ[k][h] * p) / (t + self.typ[k][h])
        return p

    def ll(self, text):
        """Mean log-probability per character; the unseen-character log-probability for an empty text."""
        if not text:
            return self.unk_lp
        s = self.BOS * (self.n - 1) + text
        lp = sum(math.log(self.prob(s[i], s[i - self.n + 1:i])) for i in range(self.n - 1, len(s)))
        return lp / len(text)


def chr_lines(path):
    return [norm(l) for l in open(path, encoding="utf-8") if l.strip()]


def eng_lines(phoenix_pages, alto_root, wordlist):
    vocab = {w.strip().lower() for w in open(wordlist, encoding="utf-8") if w.strip()}
    out = []
    for r in jsonl(phoenix_pages):
        lccn, date, ed, seq = r["page"].split("/")
        y, m, d = date.split("-")
        lines, _ = cs.alto_lines(Path(alto_root) / lccn / y / m / d / ed / seq / "ocr.xml")
        for ln in lines:
            if ln["block_lang"] != "chr" and cs.english_line(ln["words"], vocab):
                out.append(norm(" ".join(ln["words"])))
    return out


def build_models(a):
    cl = chr_lines(a.chr_text)
    el = eng_lines(a.phoenix_pages, a.alto_root, a.wordlist)
    mc, me = WittenBell(cl), WittenBell(el)
    info = {"order": ORDER, "smoothing": "Witten-Bell, interpolated, uniform base over vocab+1",
            "chr": {"text": a.chr_text, "sha256": file_sha(a.chr_text), "lines": len(cl), "chars": mc.chars,
                    "vocab": len(mc.vocab)},
            "eng": {"phoenix_pages": a.phoenix_pages, "phoenix_pages_sha256": file_sha(a.phoenix_pages),
                    "pages": len(jsonl(a.phoenix_pages)), "wordlist_sha256": file_sha(a.wordlist),
                    "lines": len(el), "chars": me.chars, "vocab": len(me.vocab),
                    "text_sha256": hashlib.sha256("\n".join(el).encode()).hexdigest()}}
    return mc, me, info


# ---------------------------------------------------------------- items
def heights_median(boxes):
    return float(np.median([b[3] - b[1] for b in boxes])) if boxes else float("nan")


def cmd_fitset(a):
    pages = [l.strip() for l in open(a.pages) if l.strip()]
    s1 = {r["page"]: r for r in jsonl(a.s1)}
    cl = {r["page"]: r for r in jsonl(a.classified)}
    truth = {r["page"]: r for r in jsonl(a.truth)}
    out, stats = [], defaultdict(int)
    for p in pages:
        boxes = [l for l in cl[p]["lines"]]
        assert [l["box"] for l in boxes] == [l["box"] for l in s1[p]["lines"]], f"{p}: classified != s1"
        med = heights_median([l["box"] for l in boxes if l["box"][2] > l["box"][0] and l["box"][3] > l["box"][1]])
        keep = [l for l in boxes if l["box"][2] - l["box"][0] >= 8 and l["box"][3] - l["box"][1] >= 8]
        stats["s1_boxes"] += len(boxes)
        stats["boxes_8px"] += len(keep)
        t = truth[p]["lines"]
        m = iou_matrix([l["box"] for l in t], [l["box"] for l in keep]) if keep and t else np.zeros((len(t), len(keep)))
        for j, l in enumerate(keep):
            if not len(t) or m[:, j].max() < 0.5:
                continue
            i = int(m[:, j].argmax())
            stats["matched"] += 1
            letters = [c for c in t[i]["text"] if c.isalpha()]
            if not letters:
                stats["dropped_no_letters"] += 1
                continue
            share = sum(bool(SYL.match(c)) for c in letters) / len(letters)
            y = 1 if share >= 0.8 else 0 if share <= 0.05 else None
            if y is None:
                stats["dropped_mixed"] += 1
                continue
            if l.get("p_chr") is None:
                stats["dropped_no_p_chr"] += 1
                continue
            stats["cherokee" if y else "latin"] += 1
            out.append({"item_id": f"loc:{p}#{l['tid']}", "page_id": "loc:" + p.replace("/", "_"), "page": p,
                        "scan": str(Path(a.scans) / (p.replace("/", "_") + ".jpg")), "box": l["box"],
                        "p_chr": l["p_chr"], "rel_h": (l["box"][3] - l["box"][1]) / med, "y": y,
                        "truth_line": t[i]["id"], "iou": round(float(m[i, j]), 4), "syl_share_truth": share})
    cs_write(a.out, out)
    stats["pages"] = len(pages)
    print(json.dumps(stats, indent=1))


def cs_write(path, rows):
    with open(path, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")


def cmd_pages(a):
    summary = jsonl(a.summary)
    first = [l.strip() for l in open(a.first_check) if l.strip()]
    pilot = [l.strip() for l in open(a.pilot) if l.strip()]
    m = json.load(open(a.testset_manifest, encoding="utf-8"))
    loc = set()
    for s in m["sets"].values():
        if isinstance(s, dict) and isinstance(s.get("pages"), list):
            loc.update(s["pages"])
    ts = [r["doc_id"] for r in summary if cs.inv_id(r["doc_id"]) in loc]
    assert len(first) == 22 and len(pilot) == 5
    excl = sorted(set(first) | set(pilot) | set(ts))
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    (out / "excluded-pages.txt").write_text("".join(p + "\n" for p in excl))
    held = {r["doc_id"]: bool(r["held"]) for r in summary if r["boxes"] >= 1 and r["doc_id"] not in set(excl)}
    order = sorted(held, key=lambda p: sha("2128:clf2-dev:" + p))
    tune = sorted([p for p in order if held[p]][:N_HELD] + [p for p in order if not held[p]][:N_NOT_HELD])
    (out / "tuning-pages.txt").write_text("".join(p + "\n" for p in tune))
    res = {"summary_sha256": file_sha(a.summary), "first_check_pages": len(first), "pilot_pages": len(pilot),
           "testset_v2_loc_pages": len(loc), "testset_v2_ghn_pages": sorted(ts), "excluded": len(excl),
           "excluded_sha256": file_sha(out / "excluded-pages.txt"), "pool": len(held),
           "pool_held": sum(held.values()), "tuning_pages": {p: ("held" if held[p] else "not_held") for p in tune},
           "tuning_sha256": file_sha(out / "tuning-pages.txt")}
    for n in ("excluded-pages", "tuning-pages"):
        (out / f"{n}.sha256").write_text(f"{file_sha(out / (n + '.txt'))}  {n}.txt\n")
    (out / "pages.json").write_text(json.dumps(res, indent=1) + "\n")
    print(json.dumps(res, indent=1))


def cmd_devset(a):
    pages = [l.strip() for l in open(a.pages) if l.strip()]
    held = {r["doc_id"]: bool(r["held"]) for r in jsonl(a.summary)}
    inv = {r["id"]: r for r in jsonl(a.inventory)}
    found = {}
    for l in open(a.found, encoding="utf-8"):
        r = json.loads(l)
        if r["doc_id"] in pages:
            found[r["doc_id"]] = [[int(v) for v in x["box"]] for x in r["lines"]
                                  if x["box"][2] > x["box"][0] and x["box"][3] > x["box"][1]]
    out, noink, stats = [], [], defaultdict(int)
    for p in pages:
        rows = jsonl(Path(a.boxes_dir) / p.split(":", 1)[1] / "boxes.jsonl")
        head, boxes = rows[0], rows[1:]
        assert head["doc_id"] == p and [b["box"] for b in boxes] == found[p], f"{p}: boxes != found-s1"
        img = Path(a.ghn_root) / inv[cs.inv_id(p)]["file"]
        if file_sha(img) != head["scan_sha256"]:
            raise SystemExit(f"{p}: scan sha256 differs from boxes.jsonl")
        med = heights_median([b["box"] for b in boxes])
        stats["boxes"] += len(boxes)
        for b in boxes:
            if b.get("p_chr") is None:
                stats["no_p_chr"] += 1
                noink.append({"item_id": f"{p}#{b['id']}", "page_id": p, "box_id": b["id"], "held_flag": held[p],
                              "scan": str(img), "box": b["box"], "p_chr": None, "old_call": False})
                continue
            out.append({"item_id": f"{p}#{b['id']}", "page_id": p, "box_id": b["id"], "held_flag": held[p],
                        "scan": str(img), "box": b["box"], "p_chr": b["p_chr"], "old_call": b["chr"] is True,
                        "rel_h": (b["box"][3] - b["box"][1]) / med})
    cs_write(a.out, out)
    if a.noink_out:
        cs_write(a.noink_out, noink)
    stats["items"] = len(out)
    print(json.dumps(stats))


# ---------------------------------------------------------------- reading
def tess(paths, lang, tessdata, tmp):
    Path(tmp, "list.txt").write_text("".join(str(p) + "\n" for p in paths))
    subprocess.run(["tesseract", "list.txt", "out", "-l", lang, "--tessdata-dir", os.path.abspath(tessdata),
                    "--oem", "1", "--psm", "13", "--dpi", "300", "-c", "tessedit_create_tsv=1"], cwd=tmp, check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=dict(os.environ, OMP_THREAD_LIMIT="1"))
    words = defaultdict(list)
    for row in Path(tmp, "out.tsv").read_text(encoding="utf-8").splitlines()[1:]:
        f = row.split("\t")
        if len(f) >= 12 and f[0] == "5" and f[11].strip():
            words[int(f[1])].append((f[11], float(f[10])))
    res = []
    for i in range(len(paths)):
        w = words.get(i + 1, [])
        res.append((" ".join(t for t, _ in w), sum(c for _, c in w) / len(w) if w else None))
    return res


def read_page(args):
    scan, items, tessdata, eng_tessdata = args
    im = Image.open(scan).convert("L")
    W, H = im.size
    with tempfile.TemporaryDirectory() as tmp:
        paths = []
        for k, it in enumerate(items):
            x0, y0, x1, y1 = it["box"]
            pb = [max(x0 - PAD_X, 0), max(y0 - PAD_Y, 0), min(x1 + PAD_X, W), min(y1 + PAD_Y, H)]
            path = Path(tmp, f"c{k:05d}.png")
            im.crop(pb).save(path)
            paths.append(path)
        rc = tess(paths, "chr_phoenix", tessdata, tmp)
        re_ = tess(paths, "eng", eng_tessdata, tmp)
    return [{"item_id": it["item_id"], "chr_text": c[0], "chr_conf": c[1], "eng_text": e[0], "eng_conf": e[1]}
            for it, c, e in zip(items, rc, re_)]


def cmd_read(a):
    items = jsonl(a.items)
    done = {json.loads(l)["item_id"] for l in open(a.out)} if os.path.exists(a.out) else set()
    by = defaultdict(list)
    for it in items:
        if it["item_id"] not in done:
            by[it["scan"]].append(it)
    jobs = [(s, v, a.tessdata, a.eng_tessdata) for s, v in by.items()]
    print(f"{sum(len(v) for v in by.values())} boxes on {len(jobs)} pages to read", file=sys.stderr, flush=True)
    with open(a.out, "a", encoding="utf-8") as f, ProcessPoolExecutor(a.workers) as ex:
        for k, rows in enumerate(ex.map(read_page, jobs), 1):
            for r in rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
            f.flush()
            print(f"{k}/{len(jobs)} pages", file=sys.stderr, flush=True)


# ---------------------------------------------------------------- features, fit, score
def features(items, reads, mc, me):
    rd = {r["item_id"]: r for r in reads}
    X = []
    for it in items:
        r = rd[it["item_id"]]
        ct, et = norm(r["chr_text"]), norm(r["eng_text"])
        letters = [c for c in ct if c.isalpha()]
        X.append([mc.ll(ct), me.ll(et), (r["chr_conf"] or 0.0) / 100, (r["eng_conf"] or 0.0) / 100,
                  sum(bool(SYL.match(c)) for c in letters) / len(letters) if letters else 0.0,
                  it["rel_h"], it["p_chr"]])
    return np.array(X, float)


def cmd_fit(a):
    from sklearn.linear_model import LogisticRegression
    from sklearn.preprocessing import StandardScaler
    items = jsonl(a.items)
    mc, me, info = build_models(a)
    X = features(items, jsonl(a.reads), mc, me)
    y = np.array([it["y"] for it in items])
    sc = StandardScaler().fit(X)
    lr = LogisticRegression(max_iter=2000).fit(sc.transform(X), y)
    p = lr.predict_proba(sc.transform(X))[:, 1]
    pred = p >= 0.5
    pages = sorted({it["page"] for it in items})
    model = {"issue": "#2128 Amendment 3", "features": FEATURES, "lm": info,
             "scaler_mean": sc.mean_.tolist(), "scaler_scale": sc.scale_.tolist(),
             "coef": lr.coef_[0].tolist(), "intercept": float(lr.intercept_[0]),
             "fit": {"items_sha256": file_sha(a.items), "reads_sha256": file_sha(a.reads), "pages": len(pages),
                     "lines": len(y), "cherokee": int(y.sum()), "latin": int((1 - y).sum()),
                     "accuracy_at_0.5": float((pred == y).mean()),
                     "cherokee_recall_at_0.5": float(pred[y == 1].mean()),
                     "latin_correct_at_0.5": float((~pred[y == 0]).mean()),
                     "sklearn": "LogisticRegression(max_iter=2000), default L2 C=1, on standardised features"}}
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    (out / "model.json").write_text(json.dumps(model, indent=1) + "\n")
    with open(out / "fit-features.jsonl", "w") as f:
        for it, x, pp in zip(items, X, p):
            f.write(json.dumps({"item_id": it["item_id"], "y": it["y"], "x": [round(v, 5) for v in x],
                                "p": round(float(pp), 5)}) + "\n")
    print(json.dumps({k: model[k] for k in ("coef", "intercept", "fit")}, indent=1))
    print(json.dumps(info, indent=1))


def predict(model, X):
    z = (X - np.array(model["scaler_mean"])) / np.array(model["scaler_scale"])
    return 1 / (1 + np.exp(-(z @ np.array(model["coef"]) + model["intercept"])))


def cmd_score(a):
    model = json.load(open(a.model))
    items = jsonl(a.items)
    mc, me, info = build_models(a)
    for k in ("chr", "eng"):
        for f in ("sha256", "text_sha256", "chars"):
            if f in model["lm"][k] and model["lm"][k][f] != info[k][f]:
                raise SystemExit(f"language model {k} {f} differs from the fitted model's")
    X = features(items, jsonl(a.reads), mc, me)
    p = predict(model, X)
    with open(a.out, "w", encoding="utf-8") as f:
        for it, x, pp in zip(items, X, p):
            f.write(json.dumps({**it, "x": [round(v, 5) for v in x], "prob": round(float(pp), 6)},
                               ensure_ascii=False) + "\n")
    print(json.dumps({"items": len(items), "prob_ge_0.05": int((p >= P_MIN).sum()), "prob_ge_0.5": int((p >= 0.5).sum())}))


# ---------------------------------------------------------------- sheets
def render(crops, out):
    try:
        font = ImageFont.truetype(cs.FONT, 30)
    except OSError:
        font = ImageFont.truetype(cs.FONT_FALLBACK, 30)
    sa, sb = out / "sheets-a", out / "sheets-b"
    for d in (sa, sb):
        d.mkdir(exist_ok=True)
    sheet_w = cs.MARGIN + cs.GAP + cs.MAX_W + cs.GAP
    n = 0
    for s in range(0, len(crops), cs.PER_SHEET):
        chunk = crops[s:s + cs.PER_SHEET]
        rows_h = [max(cs.TARGET_H, c.size[1]) + 2 * cs.GAP for _, c in chunk]
        sheet = Image.new("L", (sheet_w, sum(rows_h) + 1), 255)
        dr = ImageDraw.Draw(sheet)
        y = 0
        for (cid, c), rh in zip(chunk, rows_h):
            dr.rectangle([0, y, cs.MARGIN - 1, y + rh - 1], fill=225)
            tb = dr.textbbox((0, 0), cid, font=font)
            dr.text((12, y + (rh - (tb[3] - tb[1])) // 2 - tb[1]), cid, fill=0, font=font)
            sheet.paste(c, (cs.MARGIN + cs.GAP, y + (rh - c.size[1]) // 2))
            dr.line([0, y, sheet_w, y], fill=120, width=1)
            y += rh
        dr.line([0, y, sheet_w, y], fill=120, width=1)
        n += 1
        name = f"sheet-{n:03d}"
        sheet.save(sa / f"{name}.png", optimize=True)
        shutil.copyfile(sa / f"{name}.png", sb / f"{name}.png")
        ids = "".join(cid + "\n" for cid, _ in chunk)
        for d in (sa, sb):
            (d / f"{name}-crop-ids.txt").write_text(ids)
    return n


def cmd_sheets(a):
    out = Path(a.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    pages = [l.strip() for l in open(a.pages) if l.strip()]
    scored = jsonl(a.scored)
    assert {r["page_id"] for r in scored} == set(pages)
    items, stats = [], {}
    for p in pages:
        rows = [r for r in scored if r["page_id"] == p]
        hi = [r for r in rows if r["prob"] >= P_MIN]
        other = sorted((r for r in rows if r["prob"] < P_MIN), key=lambda r: sha("2128:clf2-dev-box:" + p + ":" + r["box_id"]))
        chosen = other[:N_OTHER]
        N_o, n_o = len(other), len(chosen)
        stats[p] = {"held": rows[0]["held_flag"], "scored": len(rows), "prob_ge_0.05": len(hi),
                    "N_below": N_o, "n_sampled": n_o}
        for r, sel in [(r, "prob_ge_0.05") for r in hi] + [(r, "sampled_below") for r in chosen]:
            row = {"source": "ghn", "page_id": p, "held_flag": r["held_flag"], "box_id": r["box_id"], "box": r["box"],
                   "prob": r["prob"], "x": r["x"], "p_chr": r["p_chr"], "old_call": r["old_call"], "selection": sel,
                   "weight": 1.0 if sel == "prob_ge_0.05" else N_o / n_o, "N_page_scored": len(rows)}
            items.append((sha("2128:clf2-dev-shuffle:" + p + ":" + r["box_id"]), row, Path(r["scan"]), tuple(r["box"])))
    cal = [r for r in jsonl(a.first_key) if r["source"] == "calibration"]
    assert len(cal) == 400
    P = cs.PAD
    for r in cal:
        img = Path(a.loc_images) / (r["page_id"].split(":", 1)[1] + ".jpg")
        x0, y0, x1, y1 = r["crop_box_px"]
        row = {k: r[k] for k in ("source", "page_id", "box_id", "line_id", "known_script", "lookalike", "lookalike_share")}
        row["first_check_crop_id"] = r["crop_id"]
        items.append((sha("2128:clf2-dev-shuffle:" + r["page_id"] + ":" + r["box_id"]), row, img,
                      (x0 + P, y0 + P, x1 - P, y1 - P), r))
    items.sort(key=lambda t: t[0])
    by_img = defaultdict(list)
    for i, t in enumerate(items):
        by_img[t[2]].append(i)
    cut = {}
    for img, idx in by_img.items():
        with Image.open(img) as im0:
            im = im0.convert("L")
        for i in idx:
            cut[i] = cs.crop_scaled(im, *items[i][3])
            if len(items[i]) == 5:  # a reused calibration line: same pixels as the first check
                assert cut[i][1] == items[i][4]["crop_box_px"] and list(cut[i][0].size) == items[i][4]["crop_size"]
    crops, keyrows = [], []
    for i, t in enumerate(items):
        crop, padded = cut[i]
        cid = f"D{i + 1:04d}"
        crops.append((cid, crop))
        keyrows.append({"crop_id": cid, **t[1], "crop_box_px": padded, "crop_size": list(crop.size),
                        "sheet": f"sheet-{i // cs.PER_SHEET + 1:03d}", "row": i % cs.PER_SHEET + 1})
    n = render(crops, out)
    kp = out / "key.jsonl"
    fd = os.open(kp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        for r in keyrows:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    os.chmod(kp, 0o600)
    man = {"issue": "#2128 Amendment 3, tuning sheets", "tuning_pages_sha256": file_sha(a.pages),
           "scored_sha256": file_sha(a.scored), "pages": stats, "selection": f"prob >= {P_MIN} (weight 1) plus "
           f"{N_OTHER} others by sha256('2128:clf2-dev-box:'+page_id+':'+box_id), weight N/n",
           "shuffle": "sha256('2128:clf2-dev-shuffle:'+page_id+':'+box_id)",
           "crops": {"total": len(crops), "ghn": len(crops) - len(cal), "calibration": len(cal)},
           "sheets": {"count": n, "per_sheet": cs.PER_SHEET}}
    (out / "manifest.json").write_text(json.dumps(man, indent=1) + "\n")
    print(json.dumps({"crops": man["crops"], "sheets": man["sheets"]}))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    lm = ("--chr-text", "--phoenix-pages", "--alto-root", "--wordlist")
    spec = {"fitset": ("--s1", "--classified", "--truth", "--pages", "--scans", "--out"),
            "pages": ("--summary", "--first-check", "--pilot", "--testset-manifest", "--out-dir"),
            "devset": ("--pages", "--boxes-dir", "--found", "--summary", "--inventory", "--ghn-root", "--out"),
            "read": ("--items", "--tessdata", "--eng-tessdata", "--out"),
            "fit": ("--items", "--reads", "--out-dir") + lm,
            "score": ("--model", "--items", "--reads", "--out") + lm,
            "sheets": ("--scored", "--pages", "--first-key", "--loc-images", "--out-dir")}
    for name, args in spec.items():
        s = sub.add_parser(name)
        for k in args:
            s.add_argument(k, required=True)
        if name == "devset":
            s.add_argument("--noink-out", help="also write the boxes with no p_chr (no ink) here")
        if name == "read":
            s.add_argument("--workers", type=int, default=4)
    a = ap.parse_args(argv)
    globals()["cmd_" + a.cmd](a)


if __name__ == "__main__":
    main()
