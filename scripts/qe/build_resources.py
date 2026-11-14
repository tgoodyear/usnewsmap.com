#!/usr/bin/env python3
"""Builds the training side of the reference-free OCR quality measure (`usnm_core::qe`): per
language, the full lexicon as a word list and the character 5-gram model's counts.
`usnm-boinc quality qe-pack` turns them into the shipped artifact.

    build_resources.py --res DIR --yi-list FILE --langs eng,fra --out DIR [--lm-chars 3000000]

--res is the lake's qe-resources directory (hunspell/, gutenberg/, wikisource/, SOURCES.tsv) with
wordfreq 3.1.1 installed in the running Python. For each language OUT/<lang>/ gets:

- words.txt: the full lexicon, folded, sorted, one word per line: wordfreq's whole list, every
  form the Hunspell dictionary expands to (`unmunch`), and every word of the period text the
  Hunspell checker accepts as written or capitalised (compounds and forms `unmunch` can't
  expand). This is the study's lexicon (docs/reports/2026-11-ocr-quality-estimation), which
  asked the checker about each word it met, made static.
- words-wordfreq.txt: wordfreq's list alone, for languages whose only Hunspell licence is GPL.
- charlm.jsonl: the character model's counts, one JSON array per n-gram or history:
  [text, count as an n-gram, count of continuations as a history, distinct continuations].
- meta.json: the model's order and vocabulary size, how much period text went in, and what
  it came from.

The normalisation, the training text and the model are the study's (`qe_lib.py` and
`metrics.py --lm-sources gutenberg+wikisource`), copied here so that the model is the one the
study validated: Gutenberg books first, topped up from Wikisource to 3 million characters, plus a
frequency-weighted wordfreq sample. Nothing here reads any reader's output.
"""
import argparse, glob, json, os, random, re, subprocess, sys, unicodedata
from collections import Counter

# Study language (ISO 639-3) -> (wordfreq code, hunspell dictionaries under hunspell/, period-text code)
LANGS = {
    "eng": ("en", ["en/en_US"], "en"),
    "fra": ("fr", ["fr_FR/dictionaries/fr"], "fr"),
    "deu": ("de", ["de/de_DE_frami"], "de"),
    "fin": ("fi", [], "fi"),
    "swe": ("sv", ["sv_SE/dictionaries/sv_SE"], "sv"),
    "ces": ("cs", ["cs_CZ/cs_CZ"], "cs"),
    "pol": ("pl", ["pl_PL/pl_PL"], "pl"),
    "slv": ("sl", ["sl_SI/sl_SI"], "sl"),
    "spa": ("es", ["es/es_ES"], "es"),
    "dan": ("da", ["da_DK/da_DK"], "da"),
    "nor": ("nb", ["no/nb_NO"], "no"),
    "srp": ("sh", ["sr/sr-Latn"], "sr"),
    "rus": ("ru", ["ru_RU/ru_RU"], "ru"),
    "yid": (None, [], None),
}

CYR2LAT = {"а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "ђ": "đ", "е": "e", "ж": "ž", "з": "z", "и": "i", "ј": "j",
           "к": "k", "л": "l", "љ": "lj", "м": "m", "н": "n", "њ": "nj", "о": "o", "п": "p", "р": "r", "с": "s", "т": "t",
           "ћ": "ć", "у": "u", "ф": "f", "х": "h", "ц": "c", "ч": "č", "џ": "dž", "ш": "š",
           "ѣ": "e", "і": "i", "ї": "i", "ы": "i", "й": "j", "ъ": "", "ь": "", "щ": "šć", "ю": "ju", "я": "ja", "э": "e", "ё": "jo"}
RUS_OLD = str.maketrans({"ѣ": "е", "і": "и", "ѳ": "ф", "ѵ": "и", "Ѣ": "Е", "І": "И", "Ѳ": "Ф", "Ѵ": "И"})
HEB_POINTS = re.compile("[֑-ׇ]")
HYPHEN_BREAK = re.compile(r"(\w)[-¬⸗⸗]\s*\n\s*(\w)")
WORD = re.compile(r"[^\W\d_]+(?:['’][^\W\d_]+)*")
WS = re.compile(r"\s+")


def norm_text(t):
    t = unicodedata.normalize("NFC", (t or "").replace("\r", ""))
    return HYPHEN_BREAK.sub(r"\1\2", t)


def fold(w, lang):
    w = w.replace("ſ", "s").replace("ʒ", "z").lower()
    if lang == "rus":
        w = w.translate(RUS_OLD)
        w = re.sub(r"ъ(?=\W|$)", "", w)
    elif lang == "srp":
        w = "".join(CYR2LAT.get(c, c) for c in w)
    elif lang == "yid":
        w = HEB_POINTS.sub("", w).replace("װ", "וו").replace("ױ", "וי").replace("ײ", "יי")
    return w


def lm_text(t, lang):
    t = fold(norm_text(t), lang)
    t = re.sub(r"\d", "0", t)
    return WS.sub(" ", t).strip()


def gutenberg_body(raw):
    s = raw.find("*** START OF")
    s = raw.find("\n", s) + 1 if s >= 0 else 0
    e = raw.find("*** END OF")
    return raw[s:e if e > 0 else len(raw)]


def dic_encoding(path):
    enc = "utf-8"
    for line in open(path + ".aff", encoding="latin-1"):
        if line.startswith("SET "):
            enc = line.split()[1].strip()
            break
    return {"ISO8859-1": "latin-1", "ISO8859-2": "iso8859-2", "KOI8-R": "koi8-r", "microsoft-cp1251": "cp1251"}.get(enc, enc)


def wordfreq_lexicon(lang, yi_list):
    """The study's `Lexicon.freq`: folded wordfreq words (or the Yiddish list) with their frequency,
    in wordfreq's order."""
    wf = LANGS[lang][0]
    freq = {}
    if wf:
        import wordfreq
        for w, p in wordfreq.get_frequency_dict(wf).items():
            k = fold(w, lang)
            if k and WORD.fullmatch(k):
                freq[k] = max(p, freq.get(k, 0.0))
    if lang == "yid":
        for i, w in enumerate(open(yi_list, encoding="utf-8").read().split()):
            freq[fold(w, lang)] = 0.1 / (i + 1)
    return freq


def unmunch(dic):
    """Every form the dictionary expands to, folded later; flags and morphology dropped."""
    enc = dic_encoding(dic)
    out = subprocess.run(["unmunch", dic + ".dic", dic + ".aff"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         check=False).stdout.decode(enc, "replace")
    forms = set()
    for line in out.split("\n"):
        w = line.split("/")[0].split("\t")[0].strip()
        if w and " " not in w:
            forms.add(w)
    return forms


def hunspell_accepts(dic, tokens):
    """The study's `Lexicon.prime`: which folded tokens the checker accepts as written or with a capital."""
    enc = dic_encoding(dic)

    def fits(t):
        try:
            t.encode(enc)
            return True
        except (UnicodeEncodeError, LookupError):
            return False
    cand = sorted(t for t in tokens if fits(t))
    ok = set()
    for variant in (lambda s: s, lambda s: s[:1].upper() + s[1:]):
        for i in range(0, len(cand), 200_000):
            part = cand[i:i + 200_000]
            inp = "\n".join(variant(t) for t in part) + "\n"
            out = subprocess.run(["hunspell", "-d", dic, "-l", "-i", "utf-8"], input=inp.encode("utf-8"),
                                 stdout=subprocess.PIPE, check=True).stdout.decode("utf-8", "replace")
            bad = set(out.split("\n"))
            ok |= {t for t in part if variant(t) not in bad}
    return ok


def period_text(res, lang, lm_chars):
    """The study's training text for `--lm-sources gutenberg+wikisource`."""
    g = LANGS[lang][2]
    parts = []
    if g:
        for p in sorted(glob.glob(os.path.join(res, "gutenberg", g, "*.txt"))):
            parts.append(lm_text(gutenberg_body(open(p, encoding="utf-8", errors="replace").read()), lang))
    text = " ".join(parts)[:lm_chars]
    ws = os.path.join(res, "wikisource", f"{g or 'yi'}.txt")
    if len(text) < lm_chars and os.path.exists(ws):
        text = (text + " " + lm_text(open(ws, encoding="utf-8").read(), lang))[:lm_chars]
    return text


def corpus_words(res, lang):
    """Word types of all the period text we hold for the language (not cut to the model's 3 million)."""
    g = LANGS[lang][2]
    paths = sorted(glob.glob(os.path.join(res, "gutenberg", g, "*.txt"))) if g else []
    ws = os.path.join(res, "wikisource", f"{g or 'yi'}.txt")
    if os.path.exists(ws):
        paths.append(ws)
    types = set()
    for p in paths:
        raw = open(p, encoding="utf-8", errors="replace").read()
        if "gutenberg" in p:
            raw = gutenberg_body(raw)
        types.update(fold(w, lang) for w in WORD.findall(norm_text(raw)))
    return types


def train_counts(text, n=5):
    """The study's `CharLM.train`: counts of every n-gram of length 1..n over the padded text, and for
    every history the sum of its continuations' counts and how many distinct ones there are."""
    text = "\x02" * (n - 1) + text
    vocab = set(text)
    cnt = Counter()
    for i in range(n - 1, len(text)):
        for k in range(1, n + 1):
            cnt[text[i - k + 1:i + 1]] += 1
    ctx, types = Counter(), Counter()
    for g, c in cnt.items():
        h = g[:-1]
        ctx[h] += c
        types[h] += 1
    return cnt, ctx, types, len(vocab) + 1


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--res", required=True)
    ap.add_argument("--yi-list", required=True)
    ap.add_argument("--langs", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--lm-chars", type=int, default=3_000_000)
    a = ap.parse_args(argv)
    for lang in a.langs.split(","):
        out = os.path.join(a.out, lang)
        os.makedirs(out, exist_ok=True)
        freq = wordfreq_lexicon(lang, a.yi_list)
        words = set(freq)
        with open(os.path.join(out, "words-wordfreq.txt"), "w", encoding="utf-8") as f:
            f.writelines(w + "\n" for w in sorted(words))
        hun_forms = hun_corpus = 0
        for d in LANGS[lang][1]:
            dic = os.path.join(a.res, "hunspell", d)
            forms = {fold(w, lang) for w in unmunch(dic)}
            forms = {w for w in forms if WORD.fullmatch(w)}
            hun_forms += len(forms - words)
            words |= forms
            todo = {t for t in corpus_words(a.res, lang) if t not in words}
            acc = hunspell_accepts(dic, todo)
            hun_corpus += len(acc)
            words |= acc
        with open(os.path.join(out, "words.txt"), "w", encoding="utf-8") as f:
            f.writelines(w + "\n" for w in sorted(words))

        text = period_text(a.res, lang, a.lm_chars)
        # A frequency-weighted sample of the word list, so every language has in-vocabulary coverage.
        rnd = random.Random(7)
        top = sorted(freq.items(), key=lambda kv: -kv[1])[:30000]
        bag = []
        for w, p in top:
            bag += [w] * max(1, round(p * 300_000))
        rnd.shuffle(bag)
        train = text + " " + " ".join(bag)[: max(500_000, a.lm_chars - len(text))]
        cnt, ctx, types, vocab = train_counts(train)
        keys = sorted(set(cnt) | set(ctx))
        with open(os.path.join(out, "charlm.jsonl"), "w", encoding="utf-8") as f:
            for k in keys:
                f.write(json.dumps([k, cnt.get(k, 0), ctx.get(k, 0), types.get(k, 0)], ensure_ascii=False) + "\n")
        meta = {"lang": lang, "order": 5, "vocab": vocab, "training_chars": len(train), "period_chars": len(text),
                "lexicon_words": len(words), "wordfreq_words": len(freq), "hunspell_forms": hun_forms,
                "hunspell_corpus_words": hun_corpus, "entries": len(keys)}
        json.dump(meta, open(os.path.join(out, "meta.json"), "w"), indent=1, sort_keys=True)
        print(json.dumps(meta), flush=True)
        del cnt, ctx, types, keys


if __name__ == "__main__":
    sys.exit(main())
