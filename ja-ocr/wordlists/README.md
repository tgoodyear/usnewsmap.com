# The OCR quality audit's own word lists

`quality.py` (metric v2) scores a page against its language's most frequent words. wordfreq supplies
them for most languages; these files cover languages it has no list for (`LOCAL_WORDLISTS`). Each is
the 5,000 most frequent words, most frequent first, one per line, folded as `quality.py` folds a page's
words. Rebuild them with `scripts/ja-ocr/build-wordlists.py <code>`.

| File      | Language | Source                                                                                                                                                                             | License                                                                                                 | Built          |
| --------- | -------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | -------------- |
| `haw.txt` | Hawaiian | [Hawaiian Corpus Project](https://github.com/dohliam/hawaiian-corpus) frequency list (`data/freqlist_haw.txt`, 96,140 words from Ulukau texts)                                     | CC0                                                                                                     | 6 October 2026 |
| `yi.txt`  | Yiddish  | Article pages of [Yiddish Wikipedia](https://yi.wikipedia.org/) and [Yiddish Wikisource](https://yi.wikisource.org/) (the `latest` dumps: about 3.5 million and 2.4 million words) | CC BY-SA 4.0 (Wikimedia contributors); this list is derived from them and shared under the same license | 6 October 2026 |

Folding:

- **Hawaiian:** the ʻokina and the kahakō are dropped, because 19th-century Hawaiian papers didn't print them ("olelo" for "ʻōlelo"). Only words of (consonant-)vowel syllables are kept, which drops the corpus's English.
- **Yiddish:** vowel points and other marks are dropped, and the ligatures װ, ױ and ײ are written as their two letters, as `quality.py` does with a page's words.

Limits:

- The Yiddish list is mostly modern Wikipedia text, in modern spelling. American Yiddish papers before the 1930s often spelled German-style ("דיא" for "די"), so some of their real words may score as damage. Wikisource's older texts cover part of that.
- The Hawaiian corpus is mostly 19th- and 20th-century text, with some OCR errors of its own.

Not covered: Choctaw, Dakota, Lakota, Navajo and Cherokee. No open frequency list of useful size exists for them; their pages stay undetermined or are read as English.

German needs no list of its own. `scripts/ja-ocr/build-wordlists.py ger-check` checks whether 19th-century spellings would count as damage, using the [Deutsches Textarchiv](https://www.deutschestextarchiv.de/)'s 1800–1899 texts. They don't. "daß", "Theil", "giebt" and "seyn" are not one edit from German's 20 most common words. The near-misses those texts use most are "fich", "fie" and "ift" (the long s read as f), and those are OCR damage.
