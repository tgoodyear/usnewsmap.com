#!/usr/bin/env python3
"""Build the OCR quality audit's own word lists (ocr/ja-ocr/wordlists/<code>.txt, the 5,000 most frequent
words, most frequent first) for languages wordfreq has none for. See ocr/ja-ocr/wordlists/README.md.

    ocr/ja-ocr/tools/build-wordlists.py haw   # Hawaiian Corpus Project frequency list (CC0)
    ocr/ja-ocr/tools/build-wordlists.py yi    # Yiddish Wikipedia and Wikisource articles (CC BY-SA 4.0)
    ocr/ja-ocr/tools/build-wordlists.py chr [--pdf cntl.pdf --pdf cnts.pdf]   # Cherokee New Testament (CC0)
    ocr/ja-ocr/tools/build-wordlists.py chr-check --ocr DIR_OR_TAR ...      # Cherokee: how LoC's OCR reads
                                                                          # against the list
    ocr/ja-ocr/tools/build-wordlists.py ger-check --dta DTA.zip   # German: which 19th-century words the
                                                                # audit would count as damage
                                                                # (Deutsches Textarchiv)

Downloads go to a temporary directory and send DNT: 1. Words are folded the way quality.py folds a page's
words (case, combining marks; Hawaiian ʻokina and kahakō, which 19th-century papers didn't print;
Yiddish ligatures as their two letters; Cherokee small letters as the capitals). The Cherokee list needs
pdftotext (poppler).
"""

import argparse
import bz2
import collections
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unicodedata
import urllib.request
import xml.etree.ElementTree as ET

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
OUT = os.path.join(ROOT, "ocr", "ja-ocr", "wordlists")
TOP = 5000
HAW_URL = "https://raw.githubusercontent.com/dohliam/hawaiian-corpus/master/data/freqlist_haw.txt"
# The small-print edition; the large-print one (cntl.pdf) has the same text and is given with --pdf.
CHR_URL = "https://todaysnative.org/wp-content/uploads/2019/01/cnts.pdf"
YI_URLS = ["https://dumps.wikimedia.org/yiwiki/latest/yiwiki-latest-pages-articles.xml.bz2",
           "https://dumps.wikimedia.org/yiwikisource/latest/yiwikisource-latest-pages-articles.xml.bz2"]


def fetch(url: str, path: str) -> str:
    req = urllib.request.Request(url, headers={"DNT": "1", "User-Agent": "usnewsmap-wordlists"})
    with urllib.request.urlopen(req) as r, open(path, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    return path


def strip_marks(w: str) -> str:
    return "".join(c for c in unicodedata.normalize("NFD", w) if not unicodedata.combining(c))


def write(code: str, counts: collections.Counter) -> None:
    ranked = [w for w, _ in counts.most_common(TOP)]
    with open(os.path.join(OUT, f"{code}.txt"), "w", encoding="utf-8") as f:
        f.write("\n".join(ranked) + "\n")
    print(f"wrote {code}.txt: {len(ranked)} of {len(counts)} words; top: {' '.join(ranked[:20])}")


def hawaiian(tmp: str) -> None:
    # Native Hawaiian words are (consonant)-vowel syllables; loan letters too. This drops English noise.
    cv = re.compile(r"^(?:[hklmnpwbdfgrstvz]?[aeiou])+$")
    counts: collections.Counter = collections.Counter()
    for line in open(fetch(HAW_URL, os.path.join(tmp, "haw.txt")), encoding="utf-8"):
        n, _, w = line.rstrip("\n").partition("\t")
        w = strip_marks(w.casefold())
        for mark in "ʻ‘'’":
            w = w.replace(mark, "")
        if cv.match(w):
            counts[w] += int(n)
    write("haw", counts)


LIGATURES = str.maketrans({"װ": "וו", "ױ": "וי", "ײ": "יי"})
MARKUP = [(re.compile(r"\[\[[^\]|]*:[^\]]*\]\]"), " "), (re.compile(r"<ref[^>]*/>|<ref.*?</ref>", re.S), " "),
          (re.compile(r"\{\{[^{}]*\}\}"), " "), (re.compile(r"\[\[(?:[^|\]]*\|)?([^\]]*)\]\]"), r"\1"),
          (re.compile(r"https?://\S+"), " "), (re.compile(r"<[^>]+>"), " "), (re.compile(r"'''?|==+"), " ")]
HEBREW_WORD = re.compile(r"[֐-׿יִ-ﭏ]+")


def yiddish(tmp: str) -> None:
    counts: collections.Counter = collections.Counter()
    for url in YI_URLS:
        ns = None
        for _, el in ET.iterparse(bz2.open(fetch(url, os.path.join(tmp, os.path.basename(url))))):
            tag = el.tag.rsplit("}", 1)[-1]
            if tag == "ns":
                ns = el.text
            elif tag == "text" and el.text and ns == "0" and not el.text.lstrip().startswith("#"):  # articles
                text = el.text
                for rx, sub in MARKUP:
                    for _ in range(2):
                        text = rx.sub(sub, text)
                for tok in HEBREW_WORD.findall(text):
                    w = strip_marks(unicodedata.normalize("NFC", tok)).translate(LIGATURES)
                    w = w.replace("־", "").replace("׳", "").replace("״", "")
                    if w.isalpha():
                        counts[w] += 1
            if tag == "page":
                el.clear()
    write("yi", counts)


CHAPTER = re.compile(r"^ᎠᏯᏙᎸᎢ \d+$")  # "chapter 3"
CHEROKEE = re.compile("[\u13a0-\u13ff\uab70-\uabbf]")


def ja_ocr():
    sys.path.insert(0, os.path.join(ROOT, "ocr", "ja-ocr"))
    import damage_v2

    return damage_v2


def cherokee_text(pdf: str) -> str:
    """An edition's text, from its contents page's first book on, without the running heads (the book
    titles, as the contents page lists them) and chapter headings."""
    text = subprocess.run(["pdftotext", "-enc", "UTF-8", pdf, "-"], check=True, stdout=subprocess.PIPE,
                          encoding="utf-8").stdout
    lines = [line.strip() for line in text.replace("\f", "\n").split("\n")]
    contents = lines.index("ᏗᎪᏪᎵ")
    first = next(i for i, line in enumerate(lines) if CHAPTER.match(line))
    titles = {line for line in lines[contents + 1:first] if CHEROKEE.search(line)}
    return "\n".join(line for line in lines[first:] if line not in titles and not CHAPTER.match(line))


def cherokee(tmp: str, pdfs: list[str]) -> None:
    if shutil.which("pdftotext") is None:
        raise SystemExit("chr needs pdftotext (poppler-utils, or brew install poppler)")
    damage_v2 = ja_ocr()
    editions = []
    for pdf in pdfs or [fetch(CHR_URL, os.path.join(tmp, "cnts.pdf"))]:
        words = [w for w in damage_v2.text_words(cherokee_text(pdf))[1] if damage_v2.script(w) == "cherokee"]
        editions.append(collections.Counter(words))
        with open(pdf, "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
        print(f"{os.path.basename(pdf)}: sha256 {digest}, {len(words):,} words, {len(editions[-1]):,} different")
    # A word a line break or a verse number in the margin cuts leaves two pieces. Editions break their lines
    # in different places, so with both, a word counts as often as it does in the one it is fewest in.
    counts = collections.Counter({w: min(e[w] for e in editions) for w in editions[0]
                                  if all(w in e for e in editions)})
    top = sum(c for _, c in counts.most_common(TOP))
    print(f"the top {TOP:,} are {top / sum(counts.values()):.1%} of the words")
    write("chr", counts)


def ocr_texts(path: str):
    """The OCR text files under a directory, in a batch's tar file, or the file itself."""
    import tarfile

    if os.path.isdir(path):
        for d, _, files in sorted(os.walk(path)):
            for name in sorted(files):
                if name.endswith(".txt"):
                    with open(os.path.join(d, name), encoding="utf-8", errors="replace") as f:
                        yield f.read()
    elif tarfile.is_tarfile(path):
        with tarfile.open(path) as t:
            for m in t:
                if m.isfile() and m.name.endswith(".txt"):
                    yield t.extractfile(m).read().decode("utf-8", "replace")
    else:
        with open(path, encoding="utf-8", errors="replace") as f:
            yield f.read()


def cherokee_check(paths: list[str]) -> None:
    """How OCR of Cherokee pages reads against wordlists/chr.txt: the share of its words in the list; the
    letters it has where the list has another, from words one letter from one list word only; and the
    near-misses of the top 20 it has most (the damage the audit counts)."""
    damage_v2 = ja_ocr()
    m = damage_v2.Models().get("chr")
    counts: collections.Counter = collections.Counter()
    pages = 0
    for path in paths:
        for text in ocr_texts(path):
            pages += 1
            counts.update(w for w in damage_v2.text_words(text)[1] if damage_v2.script(w) == "cherokee")
    total = sum(counts.values())
    if not total:
        raise SystemExit(f"no Cherokee words in {pages} files")
    masks: dict[str, set] = collections.defaultdict(set)
    for w in m.real:
        for i in range(len(w)):
            masks[w[:i] + "\0" + w[i + 1:]].add(w)
    letters: collections.Counter = collections.Counter()
    for w, c in counts.items():
        if w not in m.real:
            hits = {r for i in range(len(w)) for r in masks.get(w[:i] + "\0" + w[i + 1:], ())}
            if len(hits) == 1:
                (r,) = hits
                i = next(i for i, (a, b) in enumerate(zip(r, w)) if a != b)
                letters[r[i], w[i]] += c
    known = sum(c for w, c in counts.items() if w in m.real)
    print(f"{total:,} Cherokee words in {pages:,} files; {known / total:.1%} in the list, "
          f"{letters.total() / total:.1%} one letter from one list word")
    print("list letter -> OCR letter, words")
    for (a, b), c in letters.most_common(25):
        print(f"{a} U+{ord(a):04X} -> {b} U+{ord(b):04X}\t{c}")
    print("near-misses of the top 20, the word, words")
    for w, c in sorted(((w, c) for w, c in counts.items() if w in m.near), key=lambda wc: -wc[1])[:25]:
        print(f"{w}\t{m.near[w]}\t{c}")


def german_check(zip_path: str) -> None:
    """The near-misses of German's top 20 the DTA's 1800-1899 texts use most: real old spellings would show
    up here (none do; the top ones are OCR's own errors, older spellings and other languages)."""
    import zipfile

    sys.path.insert(0, os.path.join(ROOT, "ocr", "ja-ocr"))
    import quality

    m = quality.Models().get("ger")
    counts: collections.Counter = collections.Counter()
    total = 0
    with zipfile.ZipFile(zip_path) as z:
        for name in z.namelist():
            if name.endswith(".txt"):
                words = quality.text_words(z.read(name).decode("utf-8", "replace").replace("ſ", "s"))[1]
                total += len(words)
                counts.update(w for w in words if w in m.near)
    print(f"{total:,} words")
    for w, c in counts.most_common(40):
        print(f"{w}\t{m.near[w]}\t{c}\t{c / total * 1e6:.1f} per million")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0].replace("\n", " "))
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("haw", help="Hawaiian")
    sub.add_parser("yi", help="Yiddish")
    c = sub.add_parser("chr", help="Cherokee")
    c.add_argument("--pdf", action="append", default=[],
                   help="an edition's PDF (cntl.pdf, cnts.pdf; give both); default: download cnts.pdf")
    k = sub.add_parser("chr-check", help="Cherokee: how OCR reads against the list")
    k.add_argument("--ocr", action="append", required=True,
                   help="OCR text: a directory of .txt files, a batch's tar file, or a file (repeatable)")
    g = sub.add_parser("ger-check", help="German: which 19th-century words would count as damage")
    g.add_argument("--dta", help="the Deutsches Textarchiv's 1800-1899 texts, as a zip (required)")
    # The positional form (`ger-check DTA.zip`) works for one release, with a
    # warning (#277).
    g.add_argument("legacy_dta", nargs="?", help=argparse.SUPPRESS)
    a = ap.parse_args()
    if a.cmd == "ger-check":
        if a.legacy_dta is not None:
            if a.dta is not None:
                g.error("give the zip once, as --dta")
            print("warning: a positional DTA.zip is deprecated; use --dta (#277)", file=sys.stderr)
            a.dta = a.legacy_dta
        if a.dta is None:
            g.error("--dta is required")
        german_check(a.dta)
        return
    if a.cmd == "chr-check":
        cherokee_check(a.ocr)
        return
    with tempfile.TemporaryDirectory() as tmp:
        if a.cmd == "chr":
            cherokee(tmp, a.pdf)
        else:
            (hawaiian if a.cmd == "haw" else yiddish)(tmp)


if __name__ == "__main__":
    main()
