# Iris Xe OCR pilot

This is an **experimental, single-host evaluation**, not production OCR publication
or cross-host scientific consensus. It uses `usnm_iris`, separate from the CUDA and
llama.cpp `usnm_qwen` versions. Existing GPU jobs cannot run unchanged on Intel.

The pilot is fixed to **100 distinct public-domain newspaper crops, Computer 9,
Qwen3-VL-4B-Instruct INT4**, and batch 104. No account-wide preference opens it to
other volunteers. Stock `create_work --target_host 9` creates host assignments;
`enable_assignment` must be enabled. Inspect existing assignments before enabling
that server option. No retry replicas are created for this bounded evaluation.

The pinned BOINC source parses `enable_assignment` in `sched/sched_config.cpp`.
The similarly named `enable_assigned_jobs` is not recognized and must not be
used as evidence that the scheduler will deliver targeted jobs.

## Execution and release

The separate worker bundles the OpenVINO export at revision
`3ffb805d532a250e5010c5aa5ce6579e60b31d71`. It explicitly selects the Intel GPU,
with no CPU fallback or runtime model download. Release the reviewed worker from
main using `boinc-iris.yml`, then stage and offline-sign version 1.0:

```sh
scripts/boinc-app-release.sh --env prod --app usnm_iris --version 1.0 \
  --plan-class docker_intel_opencl_4b \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:WORKER_DIGEST \
  --signing-host SIGNING_HOST --prepare-only
```

Use the normal release PR and publication process. Deploy compatible server/bridge
images with the app registration, Intel plan class, validator, and assigned-job
configuration before dispatch. Do not overwrite concurrent runtime pins.

On Computer 9, expose only `/dev/dri/renderD128` to CT 110, provide the Intel OpenCL
runtime, and verify BOINC reports an Intel OpenCL GPU. Keep existing attachments,
preferences, and tasks. Drain before any required client restart. Preserve other
app settings and add this to the local project configuration:

```xml
<app>
    <name>usnm_iris</name>
    <max_concurrent>1</max_concurrent>
</app>
```

The container has two CPU reservations, a 9-GiB memory limit with no swap, a
read-only root, no network, dropped capabilities except the existing wrapper's
file-access capability, and only the render node. Do not use the wrapper's
`use_gpu=true`: that requests NVIDIA `--gpus all`, not an Intel device.

Native container checks showed that a 6-GiB hard limit OOM-kills GPU compilation.
Two repeated runs and the largest pilot crop succeeded at 8 GiB, but peak
cgroup memory reached 8,496,205,824 bytes, leaving less than 100 MiB headroom.
The release therefore reserves 9 GiB, with one task at a time on the 10-GiB
Computer 9 CT. GPU shared allocations must be included; process RSS alone
substantially understates memory use.

## Prepare, dispatch, inspect

OCR benchmark release v1.0.0 ([docs/ocr-bench.md](ocr-bench.md)) preserves the pilot's exact
images and references, and those of the paired and German campaigns. Subset
`legacy/overproof-en-100` is this acceptance set byte for byte. When comparing against it, cite
its subset digest and the release's content digest.

The operator's dataset directory contains `dataset.json`, `images/`, and
`LICENSES/`. The manifest has `evaluation_only: true` and exactly 100 `crops`,
each with a unique `pub-overproof-ca-NNN` ID, relative image path, image SHA-256,
published reference transcription, and source provenance including license,
attribution, and `for_scoring: true`. Use the cleaned, geometry-corrected corpus;
do not score older stretched-box crops.

```sh
python3 scripts/boinc/iris-campaign.py prepare --dataset DATASET \
  --out CAMPAIGN --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:WORKER_DIGEST
python3 scripts/boinc/iris-campaign.py submit --root CAMPAIGN --project PROJECT
python3 scripts/boinc/iris-campaign.py submit --root CAMPAIGN --project PROJECT --apply
python3 scripts/boinc/iris-campaign.py collect --root CAMPAIGN --seen PROJECT/iris/seen
```

For final acceptance, also pass `--terminal-results SNAPSHOT.json`. Export a
read-only JSON array from BOINC's `result` joined to `workunit`, restricted to the
campaign's workunit names and batch 104. Each record contains `workunit_name`,
`batch`, `host_id` (from `result.hostid`), `server_state`, `outcome`,
`validate_state`, and `stderr_out`. The collector rejects other hosts or duplicate
replicas and counts terminal failures even when BOINC did not upload an output
file. A successful pilot requires 100 successful outputs **and** 100 terminal,
valid BOINC records; a missing snapshot is not final acceptance.

References stay on the operator/server side, not in model prompts. Source
licenses are retained; this is evaluation, not training. `submitting` markers are
written before `create_work`; uncertain dispatch stops rather than duplicating
work. Audit the database before explicit recovery. Confirm exactly 100
assignments to host 9 and no assignments to other hosts.

Both dry-run and apply verify the active registered Linux Intel app version 1.0:
its published Dockerfile must verify against the project's code-signing public
key and contain the campaign's exact image digest. Stock `update_versions` stages
app-version files directly in `download/`, not in the hashed workunit-input
hierarchy returned by `dir_hier_path`. Workunits specify
`app_version_num=100`, so a later release cannot take over still-pending pilot
jobs. Do not replace files within version 1.0 after publication.

The validator binds results to trusted staged jobs and retains diagnostics before
rejecting failed inference. `report.json` records pending, successful, and failed
jobs and whitespace-normalized character error rate against published references.
Inspect worst-error transcriptions, truncated answers, empty outputs, timings, and
BOINC terminal outcomes; a schema-valid result alone is not an accuracy pass.
Do not describe the pilot as complete while any result remains pending.

## Paired comparison extension

Additional, separately prepared 100-job campaigns can use
`pub-reichsanzeiger-lines-NNN` crops from the cached Reichsanzeiger PAGE ground
truth. Retain the source's CC-BY attribution and Public Domain Mark for images.
Each crop must identify its PAGE line IDs, `ground_truth_status: "GT"`,
`source_dataset: "reichsanzeiger"`, source checksum, and attribution in provenance.
Use bounded adjacent line pairs from the same text region, with a matching
published reference; do not repeat images to inflate the queue. All existing
host, resource, image-signature, exact-100 and immutable dispatch checks apply.

Score each engine on the same image checksums, references and returned subset
using the same normalization. Report English Overproof and German Fraktur
separately as well as any aggregate. Previously measured full-region German
outputs are not baselines for newly cut line pairs: run those exact new images
through the comparison engines. These crops do not replace the original
100-English-crop acceptance gate or count as distinct full pages for scaling.

## Larger campaign gate

The operator authorized staging a **20,000-page** run only after the 100-job pilot
has succeeded and its results have been inspected. This is a separate campaign:
100 cropped evaluation images are not 100 full pages, and copying or repeating
them does not satisfy the larger scope. Select 20,000 distinct source page IDs,
record the per-page crop/tiling policy, preserve input checksums and provenance,
and measure full-page memory, truncation, and runtime before opening dispatch.
Staging is not permission to dispatch all 20,000 jobs automatically.

For a source-only snapshot from the existing full-resolution JPEG cache:

```sh
python3 scripts/boinc/iris-stage-pages.py \
  --cache /tank/ocr/scans/keep --index /tank/ocr/scans/index-full.jsonl \
  --acceptance inspected-pilot-acceptance.json --out iris-20000-pages \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:WORKER_DIGEST
```

The acceptance file must identify the same `worker_image`, `host_id: 9`,
`inspected: true`, and 100 each for `jobs`, `ok`, `boinc_records`,
`boinc_terminal`, and `boinc_valid`, with zero `errors` and `pending`.
The operator creates this evidence only after inspecting the pilot report.
The command copies and SHA-256-verifies exactly 20,000 distinct cached pre-1923
page IDs, keeping full-resolution inputs and attribution. It does not alter the
cache or its fetcher, create workunits, or call LoC. Existing or incomplete
staging paths refuse overwrite.

This snapshot is **not an executable 20,000-page queue**. `DISPATCH_DISABLED`
and the campaign's explicit readiness gates require validated layout-aware
line-block segmentation, page reconstruction, per-page memory/truncation/runtime
measurements, tile bindings, and explicit dispatch authorization. The crop
worker's 512-token limit must not be applied blindly to whole newspaper pages.

Keep NVIDIA devices on the existing CUDA path. The current class requires an
**Intel** OpenCL GPU and therefore excludes every NVIDIA and AMD GPU. OpenCL
availability alone does not make an OpenVINO model executable on other vendors:
the OpenVINO GPU backend used here targets Intel hardware. Supporting AMD or
other OpenCL devices requires a separately tested backend/image and admission
class, not relabeling this worker as vendor-neutral.

## Local feasibility evidence

Before BOINC integration, a separate test CT ran the same 4B INT4 export using
OpenVINO 2026.4.1 on an i7-1185G7 Iris Xe. Warm-cache newspaper-crop inference took
7.5 seconds on GPU versus 27.6 seconds on two CPU threads. The clear text card was
read exactly. The newspaper transcription had errors and CPU/GPU differences;
these two images do not establish corpus accuracy or sustained performance.

## Throughput experiment (#736)

`ocr/boinc/iris/bench.py` and `scripts/boinc/iris-bench.sh` measure one resident `VLMPipeline`
for N sequential crops and an optional persistent `CACHE_DIR`, with the worker's device checks, prompt,
f16 hint and container limits. They are experiment tools, not shipped in the worker image, and change no
BOINC behavior. On Computer 9 pipeline construction was about 70% of each task; N=8 gave 3.8x and
a warm compile cache plus N=8 gave 6.4x end-to-end crops/min with no truncations or OOMs. Reference-scored CER, the
multi-crop contract and a signed release are still required: see
[the report](reports/2026-10-iris-model-reuse/README.md). Rollback needs nothing: the published
single-crop app version is unchanged. To rerun, stop BOINC on the host first
(`systemctl stop boinc-client` in the CT) and restore it with `systemctl start boinc-client` afterwards.

### Multi-crop bundles (app version 1.1)

`ocr/boinc/iris/bundle.py` defines the v2 unit: `iris.json` (`"v": 2`) plus `pages.pack` (the crops' JPEGs
concatenated). The worker loads the model once and runs the crops in order. Each crop has its own v1-shaped
result bound to a derived v1 job (id, crop id, page checksum), so a bundle cannot hide a failed crop.
The bundle status is `ok`, `partial` or `error`; a partial bundle exits 0 so its finished crops upload, and
the validator retains it but rejects it. A crash or the watchdog (900 s + 90 s per crop) keeps the crops
already finished. A `generate()` exception aborts the remaining crops (`bundle_aborted`) because pipeline
state is untrusted afterwards. A unit without `pages.pack` takes the unchanged v1 path.

Limits: at most 32 crops and 128 MiB per unit; stage 16 (measured 12.8 crops/min warm, 5.3 GiB peak, no
truncation). Real worker image on Computer 9, 16 distinct crops, production container limits, cold
cache: 16/16 ok in 98 s (load 14.5 s), text identical to the benchmark's N=16 run. Templates:
`usnm_iris_bundle_in` / `usnm_iris_bundle_out` (result up to 2 MiB). The compile cache is not part of 1.1.

Evaluation re-run of the accepted 100 crops, host 9 only (version 1.1, `version_num` 101). Bundles use
batch 104: the patched scheduler sends `docker_intel_opencl_4b` work only from that batch, so the first
submission at batch 105 was never sent. Workunits are `usnm_iris_<id>_b104` with inputs
`iris_<id>_{pack,job}_b104`, distinct from v1 names. From the bridge (`scripts/start-boinc-bridge.sh --env prod`):
`--step iris-bundle-stage` (`IRIS_CAMPAIGN`, `IRIS_WORKER_IMAGE`, `IRIS_ARCHIVE_URL`, `IRIS_ARCHIVE_SHA256`; the
ocr-bench `legacy/overproof-en-100` subset by default), then `iris-bundle-submit` (dry run unless
`IRIS_APPLY=1`), then `iris-bundle-collect` for per-crop CER and drift from the 1.0 results.
`iris-bundle-cancel` (dry run unless `IRIS_APPLY=1`) cancels a stranded batch-105 campaign.
Rollback: deprecate version 1.1 (1.0 is untouched).

Result, campaign `overproof-100-b104` (2026-10-18; 7 bundles of 16,16,16,16,16,16,4 on host 9):
100/100 ok, normalized CER 0.02000 (1.0 pilot: 0.01975), drift from the 1.0 text 0.047%. A 16-crop
bundle took 136–146 s, of which 22–24 s was model load, so about 6.8 crops/min per bundle in production.
That is slower than the 98 s benchmark run; the load is paid once per bundle because 1.1 has no compile
cache (version 1.2 below). All 100 crops finished in 18 minutes of wall time, run one bundle at a time.
Dispatch first needed the server to map `docker_intel_opencl_4b` to the Intel GPU
(`scripts/boinc/patch-server-intel-proc-type.py`) and the feeder on the share refreshed from the image.
The 20,000-page set stays gated by its own `campaign.json`; this release does not dispatch it.

### Persistent compile cache (app version 1.2)

`job.toml` mounts the named volume `usnm-iris-compile-cache` at `/cache` (the docker_wrapper `mount`
option; the root stays read-only). `ocr/boinc/iris/cache.py` gives OpenVINO a `CACHE_DIR` under it, keyed by
the model manifest, OpenVINO and GenAI versions, the image's package list, the compile config and the GPU.
OpenVINO's writes are not atomic, so the worker trusts only files recorded in a manifest after a clean
run (fsynced, then the manifest replaced atomically). At start it deletes any unrecorded or changed file
(from a killed run), other keys' entries and an over-size cache (6 GiB). A `generate()` exception or a
failed load through the cache discards it; a failed cached load retries once without the cache. An flock
lets one task use it at a time; a second runs without it (`busy`), as does a host with too little free
space (8 GiB for a cold fill, 1 GiB warm: `no_space`). A cache problem is never a unit failure. Each
crop's diagnostics carry `compile_cache` (`cold`, `warm`, `busy`, `no_space`, `unavailable`, `error`,
`load_failed`) and the last crop the `compile_cache_commit` result.

Volunteer disk: the volume holds about 3.2 GB (160 files) per host and is outside BOINC's disk accounting.
A new worker image or driver changes the key and replaces it. To remove it: `docker volume rm usnm-iris-compile-cache`.

Verification on Computer 9 (2026-10-18), the 1.2 worker code in the 1.1 image with production
container flags, the same 16 crops each run, all 16/16 ok with identical text:

| Run                        | Cache                          | Load   | Bundle  | Crops/min | Peak cgroup |
| -------------------------- | ------------------------------ | ------ | ------- | --------- | ----------- |
| No cache                   | none                           | 14.3 s | 104.1 s | 9.2       | not sampled |
| First run                  | cold, writes 3.19 GB           | 17.0 s | 108.5 s | 8.9       | not sampled |
| Second run                 | warm                           | 2.9 s  | 88.1 s  | 10.9      | 5.90 GiB    |
| After a killed warm run    | warm, 0 dropped                | 1.7 s  | 83.1 s  | 11.6      | 3.79 GiB    |
| After a run killed at 10 s | cold, 30 partial files dropped | 14.7 s | 108.2 s | 8.9       | 7.30 GiB    |

No run hit the memory limit (`memory.events` max 0, no OOM). Generation also got about 5-10% faster
warm (81-85 s against 89-93 s for 16 crops), since kernels compiled during generation are cached too.
Rollback: deprecate version 1.2; 1.1 is unchanged and never mounts the volume.
