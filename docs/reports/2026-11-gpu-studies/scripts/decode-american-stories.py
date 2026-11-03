#!/usr/bin/env python3
"""Decode the American Stories job's id/seconds/base64-JSON records."""

import base64
import binascii
import json
import re
import sys
from pathlib import Path


ID = re.compile(r"[A-Za-z0-9._:-]{1,200}")
SECONDS = re.compile(r"[0-9]{1,9}")


def decode(path):
    rows = []
    with path.open(encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            if not line.strip():
                continue
            fields = line.rstrip("\n").split("\t")
            if len(fields) != 3 or not ID.fullmatch(fields[0]) or not SECONDS.fullmatch(fields[1]):
                raise ValueError(f"line {number}: invalid id/seconds/base64 record")
            try:
                page = json.loads(base64.b64decode(fields[2], validate=True).decode("utf-8"))
            except (ValueError, binascii.Error, UnicodeError):
                raise ValueError(f"line {number}: invalid base64-encoded UTF-8 JSON") from None
            if not isinstance(page, dict) or not isinstance(page.get("bboxes"), list):
                raise ValueError(f"line {number}: page has no bboxes array")
            texts = []
            for box in page["bboxes"]:
                if not isinstance(box, dict):
                    raise ValueError(f"line {number}: invalid bbox")
                text = box.get("raw_text", "")
                if not isinstance(text, str):
                    raise ValueError(f"line {number}: non-string bbox raw_text")
                try:
                    text.encode("utf-8")
                except UnicodeEncodeError:
                    raise ValueError(f"line {number}: invalid Unicode in bbox raw_text") from None
                if text.strip():
                    texts.append(text)
            # Match the survey's AmericanStories.run: keep bbox order and join nonblank regions.
            rows.append({"id": fields[0], "text": "\n\n".join(texts)})
    return rows


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: decode-american-stories.py INPUT OUTPUT")
    source, output = map(Path, sys.argv[1:])
    try:
        rows = decode(source)
        with output.open("w", encoding="utf-8") as dest:
            for row in rows:
                dest.write(json.dumps(row, ensure_ascii=False) + "\n")
    except (OSError, UnicodeError, ValueError) as error:
        raise SystemExit(f"American Stories decode failed: {error}") from None


if __name__ == "__main__":
    main()
