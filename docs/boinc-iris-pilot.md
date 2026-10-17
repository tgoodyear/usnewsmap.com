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
