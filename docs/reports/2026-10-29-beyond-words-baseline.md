# Beyond Words / Newspaper Navigator baseline, 29 October 2026

This is an actual source-only annotation-agreement measurement for #1102, not a fixture, an OCR-engine evaluation or an accepted picture-search feature.

## Inputs and exclusions

Newspaper Navigator: `biglam/newspaper-navigator@e2e585824cdd28d52755c481dca69ed0bfa992a1`, CC0. All **3,274,561 rows in 36 curated parts** were read, with every part's bytes, SHA256 and row count checked against the independently pinned finished provenance SHA256 `2caffb480c70a8b21d77cffafc3366620e25adc0be009bd9233611c21ebc625c`. Per-kind and total counts also matched.

Beyond Words: `biglam/loc_beyond_words@6c7f5fb3c60f02d9fe925cfc14aa7008f6c89099`, CC0. Only its two COCO JSON files were read, not the image archive or image Parquet. Their Git blobs independently match the canonical LibraryOfCongress/newspaper-navigator repository:

| File                      | SHA256                                                             | Canonical Git blob                       |
| ------------------------- | ------------------------------------------------------------------ | ---------------------------------------- |
| `train_80_percent.json`    | `9d5a53a043b00b843da5fd012ed4bbf18e82c60d70a1732e4b32b3648ac4ebd9` | `8bc0e0d439428f168c5643b91b0e25efb4cc6453` |
| `val_20_percent.json`      | `247c489f1880908bde8d8251e7a5ce99f40c5f24694cfdf21840da2663825c7f` | `57dc90cad99ed885732ed3dcb3283224e6ea5344` |

The published 2,846-train / 712-validation split was preserved. Excluded: 19 scans added after 1 December 2019, one physical scan overlapping training despite different image ids, 8,547 unverified-class annotations and 26 verified-class annotations on excluded scans. No crowd annotations remained. The result is **692 eligible validation scans with 1,358 verified picture boxes**.

## Measurement

The [portable evaluator](../../scripts/pictures/beyond_words.py) completed in **36.402 seconds**, one CPU with a 3 GiB cap. Every parsed metric, exclusion and source/part hash exactly reproduced the independently executed 35.215-second baseline. There were no image/LoC requests, model downloads, training, threshold tuning or production changes.

Scan identity is batch version, LCCN, date, edition, reel and frame; frame is not page sequence. Newspaper Navigator's normalized XYXY boxes and COCO's pixel XYWH boxes were converted explicitly and clipped to the page. The predeclared prediction threshold is 0.5; matching is descending-confidence, one-to-one, IoU at least 0.5.

| Measure                              | Class-aware | Geometry-only |
| ------------------------------------ | ----------: | ------------: |
| Predictions at the fixed threshold   |         726 |           726 |
| Matches                              |         564 |           590 |
| Unmatched verified boxes             |         794 |           768 |
| Unmatched predictions                |         162 |           136 |
| Mean best IoU per verified box       |    0.391743 |      0.409844 |
| Matched-annotation precision         |      77.69% |        81.27% |
| Verified-annotation recall           |      41.53% |        43.45% |

Retained predictions exist on 456 eligible scans. The other **236 have unknown detector coverage**, not proven absence of pictures; they were not dropped from scoring. These are annotation-agreement metrics, **not AP** or proof that unmatched predictions are false. They are not directly comparable to the detector paper's published AP.

## Reproduction and limits

Use the [operations command and input checks](../operations/pictures.md#verified-beyond-words-baseline). The complete raw result and all 36 part hashes are retained on the authorized native node at `/var/lib/usnm-pictures-retryj/beyond-words-6c7f5fb3/portable-baseline.json`; the independent original is `baseline.json`, SHA256 `3a8b3baf09ced75a36be56d9f6dd08816ade4b9e6a6cc8d9c90f90ab1e609b94`. Generated JSON is not committed, following the repository's report-artifact policy.

Still required for #1102: actual engine illustration/text-loss scoring, the exact curated page-sequence join, picture indexing/filter/display, ads, credits and a full rebuild. Phase 3 remains #1220. This source baseline does not close those gates.
