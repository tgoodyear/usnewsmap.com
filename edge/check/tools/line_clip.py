"""Cut a word crop that reaches into another printed line to its own line (the Cherokee sets; en-read's rule, #2122).

tess_pick.py --clip-band does this for en-read. The Cherokee sets have one box per word (LoC's), and their LoC TextLine
box is the tall one when a word box in it reaches into the next line, so the printed line is taken from the line's word
boxes instead: the median word height `med`, and the median top and median bottom of the words no taller than
TALL * med. The word's own box is added to that band when it is no taller than TALL * med, so an offset word keeps its
letters. A side of the crop is cut only when it reaches more than half a word height past the band, and then to the
band plus a tenth of a word height; and the cut is kept only when it makes the crop more than `shrink` shorter.
Standard library only.
"""

TALL = 1.6  # a word box taller than this many median word heights reaches into another line (as merged_lines.py, tess_pick.py)
REACH = 0.5  # a side is cut only when it reaches more than this many word heights past the line
MARGIN = 0.1  # and is cut to the line plus this many word heights
SHRINK = 0.25  # the cut is kept only when it makes the crop more than this much shorter


def _median(v: list) -> float:
    """The median; for an even count, the mean of the middle pair (so two short boxes and two tall ones give a height
    between them, and the tall ones are left out of the band)."""
    s = sorted(v)
    m = len(s) // 2
    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2


def printed_band(boxes: list, own: list = None):
    """(y0, y1, med): the printed line of a TextLine from its words' boxes [x, y, w, h], in the boxes' units, with the
    median word height; `own`, the word's own box, widens the band when it is no taller than TALL * med. None when
    there are no boxes."""
    boxes = [b for b in boxes if b and b[3] > 0]
    if not boxes:
        return None
    med = _median([b[3] for b in boxes])
    normal = [b for b in boxes if b[3] <= TALL * med]
    y0, y1 = _median([b[1] for b in normal]), _median([b[1] + b[3] for b in normal])
    if own and 0 < own[3] <= TALL * med:
        y0, y1 = min(y0, own[1]), max(y1, own[1] + own[3])
    return y0, y1, med


def clip_span(top: float, bottom: float, band, shrink: float = SHRINK):
    """(top, bottom, cut): the crop's vertical span, cut to the line `band` (printed_band's result) on a side that reaches
    more than REACH word heights past it, to the band plus MARGIN word heights. `cut` is False, and the span unchanged,
    when nothing reaches that far or the cut makes the crop no more than `shrink` shorter."""
    if band is None:
        return top, bottom, False
    y0, y1, med = band
    t = max(top, y0 - MARGIN * med) if y0 - top > REACH * med else top
    b = min(bottom, y1 + MARGIN * med) if bottom - y1 > REACH * med else bottom
    if b <= t or (t, b) == (top, bottom) or (b - t) >= (1 - shrink) * (bottom - top):
        return top, bottom, False
    return t, b, True
