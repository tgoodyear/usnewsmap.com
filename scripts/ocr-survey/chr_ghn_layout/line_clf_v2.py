# Ported from branch chr-2128 (scripts/ocr-survey/chr2128/clf2/clf2.py at 52f3ba06 and dev3.py at c83d0206),
# the code #2128's gate scored (amendments 3.1 to 3.3). The arithmetic is unchanged; the code is gathered into
# one module for the GHN staging step (#1622).
"""The Cherokee line classifier v2 ensemble that passed #2128's amendment 3.3 gate.

A line's call is `p_line >= T`, where `p_line` combines two scores with a frozen logistic regression:

- `p_v2`, the reading-based score: the box is read twice by Tesseract 5.5.0 at PSM 13 (crop padded 12 px
  sideways and 8 px up and down, greyscale, --oem 1 --dpi 300), once with `chr_phoenix` and once with
  tessdata_best `eng`. Its features are each reading's mean log-probability per character under a
  Witten-Bell character 5-gram model (Cherokee, then English), each reading's mean word confidence / 100,
  the syllabary share of the chr_phoenix reading's letters, the box height over the page's median box
  height, and the shape classifier's `p_chr` (#1830). A logistic regression fitted on LoC lines combines them.
- `p_cnn`, a small CNN on the greyscale crop (padded 4 px, scaled to 32 px high, left-aligned on white,
  256 px wide).

The frozen numbers are in frozen/line_clf_v2.json; the CNN weights are a release asset, checked by
sha256 before use. Boxes with no `p_chr` (no measurable ink) aren't scored and aren't called.
"""
import hashlib
import json
import math
import os
import subprocess
import tempfile
from collections import defaultdict
from pathlib import Path

HERE = Path(__file__).resolve().parent
FROZEN = HERE / "frozen" / "line_clf_v2.json"
ORDER = 5
PAD_X, PAD_Y = 12, 8      # Tesseract crop padding
CNN_PAD, CNN_H, CNN_W = 4, 32, 256
SYLLABARY = "[Ꭰ-᏿ꭰ-ꮿ]"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_frozen(path=FROZEN):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def norm(text):
    return " ".join(text.split())


class WittenBell:
    """Interpolated Witten-Bell character n-gram model. Each training line is padded with ORDER-1 start
    symbols; there is no end symbol. The order-0 base is uniform over the training characters plus one
    unseen-character class."""

    BOS = "\x02"

    def __init__(self, lines, order=ORDER):
        self.n = order
        self.count = [defaultdict(lambda: defaultdict(int)) for _ in range(order)]
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


def read_lines_file(path):
    with open(path, encoding="utf-8") as f:
        return [norm(l) for l in f if l.strip()]


def language_models(chr_text, eng_text, frozen):
    """The two character models, built from their training texts after checking them against the frozen
    model's record (sha256 of the Cherokee file; sha256 of the English lines joined by newlines)."""
    lm = frozen["v2"]["lm"]
    got = sha256_file(chr_text)
    if got != lm["chr"]["sha256"]:
        raise SystemExit(f"{chr_text}: sha256 {got}, the frozen model's Cherokee text is {lm['chr']['sha256']}")
    el = read_lines_file(eng_text)
    got = hashlib.sha256("\n".join(el).encode()).hexdigest()
    if got != lm["eng"]["text_sha256"]:
        raise SystemExit(f"{eng_text}: lines sha256 {got}, the frozen model's English text is {lm['eng']['text_sha256']}")
    return WittenBell(read_lines_file(chr_text)), WittenBell(el)


def tess(paths, lang, tessdata, tmp):
    """One Tesseract run over the image files: per file, the words joined by spaces and the mean word
    confidence (None when there are no words)."""
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


def read_boxes(im, boxes, chr_tessdata, eng_tessdata):
    """chr_phoenix and eng readings of each box of the greyscale page image `im`."""
    W, H = im.size
    with tempfile.TemporaryDirectory() as tmp:
        paths = []
        for k, (x0, y0, x1, y1) in enumerate(boxes):
            pb = [max(x0 - PAD_X, 0), max(y0 - PAD_Y, 0), min(x1 + PAD_X, W), min(y1 + PAD_Y, H)]
            path = Path(tmp, f"c{k:05d}.png")
            im.crop(pb).save(path)
            paths.append(path)
        rc = tess(paths, "chr_phoenix", chr_tessdata, tmp) if paths else []
        re_ = tess(paths, "eng", eng_tessdata, tmp) if paths else []
    return [{"chr_text": c[0], "chr_conf": c[1], "eng_text": e[0], "eng_conf": e[1]} for c, e in zip(rc, re_)]


def v2_features(read, rel_h, p_chr, mc, me):
    import re
    ct, et = norm(read["chr_text"]), norm(read["eng_text"])
    letters = [c for c in ct if c.isalpha()]
    syl = sum(bool(re.match(SYLLABARY, c)) for c in letters) / len(letters) if letters else 0.0
    return [mc.ll(ct), me.ll(et), (read["chr_conf"] or 0.0) / 100, (read["eng_conf"] or 0.0) / 100, syl, rel_h, p_chr]


def v2_prob(x, frozen):
    v = frozen["v2"]
    z = sum((xi - m) / s * c for xi, m, s, c in zip(x, v["scaler_mean"], v["scaler_scale"], v["coef"])) + v["intercept"]
    return 1 / (1 + math.exp(-z))


def logit(p):
    p = min(max(p, 1e-4), 1 - 1e-4)
    return math.log(p / (1 - p))


def ensemble_prob(p_v2, p_cnn, frozen):
    e = frozen["ensemble"]
    z = e["coef"][0] * logit(p_v2) + e["coef"][1] * logit(p_cnn) + e["intercept"]
    return 1 / (1 + math.exp(-z))


def make_net():
    import torch.nn as nn

    def blk(i, o):
        return nn.Sequential(nn.Conv2d(i, o, 3, padding=1), nn.BatchNorm2d(o), nn.ReLU(), nn.MaxPool2d(2))
    return nn.Sequential(blk(1, 16), blk(16, 32), blk(32, 64), blk(64, 64), nn.AdaptiveAvgPool2d((1, 4)),
                         nn.Flatten(), nn.Dropout(0.2), nn.Linear(256, 1))


def load_cnn(path, frozen):
    import torch
    got = sha256_file(path)
    if got != frozen["cnn_sha256"]:
        raise SystemExit(f"{path}: sha256 {got}, the frozen CNN is {frozen['cnn_sha256']}")
    net = make_net()
    net.load_state_dict(torch.load(path, map_location="cpu", weights_only=True))
    net.eval()
    return net


def cnn_crop(im, box):
    import numpy as np
    from PIL import Image
    Wi, Hi = im.size
    x0, y0, x1, y1 = box
    c = im.crop((max(0, x0 - CNN_PAD), max(0, y0 - CNN_PAD), min(Wi, x1 + CNN_PAD), min(Hi, y1 + CNN_PAD)))
    w, h = c.size
    nw = max(1, min(CNN_W, round(w * CNN_H / max(h, 1))))
    c = c.resize((nw, CNN_H), Image.BILINEAR)
    arr = np.full((CNN_H, CNN_W), 255, np.uint8)
    arr[:, :nw] = np.asarray(c)[:, :nw]
    return arr


def cnn_probs(net, im, boxes):
    import numpy as np
    import torch
    if not boxes:
        return []
    X = np.stack([cnn_crop(im, b) for b in boxes]).astype(np.float32) / 255
    out = []
    with torch.no_grad():
        for s in range(0, len(X), 512):
            x = torch.from_numpy(X[s:s + 512]).unsqueeze(1)
            out += torch.sigmoid(net(x).squeeze(1)).tolist()
    return out


def contrast(im):
    """Amendment 2's page contrast: the greyscale scan scaled to 1,000 px wide (box filter), p95 - p5."""
    import numpy as np
    from PIL import Image
    w, h = im.size
    small = im.resize((1000, max(1, round(h * 1000 / w))), Image.BOX)
    p5, p95 = np.percentile(np.asarray(small, dtype=np.float64), [5, 95])
    return float(p95 - p5)


def faded_pages(contrasts):
    """Amendment 2's faded stratum: the lowest floor(N/4) pages by (contrast, doc id)."""
    ranked = sorted(contrasts, key=lambda d: (contrasts[d], d))
    return set(ranked[:len(ranked) // 4])


def score_page(im, lines, frozen, mc, me, net, chr_tessdata, eng_tessdata):
    """Per line: p_v2, p_cnn and p_line (None for a box with no p_chr). `lines` are boxes.jsonl's line rows
    (box, p_chr); `im` is the page's greyscale image."""
    import statistics
    med = statistics.median([l["box"][3] - l["box"][1] for l in lines]) if lines else 1.0
    live = [l for l in lines if l.get("p_chr") is not None]
    boxes = [l["box"] for l in live]
    reads = read_boxes(im, boxes, chr_tessdata, eng_tessdata)
    pc = cnn_probs(net, im, boxes)
    out = {}
    for l, r, c in zip(live, reads, pc):
        x = v2_features(r, (l["box"][3] - l["box"][1]) / med, l["p_chr"], mc, me)
        pv = v2_prob(x, frozen)
        out[id(l)] = (pv, c, ensemble_prob(pv, c, frozen))
    return [out.get(id(l), (None, None, None)) for l in lines]
