#!/usr/bin/env python3
"""Verify that a staged report-data archive matches its reviewed inventory."""

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path, PurePosixPath

MANIFEST = "docs/reports/report-data-manifest.json"
NOTICE = "docs/reports/report-data-rights.txt"


def valid_name(name):
    return (
        isinstance(name, str)
        and name.startswith(("docs/reports/", "docs/data/"))
        and str(PurePosixPath(name)) == name
        and ".." not in PurePosixPath(name).parts
        and not any(c in name for c in "\0\r\n")
        and name != MANIFEST
    )


def staged_files(tree):
    actual = set()
    for path in tree.rglob("*"):
        if path.is_symlink():
            raise ValueError(f"symlink in staged report data: {path.relative_to(tree)}")
        if path.is_file():
            name = path.relative_to(tree).as_posix()
            if name != MANIFEST:
                if not valid_name(name):
                    raise ValueError(f"invalid staged path: {name!r}")
                actual.add(name)
        elif not path.is_dir():
            raise ValueError(f"non-regular staged report data: {path.relative_to(tree)}")
    if NOTICE not in actual:
        raise ValueError("the rights notice is missing")
    return actual


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate manifest key: {key}")
        result[key] = value
    return result


def verify(tree):
    with (tree / MANIFEST).open(encoding="utf-8") as source:
        manifest = json.load(source, object_pairs_hook=unique_object)
    if not isinstance(manifest, dict) or set(manifest) != {"version", "files"} or type(manifest["version"]) is not int or manifest["version"] != 1:
        raise ValueError("invalid report-data manifest version or fields")
    files = manifest["files"]
    if not isinstance(files, dict) or NOTICE not in files:
        raise ValueError("report-data manifest must include the rights notice")
    for name, digest in files.items():
        if not valid_name(name) or not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError(f"invalid manifest entry: {name!r}")

    actual = staged_files(tree)
    unapproved = sorted(actual - files.keys())
    missing = sorted(files.keys() - actual)
    if unapproved or missing:
        details = []
        if unapproved:
            details.append(f"unapproved files ({len(unapproved)}): {', '.join(unapproved[:10])}")
        if missing:
            details.append(f"missing approved files ({len(missing)}): {', '.join(missing[:10])}")
        raise ValueError("; ".join(details))

    changed = [name for name, expected in files.items() if sha256(tree / name) != expected]
    if changed:
        raise ValueError(f"digest mismatch ({len(changed)}): {', '.join(sorted(changed)[:10])}")
    print(f"report-data rights inventory verified: {len(files)} files and manifest")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--create", action="store_true", help="print a candidate inventory; this does not approve its contents")
    parser.add_argument("tree", type=Path, help="staged archive tree")
    args = parser.parse_args()
    try:
        if args.create:
            files = {name: sha256(args.tree / name) for name in sorted(staged_files(args.tree))}
            print(json.dumps({"version": 1, "files": files}, indent=2, sort_keys=True))
        else:
            verify(args.tree)
    except (OSError, ValueError) as exc:
        print(f"error: report-data rights inventory: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
