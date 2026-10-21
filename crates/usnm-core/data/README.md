# Word lists in usnm-core

Data files compiled into `usnm-core`. The crate's code is MIT; these files keep the licences of the data they come from, and `Cargo.toml` names both.

The english and damage lists are committed zstd-compressed (`zstd -19`) as `<name>.txt.zst`, to keep the source tree small. `crates/usnm-core/build.rs` decompresses each `data/**/*.zst` to `$OUT_DIR/data/<same path without .zst>` (the damage lists only with the feature `damage`), and the crate embeds them from there with `include_str!`. `page-language-words.txt` is small and stays plain text. The generator scripts write the `.zst` files through the `zstd` command line tool, so it must be on `PATH` to rebuild them; `--check` decompresses before it compares.

| Files                                                   | Used by                                              | Built by                                           | Source                                   | Licence      |
| ------------------------------------------------------- | ---------------------------------------------------- | -------------------------------------------------- | ---------------------------------------- | ------------ |
| `english-words.txt.zst`, `english-shared-words.txt.zst` | `usnm_core::query_language` (06 §6.3.3, #237)        | `scripts/build-english-words.py`                   | wordfreq 3.1.1                           | CC BY-SA 4.0 |
| `damage-words/<code>.txt.zst`, except `haw` and `yi`    | `usnm_core::lang`, feature `damage` (12 §12.5, #296) | `scripts/build-damage-words.py`                    | wordfreq 3.1.1, `top_n_list(code, 5000)` | CC BY-SA 4.0 |
| `damage-words/haw.txt.zst`                              | the same                                             | copied from `ja-ocr/wordlists/` by the same script | Hawaiian Corpus Project frequency list   | CC0          |
| `damage-words/yi.txt.zst`                               | the same                                             | copied from `ja-ocr/wordlists/` by the same script | Yiddish Wikipedia and Wikisource         | CC BY-SA 4.0 |
| `page-language-words.txt`                               | `usnm_core::page_language` (05 §5.5.8, #289)         | `scripts/build-page-language-words.py`             | the function words of the 30 lists above | CC BY-SA 4.0 |

`ja-ocr/wordlists/README.md` describes how the Hawaiian and Yiddish lists were built.

## The damage word lists

One `.txt.zst` file per word list code in `usnm_core::lang` (wordfreq's codes: `sh` serves Croatian and Serbian, `nb` Norwegian, `fil` Tagalog). Each holds, once decompressed, the language's 5,000 most frequent words, most frequent first, one per line, exactly as wordfreq 3.1.1's `top_n_list` returns them (the same call `ja-ocr/quality.py` makes). Order matters: the metric takes its function words and head words from the front of the list.

Rebuild or check them with wordfreq 3.1.1 installed (`pip install wordfreq==3.1.1`) and the `zstd` command line tool on `PATH`:

```sh
scripts/build-damage-words.py --check
scripts/build-damage-words.py --out-dir crates/usnm-core/data/damage-words
```

Nothing in the pipeline runs the script; the files are committed data. To read one: `zstd -dc crates/usnm-core/data/damage-words/fr.txt.zst`.

## The page-language word lists

`page-language-words.txt` holds each word list's function words: its 100 most frequent words of 2 letters or more, most frequent first, then for Danish and Norwegian their `aa` spellings (`paa` for `på`). One line per list: the list's code, the script of its words, then the words. The script builds it from `ja-ocr/damage_v2.py`'s own models, so the words are the ones the OCR audit finds a page's language by. It is about 18 KB, and every binary that uses `usnm_core::page_language` embeds it; the damage lists stay behind the `damage` feature. It is derived from the wordfreq lists and from our Hawaiian (CC0) and Yiddish (CC BY-SA 4.0) lists, so it is shared under CC BY-SA 4.0, as its header says.

Rebuild or check it with wordfreq 3.1.1 installed:

```sh
scripts/build-page-language-words.py --check
scripts/build-page-language-words.py
```

The Japanese OCR image's tests run the same check (`ja-ocr/test_page_language_parity.py`).

## wordfreq

[wordfreq](https://github.com/rspeer/wordfreq) 3.1.1, by Robyn Speer. Its code is Apache-2.0 and its data CC BY-SA 4.0 (<https://creativecommons.org/licenses/by-sa/4.0/>), so lists exported from it are shared under CC BY-SA 4.0. Its data draws on:

- Google Books Ngrams (<http://books.google.com/ngrams>) and Google Books Syntactic Ngrams, which may be used for any purpose with acknowledgement of the Google Books Ngram Viewer;
- the Leeds Internet Corpus (University of Leeds Centre for Translation Studies), Wikipedia and ParaCrawl, under Creative Commons licences;
- OPUS OpenSubtitles 2018, from the OpenSubtitles project, used with attribution to OpenSubtitles;
- the SUBTLEX lists (SUBTLEX-US, -UK, -CH, -DE and -NL) by Marc Brysbaert et al., freely available data that must be credited to the SUBTLEX authors;
- word counts from the Twitter streaming API, and other web and news text.

Cite wordfreq as: Robyn Speer (2022). rspeer/wordfreq: v3.0 (v3.0.2). Zenodo. <https://doi.org/10.5281/zenodo.7199437>.
