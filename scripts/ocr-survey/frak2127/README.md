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
| `train_b.py` | Arm B: the fixed budget and stopping rule, the dev-only checkpoint choice |
| `select_bank_pages.py` | Freezes arm C's US German bank pages and their exclusion manifest (amendment 1) |
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
