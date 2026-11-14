#!/usr/bin/env python3
"""Packs the quality measure's resource files (`usnm_core::qe`), one per language, each with its
notice, and writes the lock the BOINC tools pin them by.

    release.py --res DIR --build DIR --qe BIN --out DIR --lock FILE

--res is the lake's qe-resources directory (with licences/ and the Hunspell READMEs), --build the
output of build_resources.py, --qe the `qe` example binary (`cargo build --release -p usnm-core
--features qe --example qe`). Each language's file is OUT/<lang>.qe, with OUT/<lang>.NOTICE.txt
beside it (the same text the file carries). The model keeps 4- and 5-grams seen 3 times or more
(the pruning ocr/qe/README.md validates). The lock (ocr/qe/resources.lock.json) is rewritten with
each file's name, size, sha256, licence and lexicon.

LICENCES below is the licence decision per language: the non-GPL option where a Hunspell
dictionary is offered under several licences, and wordfreq alone where the GPL is the only one
(Czech, German; the measured loss is in ocr/qe/README.md).
"""
import argparse, glob, json, os, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from build_resources import LANGS, gutenberg_body, lm_text  # noqa: E402

LO = "https://github.com/LibreOffice/dictionaries/tree/c0865a85eb67dc377f2cf8e8fe2c4fa9e53f0dcb"
PRUNE_MIN_COUNT, PRUNE_FROM_ORDER = 3, 4
LM_CHARS = 3_000_000

# lang -> (Hunspell part or None, SPDX of the Hunspell part, licence texts to carry, readme to quote, what to say)
LICENCES = {
    "eng": ("en", "LicenseRef-SCOWL", [], "en/README_en_US.txt",
            "en_US Hunspell dictionary from SCOWL (Kevin Atkinson and others); its copyright and permission notices follow."),
    "fra": ("fr_FR", "MPL-2.0", ["MPL-2.0"], "fr_FR/dictionaries/README_dict_fr.txt",
            "Dictionnaires orthographiques français 7.7 (Olivier R., Grammalecte), under the Mozilla Public License 2.0."),
    "deu": (None, None, [], None, "The German Hunspell dictionary (de_DE_frami) is GPL only, so it is left out: wordfreq alone."),
    "fin": (None, None, [], None, "No Hunspell dictionary was used for Finnish."),
    "swe": ("sv_SE", "LGPL-3.0-only", ["LGPL-3.0-only", "GPL-3.0-only"], "sv_SE/LICENSE_en_US.txt",
            "Swedish dictionary by Göran Andersson, under the GNU Lesser General Public License version 3."),
    "ces": (None, None, [], None, "The Czech Hunspell dictionary (cs_CZ) is GPL only, so it is left out: wordfreq alone."),
    "pol": ("pl_PL", "Apache-2.0", ["Apache-2.0"], "pl_PL/README_en.txt",
            "Polish dictionary from sjp.pl (Marek Futrega), offered under GPL, LGPL, MPL, Apache 2.0 and CC; used here under Apache 2.0."),
    "slv": ("sl_SI", "LGPL-3.0-only", ["LGPL-3.0-only", "GPL-3.0-only"], "sl_SI/README_sl_SI.txt",
            "Slovenian dictionary (Amebis, Tomaž Erjavec, Aleš Košir, Primož Peterlin), offered under the GNU LGPL and GPL; used here under the LGPL (its README links the current LGPL, version 3)."),
    "spa": ("es", "MPL-1.1", ["MPL-1.1"], "es/LICENSE.md",
            "Spanish dictionary from LibreOffice, offered under GPL 3, LGPL 3 and MPL 1.1; used here under MPL 1.1."),
    "dan": ("da_DK", "MPL-1.1", ["MPL-1.1"], "da_DK/README_da_DK.txt",
            "Danish dictionary © 2020 Foreningen for frit tilgængelige sprogværktøjer, offered under GPL 2, LGPL 2.1 and MPL 1.1; used here under MPL 1.1."),
    "nor": ("no", "CC-BY-4.0", [], "no/README_NO.txt",
            "Norwegian Bokmål dictionary (Lars Bungum), from Ordbanken (Nasjonalbiblioteket, CC BY 4.0) and the 2018–2024 new-word lists (CLARIN PUB +BY); its README with the attributions follows."),
    "srp": ("sr", "MPL-2.0", ["MPL-2.0"], "sr/README.txt",
            "Serbian dictionary by Milutin Smiljanic, offered under LGPL 3, MPL 2 and GPL 3; used here under MPL 2.0."),
    "rus": ("ru_RU", "LicenseRef-Lebedev-BSD", [], "ru_RU/README_ru_RU.txt",
            "Russian dictionary, Copyright (c) 1997-2008 Alexander I. Lebedev; its copyright notice and conditions follow. This file holds a modified version: the dictionary's forms expanded and lower-cased."),
    "yid": (None, None, [], None, "No Hunspell dictionary exists for Yiddish."),
}

WORDFREQ = """wordfreq 3.1.1, by Robyn Speer (https://github.com/rspeer/wordfreq). Its data is CC BY-SA 4.0
(https://creativecommons.org/licenses/by-sa/4.0/). It draws on Google Books Ngrams (acknowledgement of the
Google Books Ngram Viewer), the Leeds Internet Corpus, Wikipedia, ParaCrawl, OPUS OpenSubtitles 2018
(attribution to OpenSubtitles), the SUBTLEX lists (Marc Brysbaert et al.), Twitter and other web and news text.
Cite as: Robyn Speer (2022). rspeer/wordfreq: v3.0 (v3.0.2). Zenodo. https://doi.org/10.5281/zenodo.7199437.
The lexicon holds its words, lower-cased and folded; the character model's training text holds a
frequency-weighted sample of them."""


def period_sources(res, lang):
    """How much of the model's text came from Gutenberg and from Wikisource, and which books."""
    g = LANGS[lang][2]
    books, parts = [], []
    if g:
        for p in sorted(glob.glob(os.path.join(res, "gutenberg", g, "*.txt"))):
            parts.append(lm_text(gutenberg_body(open(p, encoding="utf-8", errors="replace").read()), lang))
            books.append(os.path.basename(p)[:-4])
    gut = min(len(" ".join(parts)), LM_CHARS)
    wiki = 0
    ws = os.path.join(res, "wikisource", f"{g or 'yi'}.txt")
    if gut < LM_CHARS and os.path.exists(ws):
        wiki = min(LM_CHARS - gut - 1, len(lm_text(open(ws, encoding="utf-8").read(), lang)))
    return gut, wiki, books, g or "yi"


def notice(res, lang, spdx):
    hun, hspdx, texts, readme, what = LICENCES[lang]
    gut, wiki, books, code = period_sources(res, lang)
    dump = {}
    for line in open(os.path.join(res, "wikisource", "wikisource.tsv"), encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        dump[f[0]] = f
    out = [f"usnewsmap.com OCR quality measure qe-v1: resource file for language {lang}",
           "",
           "A lexicon and a character 5-gram model, built by scripts/qe/build_resources.py and scripts/qe/release.py",
           "in https://github.com/tgoodyear/usnewsmap.com (see ocr/qe/README.md). It is an adaptation of the works",
           f"below and is shared under: {spdx}.",
           "",
           "== Word list"]
    out.append(WORDFREQ if lang != "yid" else
               "The repository's Yiddish word list (ocr/ja-ocr/wordlists/yi.txt), built from Yiddish Wikipedia and\n"
               "Wikisource text, CC BY-SA 4.0 (https://creativecommons.org/licenses/by-sa/4.0/).")
    out += ["", "== Character model training text"]
    if gut:
        out.append(f"{gut:,} characters of Project Gutenberg books (public domain in the US; the Project Gutenberg\n"
                   f"header and licence were removed and only character counts are kept), ebook numbers: {', '.join(books)}.")
    if wiki:
        f = dump.get(code)
        out.append(f"{wiki:,} characters of Wikisource ({code}.wikisource.org) main-namespace text, from the dump\n"
                   f"{f[1] if f else ''} of {f[2] if f else ''}. Wikisource contributors, CC BY-SA 4.0\n"
                   "(https://creativecommons.org/licenses/by-sa/4.0/). The model keeps only character n-gram counts.")
    out += ["", "== Hunspell dictionary", what]
    if hun:
        out.append(f"Source: {LO}/{readme.split('/')[0]} (LibreOffice/dictionaries). The dictionary's forms were expanded\n"
                   "with unmunch and words of the training text it accepts were added; this is a modified version.")
        if readme:
            out += ["", f"-- {readme}", open(os.path.join(res, "hunspell", readme), encoding="utf-8", errors="replace").read().rstrip()]
        for t in texts:
            out += ["", f"-- Licence text: {t}", open(os.path.join(res, "licences", f"{t}.txt"), encoding="utf-8").read().rstrip()]
    return "\n".join(out) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--res", required=True)
    ap.add_argument("--build", required=True)
    ap.add_argument("--qe", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--lock", required=True)
    a = ap.parse_args(argv)
    os.makedirs(a.out, exist_ok=True)
    lock = {"measure": "qe-v1", "prune_from_order": PRUNE_FROM_ORDER, "prune_min_count": PRUNE_MIN_COUNT, "languages": {}}
    for lang, (hun, hspdx, _, _, _) in sorted(LICENCES.items()):
        spdx = "CC-BY-SA-4.0" + (f" AND {hspdx}" if hspdx else "")
        text = notice(a.res, lang, spdx)
        npath = os.path.join(a.out, f"{lang}.NOTICE.txt")
        open(npath, "w", encoding="utf-8").write(text)
        lexicon = "words.txt" if hun else "words-wordfreq.txt"
        out = subprocess.run([a.qe, "--command", "pack", "--build", os.path.join(a.build, lang), "--lexicon", lexicon,
                              "--notice", npath, "--licence", spdx, "--prune-min-count", str(PRUNE_MIN_COUNT),
                              "--prune-from-order", str(PRUNE_FROM_ORDER), "--out", os.path.join(a.out, f"{lang}.qe")],
                             stdout=subprocess.PIPE, check=True).stdout
        d = json.loads(out)
        lock["languages"][lang] = {"bytes": d["bytes"], "file": f"{lang}.qe", "hunspell": d["hunspell"], "licence": spdx,
                                   "lexicon_words": d["lexicon_words"], "model_entries": d["model_entries"], "sha256": d["sha256"]}
        print(lang, d["bytes"], d["sha256"], spdx, flush=True)
    with open(a.lock, "w", encoding="utf-8") as f:
        json.dump(lock, f, indent=2, sort_keys=True)
        f.write("\n")


if __name__ == "__main__":
    main()
