"""A page's text in the form the BOINC import stores it, for scripts that score two texts of one page (#1919).

The import stores `normalize_ocr_for(text, Hyphenation::for_page(engine tag, language))`
(`usnm_core::text::stored_form`). A raw Tesseract re-read still has a word broken at a line end
(`Zei⸗` and `tung`), which the published text has rejoined (`Zeitung`), so scoring one against the
other counts the rejoining as OCR error. Put both sides through `comparable` first. Text already in
this form is unchanged by it, so it is safe whichever form a side is in.

This is a port of the Rust function. `crates/usnm-core/tests/fixtures/stored_form.json` holds the
cases both sides are tested against (`test_ocr_stored_form.py` here, `stored_form_matches_the_shared_fixtures`
in usnm-core). Differences from Rust at the edges of Unicode (a letter that is Alphabetic but not a
letter category) are not covered.
"""
import re
import unicodedata

LIGATURES = {"ﬀ": "ff", "ﬁ": "fi", "ﬂ": "fl", "ﬃ": "ffi", "ﬄ": "ffl", "ﬅ": "st", "ﬆ": "st"}
MARKS = "-⸗¬‐—"
FRAKTUR_FALLBACK = ("frak2021", "deu_latf")
JA = ((0x3005, 0x3007), (0x3041, 0x3096), (0x309D, 0x309F), (0x30A1, 0x30FA), (0x30FC, 0x30FF),
      (0x31F0, 0x31FF), (0x3400, 0x4DBF), (0x4E00, 0x9FFF), (0xF900, 0xFAFF), (0xFF66, 0xFF9F),
      (0x20000, 0x2FA1F))


def is_ja(c):
    return any(lo <= ord(c) <= hi for lo, hi in JA)


def is_fraktur_engine(ocr_engine):
    """Fraktur when a model the page was read with is a Fraktur one (`Hyphenation::for_engine`).

    A full tag has `key=value` fields after the first (`tesseract/5.5.0 profile=frak2021 ...`); anything
    else, a bare profile or the older `tesseract-5.5.1/frak2021`, is Fraktur if it names `frak2021` or
    `deu_latf`.
    """
    fields = ocr_engine.split(" ")[1:]
    if any("=" in f for f in fields):
        for f in fields:
            key, _, value = f.partition("=")
            if key == "profile":
                return any(m.startswith("frak") or m == "deu_latf" for m in value.split("+"))
    return any(name in ocr_engine for name in FRAKTUR_FALLBACK)


def is_fraktur(ocr_engine, language):
    """`Hyphenation::for_page`: Fraktur if the engine says so, or the language is German."""
    return is_fraktur_engine(ocr_engine) or language == "deu"


def _letter(c):
    return c.isalpha() and not is_ja(c)


def _joins(mark, before, after, fraktur):
    if mark == "-":
        return before.isalpha() and after.isalpha()
    if mark == "—":
        return fraktur and _letter(before) and after.islower()
    return _letter(before) and _letter(after)


def _is_blank(c):
    return c != "\n" and c.isspace()


def _rejoin(s, fraktur):
    out, i, n = [], 0, len(s)
    while i < n:
        if i > 0 and s[i] in MARKS:
            j = i + 1
            while j < n and _is_blank(s[j]):
                j += 1
            if j < n and s[j] == "\n":
                k = j + 1
                while k < n and s[k].isspace():
                    k += 1
                if k < n and _joins(s[i], s[i - 1], s[k], fraktur):
                    i = k
                    continue
        out.append(s[i])
        i += 1
    return "".join(out)


def _collapse(s):
    out, space, newline = [], False, False
    for c in s:
        if c == "\n":
            newline, space = True, False
        elif _is_blank(c):
            space = space or not newline
        else:
            if out:
                out.append("\n" if newline else " " if space else "")
            space = newline = False
            out.append(c)
    return "".join(out)


def stored_form(raw, ocr_engine, language):
    """`usnm_core::text::stored_form`."""
    nfc = unicodedata.normalize("NFC", raw)
    kept = []
    for c in nfc:
        if c == "ſ":
            kept.append("s")
        elif c in "\n\t ":
            kept.append(c)
        elif unicodedata.category(c) != "Cc":
            kept.append(LIGATURES.get(c, c))
    return _collapse(_rejoin("".join(kept), is_fraktur(ocr_engine, language)))


def settled(raw, ocr_engine, language):
    """`stored_form` applied until it changes nothing: one pass is not always a fixpoint (a ligature
    expanded before a combining accent), so a raw text and its published form settle to the same text."""
    text = stored_form(raw, ocr_engine, language)
    for _ in range(4):
        nxt = stored_form(text, ocr_engine, language)
        if nxt == text:
            break
        text = nxt
    return text


def comparable(a, b, ocr_engine, language):
    """Both texts of one page in the same form, ready to score against each other; either may be raw or published."""
    return settled(a, ocr_engine, language), settled(b, ocr_engine, language)
