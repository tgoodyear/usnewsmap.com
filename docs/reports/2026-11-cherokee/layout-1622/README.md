# Line finding for pages without ALTO (#1622)

Results behind the step-4 section of [the report](../report.md). The data files the scripts wrote (per-page scores, boxes, features) are not in the tree, because `docs/reports/` keeps data as release assets. They stay with the run's other data until a data set is published for it. The tables below hold the content of those files that the report cites.

## The sample

30 of the 190 pages with a local scan and a Cherokee line (17,043 LoC lines, 2,910 of them Cherokee), in the order of the sha256 of `layout-sample-1622:<page>`, after the first six, which were set aside for tuning that was not needed:

- `sn83020866/1828-04-03/ed-1/seq-2`
- `sn83020874/1829-06-10/ed-1/seq-1`
- `sn83020874/1829-03-18/ed-1/seq-1`
- `sn83020866/1828-05-14/ed-1/seq-1`
- `sn83020874/1831-01-08/ed-1/seq-3`
- `sn83020874/1830-04-07/ed-1/seq-1`
- `sn83020874/1831-04-09/ed-1/seq-3`
- `sn83020866/1828-05-28/ed-1/seq-5`
- `sn83020866/1828-06-11/ed-1/seq-1`
- `sn83020874/1831-10-07/ed-1/seq-1`
- `sn83020874/1829-05-27/ed-1/seq-1`
- `sn83020874/1829-03-18/ed-1/seq-3`
- `sn83020874/1829-07-22/ed-1/seq-1`
- `sn83020874/1829-12-02/ed-1/seq-1`
- `sn83020874/1829-04-08/ed-1/seq-3`
- `sn83020866/1828-04-24/ed-1/seq-4`
- `sn83020874/1829-12-30/ed-1/seq-1`
- `sn83020866/1828-06-11/ed-1/seq-3`
- `sn83020874/1831-02-12/ed-1/seq-1`
- `sn83020874/1830-05-15/ed-1/seq-4`
- `sn83020874/1829-04-15/ed-1/seq-3`
- `sn83020866/1828-07-02/ed-1/seq-3`
- `sn83020866/1828-06-18/ed-1/seq-2`
- `sn83020874/1829-12-02/ed-1/seq-3`
- `sn83020874/1830-07-10/ed-1/seq-3`
- `sn83020866/1828-05-28/ed-1/seq-2`
- `sn83020874/1829-12-23/ed-1/seq-3`
- `sn83020874/1829-08-05/ed-1/seq-3`
- `sn83020874/1830-03-24/ed-1/seq-1`
- `sn83020866/1828-12-03/ed-1/seq-1`

## Finders

Usable match as `chr_layout_eval.py` defines it: centre in the middle half of the truth line's height and inside its width, at least 80% of its width covered, no more than 1.6 times as wide or tall. Strict IoU 0.5 is given for comparison; LoC's boxes are loose, so it reads low for a finder that hugs the ink.

| Finder | Truth lines | Boxes | Recall | Precision | Cherokee recall | IoU 0.5 recall | IoU 0.5 precision | Boxes holding two lines | Lines hit by two boxes | Worst page recall | Median page recall | Worst page precision | Pages under 80% recall |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Tesseract PSM 3 on the scan | 17,043 | 19,961 | 81.3% | 69.5% | 79.4% | 84.8% | 72.4% | 444 | 46 | 16.9% | 87.0% | 12.0% | 11 |
| same, cleaned | 17,043 | 19,560 | 81.3% | 70.9% | 79.4% | 84.8% | 73.9% | 444 | 46 | 16.9% | 87.0% | 12.5% | 11 |
| PSM 3 on the Sauvola-binarised scan | 17,043 | 18,742 | 89.8% | 81.6% | 84.0% | 90.9% | 82.6% | 593 | 39 | 51.3% | 95.0% | 50.7% | 5 |
| same, cleaned (chosen) | 17,043 | 18,256 | 89.7% | 83.8% | 84.0% | 90.8% | 84.8% | 530 | 39 | 51.3% | 94.9% | 52.3% | 5 |
| Sauvola at half size, cleaned | 17,043 | 19,155 | 77.0% | 68.6% | 79.2% | 80.1% | 71.2% | 1165 | 41 | 1.5% | 85.7% | 14.7% | 11 |
| Sauvola boxes plus raw boxes in the gaps | 17,043 | 18,845 | 89.8% | 81.2% | 84.1% | 90.9% | 82.2% | 659 | 39 | 51.3% | 95.0% | 50.7% | 5 |
| Kraken blla 7.1.1, default, 2 pages | 1,171 | 371 | 1.2% | 3.5% | 0.7% | 3.9% | 12.4% | 320 | 0 | | | | |

Settings. Tesseract 5.5.0, `eng` (the script-detect image's, a fast model; layout does not depend on the model), `--psm 3 --dpi 300`, one thread, boxes from the TSV's line rows. Sauvola: the scan's paper brightness divided out (a max filter on the page reduced 8 times, smoothed), grey levels stretched between the 0.5th and 99.5th percentiles, then window 51 and k 0.3. Clean-up: drop boxes under 14 pixels high, and boxes wider than half the page and under 1.5 times the page's median line height.

## The six pages the raw layout handled worst

| Preprocessing | Recall | Precision |
| --- | ---: | ---: |
| None | 51.5% | 35.6% |
| Brightness flattened | 77.9% | 69.2% |
| Sauvola | 90.4% | 77.8% |

The pages: `sn83020874/1829-06-10/ed-1/seq-1`, `sn83020874/1829-12-30/ed-1/seq-1`, `sn83020874/1830-03-24/ed-1/seq-1`, `sn83020874/1830-04-07/ed-1/seq-1`, `sn83020874/1830-07-10/ed-1/seq-3`, `sn83020874/1831-02-12/ed-1/seq-1`.

## Reading from the finder's boxes

`chr_layout_read.py`: 600 of the 2,441 Cherokee lines that exactly one box matches, read with `chr_phoenix` at PSM 13 and the job's padding. Syllabary letters only.

| Crop | Differs from the reading at LoC's box | Distance to LoC's text | Empty readings |
| --- | ---: | ---: | ---: |
| LoC's box | 0 | 16.2% | 0 |
| Finder's box | 4.0% | 15.9% | 0 |
| Finder's box, 18 px vertical padding | 8.8% | 18.5% | 0 |

## Line script classifier, without the scale-bound features

Trained on 90,769 LoC lines (13,763 Cherokee) of the 160 pages outside the sample; scored on 15,297 finder boxes (2,447 Cherokee) of the 30 sample pages that match a truth line.

| Stage | AUC | Cherokee recall | Cherokee precision | Errors | Cherokee called English | English called Cherokee | Accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Shape features | 0.9989 | 97.1% | 96.2% | 166 | 71 | 95 | 98.91% |
| With the two lines above and below | 0.99984 | 99.0% | 98.9% | 51 | 25 | 26 | 99.67% |

## Line script classifier, with all features

Trained on 90,769 LoC lines (13,763 Cherokee) of the 160 pages outside the sample; scored on 15,297 finder boxes (2,447 Cherokee) of the 30 sample pages that match a truth line.

| Stage | AUC | Cherokee recall | Cherokee precision | Errors | Cherokee called English | English called Cherokee | Accuracy |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Shape features | 0.9991 | 97.2% | 96.6% | 151 | 68 | 83 | 99.01% |
| With the two lines above and below | 0.99977 | 99.4% | 98.3% | 57 | 16 | 41 | 99.63% |

## Twelve GHN pages

Every 58th page of `ghn-pages.jsonl`, through the chosen finder (cleaned) and the classifier without the scale-bound features. `Merged share` is the share of boxes over 1.8 times the page's median height; `ink inside` the share of the page's ink (Sauvola, page reduced 4 times) inside the boxes.

| Page | Boxes | Called Cherokee | Merged share | Ink inside | Median box height |
| --- | ---: | ---: | ---: | ---: | ---: |
| `sn83020866/1828-02-21/ed-1/seq-1` | 324 | 0 | 0.207 | 0.62 | 56 |
| `sn83020866/1828-10-08/ed-1/seq-3` | 623 | 0 | 0.034 | 0.96 | 52 |
| `sn83020874/1829-04-22/ed-1/seq-1` | 555 | 11 | 0.025 | 0.96 | 53 |
| `sn83020874/1830-02-24/ed-1/seq-3` | 490 | 124 | 0.020 | 0.98 | 47 |
| `sn83020874/1830-08-14/ed-1/seq-4` | 363 | 0 | 0.041 | 0.96 | 55 |
| `sn83020874/1831-01-01/ed-1/seq-2` | 331 | 76 | 0.048 | 0.59 | 72 |
| `sn83020874/1831-07-16/ed-1/seq-4` | 421 | 2 | 0.131 | 0.82 | 64 |
| `sn83020874/1831-12-31/ed-1/seq-2` | 538 | 1 | 0.124 | 0.97 | 63 |
| `sn83020874/1832-04-14/ed-1/seq-4` | 550 | 3 | 0.084 | 0.98 | 56 |
| `sn83020874/1832-09-01/ed-1/seq-4` | 321 | 4 | 0.168 | 0.84 | 58 |
| `sn83020874/1833-04-20/ed-1/seq-2` | 717 | 3 | 0.050 | 0.97 | 51 |
| `sn83020874/1833-11-23/ed-1/seq-4` | 804 | 10 | 0.070 | 0.99 | 51 |

Altogether 6,037 boxes; the classifier read 5,966 of them (it skips a crop with no measurable ink) and called 234 Cherokee.

## Scan brightness

`chr_scan_stats.py`, 60 pages of each source, the page reduced 8 times. First quartile, median, third quartile. Spread: the median of the brightest pixel of each 8 by 8 block minus the median of the darkest.

| | Median grey | 1st percentile grey | 99th percentile grey | Paper-to-ink spread |
| --- | --- | --- | --- | --- |
| LoC | 160.8 / 169.0 / 177.0 | 38.0 / 42.0 / 43.0 | 184.5 / 192.5 / 203.0 | 51.0 / 56.5 / 62.2 |
| GHN | 204.5 / 215.0 / 224.2 | 5.5 / 24.0 / 51.8 | 233.0 / 236.5 / 239.0 | 156.8 / 167.5 / 176.5 |
