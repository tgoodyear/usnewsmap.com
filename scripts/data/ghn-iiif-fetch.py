#!/usr/bin/env python3
"""Fetch the page images of Georgia Historic Newspapers (GHN) titles that LoC
doesn't have, from UGA's IIIF image server (#1590).

GHN publishes IIIF Presentation JSON, which its robots.txt allows:
/lccn/<lccn>.json lists a title's issues and /lccn/<lccn>/<date>/ed-<n>.json an
issue's pages, each with an image service on iiif-ha.galib.uga.edu (which has no
robots.txt). Nothing else on GHN is requested: its page and OCR views are behind
a Cloudflare challenge, and robots.txt disallows /data/ and .pdf, .txt and .xml.

A page whose image is in a LoC batch (batch_dlc_*) is LoC's own scan, which we
have from Chronicling America. Every other page is downloaded at full size
(IIIF full/full/0/default.jpg) and checked to be a complete JPEG of the size the
manifest gives.

Requests go one at a time, --delay seconds apart, with an identifying
User-Agent. 429 and 5xx responses and dropped connections are retried with
backoff (Retry-After honoured); anything else, or a response that isn't JSON or
the expected image, stops the run. Run again, it resumes: title and issue JSON
are cached in OUT/manifests, and images already downloaded and valid are kept.

Writes OUT/images/<lccn>/<date>/ed-<n>/seq-<n>.jpg, OUT/inventory.jsonl (one
line per page, LoC's included) and OUT/summary.json.

Usage: ghn-iiif-fetch.py --out OUTDIR [--lccn LCCN ...] [--delay 3] [--inventory-only]
With no --lccn it fetches both Cherokee Phoenix titles, sn83020866 and sn83020874.
"""
import argparse
import hashlib
import http.client
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

UA = "usnewsmap-ghn-iiif-fetch (trevor@goodyeartechnical.com)"
GHN = "https://gahistoricnewspapers.galileo.usg.edu"
IIIF_HOSTS = {"iiif-ha.galib.uga.edu"}
CHEROKEE_PHOENIX = ["sn83020866", "sn83020874"]
RETRY_STATUS = {429, 500, 502, 503, 504}
MAX_WAIT = 900
LCCN_RE = re.compile(r"^[a-z]{1,3}\d{8,10}$")
ISSUE_RE = re.compile(r"^/lccn/([a-z]{1,3}\d{8,10})/(\d{4}-\d\d-\d\d)/ed-(\d+)\.json$")
PAGE_RE = re.compile(r"^/lccn/([a-z]{1,3}\d{8,10})/(\d{4}-\d\d-\d\d)/ed-(\d+)/seq-(\d+)/?$")


def retry_after(value):
    try:
        return min(MAX_WAIT, max(1, int(value)))
    except (TypeError, ValueError):
        return None


class Client:
    def __init__(self, delay, retries=5, sleep=time.sleep, opener=urllib.request.urlopen):
        self.delay, self.retries, self.sleep, self.opener = delay, retries, sleep, opener
        self.last = None
        self.requests = 0

    def get(self, url):
        for attempt in range(self.retries + 1):
            if self.last is not None:
                self.sleep(max(0.0, self.last + self.delay - time.monotonic()))
            self.last = time.monotonic()
            self.requests += 1
            try:
                req = urllib.request.Request(url, headers={"User-Agent": UA})
                with self.opener(req, timeout=120) as r:
                    return r.headers.get("Content-Type", ""), r.read()
            except urllib.error.HTTPError as e:
                if e.code not in RETRY_STATUS or attempt == self.retries:
                    raise
                wait = retry_after(e.headers.get("Retry-After")) or min(MAX_WAIT, 30 * 2**attempt)
                why = f"HTTP {e.code}"
            except (urllib.error.URLError, TimeoutError, ConnectionError, http.client.IncompleteRead) as e:
                if attempt == self.retries:
                    raise
                wait, why = min(MAX_WAIT, 30 * 2**attempt), str(e)
            print(f"retry {attempt + 1}/{self.retries} in {wait}s ({why}): {url}", file=sys.stderr, flush=True)
            self.sleep(wait)
        raise AssertionError("unreachable")


def write_atomic(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".part")
    tmp.write_bytes(data)
    os.replace(tmp, path)


def cached_json(client, url, path):
    if path.exists():
        return json.loads(path.read_text())
    ctype, body = client.get(url)
    if "json" not in ctype:
        raise RuntimeError(f"{url}: got {ctype or 'no content type'}, not JSON (a challenge page?); stopping")
    data = json.loads(body)
    write_atomic(path, body)
    return data


def issue_urls(collection):
    urls = []
    for m in collection.get("manifests", []):
        u = urllib.parse.urlparse(m["@id"])
        if f"{u.scheme}://{u.netloc}" != GHN or not ISSUE_RE.match(u.path):
            raise ValueError(f"unexpected issue manifest URL {m['@id']}")
        urls.append(m["@id"])
    return urls


def pages(manifest):
    out = []
    for sequence in manifest.get("sequences", []):
        for canvas in sequence.get("canvases", []):
            for image in canvas.get("images", []):
                res = image["resource"]
                service = res["service"]["@id"]
                s = urllib.parse.urlparse(service)
                if s.scheme != "https" or s.netloc not in IIIF_HOSTS:
                    raise ValueError(f"unexpected image service {service}")
                page_url = next((r["@id"] for r in image.get("rendering", []) if r.get("format") == "text/html"), "")
                m = PAGE_RE.match(urllib.parse.urlparse(page_url).path)
                if not m:
                    raise ValueError(f"{manifest.get('@id')}: no page URL for {service}")
                lccn, date, ed, seq = m.groups()
                batch = re.search(r"/(batch_[^/]+)/", urllib.parse.unquote(s.path))
                batch = batch[1] if batch else ""
                out.append({
                    "id": f"{lccn}/{date}/ed-{ed}/seq-{seq}",
                    "lccn": lccn,
                    "date": date,
                    "edition": int(ed),
                    "seq": int(seq),
                    "batch": batch,
                    "source": "loc" if batch.startswith("batch_dlc_") else "ghn",
                    "width": res["width"],
                    "height": res["height"],
                    "image_service": service,
                })
    return out


def jpeg_size(data):
    """(width, height) of a complete JPEG (SOI ... EOI), else None."""
    if not (data.startswith(b"\xff\xd8") and data.endswith(b"\xff\xd9")):
        return None
    i = 2
    while i + 9 <= len(data):
        if data[i] != 0xFF:
            return None
        marker = data[i + 1]
        if marker == 0xFF:
            i += 1
            continue
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            return int.from_bytes(data[i + 7:i + 9], "big"), int.from_bytes(data[i + 5:i + 7], "big")
        i += 2 + int.from_bytes(data[i + 2:i + 4], "big")
    return None


def fetch_image(client, page, out):
    """(path, data, downloaded): an existing valid image is kept."""
    path = out / "images" / f"{page['id']}.jpg"
    want = (page["width"], page["height"])
    if path.exists():
        data = path.read_bytes()
        if jpeg_size(data) == want:
            return path, data, False
    url = page["image_service"] + "/full/full/0/default.jpg"
    ctype, data = client.get(url)
    got = jpeg_size(data)
    if got != want:
        raise RuntimeError(f"{url}: got {ctype} of size {got}, expected a complete {want[0]}x{want[1]} JPEG")
    write_atomic(path, data)
    return path, data, True


def summarize(inventory, client, inventory_only):
    titles = {}
    for p in inventory:
        t = titles.setdefault(p["lccn"], {"issues": set(), "pages": {}, "issues_by_source": {}, "first": {}, "last": {}})
        src = p["source"]
        issue = p["id"].rsplit("/", 1)[0]
        t["issues"].add(issue)
        t["issues_by_source"].setdefault(src, set()).add(issue)
        t["pages"][src] = t["pages"].get(src, 0) + 1
        t["first"][src] = min(t["first"].get(src, p["date"]), p["date"])
        t["last"][src] = max(t["last"].get(src, p["date"]), p["date"])
    for t in titles.values():
        t["issues"] = len(t["issues"])
        t["issues_by_source"] = {k: len(v) for k, v in t["issues_by_source"].items()}
    files = [p for p in inventory if "sha256" in p]
    return {
        "pages": len(inventory),
        "titles": titles,
        "batches": sorted({p["batch"] for p in inventory}),
        "images": len(files),
        "image_bytes": sum(p["bytes"] for p in files),
        "inventory_only": inventory_only,
        "requests_this_run": client.requests,
    }


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", required=True)
    ap.add_argument("--lccn", action="append", help="title LCCN (repeatable); default: both Cherokee Phoenix titles")
    ap.add_argument("--delay", type=float, default=3.0, help="seconds between requests (default 3)")
    ap.add_argument("--inventory-only", action="store_true", help="list pages; download no images")
    a = ap.parse_args(argv)
    lccns = a.lccn or CHEROKEE_PHOENIX
    for lccn in lccns:
        if not LCCN_RE.match(lccn):
            ap.error(f"not an LCCN: {lccn}")
    if a.delay < 1:
        ap.error("--delay must be at least 1 second")
    out = Path(a.out)
    client = Client(a.delay)

    inventory = []
    for lccn in lccns:
        collection = cached_json(client, f"{GHN}/lccn/{lccn}.json", out / "manifests" / f"{lccn}.json")
        urls = issue_urls(collection)
        print(f"{lccn}: {len(urls)} issues", flush=True)
        for url in urls:
            rel = urllib.parse.urlparse(url).path.removeprefix("/lccn/")
            inventory.extend(pages(cached_json(client, url, out / "manifests" / rel)))

    todo = [p for p in inventory if p["source"] == "ghn"]
    print(f"{len(inventory)} pages, {len(todo)} not in a LoC batch", flush=True)
    if not a.inventory_only:
        for n, page in enumerate(todo, 1):
            path, data, new = fetch_image(client, page, out)
            page.update(file=str(path.relative_to(out)), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
            print(f"{n}/{len(todo)} {'fetched' if new else 'kept'} {page['id']} {len(data)} bytes", flush=True)

    write_atomic(out / "inventory.jsonl", "".join(json.dumps(p) + "\n" for p in inventory).encode())
    summary = summarize(inventory, client, a.inventory_only)
    write_atomic(out / "summary.json", (json.dumps(summary, indent=2) + "\n").encode())
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
