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
import time
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

IDENTITY_ENDPOINT = os.environ.get("IDENTITY_ENDPOINT", "http://localhost:40342/metadata/identity/oauth2/token")
API = "2023-11-03"


TOKEN_DIR = "/var/opt/azcmagent/tokens"
KEY_MAX_BYTES = 4096


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def local_opener():
    """The endpoint is local: never through a proxy, never redirected."""
    return urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect)


def arc_token():
    """A storage token from the Arc agent's local endpoint (the challenge/key-file flow), with
    the checks of ArcIdentity in crates/usnm-store/src/credential.rs: http on loopback, no proxy
    or redirect, a 401 Basic realm challenge naming a regular `.key` file of at most 4 KiB
    directly in the agent's token directory."""
    parts = urllib.parse.urlsplit(IDENTITY_ENDPOINT)
    if parts.scheme != "http" or parts.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise SystemExit("the identity endpoint must be http on loopback")
    opener = local_opener()
    url = IDENTITY_ENDPOINT + "?api-version=2020-06-01&resource=" + urllib.parse.quote("https://storage.azure.com/", safe="")
    try:
        opener.open(urllib.request.Request(url, headers={"Metadata": "true"}), timeout=15)
        raise SystemExit("the identity endpoint gave a token without a challenge")
    except urllib.error.HTTPError as e:
        if e.code != 401:
            raise SystemExit(f"the identity endpoint answered {e.code}, not a 401 challenge")
        challenge = e.headers.get("WWW-Authenticate", "").strip()
    if not challenge.startswith("Basic"):
        raise SystemExit("the identity challenge is not Basic")
    realm = challenge[len("Basic"):].strip()
    if not realm.startswith("realm="):
        raise SystemExit("the identity challenge names no realm")
    realm = realm[len("realm="):].strip().strip('"')
    path = os.path.realpath(realm)
    token_dir = os.path.realpath(TOKEN_DIR)
    if not path.endswith(".key") or os.path.dirname(path) != token_dir:
        raise SystemExit("the identity challenge names a file outside the agent's token directory")
    if not os.path.isfile(path) or os.path.getsize(path) > KEY_MAX_BYTES:
        raise SystemExit("the identity key file is not a regular file of at most 4 KiB")
    with open(path) as fh:
        key = fh.read().strip()
    req = urllib.request.Request(url, headers={"Metadata": "true", "Authorization": "Basic " + key})
    body = json.load(opener.open(req, timeout=15))
    return body["access_token"], float(body.get("expires_on", 0))


class Token:
    """The bearer token, fetched again when less than five minutes of it remain."""

    def __init__(self):
        self.value, self.expires = arc_token()

    def get(self):
        if self.expires - time.time() < 300:
            self.value, self.expires = arc_token()
        return self.value


def check_url(url):
    """The container URL, as BlobStore::new checks it: https on an Azure Blob host, one container
    segment, no query, fragment or credentials."""
    u = urllib.parse.urlsplit(url)
    host = (u.hostname or "").lower()
    if u.scheme != "https" or not host.endswith(".blob.core.windows.net") or len(host) <= len(".blob.core.windows.net"):
        raise SystemExit("--url must be https on an Azure Blob host (*.blob.core.windows.net)")
    if u.query or u.fragment or u.username or u.password:
        raise SystemExit("--url must carry no query, fragment or credentials")
    seg = u.path.strip("/")
    if not seg or "/" in seg or seg in (".", ".."):
        raise SystemExit("--url must name one container")
    return f"https://{u.netloc}/{seg}"


STORAGE = urllib.request.build_opener(NoRedirect)  # the token goes to the configured host only


def safe_rel(rel):
    """`<title>/<part>.parquet` and nothing else: no empty, dot or dot-dot component."""
    parts = rel.split("/")
    ok = len(parts) == 2 and all(p not in ("", ".", "..") and "\\" not in p and "\0" not in p for p in parts)
    return ok and parts[1].endswith(".parquet")


def inside(root, path):
    """True when `path` resolves under `root` (symlinks followed)."""
    root = os.path.realpath(root)
    return os.path.commonpath([root, os.path.realpath(path)]) == root


def list_blobs(url, token, prefix):
    """`token` is a Token."""
    marker = ""
    while True:
        q = {"restype": "container", "comp": "list", "prefix": prefix, "maxresults": "5000"}
        if marker:
            q["marker"] = marker
        req = urllib.request.Request(url + "?" + urllib.parse.urlencode(q),
                                     headers={"Authorization": "Bearer " + token.get(), "x-ms-version": API})
        root = ET.fromstring(STORAGE.open(req, timeout=120).read())
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
    url = check_url(a.url)
    token = Token()
    want = {}
    for b in list_blobs(url, token, a.prefix):
        if b["name"].endswith(".parquet"):
            rel = b["name"][len(a.prefix):]
            if not safe_rel(rel):
                sys.exit(f"{b['name']}: not a <title>/<part>.parquet name under {a.prefix}")
            want[rel] = b
    if not want:
        sys.exit(f"nothing listed under {a.prefix}: refusing to empty the copy")
    fetched = kept = 0
    for rel, b in sorted(want.items()):
        dst = os.path.join(a.to, rel)
        if os.path.islink(dst) or os.path.islink(dst + ".partial") or os.path.islink(os.path.dirname(dst)):
            sys.exit(f"{rel}: a symlink in the copy; refusing to write through it")
        if not inside(a.to, os.path.dirname(dst)):
            sys.exit(f"{rel}: resolves outside {a.to}")
        if os.path.isfile(dst) and os.path.getsize(dst) == b["size"] and (not b["md5"] or md5_b64(dst) == b["md5"]):
            kept += 1
            continue
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        req = urllib.request.Request(url + "/" + urllib.parse.quote(b["name"]),
                                     headers={"Authorization": "Bearer " + token.get(), "x-ms-version": API})
        tmp = dst + ".partial"
        h = hashlib.md5()
        with STORAGE.open(req, timeout=120) as r, os.fdopen(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o644), "wb") as out:
            for chunk in iter(lambda: r.read(1 << 20), b""):
                h.update(chunk)
                out.write(chunk)
        if os.path.getsize(tmp) != b["size"] or (b["md5"] and base64.b64encode(h.digest()).decode() != b["md5"]):
            os.unlink(tmp)
            sys.exit(f"{b['name']}: the download does not match the listing")
        os.replace(tmp, dst)
        fetched += 1
    removed = 0
    for dirpath, _, files in os.walk(a.to):  # os.walk does not follow symlinked directories
        for f in files:
            rel = os.path.relpath(os.path.join(dirpath, f), a.to)
            if rel not in want:
                os.unlink(os.path.join(dirpath, f))  # a symlink is removed, never followed
                removed += 1
    print(f"curated pages: {len(want)} parts listed, {fetched} fetched, {kept} current, {removed} removed")


if __name__ == "__main__":
    main()
