# Reader comparison, interim results (#968)

12 November 2026. This is an interim report: Tesseract, LoC and American Stories are scored, and Qwen 3 VL 8B, Qwen 3 VL 2B and Xiaomi-OCR-0 have not run. The sets, readers, scoring and decision rules are those of the [protocol](protocol.md), and the [freeze record](protocol.md#freeze-record) lists the hashes. Departures from the protocol are in its [departures log](protocol.md#departures-log).

## What ran

Every CER below is pooled character error rate against human transcriptions, with missing output kept in the denominator, scored by the GPU studies' `gpu-study score`. Limits are 95% percentile bootstraps resampling whole source pages, 2,000 resamples, seed 202611. The normalization is the gold set's (`gold.py normalize`). Median crop CER and WER are in the tables beside it.

- **Tesseract 5.5.0**, run on our own dev node (usnm-dev2) with the Debian build, the pinned traineddata and `--dpi 300`, at PSM 6 (the protocol's primary) and PSM 3 (what gold units run). No crop failed or timed out. A few came back empty: 2 on `fr-new` and 2 on `de-rz81` at PSM 3, and 7 on `de-nzz` at PSM 6 and 13 at PSM 3. They are scored as empty.
- **LoC** on `en-op`: the clipped ALTO text, 345 of 345 crops aligned.
- **American Stories** on `en-op`: the readings from Study 3, unchanged. It returned text on 59 of the 345 crops, and the [GPU studies report](../2026-11-gpu-studies/report.md) found the same on its 611 crops.

## Tesseract by set

| Set | Crops | Pages | Reader | CER (95% limits) | WER | Median crop CER | PSM 3 minus PSM 6, points (95% limits) |
| --- | ---: | ---: | --- | --- | ---: | ---: | --- |
| `en-op` (second use) | 345 | 18 | PSM 6 | 4.06% (2.96 to 5.17) | 13.26% | 2.30% | |
| | | | PSM 3 | 4.42% (3.02 to 5.84) | 13.68% | 2.39% | +0.35 (-0.11 to 0.96) |
| `fr-new` (held out) | 60 | 20 | PSM 6 | 11.52% (6.31 to 17.17) | 33.91% | 5.47% | |
| | | | PSM 3 | 14.41% (7.25 to 23.51) | 36.04% | 5.45% | +2.90 (-0.02 to 8.16) |
| `fr-52` (second use) | 24 | 8 | PSM 6 | 5.14% (3.38 to 7.73) | 20.45% | 4.39% | |
| | | | PSM 3 | 8.91% (3.39 to 17.13) | 23.28% | 4.34% | +3.77 (-0.08 to 10.83) |
| `fi-new` (held out) | 60 | 20 | PSM 6 | 10.46% (9.34 to 11.42) | 54.10% | 10.39% | |
| | | | PSM 3 | 10.41% (9.31 to 11.33) | 54.03% | 10.41% | -0.05 (-0.14 to 0.01) |
| `fi-52` (second use) | 18 | 6 | PSM 6 | 7.88% (4.12 to 10.11) | 43.06% | 9.21% | |
| | | | PSM 3 | 7.94% (4.07 to 10.16) | 43.13% | 9.21% | +0.06 (0.00 to 0.19) |
| `sv-new` (held out) | 60 | 22 | PSM 6 | 8.89% (6.93 to 10.48) | 38.23% | 11.02% | |
| | | | PSM 3 | 8.75% (6.79 to 10.42) | 37.98% | 10.40% | -0.13 (-0.25 to -0.03) |
| `sv-52` (second use) | 10 | 4 | PSM 6 | 11.01% (10.60 to 11.41) | 47.10% | 11.41% | |
| | | | PSM 3 | 10.82% (9.85 to 11.22) | 46.46% | 11.14% | -0.19 (-1.34 to 0.00) |
| `de-rz81` (Study 11 pages) | 322 | 81 | PSM 6 | 4.90% (4.02 to 5.89) | 14.68% | 3.43% | |
| | | | PSM 3 | 4.66% (3.63 to 5.90) | 14.47% | 2.68% | -0.23 (-0.91 to 0.69) |
| `de-nzz` (second use) | 360 | 167 | PSM 6 | 13.36% (10.43 to 16.76) | 39.50% | 5.26% | |
| | | | PSM 3 | 15.57% (12.23 to 19.60) | 39.32% | 5.45% | +2.21 (0.06 to 4.64) |

German uses `frak2021` at both PSMs, French `fra`, Finnish `fin`, Swedish `swe`, English `eng`. `sv-52` has 4 pages, so by rule 1 it decides nothing.

PSM 3 and PSM 6 differ with limits that exclude zero in two places: Swedish `sv-new` (PSM 3 lower by 0.13 points) and NZZ (PSM 3 higher by 2.21 points). Every other pair has limits that include zero, or only touch it. The PSM that the live German feed uses (3) is the better of the two on `de-rz81` and the worse on `de-nzz`, and the `de-rz81` difference has limits that include zero.

## English: LoC and American Stories

`en-op` is a second-use set, so it can't decide the English recommendation by rule 5.

| Reader | Crops read | CER (95% limits) | WER | Against Tesseract PSM 6, points (95% limits) |
| --- | ---: | --- | ---: | --- |
| Tesseract PSM 6 | 345 | 4.06% (2.96 to 5.17) | 13.26% | |
| LoC | 345 | 9.54% (7.14 to 11.77) | 29.14% | +5.48 (3.12 to 7.68) |
| American Stories | 59 of 345 | 97.63% over all 345 | 98.54% | not meaningful |

LoC's text is worse than Tesseract's on these crops by rule 1. The October pilot measured 9.22% for LoC on the same crops with a different extraction, so the 0.3 point gap can come from the extraction rule alone and is not a change in LoC's text.

American Stories returns text on 59 crops, so its 345-crop CER is mostly missing output. On those 59 crops, American Stories scores 85.86% CER (limits 83.47 to 88.64), against 2.87% for Tesseract and 10.13% for LoC on the same 59. The GPU studies report reached the same conclusion for crops and notes that American Stories' layout model is built for pages.

## German Tesseract against LoC

The German Tesseract text (`frak2021`, PSM 3, 300 DPI) is live in search. The question was whether it beats LoC's text, by damage band. This report can't answer it. LoC has no text for `de-rz81` or `de-nzz`, so neither has an ours-against-LoC pair, and neither is in the corpus, so neither has a damage band. No German Chronicling America page has a human transcription yet. The absolute German numbers are in the table above: 4.66% on Reichsanzeiger and 15.57% on NZZ at PSM 3.

For the human-truth draw, the live German `frak2021` pages are 19,896 of the 21,290 in the release's replacement list, and 19,508 of those have a standard page id. By LoC damage (`loc_damage`) those split into 3,568 below 0.10, 1,110 from 0.10 to 0.20, 23 from 0.20 to 0.25, 10,139 from 0.25 to 0.32 and 4,668 from 0.32 up. The draw uses masters already on the scan server, so it needs nothing from LoC. The page draw and crop boxes are in the hand-off to the human-truth workstream.

## What is not in this report

- Qwen 3 VL 8B (request version 3, loop guard), Qwen 3 VL 2B and Xiaomi-OCR-0. Benchmark staging and submission handle request version 1 on container workers only, so guarded 8B and Xiaomi can't be dispatched yet. Native benchmark routing is being built. Without those columns there is no per-language recommendation: rule 2 needs a reader that beats Tesseract PSM 6, and none has run.
- English held-out set (`en-bln`): dropped, because BLN600's images are bilevel TIFFs that a unit can't carry unchanged under its No-Derivatives licence. English has only the second-use set, so its result stays provisional.
- `de-rz700` and `de-onb`: not frozen, because their crop lists (Study 7's and the benchmark release's) can't be rebuilt from the GPU studies' data release; see the departures log.
- The GPU and CPU host comparison. No CPU app version is live.

## Reproducing

`scripts/ocr-survey/tesseract_crops.py`, `scripts/ocr-survey/loc_column.py`, `scripts/ocr-survey/reader_draw.py` and the scripts in `scripts/` beside the protocol rebuild every input and score in this report. The sets and per-crop outputs stay on usnm-dev2 because the source terms of several sets restrict redistribution.
