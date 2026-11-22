"""Shared text handling for the quality-estimation study: normalisation, folding, tokens, lexica,
near-miss search, character language models, garbage and script checks. Standard library plus
rapidfuzz (edit operations) and wordfreq 3.1.1 (frequency lists)."""
import math, os, re, subprocess, unicodedata
from collections import Counter, defaultdict

from rapidfuzz.distance import Levenshtein

# Study language (ISO 639-3) -> (wordfreq code, hunspell dictionaries under hunspell/, Gutenberg dir, script)
LANGS = {
    "eng": ("en", ["en/en_US"], "en", "Latin"),
    "fra": ("fr", ["fr_FR/dictionaries/fr"], "fr", "Latin"),
    "deu": ("de", ["de/de_DE_frami"], "de", "Latin"),
    "fin": ("fi", [], "fi", "Latin"),
    "swe": ("sv", ["sv_SE/dictionaries/sv_SE"], "sv", "Latin"),
    "ces": ("cs", ["cs_CZ/cs_CZ"], "cs", "Latin"),
    "pol": ("pl", ["pl_PL/pl_PL"], "pl", "Latin"),
    "slv": ("sl", ["sl_SI/sl_SI"], "sl", "Latin"),
    "spa": ("es", ["es/es_ES"], "es", "Latin"),
    "dan": ("da", ["da_DK/da_DK"], "da", "Latin"),
    "nor": ("nb", ["no/nb_NO"], "no", "Latin"),
    # Serbian: every token is transliterated to Latin first, so one lexicon and one model serve both scripts.
    "srp": ("sh", ["sr/sr-Latn"], "sr", "Latin"),
    # Russian: pre-1918 letters are folded to the modern ones before lookup and scoring.
    "rus": ("ru", ["ru_RU/ru_RU"], "ru", "Cyrillic"),
    "yid": (None, [], None, "Hebrew"),
}
SYLLABIC = {"ces", "srp", "slv", "pol"}  # words without a vowel letter are normal ("vlk", "smrt", "w")

CYR2LAT = {"а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "ђ": "đ", "е": "e", "ж": "ž", "з": "z", "и": "i", "ј": "j",
           "к": "k", "л": "l", "љ": "lj", "м": "m", "н": "n", "њ": "nj", "о": "o", "п": "p", "р": "r", "с": "s", "т": "t",
           "ћ": "ć", "у": "u", "ф": "f", "х": "h", "ц": "c", "ч": "č", "џ": "dž", "ш": "š",
           # pre-Vuk and Russian-style letters seen in Serbian print
           "ѣ": "e", "і": "i", "ї": "i", "ы": "i", "й": "j", "ъ": "", "ь": "", "щ": "šć", "ю": "ju", "я": "ja", "э": "e", "ё": "jo"}
RUS_OLD = str.maketrans({"ѣ": "е", "і": "и", "ѳ": "ф", "ѵ": "и", "Ѣ": "Е", "І": "И", "Ѳ": "Ф", "Ѵ": "И"})
HEB_POINTS = re.compile("[֑-ׇ]")
HYPHEN_BREAK = re.compile(r"(\w)[-¬⸗⸗]\s*\n\s*(\w)")
WORD = re.compile(r"[^\W\d_]+(?:['’][^\W\d_]+)*")
WS = re.compile(r"\s+")


def norm_text(t: str) -> str:
    """NFC, line-end hyphenation joined."""
    t = unicodedata.normalize("NFC", (t or "").replace("\r", ""))
    return HYPHEN_BREAK.sub(r"\1\2", t)


def fold(w: str, lang: str) -> str:
    """Lower-case lookup form of a word (or a whole text) for `lang`."""
    w = w.replace("ſ", "s").replace("ʒ", "z").lower()
    if lang == "rus":
        w = w.translate(RUS_OLD)
        w = re.sub(r"ъ(?=\W|$)", "", w)
    elif lang == "srp":
        w = "".join(CYR2LAT.get(c, c) for c in w)
    elif lang == "yid":
        w = HEB_POINTS.sub("", w).replace("װ", "וו").replace("ױ", "וי").replace("ײ", "יי")
    return w


def words(t: str):
    return WORD.findall(norm_text(t))


def cer_text(t: str, folded: bool) -> str:
    """The CER comparison form: whitespace runs collapsed (legacy-ws-v1); folded also maps long s,
    the double hyphen and superscript e as `gpu-study score --fold` does."""
    t = norm_text(t)
    if folded:
        t = t.replace("ſ", "s").replace("⸗", "-").replace("⸗", "-").replace("ͤ", "̈").replace("ʒ", "z").replace("¬", "-")
        t = unicodedata.normalize("NFC", t)
    return WS.sub(" ", t).strip()


def cer(ref: str, hyp: str, folded=True):
    r, h = cer_text(ref, folded), cer_text(hyp, folded)
    if not r:
        return None
    return Levenshtein.distance(r, h) / len(r)


def script_of(ch: str):
    try:
        n = unicodedata.name(ch)
    except ValueError:
        return None
    for s in ("LATIN", "CYRILLIC", "HEBREW", "GREEK"):
        if n.startswith(s):
            return s.title()
    return "Other"


def base_letter(ch: str) -> str:
    return unicodedata.normalize("NFD", ch)[0].lower()


VOWELS = set("aeiouyæøœ")


def garbage_flags(tok: str, lang: str):
    """Taghva-style garbage-string rules for one whitespace token; returns the set of rules it breaks."""
    f = set()
    t = tok.strip(".,;:!?\"'()[]{}«»„“”‘’-–—*")
    if not t:
        return f
    if len(t) > 25:
        f.add("long")
    if re.search(r"(.)\1\1\1", t):
        f.add("repeat")
    inner = t[1:-1]
    if sum(1 for c in inner if not c.isalnum()) > 2:
        f.add("punct")
    alnum = sum(c.isalnum() for c in t)
    if len(t) >= 3 and alnum < len(t) - alnum:
        f.add("symbols")
    lo, up = sum(c.islower() for c in t), sum(c.isupper() for c in t)
    if lo and up and (up > lo or re.search(r"[^\W\d_]*[a-zß-ÿа-я][A-ZÀ-ÞА-Я]", t)):
        f.add("case")
    if re.search(r"[^\W\d_]\d+[^\W\d_]", t):
        f.add("digits")
    letters = [c for c in t if c.isalpha()]
    if letters and all(script_of(c) == "Latin" for c in letters) and lang not in SYLLABIC:
        bl = "".join(base_letter(c) for c in letters)
        if len(bl) >= 4 and not any(c in VOWELS for c in bl):
            f.add("novowel")
        if re.search(r"[^aeiouyæøœ\W]{6,}", bl):
            f.add("consonants")
    return f


class Lexicon:
    """Word lookup for one language: wordfreq's full list (or the repo's Yiddish list) plus Hunspell.
    Hunspell answers are filled in batches by `prime` (the command-line checker, `-l`)."""

    def __init__(self, lang, res_dir, yi_list=None):
        self.lang = lang
        wf, dics, _, self.script = LANGS[lang]
        self.freq = {}
        if wf:
            import wordfreq
            for w, p in wordfreq.get_frequency_dict(wf).items():
                k = fold(w, lang)
                if k and WORD.fullmatch(k):
                    self.freq[k] = max(p, self.freq.get(k, 0.0))
        if lang == "yid" and yi_list:
            for i, w in enumerate(open(yi_list, encoding="utf-8").read().split()):
                self.freq[fold(w, lang)] = 0.1 / (i + 1)
        self.dics = [os.path.join(res_dir, "hunspell", d) for d in dics]
        self.hun = {}

    def prime(self, tokens):
        todo = sorted({t for t in tokens if t not in self.freq and t not in self.hun})
        if not todo:
            return
        ok = set()
        for d in self.dics:
            enc = "utf-8"
            for line in open(d + ".aff", encoding="latin-1"):
                if line.startswith("SET "):
                    enc = line.split()[1].strip()
                    break
            enc = {"ISO8859-1": "latin-1", "ISO8859-2": "iso8859-2", "KOI8-R": "koi8-r", "microsoft-cp1251": "cp1251"}.get(enc, enc)

            def fits(t):
                try:
                    t.encode(enc)
                    return True
                except (UnicodeEncodeError, LookupError):
                    return False
            cand = [t for t in todo if fits(t)]  # a word the dictionary can't encode is not in it
            for variant in (lambda s: s, lambda s: s[:1].upper() + s[1:]):
                inp = "\n".join(variant(t) for t in cand) + "\n"
                out = subprocess.run(["hunspell", "-d", d, "-l", "-i", "utf-8"], input=inp.encode("utf-8"),
                                     stdout=subprocess.PIPE, check=True).stdout.decode("utf-8", "replace")
                bad = set(out.split("\n"))
                ok |= {t for t in cand if variant(t) not in bad}
        for t in todo:
            self.hun[t] = t in ok

    def known(self, t):
        return t in self.freq or self.hun.get(t, False)


class NearMiss:
    """Lexicon words within edit distance 1 (any token) or 2 (tokens of 7+ letters), found with a
    one-deletion index over the `top` most frequent words (SymSpell's method)."""

    def __init__(self, lex: Lexicon, top=150_000):
        words_ = sorted(lex.freq, key=lambda w: -lex.freq[w])[:top]
        self.idx = defaultdict(list)
        for w in words_:
            if len(w) < 2:
                continue
            self.idx[w].append(w)
            for i in range(len(w)):
                self.idx[w[:i] + w[i + 1:]].append(w)

    @staticmethod
    def _dels(t):
        return {t[:i] + t[i + 1:] for i in range(len(t))}

    def candidates(self, t):
        keys = {t} | self._dels(t)
        maxd = 1
        if len(t) >= 7:
            maxd = 2
            for d in list(keys):
                keys |= self._dels(d)
        out = {}
        for k in keys:
            for w in self.idx.get(k, ()):
                if w != t and w not in out:
                    d = Levenshtein.distance(t, w, score_cutoff=maxd)
                    if d <= maxd:
                        out[w] = d
        return out


def edit_ops(src: str, dst: str):
    """Character edit operations turning `src` (the lexicon word) into `dst` (what was read), as
    (expected, read) pairs: substitution (a, b), deletion (a, ""), insertion ("", b)."""
    ops = []
    for op in Levenshtein.editops(src, dst):
        if op.tag == "replace":
            ops.append((src[op.src_pos], dst[op.dest_pos]))
        elif op.tag == "delete":
            ops.append((src[op.src_pos], ""))
        else:
            ops.append(("", dst[op.dest_pos]))
    return ops


class CharLM:
    """Interpolated Witten-Bell character n-gram model."""

    def __init__(self, order=5):
        self.n = order
        self.cnt = Counter()
        self.ctx = Counter()
        self.types = Counter()
        self.vocab = set()

    def train(self, text):
        text = "\x02" * (self.n - 1) + text
        self.vocab |= set(text)
        cnt = self.cnt
        for i in range(self.n - 1, len(text)):
            for k in range(1, self.n + 1):
                cnt[text[i - k + 1:i + 1]] += 1
        for g, c in cnt.items():
            h = g[:-1]
            self.ctx[h] += c
            self.types[h] += 1
        self.V = len(self.vocab) + 1

    def logprob(self, text):
        """Total log2 probability of `text` and the number of characters scored."""
        text = "\x02" * (self.n - 1) + text
        cnt, ctx, types = self.cnt, self.ctx, self.types
        lp = 0.0
        base = 1.0 / self.V
        for i in range(self.n - 1, len(text)):
            p = base
            c = text[i]
            for k in range(1, self.n + 1):
                h = text[i - k + 1:i]
                ch = ctx.get(h, 0)
                if ch == 0:
                    break
                th = types[h]
                p = (cnt.get(h + c, 0) + th * p) / (ch + th)
            lp += math.log2(p)
        return lp, len(text) - (self.n - 1)


def lm_text(t, lang):
    t = fold(norm_text(t), lang)
    t = re.sub(r"\d", "0", t)
    return WS.sub(" ", t).strip()


def gutenberg_body(raw: str) -> str:
    s = raw.find("*** START OF")
    s = raw.find("\n", s) + 1 if s >= 0 else 0
    e = raw.find("*** END OF")
    return raw[s:e if e > 0 else len(raw)]
