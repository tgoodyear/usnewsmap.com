#!/usr/bin/env python3
"""Fetch the page images of Georgia Historic Newspapers (GHN) titles that LoC
doesn't have, from UGA's IIIF image server (#1590).

GHN publishes IIIF Presentation JSON, which its robots.txt allows:
/lccn/<lccn>.json lists a title's issues and /lccn/<lccn>/<date>/ed-<n>.json an
issue's pages, each with an image service on iiif-ha.galib.uga.edu (which has no
robots.txt). Some listed issues have no manifest (a 404; seen where an issue
lacks pages). For those, the issue's HTML page, /lccn/<lccn>/<date>/ed-<n>/
(also allowed), is read for its page links and image services, and each image's
size is taken from the image service's info.json. Nothing else on GHN is
requested: robots.txt disallows /data/ and .pdf, .txt and .xml, and a
Cloudflare challenge is never answered; one stops the run.

A page whose image is in a LoC batch (batch_dlc_*) is LoC's own scan, which we
have from Chronicling America. Every other page is downloaded at full size
(IIIF full/full/0/default.jpg) and checked to be a complete JPEG of the size the
manifest gives.

Requests go one at a time, --delay seconds apart, with an identifying
User-Agent. Redirects aren't followed. 429 and 5xx responses and dropped
connections are retried with backoff, never sooner than Retry-After asks (a
wait over 15 minutes stops the run instead); anything else, or a response that
isn't JSON or the expected image, stops the run. Run again, it resumes: title
and issue JSON, issue pages and info.json are cached in OUT/manifests, and
images already downloaded are checked again and kept if valid.

Writes OUT/images/<lccn>/<date>/ed-<n>/seq-<n>.jpg, OUT/inventory.jsonl (one
line per page, LoC's included) and OUT/summary.json.

Usage: ghn-iiif-fetch.py --out OUTDIR [--lccn LCCN ...] [--delay 3] [--inventory-only]
With no --lccn it fetches both Cherokee Phoenix titles, sn83020866 and sn83020874.
"""
import argparse
import email.utils
import hashlib
import html
import http.client
import json
import math
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
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
# A page thumbnail on an issue's HTML page: <a href="PAGE"><div><img src="SERVICE/full/240,/0/default.jpg"
THUMB_RE = re.compile(r'<a href="([^"]+)">\s*<div>\s*<img src="([^"]+?)/full/[^"/]+/0/default\.jpg"')


def retry_after(value, now=None):
    """Seconds a Retry-After header (delta-seconds or HTTP-date) asks for, else None."""
    value = (value or "").strip()
    if value.isdigit():
        return int(value)
    try:
        when = email.utils.parsedate_to_datetime(value)
    except (TypeError, ValueError):
        return None
    if when.tzinfo is None:
        when = when.replace(tzinfo=timezone.utc)
    return max(0, math.ceil((when - (now or datetime.now(timezone.utc))).total_seconds()))


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Returning None makes urllib raise the 3xx as an HTTPError, which stops the run.
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, delay, retries=5, sleep=time.sleep, opener=urllib.request.build_opener(NoRedirect).open):
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
                asked = retry_after(e.headers.get("Retry-After") if e.headers else None)
                e.close()
                if asked is not None and asked > MAX_WAIT:
                    raise RuntimeError(f"{url}: HTTP {e.code} asks to wait {asked}s, over {MAX_WAIT}s; stopping") from e
                wait = max(asked or 0, min(MAX_WAIT, 30 * 2**attempt))
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


def page_record(page_url, service, width, height, listing):
    s = urllib.parse.urlparse(service)
    if s.scheme != "https" or s.netloc not in IIIF_HOSTS:
        raise ValueError(f"unexpected image service {service}")
    m = PAGE_RE.match(urllib.parse.urlparse(page_url).path)
    if not m:
        raise ValueError(f"no page URL for {service} (got {page_url!r})")
    lccn, date, ed, seq = m.groups()
    batch = re.search(r"/(batch_[^/]+)/", urllib.parse.unquote(s.path))
    batch = batch[1] if batch else ""
    return {
        "id": f"{lccn}/{date}/ed-{ed}/seq-{seq}",
        "lccn": lccn,
        "date": date,
        "edition": int(ed),
        "seq": int(seq),
        "batch": batch,
        "source": "loc" if batch.startswith("batch_dlc_") else "ghn",
        "width": width,
        "height": height,
        "image_service": service,
        "listing": listing,
    }


def pages(manifest):
    out = []
    for sequence in manifest.get("sequences", []):
        for canvas in sequence.get("canvases", []):
            for image in canvas.get("images", []):
                res = image["resource"]
                page_url = next((r["@id"] for r in image.get("rendering", []) if r.get("format") == "text/html"), "")
                out.append(page_record(page_url, res["service"]["@id"], res["width"], res["height"], "manifest"))
    return out


def html_pages(client, url, path):
    """Pages listed on an issue's HTML page; image sizes come from IIIF info.json."""
    body = path.read_bytes() if path.exists() else None
    if body is None:
        ctype, body = client.get(url)
        if "html" not in ctype:
            raise RuntimeError(f"{url}: got {ctype or 'no content type'}, not HTML; stopping")
    issue = urllib.parse.urlparse(url).path
    listed = {}
    for href, service in THUMB_RE.findall(body.decode("utf-8", "replace")):
        href, service = html.unescape(href), html.unescape(service)
        if href.startswith(issue):
            listed.setdefault(href, service)
    if not listed:
        raise RuntimeError(f"{url}: lists no pages (a challenge page?); stopping")
    out = [page_record(href, service, None, None, "html") for href, service in listed.items()]
    if not path.exists():
        write_atomic(path, body)
    for page in out:
        info = cached_json(client, page["image_service"] + "/info.json", path.with_suffix("") / f"seq-{page['seq']}.info.json")
        page.update(width=int(info["width"]), height=int(info["height"]))
    return out


def issue_pages(client, url, out):
    manifest = out / "manifests" / urllib.parse.urlparse(url).path.removeprefix("/lccn/")
    listing = manifest.with_suffix(".html")
    if not listing.exists():
        try:
            return pages(cached_json(client, url, manifest))
        except urllib.error.HTTPError as e:
            if e.code != 404:
                raise
            e.close()
            print(f"no manifest (404), reading the issue page: {url}", flush=True)
    return html_pages(client, url.removesuffix(".json") + "/", listing)


def jpeg_size(data):
    """(width, height) of a complete JPEG, else None.

    Complete: SOI, well-formed segments, a frame header, at least one scan with
    entropy-coded data, and EOI as the last two bytes.
    """
    n = len(data)
    if not (data.startswith(b"\xff\xd8") and data.endswith(b"\xff\xd9")):
        return None
    i, size, scans = 2, None, 0
    while i + 2 <= n:
        if data[i] != 0xFF:
            return None
        marker = data[i + 1]
        if marker == 0xFF:  # fill byte
            i += 1
            continue
        if marker == 0xD9:  # EOI
            return size if scans and i + 2 == n else None
        if 0xD0 <= marker <= 0xD7 or marker == 0x01:  # no length
            i += 2
            continue
        if i + 4 > n:
            return None
        end = i + 2 + int.from_bytes(data[i + 2:i + 4], "big")
        if end - i < 4 or end > n:
            return None
        if 0xC0 <= marker <= 0xCF and marker not in (0xC4, 0xC8, 0xCC):
            if end - i < 10:
                return None
            size = int.from_bytes(data[i + 7:i + 9], "big"), int.from_bytes(data[i + 5:i + 7], "big")
        i = end
        if marker == 0xDA:  # SOS: entropy-coded data runs to the next marker other than RSTn
            if size is None:
                return None
            j = i
            while True:
                j = data.find(b"\xff", j)
                if j < 0 or j + 1 >= n:
                    return None
                if data[j + 1] == 0x00 or 0xD0 <= data[j + 1] <= 0xD7:
                    j += 2
                    continue
                break
            if j == i:
                return None
            scans += 1
            i = j
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
        "issues_without_manifest": sorted({p["id"].rsplit("/", 1)[0] for p in inventory if p["listing"] == "html"}),
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
            inventory.extend(issue_pages(client, url, out))

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
