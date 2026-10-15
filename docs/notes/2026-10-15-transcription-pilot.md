# Historical-newspaper transcription pilot

**Status:** implemented, live volunteer acceptance UNVERIFIED.
**Date:** 15 October 2026.
**Measured on:** synthetic tests and a fresh public Overproof-CA conversion.
**Code:** `qwen::benchmark`, `scripts/ocr-survey/transcription.py`.
**Issue:** [#542](https://github.com/tgoodyear/usnewsmap.com/issues/542).

## Experiment

The [pilot](../boinc-transcription.md) separates central supervised adaptation
from volunteer inference. It reuses `usnm_qwen`, with one fixed model per campaign.
Gold text remains on the server; volunteers receive image pixels, the
existing verbatim-transcription instruction and reference-free source notices.

The exporter creates immutable title/source-disjoint splits, preserves image
bytes and licence files, and emits training conversations only for train and
validation. Training requires explicit source rights checks. The first English
baseline is evaluation-only, not permission to train on the HIPE distribution.

## Local measurements

| Measurement         | Result                                   | Method                                                   |
| ------------------- | ---------------------------------------- | -------------------------------------------------------- |
| Public source fetch | 2 LoC pages                              | `gold.py public-fetch --dataset overproof-ca --pages 2`  |
| Converted selection | 20 crops                                 | `public-convert --count 20 --per-page 10`, Pillow 11.3.0 |
| Split               | 20 test crops, no training conversations | `transcription.py --evaluation-only`, seed 542           |
| Source title groups | 1                                        | LCCN parsed from source page identifiers                 |

These measure preparation, not OCR accuracy or training benefit. The small
selection is not representative of the corpus. The oversized-box issue
[#531](https://github.com/tgoodyear/usnewsmap.com/issues/531) remains open:
conversion-specific crop/reference alignment must be checked before an accuracy
claim, not inferred from engine agreement.

All 20 local crop images were visually checked against their reference
boundaries before the live selection. Conversion-specific crops
`pub-overproof-ca-006` and `pub-overproof-ca-010` span two columns and are excluded
before inference, leaving 18 evaluation-only crops. This is an operational
acceptance sample, not a representative accuracy benchmark.

The signed Qwen CPU payload uses app/version/platform-qualified physical
filenames. The initial publish job nevertheless reported success while the stock
registration tool aborted on an older Surya download-file collision; follow-up
[#547](https://github.com/tgoodyear/usnewsmap.com/issues/547) tracks the false
success and registration isolation. Signed files must not be renamed or existing
download files overwritten to recover.

## Remaining measurements

The first campaign must verify signed app distribution, anonymous worker pulls,
two-host execution and canonical assimilation. Central adapter training,
quantized-model export, held-out improvement and calibration of the quality
estimator remain UNVERIFIED. No adapted checkpoint is shipped by this change.
