#!/usr/bin/env python3
"""The one write the controller makes into the hand-off root itself (#1367): its plan's
held-pages parts. onprem-step.sh runs this for `--target handoff --step write-held-pages`.

    handoff_write.py --root DIR held-pages --plan NAME < {"parts": {"scheduler/held-pages/NAME.0000.json": "<json text>"}}

Each part is written through a temporary name and renamed; the plan's other parts go. Other
plans' parts are never touched. Everything else the controller writes goes through the
spool-out, collect, claim and export steps, which keep their on-disk layouts.
"""

import argparse
import json
import os
import re
import sys
from pathlib import Path

PREFIX = "scheduler/held-pages/"
PLAN = re.compile(r"[A-Za-z0-9-]{1,64}")  # the scheduler's plan pattern
MAX_STDIN = 256 * 1024 * 1024
# crates/usnm-ocr-local/src/held_pages.rs: the reader's limits and entry rules.
MAX_BYTES = 64 << 20
MAX_LISTS = 1024
SEGMENT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}")
DIGEST = re.compile(r"[0-9a-f]{64}")


class WriteError(Exception):
    pass


def stem(plan):
    """The file name stem of a plan's lists; batch_scheduler.held_pages_stem has the same rule."""
    return plan if plan[:1].isalnum() else f"p_{plan}"


def validate_part(body):
    """Reject what the claim's reader would: it fails every plan's claim on one bad list."""
    if len(body.encode()) > MAX_BYTES:
        raise WriteError("a held-pages list is over the reader's size limit")
    try:
        doc = json.loads(body)
    except ValueError:
        raise WriteError("held-pages list is not JSON") from None
    if not isinstance(doc, dict) or set(doc) != {"schema_version", "pages"} \
            or type(doc["schema_version"]) is not int or doc["schema_version"] != 1:
        raise WriteError("held-pages list must be {schema_version: 1, pages: {...}}")
    pages = doc["pages"]
    if not isinstance(pages, dict):
        raise WriteError("held-pages pages must be an object")
    for doc_id, digests in pages.items():
        if not (SEGMENT.fullmatch(doc_id) and isinstance(digests, list) and digests
                and all(isinstance(d, str) and DIGEST.fullmatch(d) for d in digests)):
            raise WriteError(f"invalid held-pages entry for {doc_id[:40]}")


def check_root(root):
    info = root.stat()
    if (not root.is_absolute() or root.resolve() != root or info.st_uid != os.geteuid()
            or info.st_mode & 0o777 != 0o700):
        raise WriteError("hand-off root must be canonical, owned by the runner user and mode 0700")


def held_pages(root, plan, parts):
    if not PLAN.fullmatch(plan):
        raise WriteError("invalid plan")
    name_re = re.compile(re.escape(f"{PREFIX}{stem(plan)}.") + r"[0-9]{4}\.json")
    if not all(name_re.fullmatch(name) and isinstance(body, str) for name, body in parts.items()):
        raise WriteError("invalid held-pages parts")
    for body in parts.values():
        validate_part(body)
    directory = root / PREFIX.rstrip("/")
    directory.mkdir(parents=True, exist_ok=True)
    if directory.resolve() != directory:
        raise WriteError("unsafe held-pages directory")
    # The reader refuses more than MAX_LISTS lists in all: count the other plans' too.
    others = [path for path in directory.glob("*.json") if not path.name.startswith(f"{stem(plan)}.")]
    if len(others) + len(parts) > MAX_LISTS:
        raise WriteError(f"more than {MAX_LISTS} held-pages lists in all")
    for name, body in parts.items():
        staged = directory / f".{stem(plan)}.tmp"
        with staged.open("wb") as out:
            out.write(body.encode())
            out.flush()
            os.fsync(out.fileno())
        os.replace(staged, root / name)
    removed = 0
    for path in directory.glob(f"{stem(plan)}.*.json"):
        if f"{PREFIX}{path.name}" not in parts and name_re.fullmatch(f"{PREFIX}{path.name}"):
            path.unlink()
            removed += 1
    return {"written": len(parts), "removed": removed}


def main(argv=None, stdin=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    one = sub.add_parser("held-pages")
    one.add_argument("--plan", required=True)
    args = parser.parse_args(argv)
    try:
        if not args.root.is_dir() or args.root.is_symlink():
            raise WriteError("hand-off root is not a directory")
        check_root(args.root)
        raw = (stdin or sys.stdin).read(MAX_STDIN + 1)
        if len(raw) > MAX_STDIN:
            raise WriteError("input exceeds limit")
        parts = json.loads(raw)["parts"]
        if not isinstance(parts, dict):
            raise WriteError("invalid held-pages parts")
        print(json.dumps(held_pages(args.root, args.plan, parts), sort_keys=True))
    except (WriteError, OSError, ValueError, KeyError, TypeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
