# Per-word script detection (#1666)

Pages that mix scripts inside a line need each word sent to the right OCR model. This directory holds the first case, the _Cherokee Phoenix_ (1828–1834). On a line that is mostly syllabary, the classifier finds the words that are not syllabary: Latin words, numerals, rules and margin noise. Those go to `eng`, and the rest go to the syllabary model (`chr`, or `chr_phoenix`, fine-tuned in #1621).

The tools run in the image built from `Containerfile`: Tesseract with `eng`, and Python with numpy, scipy, Pillow, scikit-learn and PyTorch (CPU). The syllabary models are mounted and passed with `--tessdata`. Each subcommand of `script_detect.py` takes named flags, and `--help` lists them.

```sh
podman build -t usnm-script-detect -f ocr/script-detect/Containerfile ocr/script-detect
podman run --rm -v "$PWD/ocr/script-detect:/work" usnm-script-detect python3 -m unittest -v test_script_detect
```

| Step                                       | What it does                                                                                                                                         |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------- |
| `survey`                                   | Counts the kinds of word LoC read on syllabary lines (lines where LoC read two or more syllabary words)                                              |
| `words`                                    | One record per word of every syllabary line: its box, its line's box, the boxes and LoC text of the lines before and after, and its text block's box |
| `ocr`                                      | One Tesseract model over every syllabary line (or the test set's crops) at PSM 13; word boxes come back in page pixels                               |
| `synth`                                    | Syllabary lines with real English words from the same paper pasted in, for the Latin class                                                           |
| `features`                                 | The classifier's inputs for each word (`sd_features.py`)                                                                                             |
| `glyph-donors`, `glyph-crops`, `glyph-cnn` | Word crops from the English columns and for every word, and the two cross-fitted glyph CNNs that score them (`sd_cnn.py`)                            |
| `train`                                    | Labels (hand labels, then rules), the report on held-out pages, and the final model                                                                  |
| `route`                                    | Routes each word of the frozen test set's lines and reads each run of same-route words with its model                                                |
| `score`                                    | The test set's syllabary-letter CER on the silver tier, on all lines and on the hand-checked lines                                                   |
| `sheet`                                    | Contact sheets for labelling by eye: each row is a word in its line, boxed and numbered                                                              |

## Inputs

- LoC's ALTO for the 190 _Phoenix_ pages that have a syllabary line, with the page scans. The word boxes come from LoC's segmentation.
- The 1860 New Testament form list (`crates/usnm-core/data/damage-words/chr-attested.txt.zst`) and the English word list (`crates/usnm-core/data/english-words.txt.zst`).
- The frozen Cherokee test set of #1620 (`testset/v2`, with `cherokee_testset.py`). Its 29 pages are never trained on: `words` marks them `test`, and `train` leaves them out.

## The classifier

It works in two stages: a glyph CNN that looks only at the word's letters, and a router that weighs that verdict against the context and the two OCR readings.

### Glyph CNN

Inline Latin is rare, but the same scans carry thousands of words of the same roman type in their English columns. A small CNN (four convolution blocks) learns from those three classes: Latin (48,300 English-column words; capital-only words are left out because in this type they look like the syllabary), other (numerals and punctuation, from the English columns and the rules), and syllabary (up to 300 rule-labelled words per page). Each crop is cut tight to the word's ink and scaled to 32 pixels high, so the CNN judges letter shapes only.

It is cross-fitted. The pages are split in two by a hash of their names, a CNN is trained on each half, and every word is scored by the CNN that did not see its page (test pages get the mean of both). On the half it did not train on, it calls 99% of English-column words Latin, and 2 to 6% of syllabary words Latin.

### Router

For every word, the router's inputs come from five crops, as the issue asks:

- the word itself;
- its line;
- the line before and the line after;
- the text block it sits in.

Each crop becomes a small set of measures:

- **Letter shapes**, relative to the line's body (the band between the tops and bottoms of most letters). Two measures are how much ink sits in the upper third of the body, and the share of shapes that are short (x-height), rise above the body or drop below it. Latin lowercase leaves the upper third nearly empty; the syllabary, cut at capital height, fills it. The others are letter width and spacing.
- **Scan quality**: paper-to-ink contrast, how cleanly Otsu separates them, sharpness, speckle, stroke width and uneven paper. With these the model can weigh the readings by how degraded the crop is.

To these come the two models' readings of the word, from a PSM 13 pass of each over the whole line:

- each model's confidence, and the difference between them;
- whether either reading is a known word, in the New Testament forms or the English list;
- how plausible the `eng` reading is as Latin-script text, from letter bigrams of the word list;
- lowercase letters, and capitals with no syllabary lookalike in this type;
- whether `eng` read only the 16 capitals the syllabary shares with Latin small capitals (R D W G P M B A Z E T J K S H L);
- the same readings for the words either side and for the line;
- the glyph CNN's three probabilities for the word, for the words either side and averaged over the line.

LoC's own reading is never an input. The labels are built from it, and pages from other sources have none.

The router is scikit-learn's `HistGradientBoostingClassifier` (300 trees of depth 3, classes weighted equally). It gives the probability that a word is not syllabary, and words at or above the threshold (0.9, chosen on the development pages) go to `eng`. The design splits the work this way for three reasons:

- The glyphs are best learned where there is plenty of each script: the English columns and the syllabary text of the same scans. A CNN trained only on inline Latin would have 71 real words to learn from.
- What the glyphs can't settle, the context and readings can. ᎥᏝ and Latin "if" look alike, and the 16 shared capitals make `eng` read ᎠᎴ as "DE" or "DS". Gradient-boosted trees combine those signals with the CNN's verdict and the shape and quality measures, with no scaling.
- Both stages train on CPU: the CNN in under an hour, the router in minutes. The router's permutation importances show what it relies on, which is mainly the CNN's syllabary probability, then the syllabary model's confidence.

For another script pair the same features apply with that pair's models and word lists (`usnm_core::lang`). The shared-letter set becomes that pair's: for Cyrillic and Latin, А В Е К М Н О Р С Т Х; for Fraktur and Antiqua there is none. The donors for `synth` are Antiqua or Latin lines of the same paper.

## Labels

| Source    | How                                                                                                                                                                                                                                                                                                                                                                                                                                      | Precision checked by eye                                          |
| --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| Hand      | `labels/hand.jsonl`: 486 words labelled from contact sheets. 200 are a random sample (80 training, 120 test pages), 60 are words in parentheses, 225 are every word of the 64 lines where `eng` found Latin-looking words, and 1 corrects a rule label                                                                                                                                                                                   | –                                                                 |
| Rules     | Syllabary: LoC read a New Testament form of two or more letters, or a syllabary word of three or more letters in a block that is nine-tenths syllabary which `eng` does not read as an English word of four or more letters. Other: LoC read no letters (numerals, punctuation, rules). Latin: `eng` read an English word of four or more letters with two lowercase at confidence 85 or more                                            | syllabary 40/40, other 39/40, Latin 8/9                           |
| Synthetic | `synth`: one word, or a run of two to four, from an English line of the same page (or another training page) replaces as many words of a syllabary line. The paste is scaled so its capital height matches the syllabary's, times a random factor from 0.85 to 1.5; its baseline is set on the host's, and its tones are mapped onto the host's paper and ink. A lone pasted word has four or more letters or a capital: see `cmd_synth` | a sample of 20 lines looked right but for a few mis-scaled pastes |

Labels: `l` Latin, `o` other (numerals, punctuation, rules, blank boxes), `s` syllabary. The router sends `l` and `o` to `eng`.

## Results

### Amount of Latin on syllabary lines

LoC read every word of the 16,086 syllabary lines as syllabary (`survey`). Its 86,040 words on those lines sort into 28,356 New Testament forms, 54,511 other syllabary readings, 1,314 numerals, 1,813 punctuation and 46 ellipses. It read no Latin letters at all.

Our own labels give these counts:

- In a random sample of 200 words, 186 are syllabary, 14 are other (numerals, rules, blank boxes) and none is Latin.
- 71 words are confirmed Latin by hand, on 52 lines of 4 pages. 55 of them are on one page (1828-09-03, seq. 2), a vocabulary lesson that prints each Cherokee word with its transliteration (`ᎠᏍᎦᏯ a-ska-ya`). The others are inline English such as "(Cherokee Academy)", "u-wo-du [pretty]", "at widow Nettle Carri-", "(a pair of elegant", "Judge Clayton", and an italic hymn heading ("Morning Hymn. C. M.") that LoC put on a syllabary line.
- The search that found these needed `eng` to read Latin-looking words, so 71 is a lower bound.

Inline Latin is rare: under 0.1% of words and 0.3% of lines. Numerals, rules and specks are about 7% of words, and the syllabary model reads them as letters.

### The classifier on held-out pages (threshold 0.9)

These are the figures for the router that reads with round two of `chr_phoenix` (#1673), the model it ships with.

- **The development pages** are one training page in five, chosen by a hash of the page name: 27 of the 161 training pages, with 12,123 labelled words.
- **Nested evaluation.** For these figures, a second pair of glyph CNNs is trained with the development pages and the two real-Latin pages left out (`glyph-cnn --exclude-dev --exclude-page … --name cnn-eval`). So neither stage has seen a page it is evaluated on. The shipped model uses the CNNs trained on every training page.
- **The test pages** are the frozen set's 29 pages, never trained on.
- **Hand-labelled words that were copied into synthetic lines** count with the synthetic lines.

|                                                                      | Syllabary sent to `eng` | Latin sent to `eng` | Other sent to `eng` |
| -------------------------------------------------------------------- | ----------------------- | ------------------- | ------------------- |
| Development pages, rule and hand labels                              | 15 / 9,503 (0.16%)      | –                   | 329 / 345           |
| Development pages, synthetic lines                                   | 2 / 1,311               | 913 / 921 (99.1%)   | 41 / 43             |
| Test pages, hand labels                                              | 1 / 122                 | 11 / 11             | 13 / 13             |
| The two training pages with real Latin held out (`--extra-dev-page`) | 0 / 85                  | 26 / 60             | 24 / 25             |

Of the 15 real syllabary words sent to `eng` on the development pages, 4 begin with Ꭵ (ᎥᎥ twice, ᎥᎴ, ᎥᏰᏲ). Ꭵ looks like a Latin `i`, and `eng` reads ᎥᎥ as "ii". Two others are dates that LoC read as syllabary (`17.Ꮔ`, `1ᎦᏘ0,`), where the rule label is wrong and `eng` is the right route anyway.

English inline is caught. The lesson page's transliterations are not, when that page has been seen by neither stage:

- The CNN leans Latin on most of them, but is rarely sure. Of the 61 Latin words on the two held-out pages (60 hand-labelled, 1 by rule), 54 score above 0.5 and 27 score 0.95 or more.
- `eng` reads them at confidence 30 to 60, a combination the router has seen on syllabary far more than on Latin. So it routes 26 of the 60 at 0.9 (43 at 0.5).
- Also routing every word the CNN is sure of adds nothing at 0.99, and at 0.9 it would add 9 Latin words for 40 more syllabary words.

The shipped model is trained on the lesson page. A transliteration source for the synthetic lines is the next step for this kind of Latin.

### Test set CER at PSM 13

Syllabary-letter CER from `cherokee_testset.py`'s measure (`score`), on the frozen test set's 700 silver lines, all 2,160 lines and the 70 hand-checked lines (whose checked text is the reference). Round one of `chr_phoenix` is #1621's first fine-tune; round two is #1673.

| Reading                                                           | Silver    | All        | Checked   |
| ----------------------------------------------------------------- | --------- | ---------- | --------- |
| LoC                                                               | 17.98%    | 34.49%     | 8.39%     |
| `chr` (stock)                                                     | 19.66%    | 35.84%     | 12.20%    |
| `chr+eng`                                                         | 24.75%    | 39.00%     | 15.26%    |
| `chr_phoenix` round one                                           | 14.11%    | 30.53%     | 9.69%     |
| `chr_phoenix` round one `+eng`                                    | 15.82%    | 31.72%     | 10.76%    |
| Routed, reading with round one                                    | 10.21%    | 27.64%     | 5.26%     |
| `chr_phoenix` round two                                           | 10.22%    | 27.61%     | 6.56%     |
| `chr_phoenix` round two `+eng`                                    | 12.86%    | 29.67%     | 8.92%     |
| Cut at the routed words, every piece read by round two (ablation) | 10.87%    | 27.98%     | 6.64%     |
| **Routed, reading with round two**                                | **9.21%** | **26.83%** | **5.64%** |

Paired page bootstrap (`score --compare`): the test set's 29 pages (26 for the checked lines) drawn with replacement 10,000 times, both readings scored on each draw. The table gives the difference in CER points with its 95% interval.

| Routed (round two) minus | Silver                | All                  | Checked              |
| ------------------------ | --------------------- | -------------------- | -------------------- |
| round two alone          | −1.01 [−1.36, −0.68]  | −0.78 [−0.92, −0.62] | −0.92 [−2.02, +0.15] |
| round two `+eng`         | −3.64 [−4.35, −2.95]  | −2.84 [−3.18, −2.47] | −3.28 [−4.93, −1.54] |
| LoC                      | −8.77 [−10.02, −7.45] | −7.66 [−8.65, −6.67] | −2.75 [−5.70, +1.58] |

Notes on the tables:

- **Over round two alone, routing gains about one point.** That is a real gain on the silver lines and on all lines, where the intervals exclude zero. On the 70 hand-checked lines it is not established: the interval includes zero. The gain is small next to round one's, where routing cut silver CER by 3.9 points (interval −4.18 to −3.60). Round two was trained with verse numbers, punctuation and margin slivers, so it no longer reads most of them as letters, which was most of what routing fixed on this test set.
- **Tesseract's own two-language mode makes every syllabary model worse.** It tries `eng` on low-confidence words and keeps confident misreadings (ᎠᎴ as "DE" or "DS").
- **The ablation shows the gain is the routing.** It cuts lines at the routed words but reads every piece with round two, and it scores worse than round two alone (+0.65 points on silver, interval +0.31 to +0.95).
- **The test set reprints the New Testament and has almost no Latin.** Its gain comes from the "other" class, the numerals, punctuation, rules and specks routing still takes away from the syllabary model. The held-out table above measures the Latin class, which matters on news and laws but barely moves this test set.

`results/` holds the test-set scores (`scores.json`), the training report (`report.json`: label counts, the confusion by threshold and label source, and the permutation importances), and the report with the real-Latin pages held out. Both reports come from the nested evaluation.

## Reproducing

The labels are in `labels/hand.jsonl` (page, LoC word ID, label, the sheet it came from). Everything else is rebuilt with the subcommands in the order of the table at the top. Each OCR pass over the 16,086 lines took about an hour with two to three threads, and each pair of glyph CNNs about 50 minutes on four. Each other step takes minutes. The run behind these results read with round two's `chr_phoenix.traineddata` (sha256 `88eb1016…`; round one, `17dfa618…`, for its rows) with Tesseract 5.5.0 and Debian's `tesseract-ocr-eng` 4.1.0 (sha256 `7d4322bd…`). The router (`model.pkl`, sha256 `d2e02e9f…`, scikit-learn 1.4.2), the CNNs (`cnn-fold0.pt` `f3131bfa…`, `cnn-fold1.pt` `05f174ed…`, PyTorch 2.6.0) and the evaluation CNNs (`cnn-eval-fold0.pt` `d1bd3471…`, `cnn-eval-fold1.pt` `2c277e34…`) are not committed. A pickle is tied to its scikit-learn version, and models are published as release assets with a model card (`ocr/models/`) once they are used in production.

Not done yet:

- Routing inside the OCR worker.
- A transliteration source for the synthetic Latin, so the router learns that low-confidence lowercase non-words with Latin glyphs are Latin.
- Fraktur and Cyrillic.
