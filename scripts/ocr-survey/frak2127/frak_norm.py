"""Text normalisation for #2127, used for training labels and for scoring.

The issue's rule, in this order: NFC; long s to s; r rotunda to r; every quote mark to ";
every dash and hyphen to -; whitespace runs collapsed to one space and the ends stripped.
"""
import re
import unicodedata

# Every quote mark, double and single, straight and curly, low and high, guillemets and primes.
QUOTES = "\"'“”„‟«»‹›‘’‚‛″〃＂＇"
# Every dash and hyphen: the issue's - — ⸗ ¬ = plus the other dash code points that mean the same.
DASHES = "-—⸗¬=‐‑‒–―−⹀﹣－＝"
_TABLE = str.maketrans({**{c: '"' for c in QUOTES}, **{c: "-" for c in DASHES}, "ſ": "s", "ꝛ": "r"})
_WS = re.compile(r"\s+")


def normalise(text: str) -> str:
    return _WS.sub(" ", unicodedata.normalize("NFC", text).translate(_TABLE)).strip()
