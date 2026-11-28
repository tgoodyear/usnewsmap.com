#!/usr/bin/env python3
"""Fetch LoC's IIIF JPEG of each page whose JP2 crashed Tesseract (status 136), once, for english_pages.py's
--fallback-jpegs (#2122 en-read; the production worker reads the same JPEG then, worker.rs, #454).

  fetch_jpegs.py --pages pages.tsv --out DIR [--interval 1.0] [--attempts 3]

--pages is TSV with a header: doc_id and iiif_base (the fetch index's `iiif_base`); the URL is iiif_base plus
`/full/full/0/default.jpg`, as the worker builds it. Requests are at least --interval seconds apart, one at a time.
A page already in DIR (<doc_id>.jpg with a receipt line) is not fetched again. A page gets at most --attempts tries;
a 404 or another error is retried only up to that number, never more. Files are kept, written atomically, and each
result (url, status, bytes, sha256, time, attempts) is a JSON line in DIR/receipts.jsonl.
"""
import argparse
import csv
import hashlib
import json
import os
import time
import urllib.error
import urllib.request

UA = "usnewsmap-check-build/0.1 (+https://usnewsmap.com)"


def url_for(iiif_base):
    return iiif_base.rstrip("/") + "/full/full/0/default.jpg"


class Pacer:
    def __init__(self, interval, clock=time.monotonic, sleep=time.sleep):
        self.interval, self.clock, self.sleep, self.last = interval, clock, sleep, None

    def wait(self):
        if self.last is not None:
            gap = self.interval - (self.clock() - self.last)
            if gap > 0:
                self.sleep(gap)
        self.last = self.clock()


def fetch_one(url, pacer, attempts, opener=urllib.request.urlopen):
    """(status, body or None, tries)."""
    status = None
    for k in range(1, attempts + 1):
        pacer.wait()
        try:
            with opener(urllib.request.Request(url, headers={"User-Agent": UA}), timeout=120) as r:
                return r.status, r.read(), k
        except urllib.error.HTTPError as e:
            status = e.code
        except (urllib.error.URLError, OSError) as e:
            status = "error: %s" % e
    return status, None, attempts


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pages", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--interval", type=float, default=1.0)
    ap.add_argument("--attempts", type=int, default=3)
    a = ap.parse_args(argv)
    if a.interval < 1.0:
        raise SystemExit("--interval below 1 second is not allowed")
    os.makedirs(a.out, exist_ok=True)
    receipts = os.path.join(a.out, "receipts.jsonl")
    done = set()
    if os.path.exists(receipts):
        done = {r["doc_id"] for r in map(json.loads, open(receipts)) if r.get("sha256")}
    pacer = Pacer(a.interval)
    rows = list(csv.DictReader(open(a.pages), delimiter="\t"))
    with open(receipts, "a") as log:
        for n, r in enumerate(rows, 1):
            d = r["doc_id"]
            path = os.path.join(a.out, d + ".jpg")
            if d in done and os.path.exists(path):
                continue
            url = url_for(r["iiif_base"])
            status, body, tries = fetch_one(url, pacer, a.attempts)
            rec = {"doc_id": d, "url": url, "status": status, "attempts": tries, "at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
            if body:
                tmp = path + ".tmp"
                with open(tmp, "wb") as f:
                    f.write(body)
                os.replace(tmp, path)
                rec.update(bytes=len(body), sha256=hashlib.sha256(body).hexdigest())
            log.write(json.dumps(rec, sort_keys=True) + "\n")
            log.flush()
            print(f"{n}/{len(rows)} {d} {status} tries={tries}", flush=True)


if __name__ == "__main__":
    main()
