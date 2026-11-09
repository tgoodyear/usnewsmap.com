# Cherokee report data

The inputs behind the figures in [the Cherokee report](../report.md). This directory is published as the `report-2026-11-cherokee` data set; fetch it with `scripts/ops/data-assets.sh --fetch --set report-2026-11-cherokee`, which extracts it here.

## Files

| Path | What it is | Used for |
| --- | --- | --- |
| `testset-v2/manifest.json` | The frozen Cherokee test set, version 2: sources with their sha256, the 29 pages, tiers and metric | Test set v2 in steps 2 and 3 |
| `testset-v2/aligned.jsonl` | 2,160 Cherokee lines from LoC's ALTO, each aligned to the 1860 New Testament, with its tier and the reason a line was dropped | Same |
| `testset-v2/crops/` | The 2,160 PNG line crops, and `lines.jsonl` (crop, sha256, page, line, box, tier, reference and LoC text) | `scripts/ocr-survey/cherokee_testset.py score --lines` |
| `testset-v2/baselines/` | Stock Tesseract `chr` readings of the crops from the baseline run, without and with the New Testament word list | Stock rows of step 2; `chr_checked_score.py` |
| `testset-v2/SHA256SUMS` | The set's checksums as frozen. It also lists the builder scripts and the New Testament text, which are not here: the scripts are in `scripts/ocr-survey/` and the text comes from the pinned PDF | Checking the crops |
| `readings/phoenix-round1-psm7.jsonl` | `chr_phoenix` round one on the crops, PSM 7 | Step 3, line mode |
| `readings/phoenix-round1-psm13.jsonl` | `chr_phoenix` round one, PSM 13 | Step 3 table |
| `readings/phoenix-round2-psm13.jsonl` | `chr_phoenix` round two, PSM 13 | Step 3 table and summary |
| `readings/phoenix-round2-eng-psm13.jsonl` | `chr_phoenix+eng`, PSM 13 | Step 3, script detection |
| `readings/stock-chr-psm13.jsonl` | Stock `chr`, PSM 13 | Step 3, stock at PSM 13 |
| `hand-check/glyph-check-answers.jsonl` | The hand-check answers for the 200-line sample: for each disputed word, whether the print matched the reference, LoC, neither, or couldn't be told, with the time of the answer | `chr_checked_score.py --answers` |
| `hand-check/loc.jsonl` | LoC's text of the 200 sample lines | `chr_checked_score.py --engine loc=` |
| `hand-check/checked.jsonl` | Each sample line's status and, for the 70 fully resolved lines, the checked text and decisions | Hand check, step 2 |
| `hand-check/check-export-20261108.jsonl` | The check site's export of 8 November: each disputed word, the two readings, answer counts and outcome. No checker identities | Hand check counts |
| `eval/nt-reference/nt-engine-sample/` | 20 New Testament PDF pages: `manifest.json`, the reference text (`refs/`) and Tesseract `chr` `best` and `fast` readings with timings (`out/`). Page images are not included | `chr_confusions.py --eval-dir`, `chr_dict_compare.py --refs` |
| `eval/region-aware/` | 8 mixed-language _Phoenix_ pages: `manifest.json`, LoC's ALTO (`alto/`) and the region-aware hybrid re-OCR (`out/hybrid/`) | `chr_confusions.py --eval-dir` |
| `dict-compare/out/` | The dictionary comparison (#1621): readings of the New Testament pages (`nt/`) and the 8 mixed pages (`phoenix/`) by stock `chr` (`A_stock`), with the New Testament word list (`B_stock_nt`) and with ChrEn words added (`C_stock_nt_chren`) | `chr_dict_compare.py --out-dir` |

## Sources and terms

- **Page scans and OCR.** The crops, ALTO, LoC text and line positions come from the Library of Congress's Chronicling America (batches `dlc_misctopsn83020866_ver03` and `dlc_misctopsn83020874_ver02`). The _Cherokee Phoenix_ issues are from 1828–1831 and in the public domain.
- **Reference text.** The 1860 Cherokee New Testament, from the PDFs at todaysnative.org, is CC0 1.0, as recorded in `testset-v2/manifest.json` and `eval/nt-reference/nt-engine-sample/manifest.json`.
- **Our outputs.** The hand-check answers, the checked text and the Tesseract readings are this project's work, released with the report. The ChrEn dictionary and the trained models are not included.

The source-rights notice, `docs/reports/report-data-rights.txt`, is part of the release.
