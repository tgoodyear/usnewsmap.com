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
Training crops taller than 48 px are scaled down to 48 px so the training set fits the dev CT's disk;
`frak2021` reads lines at 36 px. Dev and held-out crops keep the page's resolution.

```sh
python3 cut_lines.py --data DATA --heldout heldout-pages.txt \
    --unicharset frak2021.lstm-unicharset --out lines --heldout-out lines-heldout --jobs 12
```
