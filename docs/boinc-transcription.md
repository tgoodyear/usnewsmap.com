# Transcription post-training pilot

Issue [#542](https://github.com/tgoodyear/usnewsmap.com/issues/542).
The task is verbatim historical-newspaper transcription, not article question
answering. Training is central; independent volunteer inference uses the existing
`usnm_qwen` app and its [release process](../contrib-compute/docs/app-release.md).
No gradient training, credentials or reference transcriptions go to volunteers.

Status: benchmark staging, collection and dataset export implemented. An adapted
checkpoint, its GGUF export, training resource measurements and accuracy gains are
UNVERIFIED. A baseline campaign does not establish a post-training improvement.

## Dataset preparation

Use the existing [public ground-truth fetch and conversion](ocr-scoring-guide.md).
Do not use LoC, American Stories or engine consensus as human truth.
Before an Overproof-CA accuracy run, inspect the crop/reference alignment and
exclude the oversized boxes described in [#531](https://github.com/tgoodyear/usnewsmap.com/issues/531).
Crop ids depend on the conversion, so the issue's numeric ids are not a universal
exclusion list.

Prepare a baseline without authorizing any training:

```sh
python3 scripts/ocr-survey/transcription.py \
  --source overproof-ca=DATA/overproof-ca-converted \
  --evaluation-only --out DATA/transcription-baseline
```

`--out` must not exist. The exporter preserves image bytes, original references,
source manifests and `LICENSES/`. `split.json` records the source checksums, seed,
prompt checksum, counts and title/source groups. NoDerivatives sources are refused:
BLN600 requires its existing original-file transport, not this crop exporter.

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
NonCommercial/ShareAlike training is not authorized by this exporter.
Missing title metadata groups the entire source together, rather than treating
different crops of the same page as independent. Groups are assigned
deterministically to train, validation and test; exact duplicate images or
whitespace-normalized text across groups are rejected. Repeated and syndicated
material still needs a near-duplicate audit before publishing generalization claims.

Only train and validation have `sft.json`, in
[Qwen's image/conversation format](https://github.com/QwenLM/Qwen3-VL/tree/main/qwen-vl-finetune).
The test split never supplies training conversations.

## Central adaptation

Start from `Qwen/Qwen3-VL-2B-Instruct`, with supervised LoRA on the language model
and a frozen vision encoder. Use the upstream Qwen training framework with
`train/sft.json` as training data and `validation/sft.json` for checkpoint
selection. The `image` paths are relative to each split directory; configure the
framework's `data_path` accordingly.

Record the framework commit, base-model revision, dependency lock, split
checksums, adapter configuration, image-token budget and training hardware.
Do not select checkpoints on test CER. Do not start reinforcement learning or
teacher-generated labels in the first experiment.

After measuring the adapter, merge and convert it using an explicitly verified
llama.cpp/Qwen3-VL export path. Re-run held-out inference after quantization.
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

Replace placeholders with full SHA-256 digests. A campaign has exactly one model
so device-dependent 8B/2B selection cannot confound a comparison.

```sh
usnm-boinc qwen benchmark-stage \
  --crops DATA/transcription-baseline/test/crops.jsonl \
  --root CAMPAIGN --engine engine.json --max-crops 20 --timeout-secs 1800
usnm-boinc qwen submit --root CAMPAIGN --project PROJECT --max-units 20
```

One crop is one unit, with two independent results through the existing validator
and assimilator. The benchmark input template includes `page.jpg`, `qwen.json` and a reference-free
`NOTICE.txt` with source, licence, attribution and modification notices;
despite its logical name the image is the original crop file, without re-encoding.
The job uses the full crop and an empty `loc_text`. The notice checksum is pinned
in the job's input source and checked again at submission. Human text stays only in
`CAMPAIGN/benchmark.json`, outside `units/`, and is not staged for download.
The campaign is immutable and rerunnable; a changed source, model or selection
requires a new root. Submission retains the existing write-ahead recovery markers.

Before first live submission, publish the worker through `boinc-qwen.yml`, verify
anonymous access, release the signed CPU/GPU app versions and install the current
bridge/server images. The signing key stays on the designated signing host.
Do not bypass signing or replace unrelated production settings.

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

`qwen-benchmark-prepare` replaces manual dataset/engine placement for a reviewed
selection. It resolves the committed hash-only audit under
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
then reuses the standard CER/WER scorer.

- `coverage.json`: expected, returned, pending, failed and incomplete counts.
- `by-crop.jsonl`: status, device, model, timings, memory, errors and edit counts,
  including character insertions, deletions and substitutions.
- `hypotheses.jsonl`: raw readings in the existing scorer/calibration input format.
- `cer-by-crop.csv` and `cer-summary.csv`: the existing scoring format.

Missing hypotheses count as empty strings, including pending work. These are
coverage-penalized operational scores, not a final accuracy measurement while
anything is pending. Incomplete returned text is scored as returned, not discarded.
A partially written spool stops collection and must be retried. Corrupt results
and foreign jobs/models are errors, never silently skipped. The reference
denominator stays fixed.

Compare baseline and adapted campaigns on identical crops and at the same
quantization and resolution. Report CER/WER, omissions, insertions, names/place
recovery, coverage, runtime and memory, with uncertainty clustered by page or
issue. A proposed promotion gate is a 10% relative CER reduction without material
regression in omissions, invented text or deployment cost; this is a decision
threshold, not a measured result. Before changing the quality estimator's reader,
also run `quality calibrate` against held-out human truth: correlated errors can
improve agreement without improving accuracy.
