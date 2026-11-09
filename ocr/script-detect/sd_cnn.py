"""Glyph evidence for the script router: a small CNN on word crops (#1666).

Inline Latin is rare on syllabary lines, but the same scans carry thousands of words in the same
roman type in their English columns. A model that sees only a word's glyphs can learn Latin from
those, and syllabary from the words the rules label syllabary, with no inline examples. Its three
class probabilities (Latin, other, syllabary) become inputs of the router, which keeps the context
and the readings.

Each crop is cut tight to the word's ink and scaled to CROP_H pixels high, so the CNN judges shapes
only; size relative to the line stays with the router's own measures. Capital-only English words are
left out of the Latin examples: in the 1828 type they look like the syllabary letters cut from small
capitals, and the router's readings are what tell those apart.

Cross-fitting: the pages are split in two by a hash of their names, a CNN is trained on each half,
and each word is scored by the CNN that did not see its page (test pages by the mean of both). So
every score the router trains or is measured on is out of sample.
"""
from __future__ import annotations

import zlib

import numpy as np

CROP_H, CROP_W = 32, 128
CLASSES = ("l", "o", "s")


def fold(page):
    return zlib.crc32((page + "#cnn").encode()) % 2


def word_crop(gray, box):
    """The word's ink inside its box, scaled to CROP_H high (CROP_W at most wide), ink 1.0 on 0.0,
    padded on the right. `gray` is the page or line image, `box` the word in its pixels."""
    from PIL import Image
    import sd_features as F
    x0, y0, x1, y1 = (int(v) for v in box)
    H, W = gray.shape
    x0, y0, x1, y1 = max(0, x0 - 2), max(0, y0 - 2), min(W, x1 + 2), min(H, y1 + 2)
    out = np.zeros((CROP_H, CROP_W), dtype=np.float32)
    if x1 - x0 < 2 or y1 - y0 < 2:
        return out
    g = gray[y0:y1, x0:x1]
    ink = F.binarize(g)
    rows, cols = np.nonzero(ink.any(axis=1))[0], np.nonzero(ink.any(axis=0))[0]
    if len(rows) == 0:
        return out
    g = g[rows[0]:rows[-1] + 1, cols[0]:cols[-1] + 1].astype(np.float32)
    lo, hi = np.percentile(g, 2), np.percentile(g, 98)
    g = np.clip((hi - g) / max(1.0, hi - lo), 0, 1)  # ink 1, paper 0
    h, w = g.shape
    s = CROP_H / h
    if w * s > CROP_W:
        s = CROP_W / w
    nh, nw = max(1, round(h * s)), max(1, round(w * s))
    im = Image.fromarray((g * 255).astype(np.uint8)).resize((nw, nh), Image.BILINEAR)
    out[:nh, :nw] = np.asarray(im, dtype=np.float32) / 255.0
    return out


def net():
    import torch.nn as nn

    def block(i, o):
        return nn.Sequential(nn.Conv2d(i, o, 3, padding=1), nn.BatchNorm2d(o), nn.ReLU(), nn.MaxPool2d(2))
    return nn.Sequential(block(1, 32), block(32, 64), block(64, 96), block(96, 128),
                         nn.AdaptiveAvgPool2d((1, 4)), nn.Flatten(), nn.Dropout(0.2), nn.Linear(512, len(CLASSES)))


def train(X, y, epochs=8, seed=0, threads=3, log=print):
    """X: N x CROP_H x CROP_W float crops, y: class indices. Classes are sampled equally."""
    import torch
    torch.manual_seed(seed)
    torch.set_num_threads(threads)
    rng = np.random.default_rng(seed)
    m = net()
    opt = torch.optim.AdamW(m.parameters(), lr=2e-3, weight_decay=1e-4)
    by = [np.nonzero(y == c)[0] for c in range(len(CLASSES))]
    per_epoch = 3 * min(20000, max(len(b) for b in by))
    steps = epochs * per_epoch // 256
    sched = torch.optim.lr_scheduler.OneCycleLR(opt, max_lr=2e-3, total_steps=steps)
    m.train()
    for step in range(steps):
        idx = np.concatenate([rng.choice(b, 256 // len(CLASSES) + 1) for b in by if len(b)])[:256]
        xb = X[idx].copy()
        # Augment: shift by up to 3 pixels, vary contrast, add a little noise.
        for i in range(len(xb)):
            dx, dy = rng.integers(-3, 4), rng.integers(-2, 3)
            xb[i] = np.roll(np.roll(xb[i], dx, axis=1), dy, axis=0) * rng.uniform(0.6, 1.0)
        xb += rng.normal(0, 0.05, xb.shape).astype(np.float32)
        loss = torch.nn.functional.cross_entropy(m(torch.from_numpy(xb[:, None])), torch.from_numpy(y[idx]).long())
        opt.zero_grad()
        loss.backward()
        opt.step()
        sched.step()
        if step % 200 == 0:
            log(f"step {step}/{steps} loss {loss.item():.3f}")
    m.eval()
    return m


def predict(m, X, batch=1024):
    import torch
    out = []
    with torch.no_grad():
        for i in range(0, len(X), batch):
            out.append(torch.softmax(m(torch.from_numpy(X[i:i + batch][:, None])), dim=1).numpy())
    return np.concatenate(out) if out else np.zeros((0, len(CLASSES)), dtype=np.float32)
