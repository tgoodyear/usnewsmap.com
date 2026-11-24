# Ported unchanged from branch chr-2128 at 89fb1017 (scripts/ocr-survey/chr2128/common.py), the code #2128
# scored; only these comment lines are new. Run through ghn_boxes.py for the GHN campaign (#1622).
"""Shared helpers for the #2128 Cherokee study: paths, page lists, hashes, and the earlier scripts this
study reuses unchanged (the #1622 finder, clean-up, classifier and reader; the #1620 NT aligner)."""
import hashlib
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
CHR_SCRIPTS = REPO / "docs" / "reports" / "2026-11-cherokee" / "scripts"
OCR_SURVEY = REPO / "scripts" / "ocr-survey"
for p in (CHR_SCRIPTS, OCR_SURVEY):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))


def doc_id(page):
    return page.replace("/", "_")


def scan_path(root, page, layout):
    """A page's scan: GHN keeps `<lccn>/<date>/ed-N/seq-N.jpg`, the LoC bank keeps `<doc_id>.jpg`."""
    if layout == "ghn":
        return Path(root) / f"{page}.jpg"
    return Path(root) / f"{doc_id(page)}.jpg"


def read_pages(path):
    return [l.strip() for l in open(path) if l.strip()]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def sha256_list(items):
    """sha256 of the items sorted, one per line, with a trailing newline (the page lists' convention)."""
    return hashlib.sha256(("\n".join(sorted(items)) + "\n").encode()).hexdigest()


def read_jsonl(path):
    return [json.loads(l) for l in open(path, encoding="utf-8") if l.strip()]


def write_jsonl(path, rows):
    with open(path, "w", encoding="utf-8") as f:
        for r in rows:
            f.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")


def line_id(page, tid):
    """A found line's id: the page and Tesseract's block.paragraph.line numbering (reading order)."""
    return f"{page}#{tid}"
