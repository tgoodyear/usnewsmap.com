#!/usr/bin/env python3
"""Downloads public-domain Project Gutenberg books per language from a PG mirror, for the
character language models. Keeps each raw file, strips nothing (lm.py strips the PG header and
footer), and records id, title, url and sha256 in gutenberg/books.tsv.

    fetch_gutenberg.py --catalog pg_catalog.csv --out DIR --langs cs,pl,ru,sr --max-books 10 --max-bytes 4000000
"""
import argparse, csv, hashlib, os, time, urllib.request

MIRROR = "https://aleph.pglaf.org"

def path_for(n: str) -> str:
    d = "/".join(n[:-1]) if len(n) > 1 else "0"
    return f"{MIRROR}/{d}/{n}/"

def fetch(url: str):
    req = urllib.request.Request(url, headers={"User-Agent": "research-lm-fetch/1"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.read()
    except Exception:
        return None

ap = argparse.ArgumentParser()
ap.add_argument("--catalog", required=True)
ap.add_argument("--out", required=True)
ap.add_argument("--langs", required=True)
ap.add_argument("--max-books", type=int, default=10)
ap.add_argument("--max-bytes", type=int, default=4_000_000)
ap.add_argument("--gap-secs", type=float, default=2.0)
a = ap.parse_args()
rows = [r for r in csv.DictReader(open(a.catalog, encoding="utf-8")) if r["Type"] == "Text"]
os.makedirs(a.out, exist_ok=True)
log = open(os.path.join(a.out, "books.tsv"), "a", encoding="utf-8")
for lang in a.langs.split(","):
    books = sorted((r for r in rows if r["Language"] == lang), key=lambda r: int(r["Text#"]))
    got, size = 0, 0
    for r in books:
        if got >= a.max_books or size >= a.max_bytes:
            break
        n = r["Text#"]
        dest = os.path.join(a.out, lang, f"{n}.txt")
        if os.path.exists(dest):
            got += 1; size += os.path.getsize(dest); continue
        for suffix in (f"{n}-0.txt", f"{n}-8.txt", f"{n}.txt"):
            url = path_for(n) + suffix
            body = fetch(url)
            time.sleep(a.gap_secs)
            if body:
                os.makedirs(os.path.dirname(dest), exist_ok=True)
                open(dest, "wb").write(body)
                log.write("\t".join([lang, n, r["Title"].replace("\t", " ").replace("\n", " "), r["Authors"].replace("\t", " ").replace("\n", " "), url, hashlib.sha256(body).hexdigest()]) + "\n")
                log.flush()
                got += 1; size += len(body)
                break
    print(lang, got, size, flush=True)
