#!/usr/bin/env python3
"""Real Cherokee Phoenix training lines with trusted text (#1621).

Each run of consecutive trusted words on a line of LoC's ALTO (at least --min-words of them) becomes a
training line, cut from the first word's box to the last's. A trusted word is a syllabary word that is a form of the 1860 New Testament as LoC read it, or
one the first type's misreadings, undone, turn into exactly one such form (Ꭺ read for Ꮩ;
a word-initial Ꮹ/Ꮳ/Ᏻ/Ꮐ read for Ꮎ). Punctuation LoC read around a word is kept, and so is a token of numerals and punctuation alone
(a verse number such as `19.`, a lone quote or dash) inside or at the edge of a run, as LoC read
it, so the model learns these marks are not letters; a run needs at least one trusted word. The line is cut from
the page scan and written as `<name>.png` and `<name>.gt.txt` (tesstrain's layout). Pages of the frozen
test set (`--exclude-manifest`, its page list) are never used.

    chr_real_lines.py --alto-dir loc --images images --vocab chr-attested.txt \
        --exclude-manifest testset/v1/manifest.json --out real
"""
import argparse, glob, itertools, json, os, re, sys
import xml.etree.ElementTree as ET
from PIL import Image

SYL = re.compile("[Ꭰ-Ᏽ]")
PUNCT = ".,;:!?\"'“”‘’()[]-—"
# Only marks the start model can write: its character set has no 4, ? or straight quotes.
NEUTRAL = re.compile(r"[0-35-9.,;:!()\-“”’…]+")
UNWRITABLE = re.compile("[?\"'‘4]")


def make_fix(vocab):
    def fix(w):
        at = [i for i, c in enumerate(w) if c == "Ꭺ"]
        hits = []
        if at and len(at) <= 6:
            for k in range(1, len(at) + 1):
                for sub in itertools.combinations(at, k):
                    l = list(w)
                    for i in sub:
                        l[i] = "Ꮩ"
                    if "".join(l) in vocab:
                        hits.append("".join(l))
        if not hits and w[0] in "ᏩᏣᏳᏀ" and "Ꮎ" + w[1:] in vocab:
            hits.append("Ꮎ" + w[1:])
        return hits[0] if len(hits) == 1 else None
    return fix


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--alto-dir", required=True); ap.add_argument("--images", required=True)
    ap.add_argument("--vocab", required=True); ap.add_argument("--exclude-manifest", required=True)
    ap.add_argument("--out", required=True); ap.add_argument("--min-words", type=int, default=1, help="fewest trusted words in a run")
    a = ap.parse_args()
    Image.MAX_IMAGE_PIXELS = None
    vocab = {l.strip() for l in open(a.vocab, encoding="utf-8") if l.strip()}
    fix = make_fix(vocab)
    m = json.load(open(a.exclude_manifest, encoding="utf-8"))
    exclude = {p for s in m["sets"].values() if isinstance(s, dict) and isinstance(s.get("pages"), list) for p in s["pages"]}
    os.makedirs(a.out, exist_ok=True)
    stats = {"pages": 0, "lines": 0, "kept": 0, "fixed_words": 0, "excluded_pages": 0}
    idx = open(os.path.join(a.out, "lines.jsonl"), "w", encoding="utf-8")
    for p in sorted(glob.glob(os.path.join(a.alto_dir, "sn*/*/*/*/ed-*/seq-*/ocr.xml"))):
        lccn, y, mo, d, ed, seq = p.split(os.sep)[-7:-1]
        slug = f"{lccn}/{y}-{mo}-{d}/{ed}/{seq}"
        img = os.path.join(a.images, slug.replace("/", "_") + ".jpg")
        if slug in exclude:
            stats["excluded_pages"] += 1
            continue
        if not os.path.exists(img):
            continue
        root = ET.parse(p).getroot()
        tag = lambda e: e.tag.rsplit("}", 1)[-1]
        width = next(int(float(e.get("WIDTH"))) for e in root.iter() if tag(e) == "Page")
        page = None
        stats["pages"] += 1
        for line in (e for e in root.iter() if tag(e) == "TextLine"):
            words = [s.get("CONTENT", "") for s in line if tag(s) == "String"]
            if not any(SYL.search(w) for w in words):
                continue
            stats["lines"] += 1
            # Runs of consecutive trusted words; a run ends at any word we don't trust.
            strings = [s for s in line if tag(s) == "String"]
            runs, cur = [], []
            for s in strings:
                w = s.get("CONTENT", "")
                core = w.strip(PUNCT)
                label = None
                # The start model can't write ? or straight quotes: a word carrying one ends the run.
                if core and all(SYL.match(c) for c in core) and not UNWRITABLE.search(w):
                    label = w if core in vocab else (w.replace(core, f, 1) if (f := fix(core)) else None)
                if label is None and NEUTRAL.fullmatch(w):
                    cur.append((s, w, False))
                    continue
                if label is None:
                    if cur:
                        runs.append(cur)
                    cur = []
                else:
                    cur.append((s, label, label != w))
            if cur:
                runs.append(cur)
            for run in runs:
                if sum(1 for _, lbl, _ in run if SYL.search(lbl)) < a.min_words:
                    continue
                if page is None:
                    page = Image.open(img).convert("L")
                    scale = page.width / width
                x0 = float(run[0][0].get("HPOS"))
                x1 = float(run[-1][0].get("HPOS")) + float(run[-1][0].get("WIDTH"))
                vy, h_ = float(line.get("VPOS")), float(line.get("HEIGHT"))
                pad = max(4, int(0.2 * h_ * scale))
                box = (max(0, int(x0 * scale) - pad), max(0, int(vy * scale) - pad),
                       min(page.width, int(x1 * scale) + pad), min(page.height, int((vy + h_) * scale) + pad))
                name = f"{slug.replace('/', '_')}_{line.get('ID')}_{run[0][0].get('ID')}"
                page.crop(box).save(os.path.join(a.out, name + ".png"))
                text = " ".join(lbl for _, lbl, _ in run)
                fixed = sum(1 for *_, f in run if f)
                open(os.path.join(a.out, name + ".gt.txt"), "w", encoding="utf-8").write(text + "\n")
                idx.write(json.dumps({"name": name, "page": slug, "text": text, "words": len(run), "fixed_words": fixed}, ensure_ascii=False) + "\n")
                stats["kept"] += 1
                stats["fixed_words"] += fixed
    json.dump(stats, open(os.path.join(a.out, "summary.json"), "w"), indent=1)
    print(json.dumps(stats))


if __name__ == "__main__":
    sys.exit(main())
