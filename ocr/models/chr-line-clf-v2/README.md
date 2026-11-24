# chr-line-clf-v2: Cherokee or Latin, one line at a time

This classifier decides which line boxes on a Cherokee Phoenix page are Cherokee, so that only those lines are read with `chr_phoenix`. The GHN staging step (`scripts/ocr-survey/chr_ghn_layout/ghn_boxes.py write`) uses it. It replaces the shape-only classifier of PR #1830, which failed its check on GHN pages in #2128.

A line is called Cherokee when `p_line >= 0.46`, where

`p_line = sigmoid(0.591 * logit(p_v2) + 0.375 * logit(p_cnn) - 0.135)`

- `p_v2` is a logistic regression on the line read twice by Tesseract 5.5.0 at PSM 13, with `chr_phoenix` and with tessdata_best `eng`. Its features are each reading's per-character log-likelihood under a character 5-gram model (Cherokee or English), each reading's mean word confidence, the syllabary share of the `chr_phoenix` reading, the line's height over the page median, and the #1830 shape score. It was fitted on 81,846 LoC lines.
- `p_cnn` is a small CNN on the greyscale line crop.

`scripts/ocr-survey/chr_ghn_layout/line_clf_v2.py` has the code, and `frozen/line_clf_v2.json` there has the numbers.

## Test result

#2128 amendment 3.3 froze the classifier before its test and set a gate. On 23 fresh not-held GHN pages, labelled by two calibrated labellers ([comment 7138964230](https://github.com/tgoodyear/usnewsmap.com/issues/2128#issuecomment-7138964230)):

| Measure                       | Result                 | Gate                                     |
| ----------------------------- | ---------------------- | ---------------------------------------- |
| Cherokee-line recall          | 0.911 (0.819 to 0.987) | at least 0.90, lower bound at least 0.80 |
| Latin share of Cherokee calls | 0.004 (0.000 to 0.011) | at most 0.05, upper bound at most 0.10   |

Recall varies between pages: two of the 11 pages with Cherokee lines were at 0.66 and 0.74. Held pages (merged lines) and faded pages (the lowest quarter by contrast) were not tested, so staging holds them.

## Release

The files are the [`model-chr-line-clf-v2-20261124`](https://github.com/tgoodyear/usnewsmap.com/releases/tag/model-chr-line-clf-v2-20261124) release's assets, not files in the repository:

| Asset                    | What it is                                                                                           |
| ------------------------ | ---------------------------------------------------------------------------------------------------- |
| `chr_line_clf_v2_cnn.pt` | The CNN's PyTorch weights, sha256 `5bda2baa808ac5912381bd4cb45cdaf640f1e666840b387e3457dee27cf58937` |
| `eng-lm-lines.txt`       | The English character model's training text: 78,294 lines of LoC's OCR of the 324 Phoenix pages      |
| `line_clf_v2.json`       | The same as `frozen/line_clf_v2.json` in the staging directory                                       |
| `provenance.json`        | As committed here                                                                                    |
| `SHA256SUMS`             | As committed here                                                                                    |

To fetch and check:

```sh
gh release download model-chr-line-clf-v2-20261124 --repo tgoodyear/usnewsmap.com --dir chr-line-clf-v2
(cd chr-line-clf-v2 && sha256sum -c SHA256SUMS)
```

`ghn_boxes.py write` also needs:

- `chr_phoenix.traineddata` from the `model-tesseract-chr-phoenix-20261109` release (`ocr/models/chr-phoenix/README.md`);
- tessdata_best's `eng.traineddata`, sha256 `8280aed0782fe27257a68ea10fe7ef324ca0f8d85bd2fd145d1c2b560bcb66ba`, from <https://github.com/tesseract-ocr/tessdata_best/raw/main/eng.traineddata>. The `eng` in the Debian package is tessdata_fast and is refused;
- the Cherokee character model's training text, sha256 `b8fb978e48aeaabfe2b5cfdbc3c17c117074d5f2319ab0e5d96d487947ad7650`. It is the 1860 New Testament without the 274 verses #2128 excluded, plus the Cherokee side of ChrEn (Zhang, Frey and Bansal, EMNLP 2020). ChrEn is for research use, so the text isn't released. `scripts/ocr-survey/chr2128/r3_corpus.py` on branch `chr-2128` rebuilds it.

`ghn_boxes.py` checks every one of these against its sha256 before it reads a page.

## Licence

The weights and the English text are released under this repository's MIT licence. The training data was LoC's scans and OCR text of the paper, Georgia Historic Newspapers' scans of it, and the 1860 New Testament's text (CC0, as `ocr/models/chr-phoenix/NOTICE.md` records).

## Limits

- The cross-validated Latin share of calls on the development pages was 0.036, above the 0.03 tuning cap and below the gate's 0.05. The gate measured 0.004.
- Most missed lines on the development pages were boxes that ran past the column gutter. Clipping boxes to their columns lowered recall, so the line finder is unchanged.
- The ensemble's weights were fitted on out-of-fold CNN scores. The released CNN was then retrained on all the development pages.
