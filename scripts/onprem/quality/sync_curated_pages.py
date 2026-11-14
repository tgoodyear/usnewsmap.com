#!/usr/bin/env python3
"""A current local copy of the lake's published OCR pages, for the provenance resolver (#1956).

Lists `ocr-local/pages/` in the `curated` container and copies the parquet parts that the local
directory lacks or has with another size or MD5. It reads Azure only, as the host's Arc machine
identity (Storage Blob Data Reader is enough: list and get), and writes only under --to. Local
parts the lake no longer lists are removed, so --to is a copy and nothing else.

  sync_curated_pages.py --url https://<account>.blob.core.windows.net/curated --to DIR [--prefix ocr-local/pages/]

Needs read access to the Arc agent's key file (root, or a member of `himds`). No key, SAS or
connection string is used; the token is the host's own identity token and is never printed.
"""
import argparse
import base64
import hashlib
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

IDENTITY_ENDPOINT = os.environ.get("IDENTITY_ENDPOINT", "http://localhost:40342/metadata/identity/oauth2/token")
API = "2023-11-03"


def arc_token():
    """A storage token from the Arc agent's local endpoint (the challenge/key-file flow)."""
    url = IDENTITY_ENDPOINT + "?api-version=2020-06-01&resource=" + urllib.parse.quote("https://storage.azure.com/", safe="")
    try:
        urllib.request.urlopen(urllib.request.Request(url, headers={"Metadata": "true"}))
        raise SystemExit("the identity endpoint gave a token without a challenge")
    except urllib.error.HTTPError as e:
        challenge = e.headers.get("WWW-Authenticate", "")
    if "=" not in challenge:
        raise SystemExit("the identity endpoint gave no key-file challenge")
    with open(challenge.split("=", 1)[1]) as fh:
        key = fh.read()
    req = urllib.request.Request(url, headers={"Metadata": "true", "Authorization": "Basic " + key})
    return json.load(urllib.request.urlopen(req))["access_token"]


def list_blobs(url, token, prefix):
    marker = ""
    while True:
        q = {"restype": "container", "comp": "list", "prefix": prefix, "maxresults": "5000"}
        if marker:
            q["marker"] = marker
        req = urllib.request.Request(url + "?" + urllib.parse.urlencode(q),
                                     headers={"Authorization": "Bearer " + token, "x-ms-version": API})
        root = ET.fromstring(urllib.request.urlopen(req).read())
        for b in root.iter("Blob"):
            p = b.find("Properties")
            yield dict(name=b.findtext("Name"), size=int(p.findtext("Content-Length")), md5=p.findtext("Content-MD5") or "")
        marker = root.findtext("NextMarker") or ""
        if not marker:
            return


def md5_b64(path):
    h = hashlib.md5()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return base64.b64encode(h.digest()).decode()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--url", required=True, help="the curated container URL")
    ap.add_argument("--to", required=True, help="the local copy of the prefix (created if missing)")
    ap.add_argument("--prefix", default="ocr-local/pages/")
    a = ap.parse_args()
    if not a.prefix.endswith("/"):
        sys.exit("--prefix must end with /")
    token = arc_token()
    want = {}
    for b in list_blobs(a.url.rstrip("/"), token, a.prefix):
        if b["name"].endswith(".parquet"):
            want[b["name"][len(a.prefix):]] = b
    if not want:
        sys.exit(f"nothing listed under {a.prefix}: refusing to empty the copy")
    fetched = kept = 0
    for rel, b in sorted(want.items()):
        dst = os.path.join(a.to, rel)
        if os.path.isfile(dst) and os.path.getsize(dst) == b["size"] and (not b["md5"] or md5_b64(dst) == b["md5"]):
            kept += 1
            continue
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        req = urllib.request.Request(a.url.rstrip("/") + "/" + urllib.parse.quote(b["name"]),
                                     headers={"Authorization": "Bearer " + token, "x-ms-version": API})
        tmp = dst + ".partial"
        h = hashlib.md5()
        with urllib.request.urlopen(req) as r, open(tmp, "wb") as out:
            for chunk in iter(lambda: r.read(1 << 20), b""):
                h.update(chunk)
                out.write(chunk)
        if os.path.getsize(tmp) != b["size"] or (b["md5"] and base64.b64encode(h.digest()).decode() != b["md5"]):
            os.unlink(tmp)
            sys.exit(f"{b['name']}: the download does not match the listing")
        os.replace(tmp, dst)
        fetched += 1
    removed = 0
    for dirpath, _, files in os.walk(a.to):
        for f in files:
            rel = os.path.relpath(os.path.join(dirpath, f), a.to)
            if rel not in want:
                os.unlink(os.path.join(dirpath, f))
                removed += 1
    print(f"curated pages: {len(want)} parts listed, {fetched} fetched, {kept} current, {removed} removed")


if __name__ == "__main__":
    main()
