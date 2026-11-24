# Fraktur fine-tune, #2127 (phase 1)

Scripts for the preregistered Fraktur study in issue #2127: line crops for every set, and arm B,
Tesseract `frak2021` 0.905 fine-tuned on real newspaper lines. The recipe follows
`ocr/models/chr-phoenix/recipe/` (a WordStr box per line, `lstm.train`, `--continue_from`, an
evaluation list). Nothing here reads the 115 held-out pages with any model; their lines are cut into
a separate tree for the scoring step that comes later. No image, text or model is committed.

| File | What it does |
| --- | --- |
| `frak_norm.py` | The issue's normalisation, for training labels and for scoring |
| `cut_lines.py` | PAGE-XML lines to `<name>.png` + `<name>.gt.txt`, with the split rule and the held-out checks |
| `frak_score.py` | Reads crops at PSM 13 and scores them: normalised, pooled CER (refuses held-out paths) |
| `train_b.py` | Arms B and C (`--arm C --synth-lstmf`): the fixed budget and stopping rule, the dev-only checkpoint choice |
| `select_bank_pages.py` | Freezes arm C's US German bank pages and their exclusion manifest (amendment 1) |
| `build_wordlist.py` | Arm C's German word list: DTA 1800–1899 core corpus plus wordfreq German, normalised |
| `trusted_words.py` | Arm C: words where LoC and stock frak2021 agree, in the word list, cut as word crops |
| `synth_lines.py` | Arm C: degradation tuned on dev, then synthetic training lines from the glyph bank |
| `test_frak_norm.py` | Tests of the normalisation |

## Splits

- Test (held out): the 115 pages of `heldout-pages.txt` (sha256 `6b0c03e7…7ce5`), re-derived from
  `split.csv` by the issue's rule (not Test, not Kölnische Zeitung, year 1830 to 1933) and asserted equal.
- Model selection (dev): the 17 Kölnische Zeitung Validation pages (`KoeZ=1`, `Validation=1`).
- Training: Kölnische Zeitung Train pages (`KoeZ=1`, not Test, not Validation), every page of
  Reichsanzeiger-GT (the plain `GT-PAGE` folder), NZZ black-letter and ONB/NewsEye Austrian.
- Unused: the official Test split and the other Train/Validation pages.

`cut_lines.py` asserts that no train or dev line comes from a held-out or official-Test page.

## Crops

The crop is the bounding box of the TextLine's `Coords` polygon plus `PAD_PX` = 4 px on each side,
clipped to the page, in 8-bit grey. Lines with no text of their own or that normalise to nothing are
skipped (`empty`); so are polygons under three points or boxes under 5 px on a side (`degenerate`).
Training crops taller than 36 px, `frak2021`'s network input height, are scaled down to it with a
box filter (area averaging, as Tesseract shrinks a line itself), so the training set fits the dev
CT's disk, and are kept only inside `.lstmf` files, one line each as in `chr_finetune.sh` (a TIFF plus a WordStr
box, read by `tesseract --psm 13 lstm.train`). Dev lines get `.lstmf` files at the
page's resolution and also stay as PNG for scoring. Held-out lines are PNG only, in their own tree.
Lines with a character outside `frak2021`'s character set get no `.lstmf` (lstmtraining could not
encode them); they are counted as `unencodable`.

```sh
python3 cut_lines.py --data DATA --heldout heldout-pages.txt \
    --unicharset frak2021.lstm-unicharset --tessdata TESSDATA --out lines --lstmf-out lstmf \
    --heldout-out lines-heldout --jobs 12
```

## Arm B training

`train_b.py` fine-tunes `frak2021` 0.905 (sha256 checked against `ocr/engines/tesseract/models.lock`)
with `lstmtraining --continue_from`, same character set, as `chr_finetune.sh` does. Fixed before the
run, and the same for arm C:

- Budget: 200,000 training iterations (one line each).
- Every 10,000 iterations the checkpoint becomes a model and is read on the 17 dev pages
  (`frak_score.py`); training stops early after 5 evaluations in a row without a better dev CER.
- The chosen model is the evaluated checkpoint with the lowest dev CER (earliest on a tie). The start
  model's dev CER is reported as iteration 0 but is not a candidate.
- `lstmtraining`'s evaluation list is the dev lines. The training list is shuffled with seed 2127.

```sh
python3 train_b.py --lines lines --train-lstmf lstmf/train --dev-lstmf lstmf/dev \
    --start frak2021-0.905.traineddata --heldout heldout-pages.txt --split-csv cg/split.csv --work work/b
```

Outputs in `--work`: `dev_scores.jsonl` (one row per evaluation), `dev_<iteration>.jsonl` (every dev
line's reading), `models/frakB_<iteration>.traineddata`, `train.log`, `result.json` and the chosen
`frak2127_b.traineddata`.

## Arm C bank pages

`select_bank_pages.py` freezes 250 US German Chronicling America pages for the glyph bank. The
German titles are the 85 LCCNs of the Fraktur pilot's German sample (`ocr/fraktur-pilot/pages.jsonl`).
A page needs a keep-set scan, LoC ALTO in the bulk store (its batch archive, and a locstats row with
words), a `ger` page-language decision where its batch has one, and no appearance in a US German
evaluation artefact (the de968 crops, the check site's German set, the Qwen calibration pages, the
Fraktur pilot pages, the reading-order fixtures, and #835's 1910 `iahi_feebas` pages by rule). Pages are
ranked by `sha256("2127:bank:" + page_id)`, queued by decade within each title, and taken a title at a
time, at most 15 a title. It writes `pages.txt`, `pages.jsonl`, `exclusion-manifest.json` and
`summary.json`. Needs `pyarrow`.

## Arm C: glyph bank and synthetic lines (amendment 1)

How the bank was built, in order. Every step ran in the dev CT as `nobody`.

1. **Word list** (`build_wordlist.py`).
   - The Deutsches Textarchiv's 1800–1899 core corpus as plain text
     (`dta_kernkorpus_1800-1899_2020-09-23_text.zip` from deutschestextarchiv.de/download, CC BY-SA 4.0,
     sha256 `3e90914b…3682`), every word seen at least twice.
   - Plus wordfreq 3.1.1's `large_de` list (from the PyPI wheel, sha256 `4b1c6ecf…7473`; the list's
     sha256 is `8ef04d3f…3c12`; word data CC BY-SA 4.0).
   - Both are normalised with `frak_norm` and lower-cased.
2. **LoC ALTO and scans.** The 250 bank pages' `ocr.xml` files come from their batches' bulk archives,
   streamed through the CT so that only those files are kept. The scans come from the scan host's keep set.
3. **Trusted words** (`trusted_words.py`).
   - Stock frak2021 0.905 reads each page at PSM 3, writing TSV word boxes.
   - A LoC word's partner is the frak2021 word with the highest IoU, which must be at least 0.5.
   - A word is trusted when both readings normalise to the same string, its core has at least 3
     letters and is letters only, and it is in the word list.
   - Each trusted word is cut with `usnm_glyphs::word_crop`.
4. **Bank** (Rust, `crates/usnm-glyphs`).
   - `select_bank_words.py` limits the bank to fit the CT's disk. It takes trusted words in hash order
     while any of their letters has fewer than 3,000 glyphs, so rare letters keep all their words.
   - `usnm-glyphs build-words --words trusted/words-bank.jsonl --out bank` splits each crop into one glyph
     per letter of the core (`split_word`). ſ is already folded into s, umlauts and ß are letters of
     their own, and a ligature gives its letters.
   - `usnm-glyphs clean --bank bank --k 9 --min-agree 0.5` is the same neighbour vote and settings as
     `chr_phoenix`.
5. **Synthetic lines** (`synth_lines.py`).
   - The text is real training lines' text (never dev or held-out text).
   - Each glyph is set at its own height in its word crop, and marks are set from DejaVu Serif.
   - `tune` picks one of five degradation presets. The criterion is the operation profile and CER of
     stock frak2021 on 800 rendered lines, against stock frak2021 on the 17 dev pages' real lines.
   - `render` makes one synthetic line per three of B's real training lines, scales them to 36 px and
     writes `.lstmf` files.
   - No traced font: `chr_phoenix`'s synthetic lines didn't use theirs either.
6. **Training.** `train_b.py --arm C --synth-lstmf …` uses B's budget, rule and seed, on B's real lines
   plus the synthetic lines.
