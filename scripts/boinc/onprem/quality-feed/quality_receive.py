#!/usr/bin/env python3
"""Checks and files one feeder chunk on the BOINC host (CT230), before quality-stage (#962).

quality-ship.sh runs it in CT230 as `python3 - --campaign C --chunk c00001` with this file on
stdin, after it has unpacked the chunk into <share>/quality/<campaign>/incoming/<chunk>/.

  quality_receive.py --campaign NAME --chunk NAME [--share /srv/usnm-boinc/project]
      [--places-from v1-wave9]

It checks every file against the chunk's SHA256SUMS and that every page in sample.jsonl has
its scans/<doc_id>.jpg and .xml and texts/<doc_id>.json. quality-stage takes a page whose
scan is missing from tile.loc.gov, so that check is what keeps the BOINC server from ever
asking LoC for a page. It then moves the scans and texts into the campaign's scans/ and
texts/, copies places.txt from --places-from once, and prints the sample's path inside the
bridge container (/project/...) and its languages, tab-separated, for the stage step.
"""
import argparse
import hashlib
import json
import os
import re
import shutil
import sys

NAME_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,40}$")
CHUNK_RE = re.compile(r"^c[0-9]{5}$")
DOC_RE = re.compile(r"^[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+$")
OWNER = 1000  # boincadm, as the rest of the project tree


def fail(msg):
    sys.exit(f"error: {msg}")


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--campaign", required=True)
    ap.add_argument("--chunk", required=True)
    ap.add_argument("--share", default="/srv/usnm-boinc/project")
    ap.add_argument("--places-from", default="v1-wave9")
    a = ap.parse_args(argv)
    if not NAME_RE.match(a.campaign) or not NAME_RE.match(a.places_from):
        fail("--campaign and --places-from: 1-41 of [a-z0-9-]")
    if not CHUNK_RE.match(a.chunk):
        fail("--chunk: c and five digits")
    camp = os.path.join(a.share, "quality", a.campaign)
    inc = os.path.join(camp, "incoming", a.chunk)
    if not os.path.isfile(os.path.join(inc, "READY")):
        fail(f"{inc} has no READY")

    listed = {}
    with open(os.path.join(inc, "SHA256SUMS")) as f:
        for line in f:
            want, rel = line.rstrip("\n").split("  ", 1)
            if rel.startswith("/") or ".." in rel.split("/"):
                fail(f"bad path in SHA256SUMS: {rel}")
            listed[rel] = want
    for rel, want in listed.items():
        h = hashlib.sha256()
        with open(os.path.join(inc, rel), "rb") as f:
            for block in iter(lambda: f.read(1 << 20), b""):
                h.update(block)
        if h.hexdigest() != want:
            fail(f"{rel}: sha256 differs from SHA256SUMS")
    docs = []
    with open(os.path.join(inc, "sample.jsonl")) as f:
        for line in f:
            d = json.loads(line)["doc_id"]
            if not DOC_RE.match(d):
                fail(f"not a page id: {d}")
            for rel in (f"scans/{d}.jpg", f"scans/{d}.xml", f"texts/{d}.json"):
                if rel not in listed:
                    fail(f"{d}: {rel} missing; staging it would ask LoC")
            docs.append(d)
    if not docs:
        fail("empty sample")

    for sub in ("scans", "texts"):
        os.makedirs(os.path.join(camp, sub), exist_ok=True)
        for name in os.listdir(os.path.join(inc, sub)):
            os.replace(os.path.join(inc, sub, name), os.path.join(camp, sub, name))
    places = os.path.join(camp, "places.txt")
    if not os.path.exists(places):
        shutil.copyfile(os.path.join(a.share, "quality", a.places_from, "places.txt"), places)
    for root, dirs, files in os.walk(camp):
        if os.path.relpath(root, camp).split(os.sep)[0] in ("units", "pages"):
            continue
        for n in dirs + files:
            os.lchown(os.path.join(root, n), OWNER, OWNER)
    os.lchown(camp, OWNER, OWNER)
    with open(os.path.join(inc, "langs.txt")) as f:
        langs = f.read().strip()
    if not re.match(r"^[a-z]{3}(,[a-z]{3})*$", langs):
        fail("bad langs.txt")
    sample = "/project/" + os.path.relpath(os.path.join(inc, "sample.jsonl"), a.share)
    print(f"{sample}\t{langs}\t{len(docs)}")


if __name__ == "__main__":
    main()
