#!/usr/bin/env python3
"""Period text from Wikisource for the character language models, from the official dumps.

    fetch_wikisource.py --langs ru,sr,sl,yi --out DIR [--max-chars 3000000]

For each language, streams https://dumps.wikimedia.org/<l>wikisource/latest/<l>wikisource-latest-pages-articles.xml.bz2
once, sequentially (one connection at a time, generic client name), keeping the raw bytes read in
DIR/raw/<l>-pages-articles.partial.xml.bz2. It stops once --max-chars characters of cleaned text are
kept, so for large wikis the raw file is the head of the dump, not the whole of it. Pages: main namespace
(ns 0) only, no redirects; wikitext stripped with mwparserfromhell (templates, tables, tags, refs
removed, link text kept; interlanguage and namespace link residue and lines under 60% letters
dropped); pages that are only transclusions of scanned pages (<pages .../>) yield
nothing. Writes DIR/<l>.txt and appends url, dump date, the dump's published SHA-1 (of the whole file),
the SHA-256 of the bytes read and of the text kept to DIR/wikisource.tsv.
"""
import argparse, bz2, hashlib, os, re, time, urllib.request
import xml.etree.ElementTree as ET
import mwparserfromhell

UA = {"User-Agent": "research-text-sampler/1.0 (dump reader)"}
BASE = "https://dumps.wikimedia.org"
ap = argparse.ArgumentParser()
ap.add_argument("--langs", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--max-chars", type=int, default=3_000_000)
a = ap.parse_args()
os.makedirs(os.path.join(a.out, "raw"), exist_ok=True)


def get(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=60) as r:
        return r.read().decode("utf-8", "replace")


def clean(wt):
    code = mwparserfromhell.parse(wt)
    for t in code.filter_templates(recursive=False):
        try:
            code.remove(t)
        except ValueError:
            pass
    for tag in code.filter_tags(recursive=False):
        if str(tag.tag).lower() in ("ref", "table", "pages", "references", "gallery", "math", "score", "noinclude", "includeonly"):
            try:
                code.remove(tag)
            except ValueError:
                pass
    s = code.strip_code(normalize=True, collapse=True)
    s = re.sub(r"\{\|.*?\|\}", " ", s, flags=re.S)  # leftover table markup
    s = re.sub(r"^[=*#:;|!].*$", "", s, flags=re.M)  # headings, lists, table rows
    s = re.sub(r"\[\[[^\]]*\]\]|__\w+__", " ", s)
    s = re.sub(r"(?:\b[a-z][a-z-]{1,11}:\s*){3,}", " ", s)  # interlanguage-link residue ("ar: az: bg: ...")
    s = re.sub(r"\S+:\S+", " ", s)  # namespace links ("Kategorie:...")
    lines = [l for l in s.split("\n") if l.strip() and sum(c.isalpha() for c in l) >= 0.6 * len(l.strip())]
    return re.sub(r"[ \t]+", " ", "\n".join(lines)).strip()


for lang in a.langs.split(","):
    wiki = f"{lang}wikisource"
    url = f"{BASE}/{wiki}/latest/{wiki}-latest-pages-articles.xml.bz2"
    rss = get(url + "-rss.xml")
    m = re.search(rf"/{wiki}/(\d{{8}})", rss)
    date = m.group(1) if m else "unknown"
    sha1 = ""
    try:
        for line in get(f"{BASE}/{wiki}/{date}/{wiki}-{date}-sha1sums.txt").splitlines():
            if line.endswith("pages-articles.xml.bz2") and "multistream" not in line:
                sha1 = line.split()[0]
    except Exception:
        pass
    raw_path = os.path.join(a.out, "raw", f"{lang}-pages-articles.partial.xml.bz2")
    raw = open(raw_path, "wb")
    h_raw = hashlib.sha256()
    dec = bz2.BZ2Decompressor()
    parser = ET.XMLPullParser(events=("end",))
    kept, pages = [], 0
    total = 0
    t0 = time.time()
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=120) as resp:
        done = False
        while not done:
            chunk = resp.read(1 << 20)
            if not chunk:
                break
            raw.write(chunk); h_raw.update(chunk)
            data = dec.decompress(chunk)
            while dec.eof and dec.unused_data:  # multi-stream bz2
                rest = dec.unused_data
                dec = bz2.BZ2Decompressor()
                data += dec.decompress(rest)
            parser.feed(data)
            for ev, el in parser.read_events():
                if not el.tag.endswith("}page"):
                    continue
                ns = el.find("{*}ns")
                if ns is not None and ns.text == "0" and el.find("{*}redirect") is None:
                    txt = el.find("{*}revision/{*}text")
                    if txt is not None and txt.text:
                        c = clean(txt.text)
                        if len(c) > 200:
                            kept.append(c); total += len(c); pages += 1
                el.clear()
                if total >= a.max_chars:
                    done = True
                    break
    raw.close()
    text = "\n".join(kept)[: a.max_chars]
    open(os.path.join(a.out, f"{lang}.txt"), "w", encoding="utf-8").write(text)
    h_txt = hashlib.sha256(text.encode("utf-8")).hexdigest()
    with open(os.path.join(a.out, "wikisource.tsv"), "a", encoding="utf-8") as log:  # one line per run; the last per language counts
        log.write("\t".join([lang, url, date, sha1, f"raw_read_sha256={h_raw.hexdigest()}", f"raw_bytes={os.path.getsize(raw_path)}",
                             f"text_sha256={h_txt}", f"chars={len(text)}", f"pages={pages}"]) + "\n")
    print(lang, date, pages, len(text), os.path.getsize(raw_path), f"{time.time() - t0:.0f}s", flush=True)
