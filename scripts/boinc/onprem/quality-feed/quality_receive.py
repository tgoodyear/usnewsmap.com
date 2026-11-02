#!/usr/bin/env python3
"""Checks and files one feeder chunk on the BOINC host (CT230), before quality-stage (#962).

quality-ship.sh runs it in CT230 as `python3 - --campaign C --chunk c00001` with this file on
stdin, after it has unpacked the chunk into <share>/quality/<campaign>/incoming/<chunk>/.

  quality_receive.py --campaign NAME --chunk NAME [--share /srv/usnm-boinc/project]
      [--places-from v1-wave9] [--after-submit]

It checks every file against the chunk's SHA256SUMS and that every page in sample.jsonl has
its scans/<doc_id>.jpg and .xml and texts/<doc_id>.json. quality-stage takes a page whose
scan is missing from tile.loc.gov, so that check is what keeps the BOINC server from ever
asking LoC for a page. It then moves the scans and texts into the campaign's scans/ and
texts/, copies places.txt from --places-from once, and prints the sample's path inside the
bridge container (/project/...) and its languages, tab-separated, for the stage step.

--after-submit, once quality-submit has handed every bundle to create_work (each has its
`submitted` marker), frees the chunk's copies the server no longer needs: the received scans
and texts, the per-page units' page.jpg, loc.xml and texts.json (their quality.json stays, so
staging never takes the page again), and incoming/<chunk>. The bundle units keep their files
(qwen stage cuts crops from them) and submit has copied them into the download tree.
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


def own(path):
    info = os.lstat(path)
    if (info.st_uid, info.st_gid) != (OWNER, OWNER):
        os.lchown(path, OWNER, OWNER)


def after_submit(camp, inc):
    units = os.path.join(camp, "units")
    bundles = [d for d in os.listdir(units) if os.path.isdir(os.path.join(units, d))] if os.path.isdir(units) else []
    if not bundles or any(not os.path.exists(os.path.join(units, b, "submitted")) for b in bundles):
        fail("a bundle has no submitted marker; nothing freed")
    with open(os.path.join(inc, "sample.jsonl")) as f:
        docs = [json.loads(line)["doc_id"] for line in f]
    freed = 0
    for d in docs:
        if not DOC_RE.match(d):
            fail(f"not a page id: {d}")
        paths = [os.path.join(camp, "scans", f"{d}.jpg"), os.path.join(camp, "scans", f"{d}.xml"),
                 os.path.join(camp, "texts", f"{d}.json"), os.path.join(camp, "texts", f"{d}.ours.xml")]
        paths += [os.path.join(camp, "pages", d, n) for n in ("page.jpg", "loc.xml", "texts.json")]
        for p in paths:
            if os.path.isfile(p):
                freed += os.path.getsize(p)
                os.remove(p)
    shutil.rmtree(inc)
    print(f"freed {freed} bytes for {len(docs)} pages")


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--campaign", required=True)
    ap.add_argument("--chunk", required=True)
    ap.add_argument("--share", default="/srv/usnm-boinc/project")
    ap.add_argument("--places-from", default="v1-wave9")
    ap.add_argument("--after-submit", action="store_true")
    a = ap.parse_args(argv)
    if not NAME_RE.match(a.campaign) or not NAME_RE.match(a.places_from):
        fail("--campaign and --places-from: 1-41 of [a-z0-9-]")
    if not CHUNK_RE.match(a.chunk):
        fail("--chunk: c and five digits")
    camp = os.path.join(a.share, "quality", a.campaign)
    inc = os.path.join(camp, "incoming", a.chunk)
    if a.after_submit:
        after_submit(camp, inc)
        return
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
        if root == camp:
            # Staging's own trees: never walked, they only grow.
            dirs[:] = [d for d in dirs if d not in ("units", "pages")]
        # On the share each chown is a synchronous write to disk, and texts/ holds thousands of
        # files, so only what isn't the owner's yet: without this check, half an hour a chunk.
        for n in dirs + files:
            own(os.path.join(root, n))
    own(camp)
    with open(os.path.join(inc, "langs.txt")) as f:
        langs = f.read().strip()
    if not re.match(r"^[a-z]{3}(,[a-z]{3})*$", langs):
        fail("bad langs.txt")
    sample = "/project/" + os.path.relpath(os.path.join(inc, "sample.jsonl"), a.share)
    print(f"{sample}\t{langs}\t{len(docs)}")


if __name__ == "__main__":
    main()
