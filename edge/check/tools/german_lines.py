"""Make build_tasks.py's input for German: LoC's ALTO against our Tesseract frak2021 ALTO for the same page.

German pages read by our Tesseract frak2021 model are held from publication until checked (#835); search
serves LoC's text for them, and our ALTO is kept beside it. This takes those layout files (--ours, one <doc id>.xml per page, fetched beforehand) and,
for each page in --pages, fetches LoC's ALTO and the page scan from loc.gov. It picks up to
--lines-per-page lines LoC's ALTO has as Latin-script text (at least --min-words words), drawn at random
with a seed per page, so a rerun picks the same lines. Reading a is LoC's words with their boxes;
reading b is our words whose centres fall in the LoC line's box, left to right. A line is a candidate when at
least 90% of its letters are Latin script (ä, ö, ü, ß, ſ and the other Latin letters included); a line where our text
isn't (a picture's noise, a different script) is left out. A LoC line whose ALTO box is taller than
--merged-line-factor (1.6) times the page's median line covers more than one printed line (#1978): it makes no task,
the run prints the count per page and in total, and DIR/merged-lines.jsonl lists them. The rule uses LoC's boxes, which
are reading a's.

  python3 german_lines.py --pages pages.jsonl --ours DIR --work DIR [--lines-per-page 4] \\
    [--min-words 3] [--seed 1] [--delay 1.0]

--pages is JSON lines, as for serbian_lines.py: {"doc_id": "sn84028600_1910-01-02_ed-1_seq-3",
"page": "sn84028600/1910-01-02/ed-1/seq-3", "alto": "<LoC ALTO URL on tile.loc.gov>", "title": "<newspaper>",
"scan": "<optional local scan>"}. A page's "scan", relative to --scans, is read instead of fetching the scan from
loc.gov. Writes DIR/lines.jsonl and DIR/lines/<line id>.png, and caches downloads in DIR/loc/. Needs Pillow.
"""
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))  # `python3 -I` leaves the script directory off the path
from serbian_lines import LETTER, candidates, make_parser, run  # noqa: E402

THRESHOLD = 0.9


def is_latin(ch: str) -> bool:
    return unicodedata.name(ch, "").startswith("LATIN")


def latin_share(text: str) -> float:
    letters = LETTER.findall(text)
    return sum(1 for ch in letters if is_latin(ch)) / len(letters) if letters else 0.0


def is_german(text: str) -> bool:
    return latin_share(text) >= THRESHOLD


def main() -> None:
    run(make_parser(__doc__.split("\n\n")[0]).parse_args(), is_german, "no German lines to check")


if __name__ == "__main__":
    main()
