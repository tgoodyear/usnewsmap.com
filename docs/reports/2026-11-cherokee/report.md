# Cherokee: what is searchable, how good the OCR is, and a plan

6 November 2026. Follows [#1581](https://github.com/tgoodyear/usnewsmap.com/issues/1581) (the word lists and the engine tests) and [#1590](https://github.com/tgoodyear/usnewsmap.com/issues/1590) (the Georgia Historic Newspapers images).

## Summary

- **Volume.** usnewsmap.com searches 324 pages of the _Cherokee Phoenix_ (82 issues, 1828-03-06 to 1833-10-19). They hold about 85,000 words of Cherokee syllabary, on 191 pages. That is 32% of the 1,023 pages LoC and GHN list for the paper. Images of the other 699 pages are already on our storage, without OCR. No other Cherokee-language newspaper is in the index. The largest one outside it is the _Cherokee Advocate_ (1844–1906), 895 issues on the Gateway to Oklahoma History.
- **Accuracy.** The first measured figure comes from 70 lines checked against the page images: LoC's Cherokee OCR misreads at least 8–9% of syllabary letters, and stock Tesseract 16–17% (step 2). The proxies we can compute say LoC's Cherokee OCR is weak, and weak in particular letters. ᎾᏍᎩ ("that"), the second most common word in the New Testament, is read correctly 127 times and misread as ᏩᏍᎩ, ᏣᏍᎩ or ᏳᏍᎩ 1,007 times. ᎠᎴ ("and", the most common) is read correctly 2,547 times in 2,555. The paper's name, ᏣᎳᎩ ᏧᎴᎯᏌᏅᎯ, reads exactly on 47 of 82 front pages. It appears in a standing notice in the first column, not in the masthead. LoC's word confidences are no help: 99.8% of Cherokee words have a confidence of at least 0.5, and the median is 1.0.
- **Models.** Tesseract's Cherokee model reads clean typeset text well: 1.67% character error on the New Testament PDFs. On Phoenix scans it makes the same letter confusions LoC's OCR makes. Read line by line, it improves the word-list proxy only slightly (32.5% of words known, against LoC's 31.2%). Our Qwen3-VL-2B route produced no syllabary at all, and in a 2026 benchmark none of 14 vision-language models read Cherokee.
- **The type.** The _Phoenix_ was set in one font, cast in Boston in 1827. Nine of its letters were recut in 1834, mostly because they looked too much like other letters. Three of them (Ꮩ, Ꮅ and Ꮦ) are among the pairs our engines confuse (section 4). The recut came after the paper's last issue, so every _Phoenix_ page has the 1828 shapes.
- **Plan.** Build ground truth without new transcription work where we can, train a Phoenix-specific model, OCR the 699 missing pages, and make search tolerant of the known confusions in the meantime. The search change is the quickest gain: a query for ᎾᏍᎩ now finds 35 pages, and its three common misreadings are on far more.

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
| Syllabary characters                              | 368,582               |
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

There is no checked transcription of any _Phoenix_ page, so no figure below is an error rate. They are proxies, each computed the same way for any reading of the pages so that two engines can be compared.

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

From the literature, on other material:

- Zhang et al. ([2022](https://arxiv.org/pdf/2204.11909)), 20 book pages: Tesseract 23.0% character error on full pages and 6.3% on clean crops; Google Cloud Vision 19.9% and 7.4%. Google lists Cherokee as experimental and not regularly evaluated. Google Document AI and Azure don't list it.
- GlotOCR Bench ([2026](https://arxiv.org/html/2604.12978)), 100 synthetic Cherokee sentences: none of 14 vision-language models, among them GPT-4.1, Gemini 3.1 Flash-Lite and Qwen3-VL-8B, read any sentence under 5% character error.
- No Transkribus or Kraken model for the syllabary was found, and no published handwriting work.

The New Testament results show Tesseract can read Cherokee type, and the _Phoenix_ results show it doesn't know this type. Two unrelated engines make the same confusions, which points at the glyphs. A model trained on _Phoenix_-like lines is the change most likely to help. The vision-language route is closed for now.

## 4. The press's type

Patrick Giasson's MA thesis, _The Typographic Inception of the Cherokee Syllabary_ (University of Reading, 2004), traces the first Cherokee font from Sequoyah's drawings to the foundry. It is the only source we have found that shows the 1828 letter shapes next to the 1834 ones. Everything below is from it unless marked as ours.

**The 1827 font.** The Baker & Greele foundry in Boston cut and cast the font in 1827 from Samuel Worcester's drawings. The press used it from the first _Phoenix_ in February 1828. Sixteen letters (Ꭱ Ꭰ Ꮃ Ꮐ Ꮲ Ꮇ Ᏼ Ꭺ Ꮓ Ꭼ Ꭲ Ꭻ Ꮶ Ꮪ Ꮋ Ꮮ, in the Latin shapes R D W G P M B A Z E T J K S H L) are the small capitals of the foundry's small pica roman, a Scotch Roman. The other 69 were cut new. The Cherokee was cast at small-capital height on the same small pica body as the English, so the two can share a line. With the 1834 changes and a larger companion font added in the late 1850s for capitals and titles, the same typeface set almost all printed Cherokee until the 1970s. That includes the _Cherokee Advocate_.

**The 1834 recut.** In 1834 Worcester had letters recut (the thesis says ten and describes nine) because they were "sufficiently distinct when the print is clear but we find that we cannot always rely upon it" (his letter, ABCFM 18.3.1 v.7, 234). The new type reached the press in 1835. The _Phoenix_ had stopped in May 1834, so none of its pages has the new shapes. Against our measurements:

| Letter | 1828 shape and its trouble                                            | 1834 change                     | What we measured (sections 2 and 5)                                       |
| ------ | --------------------------------------------------------------------- | ------------------------------- | ------------------------------------------------------------------------- |
| Ꮩ do   | Λ, an upside-down V, taken for Ꭺ go                                   | Small capital V                 | Ꮩ read as Ꭺ 430 times; Λ was Ꮩ in 22 of 26 hand-checked disputes          |
| Ꮅ li   | A P with a flag, taken for Ꮲ tlv                                      | Rounded off                     | Ꮅ read as Ꮲ 1,007 times, the commonest misreading                         |
| Ꮦ te   | Its short left stroke broke off the metal, leaving Ꮣ da              | Stroke enlarged, tail closed    | Ꮣ/Ꮦ 44 disagreements                                                      |
| Ꮯ tli  | G-shaped, taken for Ꮆ lo                                              | Small capital C                 | Not separated yet; see the G-shaped letters below                         |
| Ᏺ yo   | A plain h, the same as Ꮒ ni except the top serif                      | Hooked ascender                 | Not measured                                                              |
| Ꮢ sv   | Close to Ꭱ e                                                          | Counter opened, leg straightened | Not measured                                                             |
| Ꮥ de   | Close to Ꭶ ga, which it often stands next to                          | New shape                       | Not measured                                                              |
| Ꮾ wv   | Reason not given                                                      | Made like a 6                   | Ꮎ/Ꮾ 46 disagreements                                                      |
| Ꮌ mo   | Reason not given                                                      | Recut twice                     | Not measured                                                              |

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

| Text                    | 70 lines | 47 silver lines |
| ----------------------- | -------: | --------------: |
| 1860 New Testament      |    10.5% |            3.3% |
| LoC OCR                 |     8.4% |            9.2% |
| Tesseract stock         |    17.2% |           16.5% |
| Stock + NT word list    |    16.6% |           15.5% |

These are the first measured error rates for the index's Cherokee text. About half of the error measured against the unchecked reference was the reference's own. The ranking doesn't change. The lines with a "neither" answer are left out, so the 70 lean clean. The checked text is built from LoC's and the reference's words, which flatters LoC, so its figure is a floor.

The answers back up the letter findings above. Λ was Ꮩ in 22 of 26 Ꮩ/Ꭺ disputes, Ꮅ beat Ꮲ in 22 of 25, and Ꮧ beat Ꮨ in 23 of 24. All 16 disputes between Ꮎ and Ꮹ or Ꮳ came out neither or can't tell: the paper prints a G-shaped letter in ᎾᏍᎩ, not today's θ-like Ꮎ. Which letter it is hasn't been settled yet. Specks in the margin also get read as letters. Digits aren't scored at all: LoC read a verse number "24." as "9Ꮞ.".

### Step 3. A _Phoenix_ model ([#1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621))

Fine-tune Tesseract's `chr` model with `tesstrain` on the synthetic lines plus the aligned real lines, holding out the test set's pages and the standing notice for testing. Try Kraken as a second engine if Tesseract stalls. **Measure:** character error on the held-out lines, the ᎾᏍᎩ share, masthead exact reads, and the word-list proxies, against LoC and stock `chr`. Go on to step 4 only if it beats LoC on the held-out real lines.

**Dictionary-only test, 6 November.** Before any retraining, the word list inside the stock model was replaced, with the network unchanged ([`scripts/chr_dict_compare.py`](scripts/chr_dict_compare.py)). Adding the 24,479 New Testament forms to the model's 6,368 words changed about 2.3% of the tokens on the same 1,320 _Phoenix_ lines. Of the changes, 127 turned a non-word into a New Testament word and 3 went the other way. ᎾᏍᎩ was still read right only 7 times, as with the stock list, which already held the word. Adding ChrEn's 54,922 forms as well made the readings worse: 30 known words reversed, 165 other changes, and ᎾᏍᎩ down to 5. So the word list helps a little but cannot fix the letter shapes. The fine-tune keeps the New Testament list, and ChrEn goes into the rendered training text instead. Results and hashes are on [#1621](https://github.com/tgoodyear/usnewsmap.com/issues/1621).

### Step 4. OCR the 699 missing pages and re-read the 324 ([#1622](https://github.com/tgoodyear/usnewsmap.com/issues/1622))

The GHN-only pages have no layout. Find text lines with a layout model, classify each line's script (syllabary against Latin) on the crop, and read Cherokee lines with the step-3 model and English lines with the engine we use for English. On LoC's pages, keep LoC's English and replace Cherokee lines only where the new reading is better by the step-3 measures (the `cherokee_hybrid.py` path). The production run goes through BOINC, like all OCR; at about 1,000 pages it is one small campaign. **Measure:** Cherokee words searchable (from about 85,000 to an estimated 250,000 or more, if the GHN pages carry the same share), and the proxies on the new text.

### Step 5. The _Cherokee Advocate_ ([#1623](https://github.com/tgoodyear/usnewsmap.com/issues/1623))

895 issues, 1844–1906, the largest Cherokee newspaper online and absent from Chronicling America. Its host's robots.txt bars ClaudeBot by name and asks other crawlers for one request every 3 seconds. Start from its OAI-PMH metadata to count pages and issues; whether to fetch images at all is a decision for the maintainer (below). The _Advocate_ was printed decades after the _Phoenix_, so step 3's model may need a second round of fine-tuning on it.

### Decisions

Made on 6 November 2026:

- **Ground truth.** Synthetic and aligned references only (step 2). A set transcribed by a Cherokee reader would still be the only fully trustworthy test; no one is being asked for one. Results from steps 2 and 3 are reported with that limit.
- **The _Advocate_.** Metadata only for now, through the Gateway's OAI-PMH interface. Whether to fetch images is decided after the step-3 model works.
- **Order.** Step 1 (confusion-tolerant search) first.

## Reproducing

The scripts in [`scripts/`](scripts/) take only named arguments and read LoC's batch tars and the earlier evaluation outputs; they write JSON to standard output.

```sh
python3 scripts/chr_alto_stats.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2
python3 scripts/chr_masthead.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2
python3 scripts/chr_confusions.py --eval-dir EVAL_DIR
python3 scripts/chr_letter_pairs.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2 --wordlists ocr/ja-ocr/wordlists
python3 scripts/chr_dict_compare.py --out-dir OUT_DIR --refs NT_REF_DIR --wordlists ocr/ja-ocr/wordlists --baseline A_stock
python3 scripts/chr_checked_score.py --answers glyph-check-answers.jsonl --engine loc=loc.jsonl --engine stock=tesseract-chr-stock.jsonl --out checked-v1.json
python3 scripts/chr_ocr_variant_recall.py --tar dlc_misctopsn83020866_ver03.tar.bz2 --tar dlc_misctopsn83020874_ver02.tar.bz2 --wordlists ocr/ja-ocr/wordlists
```

`EVAL_DIR` holds the #1581 evaluation outputs (`nt-reference/`, `region-aware/out/hybrid/`); they are not in the repository. The search counts are from `https://api.usnewsmap.com/v1/aggregate?q=…` on 6 November 2026.
