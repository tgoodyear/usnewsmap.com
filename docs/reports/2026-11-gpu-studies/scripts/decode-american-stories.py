#!/usr/bin/env python3
"""Decode American Stories receipts or validate precomputed id/text JSONL."""

import argparse
import base64
import binascii
import json
import re
from pathlib import Path


ID = re.compile(r"[A-Za-z0-9._:-]{1,200}")
SECONDS = re.compile(r"[0-9]{1,9}")


def valid_text(text, number):
    try:
        text.encode("utf-8")
    except UnicodeEncodeError:
        raise ValueError(f"line {number}: invalid Unicode in text") from None


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
                valid_text(text, number)
                if text.strip():
                    texts.append(text)
            # Match the survey's AmericanStories.run: keep bbox order and join nonblank regions.
            rows.append({"id": fields[0], "text": "\n\n".join(texts)})
    return rows


def read_jsonl(path):
    rows = []
    with path.open(encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                raise ValueError(f"line {number}: invalid JSON") from None
            if (not isinstance(row, dict) or not isinstance(row.get("id"), str)
                    or not ID.fullmatch(row["id"]) or not isinstance(row.get("text"), str)):
                raise ValueError(f"line {number}: invalid id/text row")
            valid_text(row["text"], number)
            rows.append({"id": row["id"], "text": row["text"]})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--input-format", choices=("b64", "jsonl"), default="b64",
                        help="base64 receipts (default) or precomputed id/text JSONL")
    args = parser.parse_args()
    try:
        rows = decode(args.input) if args.input_format == "b64" else read_jsonl(args.input)
        with args.output.open("w", encoding="utf-8") as dest:
            for row in rows:
                dest.write(json.dumps(row, ensure_ascii=False) + "\n")
    except (OSError, UnicodeError, ValueError) as error:
        raise SystemExit(f"American Stories decode failed: {error}") from None


if __name__ == "__main__":
    main()
