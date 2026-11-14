"""The names shown under Cherokee letters, for the "Show letter names" option (languages/chr*.json, `labels`).

A name is the syllable in the letter's Unicode character name ("CHEROKEE LETTER SV" is "sv"), the same derivation
lookalike.label() applies to the label stored with a flagged pair, so the two never disagree. Nothing is invented: a
letter without such a name is not in the map. The map covers the syllabary letters U+13A0 to U+13F5 (86 letters); the lowercase letters (U+13F8 to U+13FD, U+AB70 to U+ABBF) are not in any task, so they are left out.

  python3 cherokee_labels.py --check    exits 1 when a chr language file's `labels` differ from the Unicode names
  python3 cherokee_labels.py --write    rewrites `labels` in languages/chr.json, chr-lines.json and chr-rare.json
"""
import argparse
import json
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import lookalike as la  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
LANGUAGES = os.path.join(HERE, "..", "languages")
FILES = ("chr.json", "chr-lines.json", "chr-rare.json")
RANGE = range(0x13A0, 0x13F6)


def names() -> dict:
    out = {}
    for cp in RANGE:
        ch = chr(cp)
        if not unicodedata.name(ch, "").startswith("CHEROKEE LETTER "):
            continue
        out[ch] = la.label(ch)
    return dict(sorted(out.items()))


def render(cfg: dict) -> str:
    return json.dumps(cfg, ensure_ascii=False, indent=2, sort_keys=True) + "\n"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    ap.add_argument("--languages-dir", default=LANGUAGES)
    a = ap.parse_args()
    want, bad = names(), []
    for f in FILES:
        path = os.path.join(a.languages_dir, f)
        with open(path, encoding="utf-8") as fh:
            cfg = json.load(fh)
        if cfg.get("labels") == want:
            continue
        if a.write:
            cfg["labels"] = want
            with open(path, "w", encoding="utf-8") as fh:
                fh.write(render(cfg))
        else:
            bad.append(f)
    if bad:
        raise SystemExit(f"labels differ from the Unicode names in {', '.join(bad)}; run with --write")


if __name__ == "__main__":
    main()
