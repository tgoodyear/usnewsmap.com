# Historical-newspaper transcription pilot

**Status:** baseline published; real-client inference and upload verified.
**Date:** 15 October 2026.
**Measured on:** synthetic tests, a fresh public Overproof-CA conversion and a live BOINC CPU client.
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

## Live baseline acceptance

PR #545 merged after all CI gates passed. The standard release from reviewed
main signed on the isolated signing host and completed secrets, safe project
initialization, application registration, corrected app-version publication and
public XML health verification. PR #557 persists the verified publisher image.

Campaign `transcription-audited-20261015` contains 18 workunits and 36 initial
results. Acceptance workunit 46, result 88 ran on real BOINC host 2 using four
CPUs and the pinned `qwen3-vl-2b-q8` model. The client downloaded the signed app
and all three reference-free inputs, produced nonempty transcription, uploaded
all three outputs and reported success: server outcome 1, exit status 0,
`unit_error=null`. Elapsed time was approximately 513 seconds, including a cold
container/model download and setup; this is not an inference-only measurement.
The remaining 17 crops were submitted only after this acceptance succeeded.
The client subsequently completed another crop and continued processing.

The dedicated test client's original 50% CPU preference exposed only two cores;
the `docker_mt` plan requires four. Its override now permits four cores and
7.2 GiB of its 8 GiB RAM. Cold container storage substantially exceeds the
advertised 2 GiB workunit bound; [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559)
tracks measurement, image separation and correct dispatch requirements.

Two-independent-host validation and canonical assimilation remain pending.
Central adapter training,
quantized-model export, held-out improvement and calibration of the quality
estimator remain UNVERIFIED. No adapted checkpoint is shipped by this change.
