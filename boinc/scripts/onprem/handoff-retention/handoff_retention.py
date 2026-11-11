#!/usr/bin/env python3
"""Frees the bridge's held copy of each OCR bundle's scans (hand-off retention step).

`usnm-boinc submit-bundle` (crates/usnm-boinc/src/bundle/submit.rs) writes two files under
<hand-off root>/bundles/<id>/: `ocr_bundle.json`, the bundle's manifest, and `pages`, the
bundle's scans end to end. It then puts the same `pages` into the download tree, where
BOINC serves it, and runs create_work. Nothing reads the held `pages` after that: spool-out
splits an assimilated result with `ocr_bundle.json` only, and reconcile asks BOINC about
the workunit, not the file. `pages` is a copy of scans that also sit under keep/ of
/tank/ocr/scans, so it is derived data (rule of 11 November 2026: derived files may be
deleted once nothing uses them), and at 20 to 45 MB a bundle it was the largest thing that
grew on the BOINC host's root disk. The step frees only `pages`. It keeps `ocr_bundle.json`
(about 12 KB, needed while any page of the bundle is unspooled), every job journal under
jobs/ (durable recovery state: archive_history.py moves those, nothing deletes them), and
every scan, staged or not.

  handoff_retention.py prune-bundles --root DIR --journal FILE [--min-age-hours H]
      [--commit SHA] [--delete] [--max-bundles N] [--max-bytes N]

A bundle's `pages` goes only when:
  - the bundle's directory is a real directory named by a sha256, and `pages` in it is a
    regular file with one link, last written at least --min-age-hours (default 2) ago, so a
    submit that wrote it but has not yet saved the page journals is never caught;
  - no job journal under <root>/jobs names the bundle (`bundle`) in a stage before
    `submitted`: a `prepared`, `reserved` or `submitting` member is still being submitted.
    A bundle no journal names, because archive_history.py moved them, qualifies.
The journals are all read first; a job directory that is not a directory, or whose record.json
is missing, unreadable or not a job record (an object with a stage), stops the run before anything is deleted. Only a job directory that has
vanished (the archive moved it) is skipped. Without --delete it changes nothing and reports what it would free. With --delete
it appends a journal line before each unlink and another after it, both fsynced, with the
--commit the run came from. --max-bundles (default 5000) and --max-bytes (default 200 GB)
bound a run; a file that would pass --max-bytes ends the run before it is touched. Run it as
root on the BOINC host: the files are root's 0600.
"""
import argparse
import json
import os
import re
import stat
import sys
import time

BUNDLE_ID = re.compile(r"[0-9a-f]{64}")
PAGES = "pages"
# A journal in one of these stages has had its create_work call; anything earlier is still
# being submitted, so its bundle's inputs are not ours to drop.
SUBMITTED = {"submitted", "published", "failed"}


def journal_line(journal, record):
    journal.write(json.dumps(record, sort_keys=True) + "\n")
    journal.flush()
    os.fsync(journal.fileno())


def bundle_stages(root):
    """Which stages each bundle's job journals are in: {bundle id: {stage, ...}}."""
    jobs = os.path.join(root, "jobs")
    held = {}
    for name in sorted(os.listdir(jobs)):
        directory = os.path.join(jobs, name)
        try:
            mode = os.lstat(directory).st_mode
        except FileNotFoundError:
            continue  # moved to the archive between the listing and now
        if not stat.S_ISDIR(mode):
            sys.exit(f"error: {directory} is not a directory; nothing deleted")
        path = os.path.join(directory, "record.json")
        try:
            with open(path) as f:
                record = json.load(f)
        except (OSError, ValueError) as e:
            # A job directory without its record is unreadable recovery state, not an
            # archived job (the archive moves the whole directory).
            sys.exit(f"error: {path} is unreadable ({e}); nothing deleted")
        if not isinstance(record, dict) or not isinstance(record.get("stage"), str):
            sys.exit(f"error: {path} is not a job record; nothing deleted")
        bundle = record.get("bundle")
        if bundle is not None and not isinstance(bundle, str):
            sys.exit(f"error: {path} names a bundle that is not a string; nothing deleted")
        if bundle:
            held.setdefault(bundle, set()).add(str(record.get("stage")))
    return held


def candidates(root, min_age, now):
    """(bundle id, path of its pages, bytes, why not) for each bundle with a pages file."""
    held = bundle_stages(root)
    base = os.path.join(root, "bundles")
    for name in sorted(os.listdir(base)):
        if not BUNDLE_ID.fullmatch(name):
            continue
        directory = os.path.join(base, name)
        path = os.path.join(directory, PAGES)
        try:
            dst = os.lstat(directory)
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(dst.st_mode):
            yield name, directory, 0, "bundle directory is not a directory"
            continue
        try:
            st = os.lstat(path)
        except FileNotFoundError:
            continue  # already freed
        if not stat.S_ISREG(st.st_mode) or st.st_nlink != 1:
            yield name, path, 0, "pages is not a regular file with one link"
        elif now - st.st_mtime < min_age:
            yield name, path, st.st_size, "pages is newer than the minimum age"
        elif held.get(name, set()) - SUBMITTED:
            yield name, path, st.st_size, "a member is still before submitted"
        else:
            yield name, path, st.st_size, None


def prune_bundles(a):
    now = time.time()
    totals = {"bundles": 0, "bytes": 0, "kept": {}, "stopped": None}
    journal = open(a.journal, "a") if a.delete else None
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now))
    try:
        for bundle, path, size, why in candidates(a.root, a.min_age_hours * 3600, now):
            if why:
                totals["kept"][why] = totals["kept"].get(why, 0) + 1
                continue
            if totals["bundles"] >= a.max_bundles or totals["bytes"] + size > a.max_bytes:
                totals["stopped"] = "bound reached"
                break
            record = {"time": stamp, "commit": a.commit, "bundle": bundle, "file": PAGES,
                      "bytes": size}
            if a.delete:
                journal_line(journal, dict(record, action="deleting"))
                try:
                    os.unlink(path)
                except FileNotFoundError:
                    pass
                journal_line(journal, dict(record, action="deleted"))
            totals["bundles"] += 1
            totals["bytes"] += size
    finally:
        if journal:
            journal.close()
    summary = {"time": stamp, "step": "prune-bundles", "deleted": bool(a.delete),
               "bundles": totals["bundles"], "gib": round(totals["bytes"] / 2**30, 2),
               "kept": totals["kept"], "stopped": totals["stopped"]}
    print(json.dumps(summary, sort_keys=True))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    pb = sub.add_parser("prune-bundles")
    pb.add_argument("--root", required=True, help="the hand-off root: holds jobs/ and bundles/")
    pb.add_argument("--journal", required=True)
    pb.add_argument("--min-age-hours", type=float, default=2.0)
    pb.add_argument("--commit", default="unknown")
    pb.add_argument("--delete", action="store_true")
    pb.add_argument("--max-bundles", type=int, default=5000)
    pb.add_argument("--max-bytes", type=int, default=200 * 10**9)
    a = ap.parse_args(argv)
    if os.path.islink(a.root) or not os.path.isdir(os.path.join(a.root, "jobs")) \
            or not os.path.isdir(os.path.join(a.root, "bundles")):
        sys.exit(f"error: {a.root} is not a hand-off root (a real directory with jobs/ and bundles/)")
    if a.min_age_hours < 0.5:
        sys.exit("error: --min-age-hours is at least 0.5")
    return prune_bundles(a)


if __name__ == "__main__":
    sys.exit(main())
