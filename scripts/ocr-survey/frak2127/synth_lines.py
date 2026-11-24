#!/usr/bin/env python3
"""Arm C's synthetic lines (#2127): training text set glyph by glyph from the US German glyph bank.

The method is chr_synth_lines.py's (ocr/models/chr-phoenix/recipe): each letter is a random kept
glyph of that letter from the bank; marks the bank has no glyphs for (digits, punctuation) are set
from DejaVu Serif; the line is then degraded like a scan. Two changes for Latin type:
  - Glyphs are not scaled to one body height (Fraktur has ascenders and descenders). Each glyph is
    scaled so its word's x-height becomes XHEIGHT of the line height, and set where it stood against
    its word's baseline (both measured on the word's own x-height letters), so ascenders and
    descenders carry over from the page.
  - The degradation strength is chosen on the 17 model-selection pages (`tune`, below).

Text. Each synthetic line's text is a real training line's text (lines.jsonl rows with split
"train": Kölnische Zeitung Train, Reichsanzeiger-GT, NZZ, ONB), drawn at random. Dev and held-out
text is never read: the script refuses a lines.jsonl row of any other split. A line whose text needs
a letter the bank has no kept glyph for is skipped and counted.

Count. SYNTH_PER_REAL synthetic lines per real training line of arm B (its train.list), fixed here.

Degradation tuning (`tune`). For each preset in PRESETS, TUNE_LINES lines are rendered (training
text, seed TUNE_SEED) at the dev crops' resolution and read by stock frak2021 at PSM 13. Their edit
operations (substitution a->b, deletion a->'', insertion ''->b, after frak_norm) are pooled into a
distribution and compared with the same distribution of stock frak2021 on the 17 dev pages' real
lines (train_b.py's iteration-0 read, dev_0.jsonl). The preset with the smallest
    total-variation distance between the two operation distributions + |log(CER_synth / CER_dev)|
is chosen. Dev lines are only the target of this comparison; their text is never rendered.

`render` then makes the lines with the chosen preset, scales each to 36 px high (box filter, like
B's training crops) and writes one-line .lstmf files with cut_lines.write_lstmf.

    synth_lines.py tune --bank bank --lines lines/lines.jsonl --dev-read work/b/dev_0.jsonl \
        --tessdata tessdata --out synth-tune --jobs 20
    synth_lines.py render --bank bank --lines lines/lines.jsonl --train-list work/b/train.list \
        --tune synth-tune/tune.json --tessdata tessdata --out synth-lstmf --jobs 20
"""
from __future__ import annotations

import argparse, collections, json, math, os, random, statistics, sys, tempfile, time
from multiprocessing import Pool

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

import frak_score
from cut_lines import TRAIN_HEIGHT_PX, write_lstmf
from frak_norm import normalise

SYNTH_PER_REAL = 1 / 3
SEED = 2127
TUNE_SEED = 21270
TUNE_LINES = 800
LINE_H = (60, 76)          # rendered line height in px; dev crops' median height is about 68
SERIF = "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf"
# Degradation presets, from lighter to heavier than chr_synth_lines.py's degrade() ("chr").
PRESETS = {
    "light": dict(blur_p=0.5, blur=(0.2, 0.6), spread=0.10, loss=0.10, lo=(5, 30), hi=(220, 250), noise=(1, 5), jpeg=None),
    "chr": dict(blur_p=0.7, blur=(0.3, 1.1), spread=0.25, loss=0.15, lo=(10, 70), hi=(190, 245), noise=(2, 12), jpeg=None),
    "chr_jpeg": dict(blur_p=0.7, blur=(0.3, 1.1), spread=0.25, loss=0.15, lo=(10, 70), hi=(190, 245), noise=(2, 12), jpeg=(40, 80)),
    "heavy": dict(blur_p=0.85, blur=(0.6, 1.6), spread=0.35, loss=0.20, lo=(30, 90), hi=(170, 230), noise=(6, 18), jpeg=(30, 70)),
    "heavier": dict(blur_p=0.95, blur=(0.9, 2.0), spread=0.40, loss=0.25, lo=(40, 110), hi=(150, 215), noise=(10, 24), jpeg=(20, 50)),
}
# Letters that sit on the baseline with no ascender or descender in Fraktur (not s, which holds ſ,
# nor z, which descends). A word's baseline and x-height are the medians of these letters' boxes.
X_LETTERS = set("acemnoruvwx")
BASELINE, XHEIGHT = 0.74, 0.40   # where the baseline sits and how tall the x-height is, as shares of H


def load_bank(bank):
    """Kept glyphs by letter: (png, top of the png below the word's baseline in x-heights, the word's x-height)."""
    rows = [json.loads(l) for l in open(os.path.join(bank, "index.jsonl"), encoding="utf-8")]
    words = collections.defaultdict(list)
    for r in rows:
        if r["letter"] in X_LETTERS:
            words[r["word_id"]].append(r["box"])
    metric = {w: (statistics.median(b[3] for b in bs), statistics.median(b[3] - b[1] for b in bs))
              for w, bs in words.items()}
    pools = collections.defaultdict(list)
    for r in rows:
        if not r.get("kept") or r["word_id"] not in metric:
            continue
        base, xh = metric[r["word_id"]]
        if xh < 4:
            continue
        b, c = r["box"], r["crop_box"]
        top = max(c[1], b[1] - 2)  # the saved glyph is its box plus 2 px, clamped to the word crop
        pools[r["letter"]].append((os.path.join(bank, "glyphs", f"{ord(r['letter']):04x}", r["id"] + ".png"),
                                   (top - base) / xh, xh))
    return pools, BASELINE, XHEIGHT


def load_glyph(p, cache):
    if p not in cache:
        raw = np.asarray(Image.open(p).convert("L"), np.float32)
        ink, paper = np.percentile(raw, 5), np.percentile(raw, 80)
        cache[p] = np.clip((raw - ink) / max(1.0, paper - ink), 0, 1) * 255
    return cache[p]


_marks: dict = {}


def mark(ch, H, xh):
    key = (ch, H)
    if key not in _marks:
        font = ImageFont.truetype(SERIF, max(6, int(xh * H / 0.53)))  # DejaVu Serif x-height ≈ 0.53 em
        im = Image.new("L", (3 * H, 2 * H), 255)
        ImageDraw.Draw(im).text((H // 2, H), ch, font=font, fill=0, anchor="ls")
        a = np.asarray(im, np.float32)
        ys, xs = np.nonzero(a < 128)
        _marks[key] = None if len(ys) == 0 else (a[ys.min():ys.max() + 1, xs.min():xs.max() + 1], H - ys.min())
    return _marks[key]


def render(text, pools, rng, H, baseline, xh, cache):
    """A grey line image of `text` at height H, or (None, the missing letter)."""
    pieces = []  # (array, y, gap_before)
    space = False
    for ch in text:
        if ch == " ":
            space = True
            continue
        if ch.isalpha():
            if not pools.get(ch):
                return None, ch
            p, top, wxh = rng.choice(pools[ch])
            g = load_glyph(p, cache)
            s = xh * H / wxh
            g = np.asarray(Image.fromarray(g).resize((max(1, round(g.shape[1] * s)), max(1, round(g.shape[0] * s))), Image.BILINEAR), np.float32)
            y = int(round(baseline * H + top * xh * H))
        else:
            m = mark(ch, H, xh)
            if m is None:
                return None, ch
            g, above = m
            y = int(round(baseline * H)) - above
        pieces.append((g, y, space))
        space = False
    if not pieces:
        return None, ""
    gap, word = max(1, int(0.03 * H)), int(0.28 * H)
    width = sum(g.shape[1] for g, _, _ in pieces) + gap * len(pieces) + word * sum(1 for *_, s in pieces if s) + H
    canvas = np.full((H, width), 255.0, np.float32)
    x = H // 2
    for g, y, sp in pieces:
        if sp:
            x += word
        y = max(0, y + rng.randint(-1, 1))
        region = canvas[y:y + g.shape[0], x:x + g.shape[1]]
        canvas[y:y + g.shape[0], x:x + g.shape[1]] = np.minimum(region, g[:region.shape[0], :region.shape[1]])
        x += g.shape[1] + gap + rng.randint(-1, 1)
    return canvas[:, :x + H // 2], None


def degrade(a, rng, p):
    im = Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))
    if rng.random() < p["blur_p"]:
        im = im.filter(ImageFilter.GaussianBlur(rng.uniform(*p["blur"])))
    r = rng.random()
    if r < p["spread"]:
        im = im.filter(ImageFilter.MinFilter(3))
    elif r < p["spread"] + p["loss"]:
        im = im.filter(ImageFilter.MaxFilter(3))
    arr = np.asarray(im, np.float32)
    lo, hi = rng.uniform(*p["lo"]), rng.uniform(*p["hi"])
    arr = lo + (arr / 255.0) * (hi - lo)
    arr += np.random.default_rng(rng.randint(0, 2**31)).normal(0, rng.uniform(*p["noise"]), arr.shape)
    im = Image.fromarray(np.clip(arr, 0, 255).astype(np.uint8))
    if rng.random() < 0.5:
        im = im.rotate(rng.uniform(-0.6, 0.6), resample=Image.BILINEAR, expand=False, fillcolor=int(hi))
    if p["jpeg"]:
        import io
        b = io.BytesIO()
        im.save(b, "JPEG", quality=rng.randint(*p["jpeg"]))
        im = Image.open(io.BytesIO(b.getvalue())).convert("L")
    return im


def train_texts(lines_jsonl):
    out = []
    for line in open(lines_jsonl, encoding="utf-8"):
        r = json.loads(line)
        if r["split"] != "train":
            continue  # dev text is never rendered (held-out lines are in another file)
        assert r["split"] == "train"
        out.append(r["text"])
    return out


def ops(ref, hyp):
    """Edit operations of an optimal alignment of ref to hyp."""
    n, m = len(ref), len(hyp)
    d = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        d[i][0] = i
    for j in range(m + 1):
        d[0][j] = j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            d[i][j] = min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1]))
    out, i, j = [], n, m
    while i or j:
        if i and j and d[i][j] == d[i - 1][j - 1] + (ref[i - 1] != hyp[j - 1]):
            if ref[i - 1] != hyp[j - 1]:
                out.append((ref[i - 1], hyp[j - 1]))
            i, j = i - 1, j - 1
        elif i and d[i][j] == d[i - 1][j] + 1:
            out.append((ref[i - 1], ""))
            i -= 1
        else:
            out.append(("", hyp[j - 1]))
            j -= 1
    return out


def profile(pairs):
    c, chars = collections.Counter(), 0
    for ref, hyp in pairs:
        chars += len(ref)
        c.update(ops(ref, hyp))
    return c, sum(c.values()) / max(1, chars)


def tv(a, b):
    ta, tb = sum(a.values()) or 1, sum(b.values()) or 1
    return 0.5 * sum(abs(a.get(k, 0) / ta - b.get(k, 0) / tb) for k in set(a) | set(b))


# Worker state, set once per process.
W = {}


def init(bank, lines_jsonl):
    W["pools"], W["baseline"], W["xh"] = load_bank(bank)
    W["texts"] = train_texts(lines_jsonl)
    W["cache"] = {}


def make_chunk(job):
    """Render lines [k0, k1) of a run; write PNG + gt (tune) or .lstmf (render)."""
    run_seed, k0, k1, preset, out, mode, tessdata = job
    texts = W["texts"]
    rng = random.Random(f"{run_seed}:{k0}")
    made, missing = 0, collections.Counter()
    for k in range(k0, k1):
        for _ in range(50):
            text = normalise(rng.choice(texts))
            H = rng.randint(*LINE_H)
            arr, miss = render(text, W["pools"], rng, H, W["baseline"], W["xh"], W["cache"])
            if arr is not None:
                break
            missing[miss] += 1
        else:
            continue
        im = degrade(arr, rng, PRESETS[preset])
        name = f"synth__{run_seed}__{k:06d}"
        if mode == "tune":
            im.save(os.path.join(out, name + ".png"))
            open(os.path.join(out, name + ".gt.txt"), "w", encoding="utf-8").write(text + "\n")
        else:
            if im.height > TRAIN_HEIGHT_PX:
                im = im.resize((max(1, round(im.width * TRAIN_HEIGHT_PX / im.height)), TRAIN_HEIGHT_PX), Image.BOX)
            write_lstmf(os.path.join(out, f"{name}__1"), [(name, im, text)], tessdata)
        made += 1
    return made, missing


def run(bank, lines_jsonl, preset, out, mode, tessdata, n, run_seed, jobs, chunk=200, resume=False):
    os.makedirs(out, exist_ok=True)
    work = [(run_seed, k, min(n, k + chunk), preset, out, mode, tessdata) for k in range(0, n, chunk)]
    if resume:
        # A chunk's lines depend only on its seed, so an unfinished chunk is made again whole
        # (same names, same lines); finished chunks are left alone.
        have = set(os.listdir(out))
        def done(w):
            return all(f"synth__{w[0]}__{k:06d}__1.lstmf" in have for k in range(w[1], w[2]))
        work = [w for w in work if not done(w)]
        print(f"[synth resume] {len(work)} unfinished chunks", flush=True)
    made, missing, t0 = 0, collections.Counter(), time.time()
    if W.get("key") != (bank, lines_jsonl):
        init(bank, lines_jsonl)  # once, in the parent: the forked workers share it
        W["key"] = (bank, lines_jsonl)
    with Pool(jobs) as pool:
        for i, (m, miss) in enumerate(pool.imap_unordered(make_chunk, work), 1):
            made += m
            missing.update(miss)
            if i % 25 == 0 or i == len(work):
                print(f"[synth {mode} {preset}] {made}/{n} lines, {time.time() - t0:.0f}s", flush=True)
    return made, missing


def tune(a):
    dev = [json.loads(l) for l in open(a.dev_read, encoding="utf-8")]
    assert len(dev) == 8621, len(dev)
    target, dev_cer = profile((r["ref"], r["hyp"]) for r in dev)
    results = {}
    for name in PRESETS:
        d = os.path.join(a.out, name)
        made, missing = run(a.bank, a.lines, name, d, "tune", a.tessdata, TUNE_LINES, f"{TUNE_SEED}-{name}", a.jobs)
        res, rows = frak_score.score(d, a.tessdata, "frak2021", a.jobs)
        prof, cer = profile((ref, hyp) for _, ref, hyp, _ in rows)
        dist = tv(prof, target) + abs(math.log(max(cer, 1e-6) / dev_cer))
        results[name] = {"lines": made, "cer": cer, "tv": tv(prof, target), "distance": dist,
                         "top_ops": [[f"{x}>{y}", c] for (x, y), c in prof.most_common(12)]}
        print(name, json.dumps(results[name], ensure_ascii=False), flush=True)
    best = min(results, key=lambda k: results[k]["distance"])
    out = {"chosen": best, "preset": PRESETS[best], "dev_cer_stock": dev_cer,
           "dev_top_ops": [[f"{x}>{y}", c] for (x, y), c in target.most_common(12)], "presets": results,
           "tune_lines": TUNE_LINES, "line_height": LINE_H}
    json.dump(out, open(os.path.join(a.out, "tune.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps({"chosen": best, "dev_cer_stock": dev_cer}))


def render_cmd(a):
    tuned = json.load(open(a.tune))
    preset = tuned["chosen"]
    real = sum(1 for l in open(a.train_list) if l.strip())
    n = round(real * SYNTH_PER_REAL)
    made, missing = run(a.bank, a.lines, preset, a.out, "render", a.tessdata, n, f"{SEED}", a.jobs, resume=a.resume)
    made = sum(1 for f in os.listdir(a.out) if f.endswith(".lstmf"))
    summary = {"real_train_lines": real, "synth_per_real": SYNTH_PER_REAL, "target": n, "made": made,
               "preset": preset, "skipped_for_missing": dict(missing.most_common()), "seed": SEED}
    json.dump(summary, open(os.path.join(a.out, "summary.json"), "w"), ensure_ascii=False, indent=1)
    print(json.dumps(summary, ensure_ascii=False))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("tune")
    r = sub.add_parser("render")
    for p in (t, r):
        p.add_argument("--bank", required=True); p.add_argument("--lines", required=True)
        p.add_argument("--tessdata", required=True); p.add_argument("--out", required=True)
        p.add_argument("--jobs", type=int, default=12)
    t.add_argument("--dev-read", required=True)
    r.add_argument("--train-list", required=True); r.add_argument("--tune", required=True)
    r.add_argument("--resume", action="store_true", help="remake only the chunks a stopped run left unfinished")
    a = ap.parse_args()
    for d in (a.out,):
        if os.path.isdir(d) and os.listdir(d) and not getattr(a, "resume", False):
            sys.exit(f"{d} is not empty; give a new directory")
    tune(a) if a.cmd == "tune" else render_cmd(a)


if __name__ == "__main__":
    sys.exit(main())
