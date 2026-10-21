#!/usr/bin/env python3
"""Export the word lists the damage metric scores pages against (#296, 12
Step 4): crates/usnm-core/data/damage-words/<code>.txt.zst
(zstd -19: needs the zstd command line tool; build.rs decompresses them).

    scripts/build-damage-words.py [--out-dir path] [--check]

Needs wordfreq 3.1.1 (`pip install wordfreq==3.1.1`, the version the OCR
audit pins in ja-ocr/requirements.txt).

For each wordfreq code in `ja-ocr/damage_v2.py`'s WORDFREQ table, the list is
`wordfreq.top_n_list(code, 5000)`, the call `damage_v2.py` makes
(`wordfreq_ranked`): wordfreq's 5,000 most frequent words, most frequent
first, as wordfreq stores them. Hawaiian (`haw`) and Yiddish (`yi`) have no
wordfreq list; theirs are copied from ja-ocr/wordlists/, first 5,000 words,
as `damage_v2.py` reads them. One word per line, in rank order; the order
matters, since the metric takes the top 100 and top 20 from the front.

`usnm_core::damage` (cargo feature `damage`) embeds these files. Nothing in
the pipeline runs this script. `--check` compares with the committed files
instead of writing them (exit 1 when any differs or is missing, or when the
directory holds a file this script doesn't write).
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT_DIR = ROOT / "crates" / "usnm-core" / "data" / "damage-words"
LOCAL_DIR = ROOT / "ja-ocr" / "wordlists"
WORDFREQ_VERSION = "3.1.1"
TOP_REAL = 5000


def zstd_cli() -> str:
    """The `zstd` command line tool, which compresses the lists (`zstd -19`)."""
    path = shutil.which("zstd")
    if path is None:
        raise SystemExit("the zstd command line tool is needed and is not on PATH (apt install zstd)")
    return path


def write_zst(path: Path, text: str) -> None:
    """Write `text` to `path` compressed with `zstd -19`."""
    path.write_bytes(subprocess.run([zstd_cli(), "-19", "-q", "-c"], input=text.encode("utf-8"),
                                    stdout=subprocess.PIPE, check=True).stdout)


def read_zst(path: Path) -> str:
    """The text of a `zstd`-compressed file."""
    return subprocess.run([zstd_cli(), "-d", "-q", "-c", str(path)], stdout=subprocess.PIPE,
                          check=True).stdout.decode("utf-8")


# damage_v2.py's WORDFREQ values and LOCAL_WORDLISTS values, as wordfreq codes.
# usnm_core::lang holds the same table; a test there checks that every code
# it names has a file here.
WORDFREQ_CODES = ["ar", "bg", "cs", "da", "de", "el", "en", "es", "fi", "fil", "fr", "he", "hu", "is", "it",
                  "lt", "lv", "nb", "nl", "pl", "pt", "ro", "ru", "sh", "sk", "sl", "sv", "uk"]
LOCAL_CODES = ["haw", "yi"]


def wordfreq_list(code: str) -> list[str]:
    import wordfreq

    if code not in wordfreq.available_languages("best"):
        raise SystemExit(f"wordfreq has no list for {code!r}")
    return wordfreq.top_n_list(code, TOP_REAL)


def local_list(code: str) -> list[str]:
    text = (LOCAL_DIR / f"{code}.txt").read_text(encoding="utf-8")
    return [w for w in text.split("\n") if w][:TOP_REAL]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out-dir", type=Path, default=OUT_DIR, help="where to write the lists")
    ap.add_argument("--check", action="store_true", help="compare with the files in --out-dir instead of writing them")
    a = ap.parse_args()

    from importlib.metadata import version

    have = version("wordfreq")
    if have != WORDFREQ_VERSION:
        raise SystemExit(f"wordfreq {WORDFREQ_VERSION} is needed, not {have}")
    lists = {code: wordfreq_list(code) for code in WORDFREQ_CODES}
    lists.update({code: local_list(code) for code in LOCAL_CODES})
    differs = False
    for code, words in sorted(lists.items()):
        if any(not w or any(c.isspace() for c in w) for w in words):
            raise SystemExit(f"{code}: a word is empty or holds whitespace")
        path = a.out_dir / f"{code}.txt.zst"
        text = "".join(w + "\n" for w in words)
        if a.check:
            same = path.exists() and read_zst(path) == text
            differs |= not same
            print(f"{path}: {'up to date' if same else 'differs'}")
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            write_zst(path, text)
            print(f"{path}: {len(words)} words")
    if a.check and a.out_dir.exists():
        extra = sorted(p.name for p in a.out_dir.iterdir() if not (p.name.endswith(".txt.zst") and p.name[:-8] in lists))
        for name in extra:
            print(f"{a.out_dir / name}: not written by this script")
        differs |= bool(extra)
    return 1 if differs else 0


if __name__ == "__main__":
    sys.exit(main())
