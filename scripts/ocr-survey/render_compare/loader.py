#!/usr/bin/env python3
"""Step 1 of #2103: per-word rows from a gold set with checked text and LoC's ALTO.

    loader.py --set-dir SET --alto-dir LOC --out words.jsonl [--lang eng] [--limit N]

SET is a `gold.py public-convert` output (manifest.jsonl, crops.jsonl with the dataset's human
transcription, LICENSES/). LOC holds each page's LoC ALTO and page image as `<stem>.xml` and `<stem>.jpg`, the
stem being the manifest's `source_image` without its extension (the layout `en_pick_gold.py` reads, and the
`loc/` folder `gold.py public-fetch --dataset overproof-ca` writes). Nothing is fetched.

Each ALTO word whose box centre is in the crop's box (the rule of crops.py and `alto-crop`) becomes a row:

  page_id, crop_id, source_image, lang, line, word_index
  box [x, y, w, h]   the ALTO box in page-image pixels
  ocr_word, wc       LoC's reading and its confidence (null when ALTO has none)
  truth_word, op     the aligned checked word, and match | sub | ins (ins: no truth word was aligned to it)
  label              1 when the OCR word is the truth word, else 0
  real_word_error    a wrong word (op sub) that is itself a word of the language's damage-words list
  page_damage        damage_v2's damage_rate for the whole page's ALTO text, against that list
  source_license     the set's licence string

The alignment is a word-level edit alignment of the crop's ALTO words (reading order, a line-end hyphen kept on
the word, as `usnm-ocr-survey alto-crop` keeps it) with the transcription's words. Its cost is gold.py's
`edit_distance` cost: the number of word edits, so the total always equals the WER edit count `gold.py score`
reports; among alignments with the same word edits it takes the one with fewer character edits. Truth words
with no OCR word (deletions) have no box and give no row; they are counted per crop in `--summary`.

Only sets with licence files and a licence that allows the use are read: no `for_scoring: false`, no
No-Derivatives licence (a render and a distance are derived from the crop), none without a licence string.
CC BY-NC-SA 4.0 (Overproof dataset 3) allows this non-commercial research test; the string is kept in every row.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import sys
import unicodedata
import xml.etree.ElementTree as ET
from pathlib import Path

import paths  # noqa: F401  (sets sys.path)
import altotext
import damage_v2
import gold

LICENCE_OK = re.compile(r"\bCC0\b|\bCC[- ]BY\b|public domain", re.I)
LICENCE_NO = re.compile(r"[- ]ND\b|no ?derivatives", re.I)


# ---------------------------------------------------------------- licences


def licence_allows(licence: str | None) -> bool:
    """A licence string we can use here: an open one, and not No-Derivatives."""
    return bool(licence) and bool(LICENCE_OK.search(licence)) and not LICENCE_NO.search(licence)


def check_set(set_dir: Path, manifest: list[dict]) -> None:
    """Refuse a set with no licence files, an unusable licence, or rows that are not for scoring."""
    lic_dir = set_dir / "LICENSES"
    if not lic_dir.is_dir() or not any(lic_dir.iterdir()):
        raise SystemExit(f"{set_dir}: no LICENSES/ files; refusing to read a set without its licence")
    for m in manifest:
        if m.get("for_scoring") is False:
            raise SystemExit(f"{m['crop_id']}: for_scoring is false (source policy); refusing")
        if not licence_allows(m.get("source_license")):
            raise SystemExit(f"{m['crop_id']}: licence {m.get('source_license')!r} does not allow this use; refusing")


# ---------------------------------------------------------------- damage words


def _zst_reader():
    spec = importlib.util.spec_from_file_location("build_damage_words", paths.ROOT / "scripts" / "data" / "build-damage-words.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.read_zst


def damage_loader(words_dir: Path = paths.DAMAGE_WORDS):
    """A `damage_v2.Models` loader (wordfreq code -> ranked words) that reads the committed lists, so wordfreq is not needed."""
    read_zst = _zst_reader()

    def load(code: str) -> list[str] | None:
        p = words_dir / f"{code}.txt.zst"
        if not p.exists():
            return None
        return [w for w in read_zst(p).split("\n") if w][: damage_v2.TOP_REAL]

    return load


class Damage:
    """The language's damage-words list, and the page damage rate computed against it."""

    def __init__(self, words_dir: Path = paths.DAMAGE_WORDS):
        self.load = damage_loader(words_dir)
        self.models = damage_v2.Models(loader=self.load)
        self._lists: dict[str, frozenset] = {}

    def words(self, lang: str) -> frozenset:
        code = damage_v2.WORDFREQ.get(lang) or damage_v2.LOCAL_WORDLISTS.get(lang) or lang
        if code not in self._lists:
            self._lists[code] = frozenset(self.load(code) or ())
        return self._lists[code]

    def is_real_word(self, lang: str, token: str) -> bool:
        word = damage_v2.token_info(token)[0]
        return word is not None and word in self.words(lang)

    def page_rate(self, lang: str, text: str) -> float | None:
        d = damage_v2.forced(text, lang, self.models)
        return d["damage_rate"] if d else None


# ---------------------------------------------------------------- ALTO


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def parse_words(xml: bytes, image_size: tuple[int, int]) -> list[dict]:
    """LoC's words in reading order: text (a line's HYP kept on its last word), box [x, y, w, h] in image pixels,
    wc, line. The ALTO page extent is mapped onto the image by proportion, as usnm-ocr-survey's position::load does."""
    root = ET.fromstring(xml)
    page = next((e for e in root.iter() if _local(e.tag) == "Page"), None)
    if page is not None and page.get("WIDTH") and page.get("HEIGHT"):
        sx, sy = image_size[0] / float(page.get("WIDTH")), image_size[1] / float(page.get("HEIGHT"))
    else:
        sx = sy = altotext.pixel_scale(xml)
    words, line_no = [], -1
    for line in root.iter():
        if _local(line.tag) != "TextLine":
            continue
        line_no += 1
        first = len(words)
        for el in line:
            tag = _local(el.tag)
            if tag == "String" and el.get("CONTENT"):
                try:
                    x, y, w, h = (float(el.get(k)) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
                except (TypeError, ValueError):
                    continue
                wc = el.get("WC")
                try:
                    wc = float(wc) if wc is not None else None
                except ValueError:
                    wc = None
                if wc is not None and not 0 <= wc <= 1:
                    wc = None
                words.append({"text": el.get("CONTENT"), "box": [x * sx, y * sy, w * sx, h * sy], "wc": wc, "line": line_no})
            elif tag == "HYP" and len(words) > first:
                words[-1]["text"] += el.get("CONTENT") or "-"
    return words


def in_crop(words: list[dict], box: list[int]) -> list[dict]:
    x0, y0, x1, y1 = box
    return [w for w in words if x0 <= w["box"][0] + w["box"][2] / 2 < x1 and y0 <= w["box"][1] + w["box"][3] / 2 < y1]


# ---------------------------------------------------------------- alignment


def truth_tokens(transcription: str) -> list[str]:
    """The transcription's words as the ALTO shows them: a guessed `[?x]` is x, an unreadable `[?]` is dropped,
    and a hyphen at a line's end stays on its word (gold.normalize would join it to the next line)."""
    t = unicodedata.normalize("NFC", gold.GUESS.sub(lambda m: m.group(1), transcription)).replace(gold.UNKNOWN, "")
    return t.split()


def align_words(truth: list[str], ocr: list[str]) -> list[tuple[str, int | None, int | None]]:
    """Word edit alignment: [(op, truth index, ocr index)], op match | sub | ins | del, in order.

    Cost is the word edit count, so it equals gold.edit_distance(truth, ocr); ties go to fewer character edits."""
    big = 1 << 30
    n, m = len(truth), len(ocr)
    cost = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(1, n + 1):
        cost[i][0] = i * big + sum(len(w) for w in truth[:i])
    for j in range(1, m + 1):
        cost[0][j] = j * big + sum(len(w) for w in ocr[:j])
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            t, o = truth[i - 1], ocr[j - 1]
            diag = cost[i - 1][j - 1] + (0 if t == o else big + gold.edit_distance(t, o))
            cost[i][j] = min(diag, cost[i - 1][j] + big + len(t), cost[i][j - 1] + big + len(o))
    ops, i, j = [], n, m
    while i or j:
        t = truth[i - 1] if i else None
        o = ocr[j - 1] if j else None
        if i and j and cost[i][j] == cost[i - 1][j - 1] + (0 if t == o else big + gold.edit_distance(t, o)):
            ops.append(("match" if t == o else "sub", i - 1, j - 1))
            i, j = i - 1, j - 1
        elif i and cost[i][j] == cost[i - 1][j] + big + len(t):
            ops.append(("del", i - 1, None))
            i -= 1
        else:
            ops.append(("ins", None, j - 1))
            j -= 1
    ops.reverse()
    return ops


def word_edits(ops) -> int:
    return sum(op != "match" for op, _, _ in ops)


# ---------------------------------------------------------------- rows


def crop_rows(manifest_row: dict, transcription: str, words: list[dict], lang: str, damage: Damage,
              page_damage: float | None) -> tuple[list[dict], dict]:
    """The per-word rows of one crop, and its counts."""
    crop = in_crop(words, manifest_row["box"])
    truth = truth_tokens(transcription)
    ops = align_words(truth, [w["text"] for w in crop])
    rows, counts = [], {"match": 0, "sub": 0, "ins": 0, "del": 0}
    k = 0
    for op, ti, oj in ops:
        counts[op] += 1
        if oj is None:
            continue
        w = crop[oj]
        tword = truth[ti] if ti is not None else None
        wrong = op != "match"
        rows.append({
            "page_id": manifest_row["page_id"], "crop_id": manifest_row["crop_id"],
            "source_image": manifest_row.get("source_image"), "lang": lang, "line": w["line"], "word_index": k,
            "box": [round(v, 1) for v in w["box"]], "ocr_word": w["text"], "wc": w["wc"],
            "truth_word": tword, "op": op, "label": 0 if wrong else 1,
            "real_word_error": bool(op == "sub" and damage.is_real_word(lang, w["text"])),
            "page_damage": page_damage, "source_license": manifest_row.get("source_license"),
        })
        k += 1
    counts["ocr_words"] = len(crop)
    counts["truth_words"] = len(truth)
    counts["word_edits"] = word_edits(ops)
    return rows, counts


def load_set(set_dir: Path, alto_dir: Path, lang: str | None = None, limit: int | None = None,
             damage: Damage | None = None) -> tuple[list[dict], list[dict]]:
    """(per-word rows, per-crop summaries) of a converted gold set."""
    from PIL import Image

    Image.MAX_IMAGE_PIXELS = None
    manifest = gold.read_jsonl(set_dir / "manifest.jsonl")
    check_set(set_dir, manifest)
    crops = {r["crop_id"]: r for r in gold.read_jsonl(set_dir / "crops.jsonl")}
    damage = damage or Damage()
    pages: dict[str, tuple[list[dict], float | None]] = {}
    rows, summaries = [], []
    for m in manifest[:limit]:
        c = crops[m["crop_id"]]
        if not c.get("transcription") or m.get("box") is None or not m.get("source_image"):
            continue
        stem = m["source_image"].rsplit(".", 1)[0]
        lg = lang or m["lang"]
        key = (stem, lg)
        if key not in pages:
            xml = (alto_dir / f"{stem}.xml").read_bytes()
            with Image.open(alto_dir / f"{stem}.jpg") as im:
                size = im.size
            pages[key] = (parse_words(xml, size), damage.page_rate(lg, altotext.parse(xml, 1.0)[0]))
        words, rate = pages[key]
        r, counts = crop_rows(m, c["transcription"], words, lg, damage, rate)
        rows += r
        summaries.append({"crop_id": m["crop_id"], "page_id": m["page_id"], **counts})
    return rows, summaries


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--set-dir", type=Path, required=True, help="gold.py public-convert output")
    ap.add_argument("--alto-dir", type=Path, required=True, help="<stem>.xml and <stem>.jpg per page")
    ap.add_argument("--out", type=Path, required=True, help="per-word rows (JSON lines)")
    ap.add_argument("--lang", help="override the manifest's language (a catalog code, eng)")
    ap.add_argument("--limit", type=int, help="the first N crops only")
    ap.add_argument("--summary", type=Path, help="per-crop counts (JSON lines)")
    a = ap.parse_args(argv)
    rows, summaries = load_set(a.set_dir, a.alto_dir, a.lang, a.limit)
    gold.write_jsonl(a.out, rows)
    if a.summary:
        gold.write_jsonl(a.summary, summaries)
    wrong = sum(r["label"] == 0 for r in rows)
    print(f"{len(rows)} words from {len(summaries)} crops: {wrong} wrong, "
          f"{sum(r['real_word_error'] for r in rows)} real-word errors", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
