# Surya 2 on BOINC

Issue: [#516](https://github.com/tgoodyear/usnewsmap.com/issues/516). Round 1 of the engine survey ([report](reports/2026-10-ocr-engine-survey/report.md), #370) ran Surya 2 on one CPU node, on 8 pages. This app, `usnm_surya`, runs it on volunteer hosts, on CPU and on NVIDIA GPUs, to read a larger sample and to measure CPU and GPU time per page. The other apps are described in [boinc.md](boinc.md) and [boinc-assessment.md](boinc-assessment.md).

Status: a probe of a few pages, to find out whether volunteers' container runtimes can give the job a GPU, how long a page takes, and how far two hosts' texts agree. The larger sample comes after it.

## What a unit is

One page per workunit: the page's full-size IIIF JPEG from LoC (`page.jpg`) and the job (`surya.json`). Staging also keeps LoC's ALTO as `loc.xml` (or records `loc.xml.missing`) on the server for later quality scoring; it is never sent to a volunteer. The ALTO request uses the shared LoC pacer and the same hold-on-block behavior as scan staging. The host reads the page in up to three input modes:

| Mode     | Input                                                                                                                                      |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `tiled`  | 1,280 px tiles overlapping by 256 px, as round 1 read pages, with each overlap split at its midpoint so a block is kept from one tile only |
| `whole`  | the whole page in one request; the model scales it to its own image budget                                                                 |
| `capped` | the whole page, scaled first so its long side is at most 2,048 px                                                                          |

A host with a usable GPU runs all three, each with a 1,800 s deadline. A CPU-only host runs `tiled` only, with a 5,400 s deadline: round 1 took a median 2,411 s per tiled page with about 8 cores busy.

Every result has five files: `text-tiled.txt`, `text-whole.txt`, `text-capped.txt` (empty for a mode that didn't run), `result.json` and `diag.txt`. The types are in `crates/usnm-ocr-contract/src/surya.rs`.

## The image

`ocr/boinc/surya/Containerfile` builds one image for both app versions, on llama.cpp's CUDA server image, pinned by digest. That build loads its CUDA backend when the container can see an NVIDIA GPU and its CPU backends otherwise. On top of it are surya-ocr 0.22.1 in a Python virtual environment, with the CPU build of torch (torch only runs Surya's layout fallback; the OCR model runs in llama-server), and `usnm-boinc`, which runs the unit.

The weights are in the image: Surya 2's GGUF model and vision projector, and the layout model, at the revisions round 1 used. `ocr/boinc/surya/weights.lock` pins each file's URL and sha256, and the build checks them. Datalab's licence (a modified Open RAIL-M) requires a copy with every distribution of the weights, and notice of its use-based restrictions, so the image carries `LICENSE` beside the weights (`/opt/surya/models/LICENSE`) and a `NOTICE`. The image records its llama.cpp base, Surya version and weights lock digest as `USNM_SURYA_LLAMA_IMAGE`, `USNM_SURYA_VERSION` and `USNM_SURYA_WEIGHTS`; a unit carries the same three values and the host refuses a unit staged for another build.

The image is several GB (the weights alone are 1.6 GB), which a volunteer downloads once per image version. `.github/workflows/boinc-surya.yml` builds it, checks that it reads a rendered line on CPU, and, when started by hand on main, pushes it to the public package `ghcr.io/tgoodyear/usnm-boinc-worker` with the tag `surya-<commit>`. The run's summary gives the digest and the values to stage with.

## App versions and plan classes

| Version | Plan class           | Sent to                                                                                                                                        | Container                                                                        |
| ------- | -------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| 1.0     | `docker_mt`          | hosts with Docker or Podman and at least 4 usable CPUs; 4 to 8 threads                                                                         | `--cpus` from the thread count, 12 GiB, no swap (`ocr/boinc/surya/job-cpu.toml`) |
| 1.1     | `docker_nvidia_cuda` | hosts with Docker or Podman whose client reports an NVIDIA GPU with CUDA 12.0 or later, compute capability 5.0 or later and 3,000 MB of memory | `--gpus all`, 2 CPUs, 8 GiB, no swap (`job-gpu.toml`)                            |

Both classes need client 8.2.13 or later and are in `contrib-compute/boinc/plan_class_spec.xml`. The scheduler sends a host the version it expects to be fastest, so a host with a qualifying GPU gets the GPU version.

### How the GPU reaches the container

The stock `docker_wrapper` adds `--gpus all` to `docker create` (or `podman create`) when the version's `job.toml` sets `use_gpu = true` (`samples/docker_wrapper/docker_wrapper.cpp` at the pinned BOINC commit). The scheduler only checks the GPU the client detected; on Linux it does not check that the container runtime can pass it through. (On Windows it does, from the GPU list the client reports for each WSL distribution, since BOINC's commit 1635c05 of June 2026.) So the host needs, beyond the NVIDIA driver:

- **Podman** (including rootless): a CDI specification for the GPUs, made with the NVIDIA Container Toolkit (`nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml`). Podman turns `--gpus all` into the CDI device `nvidia.com/gpu=all`. With SELinux enforcing, the container may also need `container_use_devices` (`setsebool -P container_use_devices on`).
- **Docker**: the NVIDIA Container Toolkit installed and configured as a Docker runtime (`nvidia-ctk runtime configure --runtime=docker`).

Without these, the container either fails to start (the wrapper's stderr, which BOINC returns with the result, holds the runtime's error) or starts without a GPU. In the second case the unit returns `no_gpu` for each mode, quickly, with the diagnostics.

The wrapper passes every GPU, not only the one BOINC assigned (`gpu_device_num`). The worker runs llama-server on the assigned one (`CUDA_VISIBLE_DEVICES`, taking BOINC's number as nvidia-smi's index, which may differ on a host with mixed GPUs) and measures that GPU's memory; the diagnostics record which GPU that was. llama.cpp's CUDA build needs a driver that supports CUDA 12.

## Diagnostics

`usnm-boinc work` always writes all five outputs, so a host that can't run Surya still returns why, as a valid result rather than a lost one. Before reading, it records in `diag.txt` (at most 64 KiB) and `result.json`:

- from BOINC's `init_data.xml`, only these fields: client version, app version, plan class, GPU type and device number, CPU count and memory bound (the file also holds the account key and host name, which are never read out);
- the container runtime and whether it is rootless (Podman's `/run/.containerenv`; Docker's `/.dockerenv`), the cgroup limits the container got, and its memory;
- the NVIDIA device nodes, the CUDA and NVIDIA variables, the driver version and whether `libcuda` is there;
- `nvidia-smi`'s GPU name, driver, memory, compute capability and full output, or its error;
- Python's probe: torch's version and CUDA check, surya-ocr's version and its inference settings.

Each mode then runs in its own process group with its deadline. The worker samples the group's memory and CPU time every 0.5 s and, on a GPU, the GPU's memory every 2 s. A mode in which Surya reports a block it could not read counts as `failed`, whatever text the rest gave. It records the outcome (`ok`, `timeout`, `failed`, `no_gpu`), exit code or signal, time, CPU seconds, peak memory, peak GPU memory above what was in use before, and the device llama-server actually used: `cuda` when its log says it offloaded layers to the GPU, else `cpu`. `diag.txt` also gets llama-server's device lines and last lines, and the end of Surya's stderr.

A unit that can't run at all (an input whose checksum doesn't match, an engine that differs from the job's) returns `unit_error` with the reason and no runs.

## Redundancy and validation

`create_work` makes each unit with `min_quorum` 2 and `target_nresults` 2, and at most 4 results. `script_validator` runs:

- `usnm_surya_check` (`usnm-boinc surya check`) on each result: shape, sizes, UTF-8 and checksums. It also copies the result to `<project>/surya/seen/<result>/`, so every host's timings and diagnostics are kept, not only the canonical result's.
- `usnm_surya_compare` (`usnm-boinc surya compare`) on each pair. Two results agree when they have the same job and engine; when, in each mode both ran, both read the page or neither did; and when, where both read it, their word-bag Dice (the measure in [boinc-assessment.md](boinc-assessment.md)) is at least the threshold. A mode only one of them ran is not compared, and two results with no mode in common don't agree. Two results that both could not run the unit (`unit_error`) do agree. Every pair is appended to `<project>/surya/pairs.jsonl` with its Dice per mode and both hosts' devices.

Vision-language output can differ between GPUs, and between CPU and GPU, so the threshold is measured, not assumed. Until the probe has measured it, `compare` accepts any Dice of 0.5 or more (`PROBE_MIN_DICE`), which only rejects unrelated text, and the logged pairs give the distribution to set it from.

`rsc_fpops_est` is 1e14 (round 1's median 18,753 CPU-seconds per tiled page at a few GFLOPS per core) and `rsc_memory_bound` 12 GiB, rounded up from the measured 11.8 GB CPU peak. A workunit can be sent to either version, so the shared admission bound must cover the CPU version even though the GPU container is capped at 8 GiB. This conservatively excludes hosts whose BOINC memory allowance is below 12 GiB; revise it only after pilot measurements.

The ALTO staged with each page is retained so the Surya outputs can be scored against LoC with the existing report tooling. There is not yet a Surya collector that produces the large-sample quality report. The attached-host pilot and its output are prerequisites to setting the agreement threshold and widening the sample.

## Running a campaign

The server must have the `usnm_surya` app (`register-apps`), the two plan classes and daemons (server and bridge images built from a commit that includes `usnm_surya`, `scripts/build-boinc-images.sh` then `scripts/provision-boinc.sh`), and both app versions:

```sh
gh workflow run boinc-surya.yml --ref main -f correlation="surya-$(date -u +%Y%m%dT%H%M%S)"
# From the run's summary: the image, its digest, SURYA_ENGINE_LLAMA and SURYA_ENGINE_WEIGHTS.
scripts/boinc-app-release.sh --env prod --app usnm_surya --version 1.0 --plan-class docker_mt \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
scripts/boinc-app-release.sh --env prod --app usnm_surya --version 1.1 --plan-class docker_nvidia_cuda \
  --worker-image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:... --signing-host HOST
```

Then, under `<share>/surya/<campaign>/`:

```sh
# Gets each page's JPEG from the scan archive, else from tile.loc.gov through its batch (see
# below); a 403, 429 or 5xx, or an HTML challenge page, writes HOLD and stops.
scripts/start-boinc-bridge.sh --env prod --step surya-stage --var SURYA_CAMPAIGN=probe1 \
  --var SURYA_PAGES=serbian:srp:sn85055101_1917-04-03_ed-1_seq-3:dlc_arlington_ver01,... \
  --var SURYA_ENGINE_IMAGE=sha256:... --var SURYA_ENGINE_LLAMA=sha256:... --var SURYA_ENGINE_WEIGHTS=sha256:... \
  --var SURYA_MAX=8 --wait-mins 15
scripts/start-boinc-bridge.sh --env prod --step surya-submit --var SURYA_CAMPAIGN=probe1 --var SURYA_MAX=8 --wait-mins 15
# As results come in:
scripts/start-boinc-bridge.sh --env prod --step surya-summary --var SURYA_CAMPAIGN=probe1 --wait-mins 15
```

Each page is `group:lang:doc_id:batch[+batch...]`. Staging never asks www.loc.gov where a page is (its item API answers Azure with a Cloudflare challenge), so the page names the batches that may hold it, as `<name>_ver<NN>`. Take them from the published set's `batches.json` in the lake's `reference` container (the version `current.json` names): every batch whose `lccns` include the page's LCCN and whose `first` to `last` dates include its date. For example, with the file downloaded:

```sh
jq -r --arg l sn82015743 --arg d 1931-04-10 '.[] | select((.curated.lccns | index($l)) and .curated.first <= $d and $d <= .curated.last)
  | "\(.batch)_ver\(.curated.version | tostring | if length < 2 then "0" + . else . end)"' batches.json | paste -sd+ -
```

For each page, staging (`crates/usnm-boinc/src/scans.rs`, shared with the assessment) takes the first of:

1. the scan archive's full-size JPEG of the page in one of its batches (`keep/<batch>/<lccn>/<date>/<edition>/<seq>.jpg`), checked against its stored sha256, when the bridge job has `SCAN_ARCHIVE_URL`;
2. the archive's JP2 of the page: its stored IIIF base, then the JPEG from tile.loc.gov (one request; the JP2 itself is not read);
3. each batch in turn on tile.loc.gov's storage service: the bag manifest and the issue's METS (kept in `<campaign>/batch-cache`) give the IIIF base, then the JPEG;
4. none: the page is marked `missing`.

Requests to LoC go one at a time, at least 5 s apart, with no retries, as for the assessment (docs/boinc-assessment.md, "Where the inputs come from"). The job's `input.url` is always the page's tile.loc.gov JPEG URL, wherever the bytes came from.

To clear a hold, read `<campaign>/HOLD`, deal with the cause, and delete it (on the share, for example with `az storage file delete`).

`surya-summary` prints a table of every kept result (page, client, plan class, runtime, GPU, and per mode the outcome, device, time, memory) and every compared pair, and writes it to `<campaign>/summary.md`. A unit left at `submitting` is resolved as for the assessment.

To check a host or the image without LoC or BOINC, make a unit from any image and run it:

```sh
docker run --rm -v "$PWD/u:/work" IMAGE usnm-boinc surya local-unit --image /work/in.jpg --dir /work --modes whole
docker run --rm --network=none --read-only --tmpfs=/tmp:rw,size=1g --gpus all -v "$PWD/u:/work" IMAGE
```

## Not verified

- That rootless Podman with a CDI specification gives the container the GPU through the stock wrapper (the probe's question).
- That llama.cpp's CUDA build falls back to its CPU backends on a host without a GPU other than the CI runner.
- Peak memory and time on volunteers' hosts, and so the plan classes' limits.
- Quality against LoC and the gold set for a stratified multi-hundred-page sample.

## Bounded immutable-input probe (#812)

The registries serve different purposes, just as for Qwen and Iris. Volunteers
pull the **public GHCR worker** by digest. The private production ACR
`crusnmprodffapzy` holds signed **app-version payloads** and server/bridge images.
Do not mirror the worker into private ACR: anonymous volunteers cannot pull it.
`images.appver` is the last publisher replay image, not a catalog of all registered
versions. An older app's artifact need not equal that pin. Anonymous `apps.php`
redirects to login; an empty 302 body is not evidence that an app is absent.
Check the production `app`/`app_version` rows and their signed Dockerfile downloads
before releasing either version. Also inspect existing version/platform directories:
absence from the DB does not permit overwriting an immutable signed payload.
On 18 October both Surya 1.0/1.1 directories existed with the older `84c1069f…`
worker while neither version was registered. A new worker requires an unused,
reviewed version pair; do not delete or replace the old signed directories. Use the normal signer; stop if locked or absent.

`surya-benchmark-stage` accepts a pinned bundle, without fetching or transforming
its images. Prepare the same two v1.0.0 smoke subsets used by the candidate trials:

```sh
scripts/ocr-bench/fetch --version v1.0.0 --verify --dest benchmark
python3 scripts/boinc/surya-benchmark.py prepare \
  --release benchmark/usnm-ocr-bench-v1.0.0 --out surya-inputs
```

The helper checks the committed release-manifest and subset digests and every
selected image/reference hash. It keeps upstream provenance and `LICENSES`,
including the English reference NC attribution. Copy the bundle, retaining its
layout, to `<share>/surya-inputs/<campaign>/` through the existing authorized
operator transport. Keep references private. `inputs.json` is an array of
`sample_id`, `group`, `language`, relative `image`/`reference` paths and their
`image_sha256`/`reference_sha256`, `source_url`, and `license`. The SHA256 of its
exact bytes, printed by prepare, is required by staging:

```sh
scripts/start-boinc-bridge.sh --env prod --step surya-benchmark-stage \
  --var SURYA_CAMPAIGN=probe-812-gpu --var SURYA_BENCHMARK_SHA256=MANIFEST_SHA256 \
  --var SURYA_ENGINE_IMAGE=sha256:WORKER --var SURYA_ENGINE_LLAMA=sha256:LLAMA \
  --var SURYA_ENGINE_WEIGHTS=sha256:WEIGHTS --wait-mins 15
```

The equivalent local command is `usnm-boinc surya benchmark-stage --manifest FILE
--manifest-sha256 SHA256 --root ROOT --engine-image sha256:WORKER --engine-llama
sha256:LLAMA --engine-weights sha256:WEIGHTS`. Staging validates all inputs before
writing, refuses an existing campaign, and writes a completion marker. Submission
rechecks the pinned manifest, every reference/image and job. Only `page.jpg` (the
original image bytes, never re-encoded) and `surya.json` enter the download tree.
The existing v1 worker's `https://tile.loc.gov/local` sentinel is retained for
local inputs; it is **not a provenance URL and is never fetched**. Actual source
URLs and license evidence remain in `benchmark-inputs.json` and the bundle.

For a small exact-host probe, set `SURYA_TARGET_HOST` on `surya-submit` (or
`surya submit --target-host ID`). This reuses stock `create_work --target_host`
and requires the real `enable_assignment=1` server setting from #719. It uses
**quorum one**, target one, maximum one result, no fallback/retry hosts, and at
most 30 units per campaign. The campaign lock, durable target policy and existing
`submitting` marker prevent concurrent dispatch, retargeting and blind retries.
Targeted workunit names include the host ID so separate CPU and GPU probe
campaigns can share immutable job identities. Untargeted campaigns retain the
original quorum-two behavior.

This fallback deliberately does **not** claim GPU+CPU quorum: the stock single
`--target_host` option cannot specify one replica per device, while ordinary
quorum-two scheduling cannot guarantee the pair. The per-result Surya check
script remains unchanged and keeps every returned result in `surya/seen`.
Quorum-one results do not generate validator comparison pairs; report
`pairs.jsonl` as absent/empty rather than claiming cross-host consensus. Separate
validated CPU/GPU outputs can be compared explicitly with `surya compare --log`
and labeled as post-collection comparison, not quorum validation.

Keep the total dispatch around 20–30 units: for example 20 GPU smoke inputs,
three LoC pages (one each Serbian, Russian and worst-English, through ordinary
`surya-stage` into a **separate** campaign), and three pinned repeat inputs on a
known eligible CPU host, each with its own target/campaign. The CPU repeat bundle
must explicitly pin its three selected members and retain original attribution.
Do not append LoC units to an immutable benchmark campaign. Recheck the #529
claim, current GPU reservations on #794, host idleness, signed-image equality,
client plan eligibility, memory/disk bounds and existing preferences before
submission. A prepared bundle or passing CI is not dispatch authorization.

Export only validator-kept results for the existing scorers:

```sh
python3 scripts/boinc/surya-benchmark.py collect --root CAMPAIGN \
  --seen PROJECT/surya/seen --out collected --binary usnm-boinc --intended-device cuda
usnm-ocr-survey bench-score --release RELEASE --subset overproof-en/smoke-10 \
  --outputs collected/cuda-tiled-english.jsonl --rule legacy-ws-v1
usnm-ocr-survey bench-score --release RELEASE --subset overproof-en/smoke-10 \
  --outputs collected/cuda-tiled-english.jsonl --rule raw-v1
```

Repeat for each device/mode and `reichsanzeiger-de/smoke-10`. Collection runs the
same structural/checksum check script again, verifies engine equality and rejects
duplicate successful outputs instead of silently selecting a favorable repeat.
The receipt retains every failure, timing, measured RSS/GPU peak and missing
sample. Use scorer missing-as-empty metrics as well as successful-output CER.
LoC outputs are baseline comparisons, not independent gold CER. No routing,
queue, volunteer preference, trust, or account policy is changed by this probe.

The authorized current-worker probe version pair is **1.2 (`docker_mt`) and
1.3 (`docker_nvidia_cuda`)**, subject to a fresh unused-version check. The old
1.0/1.1 payloads remain retired/unregistered and immutable; do not publish or
replace them. Use the same normal `boinc-app-release.sh` commands above with
these new version numbers and the requested worker digest.

### Scoped signed publication without runtime rollback

When reviewed main differs from the deployed stack, do not run a full-stack
provision to release this probe. Under the #529 claim and ordinary local mutex /
controller lease, first verify **all six lifecycle jobs idle**. Prepare each new
version with `scripts/boinc-app-release.sh --prepare-only` and the normal signing
host. Review and merge the resulting immutable appver pin before publication.
Do not reuse a locked signer or change existing signed payloads.

For publication, save the existing publish job's complete `properties.template`;
copy it as an execution-only YAML/JSON document (JSON is YAML), change only its
container image to the approved signed appver digest, and add
`APPVER_REUSE_PROJECT=1` to that container's existing environment. Preserve all
other environment, identity/secret references, resources and volume definitions.
Start the existing publish job with `az containerapp job start --yaml FILE`;
**never update its persisted template** or deploy the stack. The reviewed scoped
publisher leaves project config, credentials and `project.xml`/other app
registration untouched, while preserving signature checks, immutable-directory
checks, stock `update_versions` and exact app-version DB verification. The app
must already exist; this mode does not create it.

Check the actual execution's image, environment and volumes match the intended
full template, wait for success, then independently verify DB registration,
Dockerfile worker digest/signature and anonymous downloads. If execution is
uncertain, retain the serialization lease and reconcile rather than retrying.
Release the claim/lease only after terminal status and verification. Keep current
live server/bridge pins/settings unchanged; the appver release pin alone does not
authorize replacing them.

The NVIDIA-only exact-host variant also sets `SURYA_GPU_PROBE=1` (CLI
`--gpu-probe`): it pins `--app_version_num 103` (1.3) and reduces the shared
memory admission bound to **8 GiB**, matching `job-gpu.toml`. This cannot fall
back to CPU version 1.2. Untargeted/CPU campaigns retain the 12 GiB CPU bound.
CT214 reports 12 GiB RAM and two CPUs; the CPU plan needs four usable CPUs,
while CT100 has only 8 GiB RAM, below the CPU contract. Neither is an eligible
CPU probe host without resource changes, which this trial does not authorize.
Do not raise their limits or lower the CPU bound to force a result. If no existing
eligible CPU host is available, run the GPU-only bounded probe and report CPU
measurements unavailable rather than fabricate cross-device acceptance.
