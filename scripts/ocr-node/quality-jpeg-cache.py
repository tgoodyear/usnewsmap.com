#!/usr/bin/env python3
"""Convert quality-wave scan masters to the runner's page JPEG once, on-prem (#850).

The quality wave runner turns each archived page into the worker's page.jpg with
one recipe (grayscale, JPEG q75, JPEG2000 over 4000 px decoded at half
resolution). Decoding JPEG2000 is most of its CPU, and its Container Apps job
has 2 vCPUs. This does the same conversion in the scan-fetch container (CT 211),
which holds the masters, and puts each result in the BOINC hand-off container as

    quality-jpeg/<recipe>/<blob name>.jpg

with the source's sha256, the recipe, the Pillow version and the size as blob
metadata. The runner uses one when it is there and converts the master itself
otherwise, so a page this hasn't reached yet costs nothing. Pages the recipe
leaves alone (grayscale JPEGs under 40 Mpx and 20 MB) aren't uploaded: the
runner sends their bytes as they are.

    quality-jpeg-cache.py --account <exchange account> --list pages.txt \
        [--scans-root /tank/ocr/scans | --scan-server http://<host>:8380] \
        [--state /var/lib/usnm-quality-jpeg-cache] [--workers 2] [--limit N] [--what-if]

With --scan-server it runs on another LAN host instead, reading each master
and its sidecar from the scan server (GET /scans/keep/...), which must list
that host in its --allow.

pages.txt has one blob name per line (keep/<batch>/<lccn>/<date>/<ed>/<seq>.jp2),
in the order to convert. Each master's sha256 must match its fetcher sidecar
(<scan>.json). An existing blob is left as it is only when its size and all
provenance metadata match; a mismatch stops the run. Done pages are listed
in <state>/done-<recipe>.txt and skipped next time; a master that can't be read or
doesn't match its sidecar is reported and left for the next run. Signs in as the container's Arc
machine identity, which has the hand-off writer role (read and write, no
delete) and nothing else there.
"""

import argparse
import base64
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ProcessPoolExecutor

RECIPE = "l-q75-r2"
PILLOW_VERSION = "13.0.0"
CONTAINER = "boinc-handoff"
PREFIX = f"quality-jpeg/{RECIPE}/"
IMDS = "http://localhost:40342/metadata/identity/oauth2/token"
STORAGE = "https://storage.azure.com/"
# usnm_store::validate_path: /-separated segments of [A-Za-z0-9._-] starting with a letter or digit.
SEGMENTS = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*$")


class Refused(Exception):
    pass


def validate_blob_name(name):
    if not SEGMENTS.match(name) or ".." in name:
        raise Refused(f"{name!r}: not a blob name the hand-off job can take")
    if not name.startswith("keep/") or not name.endswith((".jp2", ".jpg")):
        raise Refused(f"{name!r}: not a kept .jp2 or .jpg scan")
    return name


def convert(raw, is_jp2):
    """The runner's recipe (run_wave9.py dl_and_prepare_page), verbatim.

    Returns (jpeg bytes, width, height), or None when the runner sends the bytes as they are.
    """
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    if is_jp2 or len(raw) > 20_000_000:
        with Image.open(io.BytesIO(raw)) as im:
            if getattr(im, "format", "") == "JPEG2000" and max(im.size) > 4000:
                im.reduce = 1
            im_l = im.convert("L")
            out = io.BytesIO()
            im_l.save(out, "JPEG", quality=75)
            return out.getvalue(), im_l.size[0], im_l.size[1]
    with Image.open(io.BytesIO(raw)) as im:
        w, h = im.size
        if w * h > 40_000_000 or im.mode != "L":
            im_l = im.convert("L")
            out = io.BytesIO()
            im_l.save(out, "JPEG", quality=75)
            return out.getvalue(), im_l.size[0], im_l.size[1]
    return None


def _get(url, opener):
    try:
        return opener(urllib.request.Request(url), timeout=120).read()
    except urllib.error.HTTPError as e:
        # 404: not fetched (or not there any more); 403: this host isn't on the server's --allow.
        raise Refused(f"{url}: HTTP {e.code}") from e


def read_master(scans_root, name, opener=urllib.request.urlopen):
    """The master's bytes and sha256, from a local scan root or a scan server URL."""
    if scans_root.startswith(("http://", "https://")):
        url = f"{scans_root.rstrip('/')}/scans/{name}"
        want = json.loads(_get(f"{url}.json", opener)).get("sha256")
        raw = _get(url, opener)
        got = hashlib.sha256(raw).hexdigest()
        if not want or got != want:
            raise Refused(f"{url}: sha256 {got} doesn't match its sidecar ({want})")
        return raw, got
    path = os.path.join(scans_root, name)
    if os.path.islink(path) or not os.path.isfile(path):
        raise Refused(f"{path}: not a regular file")
    with open(f"{path}.json") as f:
        want = json.load(f).get("sha256")
    with open(path, "rb") as f:
        raw = f.read()
    got = hashlib.sha256(raw).hexdigest()
    if not want or got != want:
        raise Refused(f"{path}: sha256 {got} doesn't match its sidecar ({want})")
    return raw, got


class ArcToken:
    """A storage token for the Arc machine identity (the himds challenge flow)."""

    def __init__(self, imds=IMDS, opener=urllib.request.urlopen):
        self.imds, self.opener = imds, opener
        self.token, self.expires = None, 0

    def get(self):
        if self.token and time.time() < self.expires - 300:
            return self.token
        url = f"{self.imds}?api-version=2020-06-01&resource={urllib.parse.quote(STORAGE, safe='')}"
        hdr = {"Metadata": "true"}
        try:
            body = self.opener(urllib.request.Request(url, headers=hdr), timeout=30).read()
        except urllib.error.HTTPError as e:
            challenge = e.headers.get("WWW-Authenticate", "")
            if e.code != 401 or "realm=" not in challenge:
                raise
            keyfile = challenge.split("realm=", 1)[1].strip()
            if not keyfile.startswith("/var/opt/azcmagent/tokens/") or ".." in keyfile:
                raise Refused(f"unexpected Arc challenge key file {keyfile!r}")
            with open(keyfile) as f:
                hdr["Authorization"] = f"Basic {f.read().strip()}"
            body = self.opener(urllib.request.Request(url, headers=hdr), timeout=30).read()
        doc = json.loads(body)
        self.token, self.expires = doc["access_token"], int(doc.get("expires_on", 0))
        return self.token


class Blobs:
    def __init__(self, base, token, opener=urllib.request.urlopen):
        self.base, self.token, self.opener = base.rstrip("/"), token, opener

    def _req(self, method, blob, data=None, headers=None):
        h = {"Authorization": f"Bearer {self.token.get()}", "x-ms-version": "2021-08-06"}
        h.update(headers or {})
        url = f"{self.base}/{urllib.parse.quote(blob)}"
        return self.opener(urllib.request.Request(url, data=data, method=method, headers=h), timeout=120)

    def put_once(self, blob, data, meta):
        """Upload once; refuse an existing blob unless size and provenance match."""
        h = {"x-ms-blob-type": "BlockBlob", "Content-Type": "image/jpeg", "If-None-Match": "*"}
        h.update({f"x-ms-meta-{k}": str(v) for k, v in meta.items()})
        try:
            self._req("PUT", blob, data, h).read()
            return True
        except urllib.error.HTTPError as e:
            if e.code not in (409, 412):
                raise
        existing = {k.lower(): v for k, v in self._req("HEAD", blob).headers.items()}
        size = int(existing.get("content-length", -1))
        if size != len(data):
            raise Refused(f"{CONTAINER}/{blob} is already there with another size ({size} bytes)")
        for key, value in meta.items():
            if existing.get(f"x-ms-meta-{key}".lower()) != str(value):
                raise Refused(f"{CONTAINER}/{blob} is already there with missing or different {key} metadata")
        return False


_blobs = None


def _init(account, base):
    global _blobs
    _blobs = Blobs(base or f"https://{account}.blob.core.windows.net/{CONTAINER}", ArcToken())


def process(scans_root, name, what_if=False, blobs=None):
    """Convert and upload one page; returns (name, outcome, seconds)."""
    t0 = time.monotonic()
    try:
        raw, sha = read_master(scans_root, name)
    except (Refused, OSError, ValueError) as e:
        # URLError is an OSError: an unreachable scan server skips the page for the next run.
        return name, f"skipped: {e}", time.monotonic() - t0
    done = convert(raw, name.endswith(".jp2"))
    if done is None:
        return name, "as-is", time.monotonic() - t0
    data, w, h = done
    blob = f"{PREFIX}{name}.jpg"
    if what_if:
        return name, f"would upload {blob} ({len(data)} bytes)", time.monotonic() - t0
    from PIL import __version__ as pil

    meta = {"source_sha256": sha, "recipe": RECIPE, "pillow": pil, "width": w, "height": h,
            "sha256": hashlib.sha256(data).hexdigest()}
    for attempt in range(5):
        try:
            up = (blobs or _blobs).put_once(blob, data, meta)
            break
        except (urllib.error.URLError, OSError) as e:
            # An HTTPError holds its response stream and can't go back to the parent
            # process, so stop the run with the status and Azure's error code instead.
            if isinstance(e, urllib.error.HTTPError) and e.code < 500 and e.code != 429:
                raise Refused(f"{CONTAINER}/{blob}: HTTP {e.code} {e.headers.get('x-ms-error-code', '')}") from None
            if attempt == 4:
                raise Refused(f"{CONTAINER}/{blob}: {e}") from None
            time.sleep(2 ** attempt * 5)
    return name, "uploaded" if up else "already-there", time.monotonic() - t0


def _work(args):
    return process(*args)


def keyed_todo(names, ledger, state):
    """Replay only new completion bytes; membership stays in a recipe-specific SQLite index."""
    spec = importlib.util.spec_from_file_location(
        "jpeg_refresh", Path(__file__).with_name("quality-jpeg-refresh.py"))
    refresh = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(refresh)
    work = Path(state) / f"keyed-{RECIPE}"
    with refresh.locked(work):
        db = refresh.connect(work, RECIPE, 1)
        try:
            while True:
                count, caught_up = refresh.consume(
                    db, ledger, 10000, lambda raw: refresh.ingest_done(db, raw))
                if caught_up:
                    break
                if not count:
                    raise Refused("completion ledger ends in a partial record; retry after it is complete")
            return [name for name in names if not db.execute(
                "SELECT 1 FROM done WHERE path=?", (name,)).fetchone()]
        finally:
            db.close()


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("--account", required=True)
    p.add_argument("--list", required=True)
    src = p.add_mutually_exclusive_group()
    src.add_argument("--scans-root", default="/tank/ocr/scans")
    src.add_argument("--scan-server", help="read masters from this scan server, e.g. http://host:8380")
    p.add_argument("--state", default="/var/lib/usnm-quality-jpeg-cache")
    p.add_argument("--workers", type=int, default=2)
    p.add_argument("--limit", type=int, default=0)
    p.add_argument("--blob-base", help=argparse.SUPPRESS)
    p.add_argument("--what-if", action="store_true")
    p.add_argument("--keyed-completions", action="store_true",
                   help="incrementally index completions on disk (install the refresh/list helpers too)")
    p.add_argument("--follow-list", action="store_true",
                   help="recheck the bounded refreshed list after each pass; requires --keyed-completions")
    p.add_argument("--poll-secs", type=int, default=300)
    a = p.parse_args(argv)
    if not re.fullmatch(r"[a-z0-9]{3,24}", a.account):
        p.error("--account is a storage account name")
    if a.scan_server and not re.fullmatch(r"https?://[A-Za-z0-9.-]+(:[0-9]+)?/?", a.scan_server):
        p.error("--scan-server is http://<host>[:port]")
    if a.follow_list and (not a.keyed_completions or not 60 <= a.poll_secs <= 3600):
        p.error("--follow-list requires --keyed-completions and --poll-secs between 60 and 3600")
    source = a.scan_server or a.scans_root
    from PIL import __version__ as pil

    if pil != PILLOW_VERSION:
        sys.exit(f"error: Pillow {pil}; the runner's recipe is pinned to {PILLOW_VERSION}")
    while True:
        run_list(a, source)
        if not a.follow_list:
            return
        time.sleep(a.poll_secs)


def run_list(a, source):
    os.makedirs(a.state, exist_ok=True)
    ledger = os.path.join(a.state, f"done-{RECIPE}.txt")
    with open(a.list) as f:
        if a.keyed_completions:
            names = []
            for _ in range(10001):
                raw = f.readline(1025)
                if not raw:
                    break
                if len(raw) > 1024:
                    raise Refused("bounded converter list has an oversized line")
                if raw.strip():
                    names.append(validate_blob_name(raw.strip()))
            if len(names) > 10000 or f.read(1):
                raise Refused("bounded converter list exceeds 10000 lines")
        else:
            names = [validate_blob_name(n.strip()) for n in f if n.strip()]
    if a.keyed_completions:
        Path(ledger).touch(exist_ok=True)
        todo = keyed_todo(names, ledger, a.state)
    else:
        done = set()
        if os.path.exists(ledger):
            with open(ledger) as f:
                done = {line.rstrip("\n").split("\t")[0] for line in f}
        todo = [n for n in names if n not in done]
    if a.limit:
        todo = todo[: a.limit]
    print(f"{len(names)} listed, {len(names) - len(todo)} done before, {len(todo)} to go", flush=True)
    counts, t0, n = {}, time.monotonic(), 0
    with open(ledger, "a") as out, ProcessPoolExecutor(
        a.workers, initializer=_init, initargs=(a.account, a.blob_base)
    ) as ex:
        for name, outcome, secs in ex.map(_work, [(source, x, a.what_if) for x in todo], chunksize=4):
            n += 1
            key = outcome.split(" ")[0]
            counts[key] = counts.get(key, 0) + 1
            if a.what_if or key == "skipped:":
                print(f"{name}: {outcome}", flush=True)
                continue
            out.write(f"{name}\t{outcome}\n")
            out.flush()
            if n % 200 == 0:
                rate = n / (time.monotonic() - t0)
                print(f"{n}/{len(todo)} {rate:.2f} pages/s {counts}", flush=True)
    print(f"finished {n} pages in {time.monotonic() - t0:.0f}s {counts}", flush=True)


if __name__ == "__main__":
    try:
        main()
    except Refused as e:
        sys.exit(f"error: {e}")
