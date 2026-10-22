# Historical-newspaper transcription pilot

**Status:** baseline published; real-client inference verified; two GPU LoRA trials queued.
**Date:** 15 October 2026.
**Measured on:** synthetic tests, a fresh public Overproof-CA conversion and a live BOINC CPU client.
**Code:** `qwen::benchmark`, `scripts/ocr-survey/transcription.py`.
**Issue:** [#542](https://github.com/tgoodyear/usnewsmap.com/issues/542).

## Experiment

The [pilot](2026-10-16-boinc-transcription.md) uses separate BOINC apps for bounded
volunteer GPU adaptation (`usnm_qwen_train`) and inference (`usnm_qwen`), with one
fixed base model per campaign. Held-out gold text remains on the server;
training workunits contain only permissively licensed train-split pixels,
targets and source notices.

The exporter creates immutable splits, disjoint by title and source, preserves
image bytes and licence files, and emits training conversations only for train
and validation. Training requires explicit source rights checks. The first English
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
before any accuracy claim, the alignment between each crop and its reference
text must be checked for this conversion rather than inferred from engine
agreement.

All 20 local crop images were visually checked against their reference
boundaries before the live selection. Conversion-specific crops
`pub-overproof-ca-006` and `pub-overproof-ca-010` span two columns and are excluded
before inference, leaving 18 evaluation-only crops. This is an operational
acceptance sample, not a representative accuracy benchmark.

The signed Qwen CPU payload uses physical filenames qualified by app, version
and platform. The initial publish job still reported success while the stock
registration tool aborted on a collision with an older Surya download file;
follow-up [#547](https://github.com/tgoodyear/usnewsmap.com/issues/547) tracks
the false success and registration isolation. To recover, do not rename the
signed files or overwrite existing download files.

## Live baseline acceptance

PR #545 merged after all CI gates passed. The standard release from reviewed
main was signed on the isolated signing host and completed secrets, safe project
initialization, application registration, corrected app-version publication and
public XML health verification. PR #557 persists the verified publisher image.

Campaign `transcription-audited-20261015` contains 18 workunits and 36 initial
results. Acceptance workunit 46, result 88 ran on real BOINC host 2 using four
CPUs and the pinned `qwen3-vl-2b-q8` model. The client downloaded the signed app
and all three reference-free inputs, produced nonempty transcription, uploaded
all three outputs and reported success: server outcome 1, exit status 0,
`unit_error=null`. Elapsed time was approximately 513 seconds, including a cold
download and setup of the container and model; this is not an inference-only
measurement.
The remaining 17 crops were submitted only after this acceptance succeeded.
The client subsequently completed another crop and continued processing.

The dedicated test client's original 50% CPU preference exposed only two cores;
the `docker_mt` plan requires four. Its override now permits four cores and
7.2 GiB of its 8 GiB RAM. Cold container storage substantially exceeds the
advertised 2 GiB workunit bound; [#559](https://github.com/tgoodyear/usnewsmap.com/issues/559)
tracks measurement, image separation and correct dispatch requirements.

Validation on two independent hosts and canonical assimilation remain pending.
Completed adapter training, quantized-model export, held-out improvement and
calibration of the quality estimator remain UNVERIFIED. No adapted checkpoint is
shipped by this change.

## Live volunteer GPU training publication

PR #570 implements independent, bounded offline LoRA trials; PR #579 deploys
combined server and bridge images without reverting the concurrent OCR worker,
signed publisher or quality scan-directory forwarding. The training worker was
published from reviewed main `19ce4da0` and is anonymously accessible:

`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:960ba48cb33e6c164764246bba2c9aded7d791b48a6aed49f39c3a95acff3ee1`

The authorized signing release published `usnm_qwen_train` 1.0,
`x86_64-pc-linux-gnu`, `docker_nvidia_train`; live app-version id is 9.
The production server is revision `0000032`. PR #582 persists the signed
training publisher image so a later provision replays the same release.

Campaign `transcription-lora-v1` was prepared from the private converted cache
and identical replay succeeded. Source-disjoint split counts are 24 train
(NewsEye BnF), 18 validation (Finnish) and 10 test (Swedish). Only 16 eligible
train examples are public trial inputs. This is a pilot of the hardware and
method on multilingual sources, not English-specialized training or an accuracy
result.

| Trial       | BOINC workunit | Initial result | Seed | Rank | Steps |
| ----------- | -------------- | -------------- | ---- | ---- | ----- |
| `75013def…` | 68             | 148            | 542  | 4    | 16    |
| `c2687d25…` | 69             | 149            | 543  | 8    | 16    |

Both initial results are unsent (`server_state=2`, `outcome=0`), with no canonical
result. Exactly two workunits exist. Submission replay and pending-result
collection succeeded; private immutable output hashes and submitted markers
were checked against the actual campaign. Structural safetensors validation
quarantines candidates and does not promote them or establish scientific validity.

The exact published container passed a real forward and backward pass through
a tiny Qwen3-VL visual model on CPU: the adapters changed and all frozen base
and vision parameters were unchanged.
The full pinned 2B configuration's 112 FP32 LoRA keys and shapes also match
the server validator (meta-only check). Neither is evidence of GPU execution
or pretrained OCR improvement.

The current attached GPU is an AMD Radeon RX 9070 XT on Windows; it is not
eligible for this Linux/NVIDIA BF16 app. The jobs remain available for compatible
volunteer GPUs. No central GPU resources were provisioned, no adapter has
returned, and no post-training accuracy gain is claimed.
