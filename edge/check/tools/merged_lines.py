"""Merged lines: an ALTO TextLine whose box covers more than one printed line (#1978).

LoC's layout sometimes boxes two lines of print as one TextLine. A task cut from it shows a line strip and a word crop
that include the line below. A TextLine is flagged when its box height is more than FACTOR times the median TextLine
height of its page. The median is over every TextLine of the page's ALTO, not only the lines a build picks.

A page with fewer than MIN_PAGE_LINES lines of positive height is never flagged: a median over so few lines can be a
merged line itself, and one tall line against two short ones says nothing about the page. Heights are in the ALTO's
own units, so the rule needs no scale. Standard library only.
"""
import json
import statistics

FACTOR = 1.6
MIN_PAGE_LINES = 5


def page_median(heights, min_lines: int = MIN_PAGE_LINES):
    """Median of the positive heights, or None when the page has fewer than `min_lines` of them."""
    pos = [h for h in heights if h and h > 0]
    return statistics.median(pos) if len(pos) >= max(1, min_lines) else None


def is_merged(height, median, factor: float = FACTOR) -> bool:
    """True when `height` is more than `factor` times `median` (None: the page has no usable median)."""
    return median is not None and height > factor * median


def flag_page(lines, factor: float = FACTOR, min_lines: int = MIN_PAGE_LINES):
    """(median or None, [flagged line dicts]) for one page. `lines` are dicts with `id` and `box` [x, y, w, h]; a flagged
    line is returned as {"id", "box", "height", "median", "ratio", "factor"}."""
    median = page_median([ln["box"][3] for ln in lines], min_lines)
    out = []
    for ln in lines:
        h = ln["box"][3]
        if is_merged(h, median, factor):
            out.append({"id": ln["id"], "box": list(ln["box"]), "height": h, "median": median, "ratio": round(h / median, 3), "factor": factor})
    return median, out


class Report:
    """Counts flagged lines per page and in total, and keeps their records for `merged-lines.jsonl`."""

    def __init__(self, factor: float = FACTOR, min_lines: int = MIN_PAGE_LINES):
        self.factor, self.min_lines = factor, min_lines
        self.pages = 0
        self.too_few = 0  # pages with too few lines to have a median
        self.per_page = {}
        self.records = []

    def page(self, page: str, lines) -> set:
        """Flag one page's lines (every TextLine of its ALTO). Returns the ids of the flagged lines."""
        median, flagged = flag_page(lines, self.factor, self.min_lines)
        self.pages += 1
        self.too_few += median is None
        if flagged:
            self.per_page[page] = len(flagged)
        words = {ln["id"]: ln.get("text", "") for ln in lines}
        for f in flagged:
            self.records.append({"page": page, **f, "text": words.get(f["id"], "")})
        return {f["id"] for f in flagged}

    @property
    def total(self) -> int:
        return len(self.records)

    def summary(self) -> dict:
        return {"factor": self.factor, "min_page_lines": self.min_lines, "pages_checked": self.pages,
                "pages_too_few_lines": self.too_few, "merged_lines": self.total, "merged_lines_per_page": dict(sorted(self.per_page.items()))}

    def lines_text(self) -> str:
        return f"{self.total} merged lines left out on {len(self.per_page)} of {self.pages} pages (box taller than {self.factor} times the page median; {self.too_few} pages had under {self.min_lines} lines and were not checked)"

    def write(self, path: str) -> None:
        with open(path, "w", encoding="utf-8") as f:
            for r in self.records:
                f.write(json.dumps(r, ensure_ascii=False, sort_keys=True) + "\n")


def add_arguments(ap) -> None:
    ap.add_argument("--merged-line-factor", type=float, default=FACTOR,
                    help=f"leave out a line whose ALTO box is taller than this times its page's median line (default {FACTOR})")
    ap.add_argument("--min-page-lines", type=int, default=MIN_PAGE_LINES,
                    help=f"a page with fewer lines than this is not checked for merged lines (default {MIN_PAGE_LINES})")
