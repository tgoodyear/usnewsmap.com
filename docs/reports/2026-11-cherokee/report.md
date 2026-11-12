# Cherokee: what is searchable, how good the OCR is, and a plan

6 November 2026, updated 9 November with the _Phoenix_ model (step 3) and 12 November with the first production re-read (step 4). Follows [#1581](https://github.com/tgoodyear/usnewsmap.com/issues/1581) (the word lists and the engine tests) and [#1590](https://github.com/tgoodyear/usnewsmap.com/issues/1590) (the Georgia Historic Newspapers images).

## Summary

- **Volume.** usnewsmap.com searches 324 pages of the _Cherokee Phoenix_ (82 issues, 1828-03-06 to 1833-10-19). They hold about 85,000 words of Cherokee syllabary, on 191 pages. That is 32% of the 1,023 pages LoC and GHN list for the paper. Images of the other 699 pages are already on our storage, without OCR. No other Cherokee-language newspaper is in the index. The largest one outside it is the _Cherokee Advocate_ (1844–1906), 895 issues on the Gateway to Oklahoma History.
- **Accuracy.** The first measured figure comes from 70 lines checked against the page images: LoC's Cherokee OCR misreads at least 8–9% of syllabary letters, and stock Tesseract 16–17% (step 2). The proxies we can compute say LoC's Cherokee OCR is weak, and weak in particular letters. ᎾᏍᎩ ("that"), the second most common word in the New Testament, is read correctly 127 times and misread as ᏩᏍᎩ, ᏣᏍᎩ or ᏳᏍᎩ 1,007 times. ᎠᎴ ("and", the most common) is read correctly 2,547 times in 2,555. The paper's name, ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ, reads exactly on 47 of 82 front pages. It appears in a standing notice in the first column, not in the masthead. LoC's word confidences are no help: 99.8% of Cherokee words have a confidence of at least 0.5, and the median is 1.0.
- **Models.** Tesseract's Cherokee model reads clean typeset text well: 1.67% character error on the New Testament PDFs. On Phoenix scans it makes the same letter confusions LoC's OCR makes. Our Qwen3-VL-2B route produced no syllabary at all, and in a 2026 benchmark none of 14 vision-language models read Cherokee. Fine-tuned on the paper's own glyphs, Tesseract beats LoC: `chr_phoenix` reads the test set's silver lines at 10.2% character error against LoC's 18.0%, and the hand-checked lines at 6.6% against 8.4%, though 70 lines are too few to settle that (step 3). It isn't deployed yet.
- **The type.** The _Phoenix_ was set in one font, cast in Boston in 1827. Nine of its letters were recut in 1834, mostly because they looked too much like other letters. Three of them (Ꮩ, Ꮅ and Ꮦ) are among the pairs our engines confuse (section 4). The recut came after the paper's last issue, so every _Phoenix_ page has the 1828 shapes.
- **Plan.** Build ground truth without new transcription work where we can, train a Phoenix-specific model, OCR the 699 missing pages, and make search tolerant of the known confusions in the meantime. Steps 1 to 3 are done: search now finds ᎾᏍᎩ on 108 pages (35 before), and the model passed its test. The BOINC re-OCR (step 4) has run for the 191 pages that have Cherokee lines: 190 were read and accepted on 11 and 12 November, one waits for a scan. The 699 missing pages and per-word script detection (#1666) for English words inside Cherokee lines are next.

## 1. Volume: what we have against what exists

### Our index

Counts from LoC's OCR batches for the two titles (`dlc_misctopsn83020866_ver03`, `dlc_misctopsn83020874_ver02`), read with [`scripts/chr_alto_stats.py`](scripts/chr_alto_stats.py) and [`scripts/chr_masthead.py`](scripts/chr_masthead.py):

| Measure                                           | Value                 |
| ------------------------------------------------- | --------------------- |
| Pages / issues                                    | 324 / 82              |
| Pages with any Cherokee words                     | 191                   |
| Pages with 100 or more Cherokee words             | 111                   |
| ALTO text blocks tagged `language="chr"`          | 615 (of 6,497)        |
| Lines in Cherokee blocks                          | 16,682 (9% of lines)  |
| Syllabary words in Cherokee blocks                | 83,449                |
| Syllabary tokens in the page text (`ocr.txt`)     | 84,833 (36,216 forms) |
| Syllabary characters                              | 342,559               |
| Cherokee words per page: median / 90th percentile | 53 / 874              |

The 9% share of lines fits Landini's content analysis of the paper, which found 83.4% of its editorial content in English ([Landini 1990](https://trace.tennessee.edu/utk_graddiss/11440)). Search works on the syllabary: on 6 November `/v1/aggregate` found ᏣᎳᎩ on 122 pages, ᎠᎴ on 117, ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ on 65 and ᏥᏌ on 38.

The quality database holds all 324 pages and their damage scores, but no second reading of any of them: no BOINC engine has read a Cherokee page.

### Cherokee print outside the index

| Source                                           | Years     | Size                                       | Online                                          | Cherokee text                                               |
| ------------------------------------------------ | --------- | ------------------------------------------ | ----------------------------------------------- | ----------------------------------------------------------- |
| _Cherokee Phoenix_, GHN-only pages               | 1828–1834 | 699 pages / 177 issues                     | Images fetched (#1590)                          | No Cherokee OCR: all of GHN's ᏣᎳᎩ hits fall on LoC's issues |
| _Cherokee Advocate_, Gateway to Oklahoma History | 1844–1906 | 895 items, all tagged Cherokee and English | Yes, from Oklahoma Historical Society microfilm | No syllabary OCR found; not in Chronicling America          |
| _Cherokee Messenger_                             | 1844–1846 | About 194 pages                            | Subscription or by request                      | Not found                                                   |
| Modern _Cherokee Phoenix_ and _Advocate_         | 1977–2014 | About 400 issues                           | Licensed access only                            | Not stated                                                  |
| Laws of the Cherokee Nation, 1852                | 1852      | 121 pages                                  | Wikisource                                      | 11 pages transcribed, not proofread (CC BY-SA)              |
| Constitution and laws, 1875                      | 1875      | 233 pages                                  | LoC                                             | Not found                                                   |
| Cherokee books on the Internet Archive           | 1824–2020 | 48 items (hymns, gospels, laws)            | Yes                                             | The Archive's automatic OCR, quality unknown                |

Sources: [Gateway collection](https://gateway.okhistory.org/explore/collections/CHRAD/); [Chronicling America's Cherokee facet](https://www.loc.gov/collections/chronicling-america/?fa=language:cherokee) (only the two _Phoenix_ titles); [Newspapers.com, _Messenger_](https://www.newspapers.com/paper/the-cherokee-messenger/23517/); [the modern papers' scanning](https://www.cherokeephoenix.org/news/cherokee-newspapers-digital-archives-in-progress/article_0a90c636-3071-592e-aec7-7317d348e5fb.html); [Wikisource, 1852 laws](https://wikisource.org/wiki/Index:Cherokee_Laws.pdf); [LoC, 1875 laws](https://www.loc.gov/item/28014177/); [Internet Archive search](https://archive.org/advancedsearch.php?q=language%3A%28chr+OR+cherokee%29).

Machine-readable Cherokee text, for vocabulary and as possible ground truth:

| Corpus                                                                              | Size                                                         | Terms                                          | Kind                                                    |
| ----------------------------------------------------------------------------------- | ------------------------------------------------------------ | ---------------------------------------------- | ------------------------------------------------------- |
| Cherokee New Testament, 1860 (our word lists)                                       | About 115,700 words per edition                              | CC0                                            | Typeset                                                 |
| [ChrEn](https://github.com/ZhangShiyue/ChrEn/tree/master/data) (Zhang et al., 2020) | 206,000 Cherokee tokens, 38,000 forms; 14,151 sentence pairs | Research use; copyright stays with the sources | Typeset books, 56% New Testament, OCR corrected by hand |
| [DAILP](https://dailp.northeastern.edu)                                             | 87 documents                                                 | CC BY-NC 4.0                                   | Letters, notices, stories; mostly manuscript            |
| Cherokee Wikipedia                                                                  | 999 articles, about 40,000 words                             | CC BY-SA                                       | Modern, born digital                                    |

**Where that leaves us.** Of the Cherokee-language newspaper pages known to be online, we search 324. The rest of the _Phoenix_ (699 pages) is the closest gap: we hold the images and the only cost is OCR. The _Advocate_ is the largest: 895 issues, 1844 to 1906. ChrEn is the one hand-corrected corpus larger than the New Testament.

## 2. Accuracy of what is searchable

There is no checked transcription of a whole _Phoenix_ page; the error rates measured on test lines are in steps 2 and 3. The figures below are proxies over all 324 pages, each computed the same way for any reading of the pages so that two engines can be compared.

| Proxy (LoC OCR, 324 pages)                                       | Value                        |
| ---------------------------------------------------------------- | ---------------------------- |
| Words in the New Testament's top 5,000                           | 31.8%                        |
| Words in the New Testament outside the top 5,000                 | 5.1%                         |
| Words one letter from exactly one known word                     | 10.8%                        |
| ᎾᏍᎩ ("that", NT rank 2) read correctly, of ᎾᏍᎩ + ᏩᏍᎩ + ᏣᏍᎩ + ᏳᏍᎩ | 127 of 1,134 (11.2%)         |
| ᎠᎴ ("and", NT rank 1) read correctly, of ᎠᎴ + ᎠᏞ + ᎤᎴ            | 2,547 of 2,555               |
| Front pages where ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ (a standing notice) reads exactly   | 47 of 82                     |
| Cherokee words with LoC confidence under 0.5                     | 0.2% (median confidence 1.0) |

None of ᏩᏍᎩ, ᏣᏍᎩ and ᏳᏍᎩ appears in either New Testament edition, so they are misreadings, not spellings. Most of the damage is in a few letters. Where LoC and Tesseract read the same 1,320 lines differently, the commonest disagreements are pairs of letters that look alike in the paper's type: Ꮧ/Ꮨ (di/ti, 159), Ꮅ/Ꮲ (li/tlv, 70), Ꮎ/Ꭷ (na/ka, 45), Ꮎ/Ꮾ (na/hnv, 46), Ꮣ/Ꮦ (da/te, 44). The press's type was not today's glyph set either: it printed Ꮩ (do) upside down, like Λ, until 31 May 1834 ([languagegeek](http://www.languagegeek.com/rotinonhsonni/tsalagi/do.html)). Tesseract's Cherokee model was trained only on modern fonts ([its font list](https://github.com/tesseract-ocr/langdata_lstm/blob/main/chr/okfonts.txt)).

Search often misses a word with one of these letters. A query for ᎾᏍᎩ finds 35 pages; ᏩᏍᎩ alone finds 80.

## 3. Models run against Cherokee

| Reading                                                           | Test                                   | Result                                                                                      | Speed             |
| ----------------------------------------------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------- | ----------------- |
| LoC OCR (the index's text)                                        | 324 _Phoenix_ pages, proxies           | Above: 31.8% known, ᎾᏍᎩ 11.2% right                                                         | n/a               |
| Tesseract 5.5 `chr`, `tessdata_best`                              | 20 New Testament pages, reference text | 1.67% character error on syllabary, 3.48% overall                                           | 3.5 s/page        |
| Tesseract 5.5 `chr`, `tessdata_fast`                              | Same                                   | 1.71% and 3.50%                                                                             | 1.9 s/page        |
| Tesseract `chr`, whole _Phoenix_ pages                            | 103 pages, proxies                     | Unusable: reads the English columns as syllabary (3.8 times LoC's token count, 11.2% known) | 44 s/page         |
| Tesseract `chr+eng`, whole pages                                  | 8 mixed pages, proxies                 | 35.2% known against LoC's 31.2%; English kept                                               | 45 s/page         |
| Tesseract `chr`, LoC's Cherokee lines only (`cherokee_hybrid.py`) | Same 8 pages, 1,320 lines              | 32.5% known; ᎾᏍᎩ 7 right, 38 misread (LoC: 11 and 50); 123 empty lines kept LoC's text      | About 70 ms/line  |
| Tesseract `chr`, line crops: upscaled 2x, or PSM 6                | 40 lines, agreement with LoC           | Less agreement than native-size PSM 7 (28.7% and 30.2% edit distance against 25.2%)         | n/a               |
| Qwen3-VL-2B Q8 (BOINC `usnm_qwen` prompt)                         | 1 New Testament page                   | No syllabary output; 100% error on Cherokee characters                                      | 624 s/page on CPU |
| Tesseract `chr_phoenix` (step 3), line crops at PSM 13            | Test set v2, 2,160 lines               | 27.6% error on all lines, 10.2% silver, 6.6% hand-checked (LoC: 34.5%, 18.0%, 8.4%)         | n/a               |
| Tesseract `chr_phoenix+eng`, line crops at PSM 13                 | Same                                   | Worse than `chr_phoenix` alone (12.9% silver); writes 863 Latin words on syllabary lines    | n/a               |

From the literature, on other material:

- Zhang et al. ([2022](https://arxiv.org/pdf/2204.11909)), 20 book pages: Tesseract 23.0% character error on full pages and 6.3% on clean crops; Google Cloud Vision 19.9% and 7.4%. Google lists Cherokee as experimental and not regularly evaluated. Google Document AI and Azure don't list it.
- GlotOCR Bench ([2026](https://arxiv.org/html/2604.12978)), 100 synthetic Cherokee sentences: none of 14 vision-language models, among them GPT-4.1, Gemini 3.1 Flash-Lite and Qwen3-VL-8B, read any sentence under 5% character error.
- No Transkribus or Kraken model for the syllabary was found, and no published handwriting work.

The New Testament results show Tesseract can read Cherokee type, and the _Phoenix_ results show it doesn't know this type. Two unrelated engines make the same confusions, which points at the glyphs. A model trained on _Phoenix_-like lines was the change most likely to help, and in step 3 it did. The vision-language route is closed for now.

## 4. The press's type

Patrick Giasson's MA thesis, _The Typographic Inception of the Cherokee Syllabary_ (University of Reading, 2004), traces the first Cherokee font from Sequoyah's drawings to the foundry. It is the only source we have found that shows the 1828 letter shapes next to the 1834 ones. Everything below is from it unless marked as ours.

**The 1827 font.** The Baker & Greele foundry in Boston cut and cast the font in 1827 from Samuel Worcester's drawings. The press used it from the first _Phoenix_ in February 1828. Sixteen letters (Ꭱ Ꭰ Ꮃ Ꮐ Ꮲ Ꮇ Ᏼ Ꭺ Ꮓ Ꭼ Ꭲ Ꭻ Ꮶ Ꮪ Ꮋ Ꮮ, in the Latin shapes R D W G P M B A Z E T J K S H L) are the small capitals of the foundry's small pica roman, a Scotch Roman. The other 69 were cut new. The Cherokee was cast at small-capital height on the same small pica body as the English, so the two can share a line. With the 1834 changes and a larger companion font added in the late 1850s for capitals and titles, the same typeface set almost all printed Cherokee until the 1970s. That includes the _Cherokee Advocate_.

**The 1834 recut.** In 1834 Worcester had letters recut (the thesis says ten and describes nine) because they were "sufficiently distinct when the print is clear but we find that we cannot always rely upon it" (his letter, ABCFM 18.3.1 v.7, 234). The new type reached the press in 1835. The _Phoenix_ had stopped in May 1834, so none of its pages has the new shapes. Against our measurements:

| Letter | 1828 shape and its trouble                              | 1834 change                      | What we measured (sections 2 and 5)                              |
| ------ | ------------------------------------------------------- | -------------------------------- | ---------------------------------------------------------------- |
| Ꮩ do   | Λ, an upside-down V, taken for Ꭺ go                     | Small capital V                  | Ꮩ read as Ꭺ 430 times; Λ was Ꮩ in 22 of 26 hand-checked disputes |
| Ꮅ li   | A P with a flag, taken for Ꮲ tlv                        | Rounded off                      | Ꮅ read as Ꮲ 1,007 times, the commonest misreading                |
| Ꮦ te   | Its short left stroke broke off the metal, leaving Ꮣ da | Stroke enlarged, tail closed     | Ꮣ/Ꮦ 44 disagreements                                             |
| Ꮯ tli  | G-shaped, taken for Ꮆ lo                                | Small capital C                  | Not separated yet; see the G-shaped letters below                |
| Ᏺ yo   | A plain h, the same as Ꮒ ni except the top serif        | Hooked ascender                  | Not measured                                                     |
| Ꮢ sv   | Close to Ꭱ e                                            | Counter opened, leg straightened | Not measured                                                     |
| Ꮥ de   | Close to Ꭶ ga, which it often stands next to            | New shape                        | Not measured                                                     |
| Ꮾ wv   | Reason not given                                        | Made like a 6                    | Ꮎ/Ꮾ 46 disagreements                                             |
| Ꮌ mo   | Reason not given                                        | Recut twice                      | Not measured                                                     |

The thesis lists further letters told apart only by a serif or a ball terminal: Ꭱ/Ꮢ, Ꭹ/Ꮍ, Ꮒ/Ᏺ, and six G-shaped letters, Ꮐ Ꮯ Ꮆ Ᏻ Ꮳ Ꮹ. Ꮃ and Ꮤ differ only slightly in structure.

**Consequences for the OCR.** The rest of this section is our reading, not the thesis's.

- **The G-shaped letter in ᎾᏍᎩ may be Ꮐ.** The hand check found a G-shaped letter where today's θ-like Ꮎ belongs. LoC reads that word as ᏩᏍᎩ, ᏣᏍᎩ or ᏳᏍᎩ, with Ꮹ, Ꮳ and Ᏻ: all three are in the G-shaped group. The 1828 and 1835 type specimens show Ꮐ (_nah_) as a plain Latin G. Worcester's 1834 order for new type asks for 9 ounces of Ꮐ against 12 of Ꮎ. So the _Phoenix_ may spell the word ᏀᏍᎩ. Against it, the thesis says the larger font of the late 1850s left Ꮐ out, "probably because it was almost never used". That may describe only the later printing. This is untested. The test: count Ꮐ in LoC's text and in Tesseract's `chr` character set, and compare _Phoenix_ crops with the specimens' Ꮐ. If it holds, Ꮐ goes into the `~ocr` table and the training text, and the checked references need the period spelling.
- **More letter pairs to try in `~ocr`.** Ᏺ/Ꮒ, Ꮢ/Ꭱ, Ꮥ/Ꭶ, Ꭹ/Ꮍ and Ꮃ/Ꮤ. Each is added only if LoC's misreadings show it, by the step 1 rule.
- **A font built from the press's type.** The 16 Latin letters can come from an open Scotch Roman with small capitals. The other 69 can be cut from glyphs on the _Phoenix_ scans themselves, since one font printed every page. Synthetic lines should set Cherokee at small-capital height next to English at full size.
- **Broken type counts as damage.** Where the stroke of Ꮦ broke off, the page shows Ꮣ. References keep the intended Ꮦ, and only the word list can recover it, so letter-shape training alone has a floor on this pair.
- **The _Advocate_ uses the same font.** Its model can start from the _Phoenix_ model, adding the 1834 shapes of the nine letters and folding the larger capitals to one case.
- **Telling Cherokee lines from English.** Cherokee is cast at small-capital height, and Worcester shrank strokes that rose above it (Ꭷ, Ꭾ). A line with almost no ascenders or descenders is likely syllabary, a cheap first test for step 4's line classifier.
- **A check on letter frequencies.** The 1834 order gives ounces of type per letter, a rough guide to how often each was set. A letter the OCR outputs far more often than its share of type suggests misreadings into it (Ꮲ: 2 ounces against Ꮅ's 14). A letter it rarely outputs suggests misreadings away from it.

The thesis is © Patrick Giasson, all rights reserved. We cite its facts and don't copy its plates into the repository or the training data.

## 5. Plan

Each step has a measure that can be recomputed from its output, and steps 1 and 2 need no new transcription.

### Step 1. Confusion-tolerant Cherokee search (quick, no OCR) ([#1619](https://github.com/tgoodyear/usnewsmap.com/issues/1619))

Search `word~ocr` with a Cherokee table of letter pairs, so ᎾᏍᎩ~ocr also finds ᏩᏍᎩ, ᏣᏍᎩ and ᏳᏍᎩ. Romanized queries (_tsalagi_ → ᏣᎳᎩ, the follow-up noted in #1581) can share the same expansion code later.

**Done in [#1624](https://github.com/tgoodyear/usnewsmap.com/pull/1624).** The table holds the 13 pairs with at least 50 occurrences among LoC's one-letter misreadings of the New Testament's 5,000 most frequent words, out of 7,870 such misreadings in 84,833 tokens ([`scripts/chr_letter_pairs.py`](scripts/chr_letter_pairs.py)). The commonest are Ꮅ read as Ꮲ (1,007), Ꮧ as Ꮨ (473) and Ꮩ as Ꭺ (430, the inverted-V do). Ꮎ goes to five letters: Ᏻ, Ꮹ, Ꭷ, Ꮾ and Ꮳ (101 to 145 each). For the 200 most frequent New Testament words, the 324 pages give 5,496 page hits exactly and 1,102 more with `~ocr` ([`scripts/chr_ocr_variant_recall.py`](scripts/chr_ocr_variant_recall.py)). Of the added hits, 979 match only non-words. 122 match another New Testament form and 1 matches both, so those 123 may be false hits. ᎾᏍᎩ goes from 35 pages to 109. ᎦᏙᎯ ("earth") goes from 0 to 52, because its Ꮩ is always read as Ꭺ. `~ocr` is opt-in, so a ranking penalty for variants isn't needed yet; making it the default for syllabary queries is a separate decision.

**Date-aware search and text, 8 November ([#1661](https://github.com/tgoodyear/usnewsmap.com/pull/1661)).** Every _Phoenix_ page was printed with the 1828 type, so two misreadings can be undone without `~ocr`: Ꭺ read for the Λ-shaped Ꮩ, and a word-initial Ꮹ, Ꮳ, Ᏻ or Ꮐ read for Ꮎ. A word is changed only when it isn't a New Testament form and exactly one undone reading is. It applies to pages printed before 1835, in the index text (LoC's original is kept beside it) and in plain queries. Live, a plain query for ᎾᏍᎩ now finds 108 pages (35 before) and ᎦᏙᎯ 52 (0 before); ᎠᎴ is unchanged at 117. The hand check supports the Ꮩ rule (19 answers for it, none for LoC). It doesn't yet support the Ꮎ rule: none of 38 answers chose the undone reading, so that rule rests on the New Testament alignment alone.

### Step 2. Ground truth without new transcription ([#1620](https://github.com/tgoodyear/usnewsmap.com/issues/1620))

1. **Synthetic _Phoenix_ lines.** Render New Testament and ChrEn sentences in a font with the period glyphs (Plantagenet Cherokee has the Λ do; Digohweli has it as a historic alternate). Add microfilm noise, blur and ink spread, tuned so the letter confusions on synthetic lines match the ones measured above. These give exact references for training and a first error rate.
2. **Text the paper reprinted.** The _Phoenix_ printed scripture (ᏥᏌ, Jesus, is on 38 pages) and laws later collected in the 1852 _Laws_. Align its OCR lines against the 1860 New Testament and the Wikisource laws; lines that match a known passage closely give near-ground truth on real scans. Wording may differ from the later editions, so only close alignments count, and every letter that differs is reported as a disagreement, not an error.
3. **Self-checking items.** A notice that starts ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ ᎯᎠ ᏂᎦᎥ ᏧᎬᏩᎶᏗ stands in the first column of page 1; ᏧᎴᎯᏌᏅᎯ reads exactly on 61 front-page lines across the 82 issues. Checked once by eye, its text is a reference for every copy.

**Measure:** the number of real _Phoenix_ lines with a reference, and the error of LoC and Tesseract on them. That would be the first real accuracy figure for the index's Cherokee text.

**Test set version 1, 7 November ([#1625](https://github.com/tgoodyear/usnewsmap.com/pull/1625)).** Item 2 is built: whole Cherokee text blocks aligned to the 1860 New Testament give 2,160 lines on 29 pages. 716 have no sign of a wording difference (`silver`), and 1,444 are kept with the reason they look different. The set is frozen with checksums, and its pages are excluded from training. Character error on syllabary letters, against the New Testament reference:

| Tier                  | Lines | LoC OCR | Tesseract stock | Stock + NT word list |
| --------------------- | ----: | ------: | --------------: | -------------------: |
| silver                |   716 |   17.4% |           24.8% |                24.0% |
| dropped-wording       |   744 |   29.9% |           36.5% |                36.0% |
| dropped-gap-run       |   539 |   49.3% |           53.7% |                53.4% |
| dropped-too-different |   161 |   64.3% |           68.1% |                67.7% |

Stock Tesseract reads these lines worse than LoC does. The silver tier favours LoC, because its lines were chosen by aligning LoC's text. The references follow the 1860 wording, which the _Phoenix_ sometimes doesn't (one crop prints ᎯᎠᏃ where the reference has ᎯᎠᎨ). A 200-line sample, half silver and half dropped, is set aside for a glyph-by-glyph check, which will measure that error and give the first fully checked figures.

**Hand check, 7 November.** In the 200-line sample, LoC and the reference disagree on 575 words. Each was checked against the page image: does the reference match the print, does LoC, or neither? The answers were reference 183, LoC 185, neither 179 and can't tell 28. On silver lines the reference matched the print more often (109 against LoC's 47, with 57 neither). On the dropped tiers LoC matched more often, and neither ran 35–40%. The answers come from one checker who doesn't read Cherokee and may include a few misclicks. 70 lines came out fully resolved, with every disputed word answered by one of the two readings. Character error on those lines, against the checked text ([`scripts/chr_checked_score.py`](scripts/chr_checked_score.py)):

| Text                 | 70 lines | 47 silver lines |
| -------------------- | -------: | --------------: |
| 1860 New Testament   |    10.5% |            3.3% |
| LoC OCR              |     8.4% |            9.2% |
| Tesseract stock      |    17.2% |           16.5% |
| Stock + NT word list |    16.6% |           15.5% |

These are the first measured error rates for the index's Cherokee text. About half of the error measured against the unchecked reference was the reference's own. The ranking doesn't change. The lines with a "neither" answer are left out, so the 70 lean clean. The checked text is built from LoC's and the reference's words, which flatters LoC, so its figure is a floor.

The answers back up the letter findings above. Λ was Ꮩ in 22 of 26 Ꮩ/Ꭺ disputes, Ꮅ beat Ꮲ in 22 of 25, and Ꮧ beat Ꮨ in 23 of 24. All 16 disputes between Ꮎ and Ꮹ or Ꮳ came out neither or can't tell: the paper prints a G-shaped letter in ᎾᏍᎩ, not today's θ-like Ꮎ. Which letter it is hasn't been settled yet. Specks in the margin also get read as letters. Digits aren't scored at all: LoC read a verse number "24." as "9Ꮞ.".

**The paper's own glyphs, 8–9 November ([#1673](https://github.com/tgoodyear/usnewsmap.com/pull/1673)).** Item 1 changed course. The synthetic lines are set from the paper's own letters instead of a modern font. `crates/usnm-glyphs` cuts the words LoC read as New Testament forms (or as one after undoing the 1828 misreadings) out of 190 page scans and splits them into letters. It keeps the 87,657 of 92,139 glyphs whose nearest neighbours on other pages carry the same label. Word-initial Ꮎ is a class of its own. Ꮊ, Ꮌ, Ꮍ, Ꮐ and Ꮛ have no samples, and Ꮯ, Ꮚ, Ꮬ and Ꮾ almost none. From the bank came:

- **Synthetic lines.** New Testament text set glyph by glyph and degraded like the microfilm, 20,000 lines per training round.
- **A traced font.** "Phoenix 1828", averaged and traced from the best samples, covers 72 letters plus the word-initial Ꮎ. It is CC0, a research tool rather than a revival.
- **New examples on check.usnewsmap.com.** 76 letters now show crops from this bank. The tasks and the 833 answers so far are unchanged.

No glyph from a test-set page was used for training. The bank the model was trained from did let test-page glyphs vote when deciding which training glyphs to keep; the crate no longer does (`clean --exclude-manifest`).

### Step 3. A _Phoenix_ model ([#1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621))

Fine-tune Tesseract's `chr` model with `tesstrain` on the synthetic lines plus the aligned real lines, holding out the test set's pages and the standing notice for testing. Try Kraken as a second engine if Tesseract stalls. **Measure:** character error on the held-out lines, the ᎾᏍᎩ share, masthead exact reads, and the word-list proxies, against LoC and stock `chr`. Go on to step 4 only if it beats LoC on the held-out real lines.

**Dictionary-only test, 6 November.** Before any retraining, the word list inside the stock model was replaced, with the network unchanged ([`scripts/chr_dict_compare.py`](scripts/chr_dict_compare.py)). Adding the 24,479 New Testament forms to the model's 6,368 words changed about 2.3% of the tokens on the same 1,320 _Phoenix_ lines. Of the changes, 127 turned a non-word into a New Testament word and 3 went the other way. ᎾᏍᎩ was still read right only 7 times, as with the stock list, which already held the word. Adding ChrEn's 54,922 forms as well made the readings worse: 30 known words reversed, 165 other changes, and ᎾᏍᎩ down to 5. So the word list helps a little but cannot fix the letter shapes. The fine-tune keeps the New Testament list, and ChrEn goes into the rendered training text instead. Results and hashes are on [#1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621).

**The fine-tune, 9 November ([#1673](https://github.com/tgoodyear/usnewsmap.com/pull/1673)).** `chr_phoenix` is stock `chr` with the New Testament word list, trained further with `lstmtraining` on CPU in two rounds of 20,000 iterations. Each round used about 14,400 real lines and 20,000 synthetic ones. The real lines are runs of words LoC read as New Testament forms, cut from 161 pages. ChrEn wasn't used in the end; the synthetic text is the New Testament alone. Round two added verse numbers, punctuation and margin specks to the training lines, after round one read "28" as ᎯᎯ and invented letters at line ends. Scores on test set v2 (2,160 line crops from the same 29 pages, re-tiered: 700 silver), at PSM 13:

| Reading         | All lines | Silver | Hand-checked (70) |
| --------------- | --------: | -----: | ----------------: |
| LoC OCR         |     34.5% |  18.0% |              8.4% |
| Tesseract stock |     35.8% |  19.7% |             12.2% |
| Round one       |     30.5% |  14.1% |              9.7% |
| `chr_phoenix`   |     27.6% |  10.2% |              6.6% |

- **It passes this step's test.** On silver lines it is 7.8 points better than LoC (95% page bootstrap −9.1 to −6.3), on 26 of 29 pages. On the hand-checked lines it is better on 38 and worse on 15, but 70 lines don't settle it (−4.9 to +1.9).
- **It fixes the 1828 letters.** On silver lines, Ꮩ read as Ꭺ falls from 192 to 17, Ꮅ as Ꮲ from 191 to 2, Ꮧ as Ꮨ from 119 to 8, and Ꮎ misread from 155 to 7.
- **Some verses may be memorised.** The synthetic text and every test reference are New Testament text. On the 69 silver lines with no word pair in the synthetic text, it still beats LoC, by less: 15.6% against 20.7%. News pages are unscored.
- **Line mode.** At PSM 7, both it and stock `chr` return nothing on 166 of the 2,160 crops, mostly tall ones; PSM 13 reads them. The stock figures in step 2's tables come from the earlier baseline run; at PSM 13, stock reads the silver lines at 19.7%.

The model is Apache-2.0, like the start model. It is documented in `ocr/models/chr-phoenix/` and isn't deployed.

### Step 4. OCR the 699 missing pages and re-read the 324 ([#1622](https://github.com/tgoodyear/usnewsmap.com/issues/1622))

The GHN-only pages have no layout. Find text lines with a layout model, classify each line's script (syllabary against Latin) on the crop, and read Cherokee lines with the step-3 model and English lines with the engine we use for English. On LoC's pages, keep LoC's English and replace Cherokee lines only where the new reading is better by the step-3 measures (the `cherokee_hybrid.py` path). The production run goes through BOINC, like all OCR; at about 1,000 pages it is one small campaign. **Measure:** Cherokee words searchable (from about 85,000 to an estimated 250,000 or more, if the GHN pages carry the same share), and the proxies on the new text.

**Re-read of the pages with Cherokee lines, 11 and 12 November ([#1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621)).** This is the first part of step 4 to run in production: the 191 of the 324 pages that have Cherokee lines. The other 133 have none and were skipped. The `chr_phoenix` model (step 3) read the Cherokee lines on LoC's pages, and LoC's text stayed for everything else (`export --line-read chr`, [the runbook](../../boinc/apps/ocr-bundles.md)). A page is accepted when every line the job did not read is LoC's text byte for byte, and at least half of the lines it did read took the model's reading (`line-merge-v1`). An accepted page keeps LoC's text beside ours (`text_loc`). The 699 pages without OCR are not part of this run.

| Export | Pages | Lines read | Model's reading | LoC's kept (fallback) | Accepted | Lines changed that weren't read |
| ------ | ----: | ---------: | --------------: | --------------------: | -------: | ------------------------------: |
| Test batch | 5 | 648 | 586 | 62 | 5 | 0 |
| 2 | 61 | 5,229 | 4,972 | 257 | 61 | 0 |
| 3 | 62 | 5,415 | 5,170 | 245 | 62 | 0 |
| 4 | 62 | 5,374 | 5,059 | 315 | 62 | 0 |
| **Total** | **190** | **16,666** | **15,787** | **879** | **190** | **0** |

- **Fallbacks.** A line goes back to LoC's reading when the model read nothing (9 lines), fewer than half as many syllabary letters as LoC (280), or fewer New Testament words than LoC (590), as in `reading_fails` in `usnm-core`.
- **Totals.** 94.7% of the 16,666 lines took the model's reading. All 190 pages were accepted. The run's stop rule halts on any line changed that the job hadn't read, which would be a bug, and none changed. `line-report` also gives a damage figure per page for ours and for LoC's text. Averaged over pages it was lower for ours in exports 2, 3 and 4: 0.0261 against 0.0341, 0.0437 against 0.0528, and 0.0631 against 0.0722. Over all 190 pages it was lower for ours on 50, higher on 3 and equal on 137. It is a damage score; step 3 reports character error.
- **Page not read.** `sn83020874/1829-03-04/ed-1/seq-2` has Cherokee lines but no scan on our storage. It waits until one can be fetched.
- **Test-set pages.** 29 of the 190 pages are test-set pages (13 in export 2, 10 in export 3, 6 in export 4). The production run read them as approved, so their numbers are production output. The model's training lines were cut from the other 161. None of this measures how well the model reads unseen pages; step 3's test does that.
- **Search.** `collect` publishes each accepted page to the OCR output (`ocr-out`); the 190 parts have not been listed to confirm it. The pages reach search at the next full rebuild. Whether they wait for the planned one or get one of their own is open.

**Machine admission.** Each machine ran gold units on the model's worker image (`840487b2…`) and was admitted when its English gold reports passed, under a line-job trust scope of its own (#1753): `chr/chr_phoenix`, with the same `max_cer` (0.0873) as English. Only that entry of `gold/trust.json` changed, checked against a backup each time, and English trust stayed byte for byte as it was.

- One of our machines passed with 20 of 20 gold units, each at 0.0716 character error. It ran the test batch and exports 2 and 3 alone.
- A second machine's first set of 20 units had one unit that skipped a crop (29 of 30), after its client had been paused and resumed. It was not admitted from that set. A fresh set of 20 passed with 30 crops each and the same 0.0716, and the machine took 20 of the 62 jobs of export 4. The first set was kept as evidence.
- A third machine had finished none of its 20 units by the end of the run, because its two CPUs were held by OCR tasks that average 2.5 hours. It is not in the scope. One more `gold-trust` adds it when its units finish.

**Scheduler batch check.** The first gold units could not be sent. The scheduler's batch patch expected batch 1 of every Docker plan class it didn't list, so it refused every unit in batch 1621 for the new `docker_lines` class, silently: the units never got a result row, and the scheduler's log said `App version for assigned WU not found` ([#1772](https://github.com/tgoodyear/usnewsmap.com/pull/1772)). With the batch bounds in `plan_class_spec.xml` now authoritative for any class that names them, the units went out and the run above followed. The runbook now lists the scheduler check, how long gold waits behind a machine's running OCR, and partial trust ([#1793](https://github.com/tgoodyear/usnewsmap.com/pull/1793)).

**Script detection.** Tesseract's own fallback can't split the scripts. With `chr+eng` it re-reads low-confidence words in English and wrote 863 Latin words on test lines that have essentially none, because 16 of the type's letters are Latin small capitals. Per-word script detection is [#1666](https://github.com/tgoodyear/usnewsmap.com/issues/1666). Its classifier looks at the word crop and at the lines around it, since an English name usually follows its syllabary form in parentheses.

### Step 5. The _Cherokee Advocate_ ([#1623](https://github.com/tgoodyear/usnewsmap.com/issues/1623))

895 issues, 1844–1906, the largest Cherokee newspaper online and absent from Chronicling America. Its host's robots.txt bars ClaudeBot by name and asks other crawlers for one request every 3 seconds. Start from its OAI-PMH metadata to count pages and issues; whether to fetch images at all is a decision for the maintainer (below). The _Advocate_ was printed decades after the _Phoenix_, so step 3's model may need a second round of fine-tuning on it.

### Step 5 result: the _Advocate_'s holdings in the Gateway's metadata

Harvested on 12 November 2026 from the Gateway's OAI-PMH endpoint, set `collection:CHRAD` ("Cherokee Advocate"), in two requests (`ListSets`, then one `ListRecords` that returned the whole set with no resumption token). Before the first request I read the site's robots.txt, which allows `/oai/` for crawlers without their own entry (it bars ClaudeBot by name and sets a crawl delay of 3 seconds for the rest), and its terms of use, which make the content available for research and say nothing against automated metadata access. The script sends a generic research-script User-Agent, waits 3 seconds between requests, and stops on any HTTP error. No images and no OCR text were fetched.

| Count                               | Value                 |
| ----------------------------------- | --------------------- |
| Issues (records)                    | 895                   |
| Pages (sum of each record's extent) | 3,578                 |
| Years with at least one issue       | 42, from 1845 to 1906 |

Every record carries an extent ("four pages", "six pages") and a date, so the page count is the sum of stated extents, not a count of page images. 863 issues are four pages, 16 are two, and 16 run from one to eight. By year, the holdings fall into three blocks:

- 1845 to 1849: 6 issues, 26 pages. The early run is thin.
- 1850 to 1869: none. The Gateway's metadata lists no issue for these 20 years.
- 1870 to 1906: the other 889 issues. 1876 to 1883 has 354 of them, and 1900 to 1906 has 286.

The metadata puts the first issue in 1845, not 1844 as the plan above says, and 3,578 stated pages is the size of the page-image set if a later step fetches images. The totals (first line) and per-year counts (one line each) are in [`advocate-holdings.jsonl`](advocate-holdings.jsonl), converted from the script's JSON output. Re-run with `python3 docs/reports/2026-11-cherokee/scripts/chr_advocate_oai.py --set-name "Cherokee Advocate" --out advocate-holdings.json`; its offline tests are `python3 -m unittest docs/reports/2026-11-cherokee/scripts/test_chr_advocate_oai.py`.

### Decisions

Made on 6 November 2026:

- **Ground truth.** Synthetic and aligned references only (step 2). A set transcribed by a Cherokee reader would still be the only fully trustworthy test; no one is being asked for one. Results from steps 2 and 3 are reported with that limit.
- **The _Advocate_.** Metadata only for now, through the Gateway's OAI-PMH interface. Whether to fetch images is decided after the step-3 model works.
- **Order.** Step 1 (confusion-tolerant search) first.

## Reproducing

The scripts in [`scripts/`](scripts/) take only named arguments and write JSON to standard output. Run them from the repository root.

```sh
S=docs/reports/2026-11-cherokee/scripts
D=docs/reports/2026-11-cherokee/data
scripts/ops/data-assets.sh --fetch --set report-2026-11-cherokee
python3 $S/chr_alto_stats.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2
python3 $S/chr_masthead.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2
python3 $S/chr_confusions.py --eval-dir $D/eval
python3 $S/chr_letter_pairs.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2 --wordlists ocr/ja-ocr/wordlists
python3 $S/chr_dict_compare.py --out-dir $D/dict-compare/out --refs $D/eval/nt-reference/nt-engine-sample/refs --wordlists ocr/ja-ocr/wordlists --baseline A_stock
python3 $S/chr_checked_score.py --answers $D/hand-check/glyph-check-answers.jsonl --engine loc=$D/hand-check/loc.jsonl --engine stock=$D/testset-v2/baselines/tesseract-chr-stock.jsonl --out checked-v1.json
python3 scripts/ocr-survey/cherokee_testset.py score --lines $D/testset-v2/crops/lines.jsonl --pred loc --pred chr_phoenix=$D/readings/phoenix-round2-psm13.jsonl --tier silver
python3 $S/chr_ocr_variant_recall.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2 --wordlists ocr/ja-ocr/wordlists
```

**Inputs.** LoC's batches come from `https://chroniclingamerica.loc.gov/data/ocr/<batch>.tar.bz2`:

| Batch                                 | sha256                                                             |
| ------------------------------------- | ------------------------------------------------------------------ |
| `dlc_misctopsn83020866_ver03.tar.bz2` | `32fdb37c101f552c8a58bd3461fdf969d2c1a6dd57682fa2213cee09ac82e9cc` |
| `dlc_misctopsn83020874_ver02.tar.bz2` | `b4dbf6c3f53ec5f43ae8bdd9434e1b5ac7363d830c0dd350547c957d02d976a0` |

The word lists are in the repository. The other inputs are in the release [`data-report-2026-11-cherokee-20261109-82de31f`](https://github.com/tgoodyear/usnewsmap.com/releases/tag/data-report-2026-11-cherokee-20261109-82de31f), with the source-rights notice. `scripts/ops/data-assets.sh --fetch --set report-2026-11-cherokee`, run from the repository root, checks the tarball against `ops/data-assets.json` and extracts it to `data/`, whose README describes each file:

| Asset                                 | SHA-256                                                            |
| ------------------------------------- | ------------------------------------------------------------------ |
| `report-2026-11-cherokee.tar.gz`      | `a5cbccdc1bcad45298e64a8fa832150ba4aaebff790e5116ee35065d88b429f6` |
| `report-2026-11-cherokee.members.txt` | `8ad10ab845529fb56f2a89335faa13fa1694a97879483ace4c44961ecc09b9ad` |

- `chr_confusions.py` reads the #1581 evaluation outputs in `data/eval/` (`nt-reference/nt-engine-sample/` and `region-aware/out/hybrid/`).
- `chr_dict_compare.py` reads the dictionary comparison's readings in `data/dict-compare/out/` and the New Testament references in `data/eval/nt-reference/nt-engine-sample/refs/`.
- `chr_checked_score.py` reads the hand-check answers and LoC text in `data/hand-check/` and stock Tesseract's readings in `data/testset-v2/baselines/`.
- `cherokee_testset.py score` scores the readings in `data/readings/` (`chr_phoenix` rounds one and two, `chr_phoenix+eng`, stock `chr` at PSM 13) against the test set v2 crops in `data/testset-v2/crops/`. Its `SHA256SUMS` checks the crops.

**The page list for the line jobs (#1621).** [`phoenix-pages.jsonl`](phoenix-pages.jsonl) lists the 324 pages, one JSON line each: the page, its batch and version, its ALTO's sha256, the lines `export --line-read chr` reads, whether test set v2 holds it, and the local copy of its scan (LoC's full-size IIIF JPEG: URL, bytes, sha256), or `null`. It was made without fetching anything, from the two archives above unpacked and the scans already on our storage:

```sh
usnm-boinc line-pages --loc-alto <the two archives' ocr.xml, unpacked> \
  --title sn83020866=dlc_misctopsn83020866_ver03:print --title sn83020874=dlc_misctopsn83020874_ver02:print \
  --scans <scan copies> --test-lines $D/testset-v2/crops/lines.jsonl > phoenix-pages.jsonl
```

191 pages have lines to read, and 190 of those have a local scan. The one without is `sn83020874/1829-03-04/ed-1/seq-2`. Its 29 test-set pages are marked.

The search counts are from `https://api.usnewsmap.com/v1/aggregate?q=…` on the dates given.
