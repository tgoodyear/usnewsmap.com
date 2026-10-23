# OCR

Data, locks, templates and image definitions for the OCR pipeline: the engine catalog and routes, the engine contract's JSON Schemas, the BOINC app definitions, and the OCR benchmark. The Rust that uses them is in [`crates/usnm-ocr-*`](../crates/README.md) and [`crates/usnm-boinc`](../crates/README.md), and the scripts that build, release and operate the pipeline are in [`scripts/ocr-*`](../scripts/README.md), [`scripts/boinc/`](../scripts/README.md) and [`scripts/ocr-node/`](../scripts/README.md). The Python that runs inside the Iris, Surya and training worker images is in `boinc/`. The design is in [12](../docs/design/12-onprem-ocr-pipeline.md) and [13](../docs/design/13-ocr-engines.md).

| Directory                | What it holds                                                                                                                                    | Status                                                                       |
| ------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------- |
| [`engines/`](engines/)   | The engine catalog with each engine's and model's licence, and the Containerfiles and model locks of the Tesseract and NDLOCR-Lite engine images | live                                                                         |
| [`routing/`](routing/)   | `routes.toml`: which engine and profile a page goes to                                                                                           | live; the file is the minimal Tesseract route, full matching is a later step |
| [`contract/`](contract/) | The JSON Schemas of the engine contract `usnm-ocr-engine/1` and the fixtures the conformance suite uses                                          | live                                                                         |
| [`boinc/`](boinc/)       | The BOINC apps: the worker image, job files, server daemons, input and output templates, and one directory per app image                         | status: see [docs/boinc.md](../docs/boinc.md)                                |
| [`bench/`](bench/)       | The OCR benchmark: release recipes and index, audits, adapter locks, recorded results and the Serial Set pilot                                   | live (the `ocr-bench` workflow); see below for the trial records             |

## `engines/`

`engines.toml` is the catalog: for each engine and each model, its licence, author, source and credit line, which profiles (models plus languages) exist and whether the engine is cleared for production. Only Tesseract and the pinned CPU NDLOCR-Lite adapter have production profiles; the other entries are inventory. [`engines/README.md`](engines/README.md) has the schema and the licence gate.

| Path           | What                                                                                                                                                      |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `engines.toml` | The catalog                                                                                                                                               |
| `tesseract/`   | `Containerfile`; `models.lock` (each model's URL and sha256); `fpshim.c`, preloaded so Tesseract's floating-point traps do not kill OpenJPEG on some JP2s |
| `ndlocr-lite/` | `Containerfile`; `models.lock` (the pinned NDLOCR-Lite commit and each model's URL and sha256); the licence and notice files                              |

Read by:

- `usnm-ocr-route` embeds `engines.toml` and checks it. The on-prem worker (`usnm-ocr-worker`) embeds a copy and `tesseract/models.lock`; `usnm-ocr-adapter` embeds `ndlocr-lite/models.lock`.
- The web app parses `engines.toml` at build time for the credits page (`web/vite.config.ts`, `web/src/credits/data.ts`). `docker/Dockerfile.api` and `docker/Dockerfile.ingest` copy it into their builds.
- The image builds: `ci.yml` and `.github/actions/boinc-worker-build` build `tesseract/Containerfile`, and `.github/workflows/boinc-ndlocr-lite.yml` builds `ndlocr-lite/Containerfile`. `scripts/boinc/boinc-app-release.sh` builds the Tesseract image in ACR.

## `routing/`

`routes.toml` lists routes: a match, and the engine and profile candidates for it. Today it has one route that sends every page to `tesseract-5.5` with profile `auto`. `usnm-ocr-worker routes check` validates it against the catalog (its `--routes` default is this file), and a test in `usnm-ocr-route` does too.

## `contract/`

The engine contract is the protocol between the worker and an engine adapter. The Rust types in `usnm-ocr-contract` are the source; `v1/{hello,page,result,boxes}.schema.json` are generated from them. `fixtures/` holds the inputs of `usnm-ocr-worker engines conformance` ([README](contract/fixtures/README.md)).

To regenerate the schemas:

```sh
cargo run -p usnm-ocr-contract --bin usnm-ocr-contract-schema -- --out-dir ocr/contract/v1
```

A test in `usnm-ocr-contract` fails when the files differ from the types. A change under `contract/` also starts the `boinc-quality-native` workflow and the Rust job in `ci.yml`.

## `boinc/`

The files that define the volunteer apps. The stock BOINC server runs them; `usnm-boinc` stages, validates and collects them.

| Path                                 | What                                                                                                                                                                                                                                                     |
| ------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Containerfile`, `job.toml`          | The worker image (Tesseract plus `usnm-boinc work`) and the docker-wrapper job file for `usnm_ocr`, `usnm_assess`, `usnm_gold` and the docker version of `usnm_quality`                                                                                  |
| `daemons.xml`, `plan_class_spec.xml` | Fragments merged into a project's daemons and plan classes                                                                                                                                                                                               |
| `usnm_<app>_in`, `usnm_<app>_out`    | BOINC input and output templates for `usnm_ocr`, `usnm_assess`, `usnm_ndlocr`, `usnm_surya`, `usnm_quality`, `usnm_qwen`, `usnm_qwen_train` and `usnm_iris`, and the output template of `usnm_gold` (`usnm-boinc gold submit` writes its input template) |
| `iris/`                              | `usnm_iris`: Qwen3-VL-4B INT4 on OpenVINO for an Intel Iris Xe GPU. `bench.py` is an experiment and is not in the image. See [the pilot note](../docs/notes/2026-10-16-boinc-iris-pilot.md).                                                             |
| `ndlocr-lite/`                       | `usnm_ndlocr`: the Japanese NDLOCR-Lite app. See [docs/boinc-ndlocr-lite.md](../docs/boinc-ndlocr-lite.md).                                                                                                                                              |
| `qwen/`                              | `usnm_qwen`: Qwen3-VL on llama.cpp; the model, llama.cpp and disk-policy locks, CPU and GPU job files and one CI measurement. See [docs/boinc-quality.md](../docs/boinc-quality.md).                                                                     |
| `surya/`                             | `usnm_surya`: Surya 2 on llama.cpp, with CPU, NVIDIA and Intel job files. See [docs/boinc-surya.md](../docs/boinc-surya.md).                                                                                                                             |
| `train/`                             | `usnm_qwen_train`: bounded LoRA transcription trials. See [the transcription note](../docs/notes/2026-10-16-boinc-transcription.md).                                                                                                                     |

Status of each app: see the document in its row, and [docs/boinc.md](../docs/boinc.md) for the pilot as a whole.

Read by:

- `scripts/boinc/install.py` installs templates and job files into a project; `scripts/boinc/boinc-app-release.sh` builds the worker image and stages the files; `contrib-compute/images/bridge/Containerfile` copies templates and the Iris and training helpers into the bridge image.
- `usnm-boinc` and `usnm-qwen-app` embed the Qwen locks, the disk policy and the benchmark template.
- One workflow builds each image: `boinc-worker.yml`, `boinc-ndlocr-lite.yml`, `boinc-qwen.yml`, `boinc-surya.yml`, `boinc-iris.yml` and `boinc-train.yml`. Each builds without publishing on a pull request that touches what it is built from, and publishes only from a manual run on `main`.

## `bench/`

The OCR benchmark ([docs/ocr-bench.md](../docs/ocr-bench.md)): versioned archives of newspaper images and reference transcriptions that any engine can be scored on.

| Path                     | What                                                                                                                                          |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `releases.json`          | The index: for each release its archive name and sha256, content digest, sample count and subsets                                             |
| `v1.0.0/`, `v1.1.0/`     | One release's recipe (`spec.json`) and the sha256 of every input file (`inputs.sha256`). A release's files never change once it is published. |
| `annotation.schema.json` | The schema of the annotation fields on samples ([annotation policy](../docs/ocr-bench-annotation-policy.md))                                  |
| `audits/`                | Measured coverage of each release                                                                                                             |
| `adapters/`              | Locks for the candidate engines tried on the benchmark on 18 October 2026 (Calamari, CHURRO, EffOCR)                                          |
| `results/`               | Recorded outputs and scores of those trials, and of the Qwen CUDA against Vulkan comparison                                                   |
| `serialset-pilot/`       | The GovInfo Serial Set pilot: four page images, references, source ALTO, Tesseract and source OCR outputs, reports and checksums              |

The page images and the raw native result archives are not tracked. They are the `bench-assets` set of `scripts/ops/data-assets.sh`: `scripts/ops/data-assets.sh --fetch --set bench-assets` puts them back ([docs/ocr-bench.md](../docs/ocr-bench.md)).

Read by:

- `usnm-ocr-survey` (`bench-build`, `bench-verify`, `bench-select`, `bench-score`) builds and verifies releases from `spec.json`, `inputs.sha256` and `releases.json`, and scores outputs against a release.
- `scripts/ocr-bench/fetch` downloads and verifies a release from the entry in `releases.json`; `scripts/ocr-bench/release-ci.sh` builds, checks and publishes them; `scripts/ocr-bench/serialset-pilot.py` works on `serialset-pilot/`; `scripts/boinc/surya-benchmark.py` reads `releases.json`.
- The `ocr-bench` workflow runs on a pull request that touches this directory, `scripts/ocr-bench/` or `crates/usnm-ocr-survey/`, and publishes a release on `main` when its GitHub release does not exist yet.
- `adapters/` and `results/` are cited by [the candidate feasibility note](../docs/notes/2026-10-18-ocr-bench-candidate-feasibility.md). The scripts that produced them are in [`scripts/archive/2026-10/`](../scripts/archive/README.md).

To add a release, follow [How a release is built](../docs/ocr-bench.md#how-a-release-is-built).
