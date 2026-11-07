# Transcription post-training pilot

Issue [#542](https://github.com/tgoodyear/usnewsmap.com/issues/542).
The task is verbatim historical-newspaper transcription, not article question
answering. Independent, bounded LoRA trials run on volunteer NVIDIA GPUs through
the separate `usnm_qwen_train` BOINC app; inference uses `usnm_qwen`.
Only permissively licensed **training** examples and labels are public inputs.
Credentials and held-out validation and test references never go to volunteers.

Status: signed GPU training app published and two bounded trials queued
(`transcription-lora-v1`, workunits 68 and 69). The AMD and Windows hardware
currently attached is ineligible for the Linux NVIDIA app, so GPU execution is
pending. See the [live evidence](2026-10-15-transcription-pilot.md). An
adapted checkpoint, its GGUF export, training resource measurements and accuracy
gains are UNVERIFIED. A baseline campaign does not establish a post-training
improvement.

## Dataset preparation

Use the existing [public ground-truth fetch and conversion](../ocr-scoring-guide.md).
Do not use LoC, American Stories or engine consensus as human truth.
Before an Overproof-CA accuracy run, inspect the alignment between each crop
and its reference text, and exclude the oversized boxes described in
[#531](https://github.com/tgoodyear/usnewsmap.com/issues/531).
Crop ids depend on the conversion, so the issue's numeric ids are not a universal
exclusion list.

Prepare a baseline without authorizing any training:

```sh
python3 scripts/ocr-survey/transcription.py \
  --source overproof-ca=DATA/overproof-ca-converted \
  --evaluation-only --out DATA/transcription-baseline
```

`--out` must not exist. The exporter preserves image bytes, original references,
source manifests and `LICENSES/`. `split.json` records the source checksums,
seed, prompt checksum, counts and title and source groups. NoDerivatives sources
are refused: BLN600 requires its existing original-file transport, not this crop
exporter.

For training, supply several eligible sources or at least three known titles:

```sh
python3 scripts/ocr-survey/transcription.py \
  --source austrian=DATA/austrian-converted \
  --source newseye-bnf=DATA/bnf-converted \
  --source newseye-nlf-sv=DATA/sv-converted \
  --allow-training-source austrian \
  --allow-training-source newseye-bnf \
  --allow-training-source newseye-nlf-sv \
  --out DATA/transcription-sft
```

These example sources are multilingual, not an English specialization dataset.
Training requires an explicit rights check per source and a supported permissive
licence. Scoring permission is not training or model-distribution permission.
Training on NonCommercial or ShareAlike sources is not authorized by this
exporter.
Missing title metadata groups the entire source together, rather than treating
different crops of the same page as independent. Groups are assigned
deterministically to train, validation and test; exact duplicate images or
whitespace-normalized text across groups are rejected. Repeated and syndicated
material still needs a near-duplicate audit before publishing generalization claims.

Only train and validation have `sft.json`, in
[Qwen's image/conversation format](https://github.com/QwenLM/Qwen3-VL/tree/main/qwen-vl-finetune).
The test split never supplies training conversations.

## Volunteer GPU adaptation

The [training worker](../../boinc/apps/train/worker.py) starts from
`Qwen/Qwen3-VL-2B-Instruct@89644892e4d85e24eaac8bacfd4f463576704203`.
It freezes the base and vision encoder, trains language-attention Q/V LoRA only,
masks image and prompt tokens, and refuses target truncation, non-finite losses
or unchanged adapters. These are independent trials, not synchronized gradient
aggregation across unreliable hosts. The initial pilot is two seeds/ranks, 16
steps each, on at most 16 train-only examples. This is a pilot of the hardware
and the method, using multilingual NewsEye sources; it does not demonstrate an
English OCR improvement.

The dedicated `docker_nvidia_train` plan requires Linux x86_64, BOINC 8.2.13,
Docker or Podman with NVIDIA GPU access, CUDA 12.8 and an Ampere-or-newer GPU
with at least 15 GiB VRAM. Scheduling reserves two CPUs, 16 GiB host memory and
40 GiB disk (including the cold CUDA and model image). The sandbox is offline,
read-only except for outputs and tmpfs, with no credentials, no CPU fallback and
a one-hour trial timeout. Limits on steps, rank, tokens, images, archives and
output are enforced. Before importing PyTorch, the worker reads BOINC's assigned
GPU ordinal from `init_data.xml` and restricts CUDA visibility to that device;
multi-GPU hosts must not run every trial on device zero. A missing or ambiguous
assignment fails explicitly.

`boinc-train.yml` builds the pinned CUDA image, verifies every model file
against its pinned Git and LFS identity, and runs a real forward and backward
pass of a tiny Qwen3-VL visual model on CPU. That smoke test proves the
optimizer, masking and frozen-base behavior, **not** GPU execution, pretrained
quality or sufficient VRAM. Main-only publication uses the public worker
package; the normal signing and release process publishes `usnm_qwen_train` with
`--plan-class docker_nvidia_train --worker-image <digest>`.

After deploying the current server and bridge and the signed training app:

```sh
scripts/start-boinc-bridge.sh --env prod --step train-prepare \
  --var TRAIN_CAMPAIGN=transcription-lora-v1 \
  --var TRAIN_WORKER_IMAGE=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:WORKER_DIGEST \
  --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step train-submit \
  --var TRAIN_CAMPAIGN=transcription-lora-v1 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step train-collect \
  --var TRAIN_CAMPAIGN=transcription-lora-v1 --wait-mins 15
```

Preparation uses the existing converted NewsEye BnF, Finnish and Swedish cache,
the rights-gated exporter and splits that are disjoint by source and title. It
retains source licences and notices, installs atomically and checks immutable
replay. Overproof, BLN600, NZZ and American Stories are not training inputs.
Only `training.zip` and the immutable trial configuration enter the public
download tree. Missing or changed data fail explicitly. An uncertain
`submitting` marker requires database reconciliation before retry, never blind
duplicate submission.
Trial hashes bind the campaign name, worker image and dataset identities, so
separately named campaigns cannot collide on workunit names or quarantine lookup.

Each trial requests one result, because floating-point GPU training cannot be
validated by byte-for-byte consensus. The validator binds the returned identity,
configuration and checksums to a submitted job, checks the exact safetensors
keys, shapes and FP32 data, rejects non-finite tensors and quarantines the
candidate under `PROJECT/qwen-train/seen`. It never loads pickle,
executes uploaded code or treats structural success as scientific acceptance.
Collection writes private `out/candidates.json` with explicit pending coverage.
GPU timings and loss curves are volunteer-reported, not trusted accuracy evidence.
No returned adapter is merged, exported or promoted automatically.

Record the worker commit, base-model revision, dependency constraints, split
checksums, adapter configuration, image-token budget and training hardware.
Do not select checkpoints on test CER. Do not start reinforcement learning or
teacher-generated labels in the first experiment.

After measuring the adapter, merge and convert it using an explicitly verified
llama.cpp export path for Qwen3-VL. Re-run held-out inference after
quantization.
The current released worker contains only the original 2B and 8B models; an
adapted model requires new pinned weights, a tested worker image and a signed app
version. Never label baseline readings as adapted-model results.

## Benchmark workunits

Create an `engine.json` with the released worker's exact digests and one model:

```json
{
  "image": "sha256:WORKER_DIGEST",
  "llama": "sha256:LLAMA_DIGEST",
  "weights": "sha256:WEIGHTS_LOCK_DIGEST",
  "models": [
    {
      "name": "qwen3-vl-2b-q8",
      "file": "Qwen3-VL-2B-Instruct-Q8_0.gguf",
      "mmproj": "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf",
      "min_gpu_mb": 0
    }
  ]
}
```

Replace placeholders with full SHA-256 digests. A campaign has exactly one
model, so a device-dependent choice between the 8B and 2B models cannot confound
a comparison.

```sh
usnm-boinc qwen benchmark-stage \
  --crops DATA/transcription-baseline/test/crops.jsonl \
  --root CAMPAIGN --engine engine.json --max-crops 20 --timeout-secs 1800
usnm-boinc qwen submit --root CAMPAIGN --project PROJECT --max-units 20
```

One crop is one unit, with two independent results through the existing
validator and assimilator. The benchmark input template includes `page.jpg`,
`qwen.json` and a reference-free `NOTICE.txt` with source, licence, attribution
and modification notices. Despite its logical name, `page.jpg` is the original
crop file, without re-encoding. The job uses the full crop and an empty
`loc_text`. The notice checksum is pinned in the job's input source and checked
again at submission. Human text stays only in `CAMPAIGN/benchmark.json`, outside
`units/`, and is not staged for download. The campaign is immutable and
rerunnable; a changed source, model or selection requires a new root. Submission
retains the existing write-ahead recovery markers.

Before first live submission, publish the worker through `boinc-qwen.yml`,
verify anonymous access, release the signed CPU and GPU app versions and install
the current bridge and server images. The signing key stays on the designated
signing host. Do not bypass signing or replace unrelated production settings.

The deployed bridge supports these steps:

```sh
scripts/start-boinc-bridge.sh --env prod --step qwen-benchmark-prepare \
  --var QWEN_CAMPAIGN=transcription-prepared-v1 \
  --var QWEN_SELECTION=transcription-audited-20261015 \
  --var QWEN_MAX=18 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step qwen-benchmark-stage \
  --var QWEN_CAMPAIGN=transcription-baseline-v1 \
  --var QWEN_CROPS=/project/qwen/transcription-baseline-v1/test/crops.jsonl \
  --var QWEN_ENGINE=/project/qwen/transcription-baseline-v1/engine.json \
  --var QWEN_MAX=20 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step qwen-submit \
  --var QWEN_CAMPAIGN=transcription-baseline-v1 --var QWEN_MAX=20 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step qwen-benchmark-collect \
  --var QWEN_CAMPAIGN=transcription-baseline-v1 \
  --var QWEN_SPOOL=/project/usnmocr/results/0 --wait-mins 15
```

`qwen-benchmark-prepare` replaces manual placement of the dataset and engine for
a reviewed selection. It resolves the committed hash-only audit under
`fixtures/ocr-gold/selections/` against the existing converted public cache at
`/project/gold/build-v1/out/` (built by `gold-bundle`). Missing or duplicate
matches, changed pixels, references or provenance, NoDerivatives sources and
mutable engine pins fail before installation. Preparation installs atomically,
retains licences, records output checksums and is idempotent only for identical
inputs and outputs. Existing campaigns without its `prepared.json` marker are
not adopted or overwritten: use a new campaign name. A held lock requires
explicit recovery after confirming no preparation is running.

Preparation then runs benchmark staging, which generates and checks the
reference-free notices; it never submits work. `qwen-benchmark-stage` remains
available for an already prepared campaign. References stay outside the public
download directory, and the reviewed evaluation selection grants no training
permission. Never submit the same selection under multiple roots merely to test
preparation.

## Results and promotion

`qwen benchmark-collect --root CAMPAIGN --spool SPOOL --out OUT` reads only
canonical assimilator outputs, checks hashes, job identity, engine and crop ids,
then reuses the standard CER and WER scorer.

- `coverage.json`: expected, returned, pending, failed and incomplete counts.
- `by-crop.jsonl`: status, device, model, timings, memory, errors and edit counts,
  including character insertions, deletions and substitutions.
- `hypotheses.jsonl`: raw readings in the input format of the existing scorer
  and calibration.
- `cer-by-crop.csv` and `cer-summary.csv`: the existing scoring format.

Missing hypotheses count as empty strings, including pending work. These are
coverage-penalized operational scores, not a final accuracy measurement while
anything is pending. Incomplete returned text is scored as returned, not discarded.
A partially written spool stops collection and must be retried. Corrupt results
and foreign jobs or models are errors, never silently skipped. The reference
denominator stays fixed.

Compare baseline and adapted campaigns on identical crops and at the same
quantization and resolution. Report CER and WER, omissions, insertions, the
recovery of names and places, coverage, runtime and memory, with uncertainty
clustered by page or issue. A proposed promotion gate is a 10% relative CER
reduction without material regression in omissions, invented text or deployment
cost. That gate is a decision threshold, not a measured result. Before changing
the quality estimator's reader, also run `quality calibrate` against held-out
human truth, because correlated errors can improve agreement without improving
accuracy.
