# BOINC batch pilot

A volunteer worker pool alongside the on-prem OCR nodes
([#438](https://github.com/tgoodyear/usnewsmap.com/issues/438)).
Production and experimental tasks, including Qwen, share the same account,
CPU and GPU choices and resource limits. No per-experiment profile update is
required. Operators bound campaigns with the [native NVIDIA workunit window
and stop procedure](../contrib-compute/docs/app-release.md).

The pilot uses stock BOINC, one cached page per task, pinned Tesseract 5.5.0,
the model lock and language routing. It publishes native ALTO and Parquet to
`ocr-out`; `ocr-sync` and release decisions are unchanged. Other apps:

- `usnm_assess`: [JP2 versus IIIF JPEG comparison](boinc-assessment.md) (#456).
- `usnm_gold`: [score a host's engine on public ground truth](#gold-check).
- `usnm_surya`: [Surya 2 on CPUs and NVIDIA GPUs](boinc-surya.md) (#516).
- `usnm_ndlocr`: [Japanese NDLOCR-Lite/default-route CPU work](boinc-ndlocr-lite.md),
  two-reading character-similarity validation and collection into `ocr-ja`.
- `usnm_quality` and `usnm_qwen`: [per-page quality scoring without ground
  truth and Qwen3-VL deterministic word-crop transcription](boinc-quality.md) (#522).

Start with the [volunteer guide](boinc-volunteer.md).
The opt-in [batch controller](boinc-batch-scheduler.md) schedules allocated
cached pages with bounded queue backpressure; operators activate it separately.
The [post-training pilot](boinc-transcription.md) evaluates public-gold
baselines and adapted models with `usnm_qwen`.

## Workbook query validation

Run `python3 scripts/validate-boinc-workbook.py --workspace <customer-GUID>`
with the Mac's existing Azure CLI user credential. On an authorized Arc host use
`--identity arc`; the standard localhost:40342 challenge and token stay on that
host, and its existing identity must already have workspace Reader access.
Arc membership alone does not grant query access. No login, grant, workload or
resource is created by validation. `--subscription <id>` selects the existing
Azure CLI account without changing global CLI configuration.

The shared `scripts/log_analytics.py` client posts to
`https://api.loganalytics.io/v1/workspaces/<customer-GUID>/query` with audience
`https://api.loganalytics.io`. #528 isolated intermittent authorization failures
to a faulty Traffic Manager frontend: the same authorized user and Arc identity
worked on other frontends. Neither the replacement hostname with this token nor
the ARM-ID resource-context route supplied reliable workspace-data acceptance
in these tests. The client never pins an IP or bypasses Traffic Manager.

Only HTTP403 **InsufficientAccessError**, HTTP429 and HTTP5xx are retried.
Every attempt uses a new connection so DNS re-resolves, with 3–6s jitter,
at most six attempts and a 60s query budget (token acquisition is separate).
Other 403s, 401, 400, transport failures and partial-success error responses
fail closed. Attempt/status/`via` diagnostics go to stderr, never tokens.
A persistent authorization error still fails; retries do not confer permission.
The BOINC execution-log reader, saved-query CLI, cluster report and quality-row
reader share this implementation.

Validation runs a constant positive control and all nine source workbook KQL
queries (default seven days, grain one hour), emitting per-panel status, actual
row counts, query hashes, attempts and aggregate-only samples. Raw log messages
are excluded. A failed/partial response is never represented as zero rows.
Empty successful panels do not establish actual status/filter coverage or fresh
work-volume telemetry. The normal BOINC post-deploy health step runs validation
as a warning-level check; it does not redeploy resources to query them.
Its receipt is retained in a separate private `.boinc-work-*` directory beneath
the checkout (path printed to stderr), outside the deployment scratch cleanup.
Remove that directory manually after inspecting the receipt.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="images/pipeline-boinc-dark.svg">
  <img alt="OCR and search pipeline with BOINC: LoC scans go through the on-prem scan fetcher and OCR nodes to ocr-out, ocr-sync, the curated lake, release, index and API. The scan fetcher pushes cached scans to a hand-off container; a hand-off job in the main environment reserves pages and later collects results; the BOINC bridge pulls each job from the container, volunteers run the pinned worker, and the result spool and gold trust list go back through the container, so collect publishes to the same ocr-out as the OCR nodes for trusted hosts and holds an untrusted host's result in quarantine." src="images/pipeline-boinc.svg" width="960">
</picture>

## Boundaries

- Five registered BOINC platforms (`contrib-compute/boinc/project.xml`): `x86_64-pc-linux-gnu`, `aarch64-unknown-linux-gnu`, `arm64-apple-darwin`, `x86_64-apple-darwin`, and `windows_x86_64` (#533).
- Dual native + container distribution:
  - **Container apps** (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, plus container versions of `usnm_quality` and `usnm_qwen`) use BOINC's `docker_wrapper` under plan classes `docker`, `docker_mt`, and `docker_nvidia_cuda`. The stock wrapper supports both Docker and Podman on Linux, Podman via `Run_Podman` on macOS (with our `--format` pipe quoting fix in `scripts/boinc/patch-mac-wrapper.py`; see [boinc-buda-arm-evaluation.md](notes/boinc-buda-arm-evaluation.md)), and WSL2 `boinc-buda-runner` Podman on Windows. The [volunteer guide](boinc-volunteer.md) supplies an opt-in Linux rootless Podman profile and a mandatory startup preflight (`scripts/boinc/volunteer_preflight.py`, parameterized by `--job-toml`) for slot access, actual cgroup limits, and pausing and resuming.
  - **Native apps** (`usnm_quality` and `usnm_qwen`) run directly on the volunteer's host without a container runtime ([boinc-quality.md](boinc-quality.md)). `usnm_quality` ships as a standalone CPU binary (`crates/usnm-quality-app`, built by `.github/workflows/boinc-quality-native.yml`) across all five platforms. `usnm_qwen` ships as a native driver binary paired with the pinned `llama.cpp` release archives in `ocr/boinc/qwen/llama.lock` and `<sticky/><no_delete/>` GGUF model files under plan classes `apple_gpu_8b` (Apple Silicon Metal, >= 7,000 MB available unified RAM), `nvidia_cuda_8b` (>= 7,000 MB VRAM), `nvidia_cuda_2b` (>= 3,000 MB VRAM), and `mt` (4–8 CPU threads).
  - **The `boinc_finish_called` contract**: because BOINC's client treats a plain executable exiting `0` without writing `boinc_finish_called` in the slot directory as a premature exit and restarts it up to 100 times before aborting with `ERR_TOO_MANY_EXITS` (`client/app_control.cpp:533-546`), native binaries use `<copy_file/>` on all slot inputs and outputs, update `fraction_done`, and write `boinc_finish_called` containing `0\n` as their final act before exiting `0`.
- Explicit cohorts of 1-100 pages from one batch, selected from the prefetch
  manifest. No automatic consumption of Azure queues or LoC fetching.
- Central bridge: native work reservations, BOINC staging and submission,
  validation and publishing. Only it can access Azure through the existing Entra
  credential path. Volunteers get images and processing metadata, never
  credentials.
- The job and result file transport (version 1) is separate from the planned
  ADR-0016 long-lived engine protocol. Engine provenance remains `contract=0`
  with `route=boinc@r1`; models and routing are shared with the current worker.
- A task's container runs Tesseract directly. There is no nested Podman or
  Docker daemon. BOINC's Docker wrapper controls the outer container.

```
explicit page selection + cached scans
  -> native requested-work reservation + local dispatch journal
  -> stock BOINC -> pinned worker container
  -> stock batch_collect_assimilator spool
  -> usnm-boinc collect -> native ALTO/Parquet in ocr-out
  -> existing ocr-sync -> existing release/index
```

## Build the worker image

First build the existing engine image. Keep its pinned base, package snapshot,
and `models.lock`, including `frak2021`; the BOINC worker verifies the selected
models' bytes at execution. The lock includes Spanish (`spa`), French (`fra`),
Italian (`ita`), Yiddish (`yid`, then `heb` fallback), Hebrew (`heb`), Danish
(`dan`), Norwegian (`nor`, for `nor`/`nob`/`nno`), Finnish (`fin`), Swedish (`swe`),
Lithuanian (`lit`), Slovenian (`slv`), Hungarian (`hun`) and Slovak (`slk`).
These thirteen tessdata_best additions are pinned to the same revision as the
existing Apache-2.0 models and add 126,180,270 bytes (120.33 MiB) of model data
to both images before compression. The BOINC Containerfile inherits them from
the Tesseract image; it has no separate model list.

After changing the lock, republish **both** images. Run and score a pilot per
new language with the existing quality checks (including #522) before replacing
LoC text. Yiddish also needs the reader work in #328; Danish and Norwegian
1880s–1910s may need Fraktur routing (#325). Adding models alone does not
perform these operational steps.

Then build the BOINC image using that engine image's
immutable digest (these examples run from the repository root):

```sh
docker build -f ocr/engines/tesseract/Containerfile \
  -t usnm-tesseract:pilot ocr/engines/tesseract
docker image inspect usnm-tesseract:pilot --format '{{.Id}}'

docker build -f ocr/boinc/Containerfile \
  --build-arg TESSERACT_IMAGE=sha256:ENGINE_IMAGE_ID \
  --build-arg TESSERACT_FROM=usnm-tesseract:pilot \
  -t usnm-boinc:pilot .
```

Replace the placeholder with the full engine image ID. `TESSERACT_FROM` is
needed with Docker's BuildKit, which can't resolve a bare local image id in
`FROM`; podman can, and it defaults to `TESSERACT_IMAGE`. Volunteers pull the
published image ([Publish the worker image](#publish-the-worker-image)); an app
version records its registry digest,
`ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST`, not the engine base's
pin. Rebuild and sign a new BOINC app version when changing code, models, image,
or runtime configuration. The Rust build uses the repository's locked
dependencies; capture the build revision and compiler in your release.

### Publish the worker image

Volunteers pull the worker image from a public GHCR package,
`ghcr.io/tgoodyear/usnm-boinc-worker` (ADR-0019, addendum). The workflow
`.github/workflows/boinc-worker.yml` builds and smoke-tests separate native
`linux/amd64` and `linux/arm64` images on matching Ubuntu runners. The Debian and
Rust bases are pinned per architecture in that workflow; no QEMU setup is used.
Publishing starts by hand from `main` and combines both tested images into one OCI
index only after both builds pass. Pull requests build and test without pushing:

```sh
c="worker-$(date -u +%Y%m%dT%H%M%S)"
gh workflow run boinc-worker.yml --ref main -f correlation="$c"
# The run list is eventually consistent: wait for the run with this correlation id.
until run=$(gh run list --workflow boinc-worker.yml --event workflow_dispatch --limit 20 \
  --json databaseId,displayTitle -q "map(select(.displayTitle | endswith(\"($c)\")))[0].databaseId // empty") &&
  [ -n "$run" ]; do sleep 5; done
gh run watch "$run" --exit-status
gh run view "$run" --json url -q .url   # the run's summary holds the digest
```

The index job's summary and outputs `digest` and `image` give the **index**
digest to use in `scripts/boinc/install.py --image` and `images.worker` in
`contrib-compute/infra/boinc/env/<env>.json`. The summary also records each child
manifest and its `tesseract_image` engine configuration ID; the two engine IDs
differ even though their Tesseract version and model lock match.

`GOLD_ENGINE_IMAGE` and `ASSESS_ENGINE_IMAGE` use the worker index digest.
`GOLD_ENGINE_BASE` and `ASSESS_ENGINE_BASE` must match the architecture executing
the cohort. The current bridge settings hold only one base ID: keep assessment
and gold campaigns restricted to a matching architecture until per-architecture
expectations are supported. Start Mac validation with reserved `usnm_ocr` jobs;
this build support does not establish cross-architecture gold agreement.

The image is tagged with the commit; only the digest is used. A new GHCR package
is private, and a workflow can't change that. Once, after the first push:

1. Open the package's settings: github.com/users/tgoodyear/packages/container/usnm-boinc-worker/settings.
2. Under "Danger Zone", choose "Change visibility", then "Public", and type the package name to confirm.
3. Check that it is linked to this repository (the image's
   `org.opencontainers.image.source` label links it at the first push).
4. From a Docker that is logged out of GHCR, on a host that is not the maintainer's:
   `docker logout ghcr.io; docker pull ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<digest>`.

Later pushes keep the package public. Verify an anonymous pull on each native
architecture before releasing Mac app versions. See the [Mac release procedure](../contrib-compute/docs/app-release.md#mac-app-versions).

Before changing the installed worker image, stop creating new cohorts. Finish
exporting and submitting already allocated cohorts with their original image,
and drain all existing workunits through collection. Resolve uncertain and
failed tasks before rolling the app version; do not send their existing requests
to a different image. The worker checks the exact Tesseract version, profile and
model bytes, and the collector checks the result against the job's immutable
engine metadata. Installing a new app version does not rewrite existing requests
or bind `create_work` to an image automatically. Resume with fresh cohorts and
the new image pin after the old cohort is drained. Retain old signed files and
journals for recovery.

## Local synthetic demonstration

Requires Rust and the worker image above. `pilot_fixture` creates a rendered
`HELLO WORLD` page plus native manifest, sidecar, curated and reference stores,
and file-backed state. It makes no network requests and contains no production
data. Use a fresh directory:

```sh
cargo run --locked -p usnm-boinc --example pilot_fixture -- --dir /tmp/usnm-boinc-demo
cargo build --locked -p usnm-boinc

target/debug/usnm-boinc export \
  --manifest /tmp/usnm-boinc-demo/manifest.parquet \
  --pages /tmp/usnm-boinc-demo/pages.txt --batch test_pilot \
  --scans /tmp/usnm-boinc-demo/scans --root /tmp/usnm-boinc-demo/dispatch \
  --plan pilot-demo --image sha256:WORKER_IMAGE_ID \
  --curated-url /tmp/usnm-boinc-demo/curated \
  --reference-url /tmp/usnm-boinc-demo/reference \
  --state-file /tmp/usnm-boinc-demo/state.json
```

The exporter prints a job ID. Run its directory through the worker image:

```sh
docker run --rm --network=none --read-only --cap-drop=ALL --cap-add=DAC_OVERRIDE \
  --security-opt=no-new-privileges --cpus=1 --memory=1536m \
  --memory-swap=1536m --pids-limit=64 --tmpfs=/tmp:rw,size=512m \
  -v /tmp/usnm-boinc-demo/dispatch/jobs/JOB_ID:/work usnm-boinc:pilot
```

On SELinux hosts, label only this scratch directory appropriately (e.g. a `:Z`
volume). The stock spool names are `<workunit>__file_<logical_name>`; for this
local test copy the three output files into a scratch spool with that naming,
where the workunit is `usnm_JOB_ID`, then run:

```sh
target/debug/usnm-boinc collect --root /tmp/usnm-boinc-demo/dispatch \
  --spool /tmp/usnm-boinc-demo/spool --job JOB_ID \
  --curated-url /tmp/usnm-boinc-demo/curated --out-url /tmp/usnm-boinc-demo/out
```

Run it twice: publication remains one ALTO/Parquet pair. The Rust integration
suite also runs these outputs through the real `ocr-sync` in directory mode:

```sh
cargo test --locked -p usnm-boinc
python3 -m unittest discover -s scripts/boinc -p 'test_*.py'
```

## Connect a stock BOINC project

Create your own Linux BOINC project server using the upstream setup
documentation. Stock means unmodified upstream BOINC software: BOINC Central
hosting or public project registration is not required. Friends attach manually
to your server's project URL. Registering `usnm_ocr` below means registering it
on your own server. This repository does not provision its web server or
database or ship BOINC binaries. Use BOINC 8.2.13+ clients and a recent Linux
x86-64 `docker_wrapper` supporting `workdir` and `create_args`. Install or merge
upstream's Docker plan class and test that the requested runtime limits, pause
and resume, and output permissions work. The minimal CPU `docker` class from the
upstream sample is supplied in `ocr/boinc/plan_class_spec.xml`: copy it into a
project without a plan-class file, or merge its class into an existing file,
retaining the other classes. Restart project daemons after changing the project
configuration. Installing an app version alone does not install its scheduler
plan class.

The current stock wrapper source parses `workdir` as the slot's container mount
path and appends `create_args` to `docker create` (these fields are absent from
some published job-file docs). Pin and record the wrapper's source revision and
binary hash, and verify its behavior in the live acceptance exercise; an older
wrapper may ignore these settings. See the linked wrapper source.

`job.toml` drops all capabilities except `DAC_OVERRIDE`, which allows the
container's UID 0 to read and write the BOINC-owned slot without changing the
host slot permissions. Root with all capabilities dropped cannot reliably do
that when the slot and input files belong to another UID. The root filesystem
stays read-only, networking is disabled, and the slot is the only configured
host mount. Outputs and `fraction_done` are mode 0644 so the client can read
them. This has been checked with an OCI runtime and a deliberately foreign-owned
slot; it still needs confirmation through the actual BOINC wrapper on each
supported host.

Register the application in the BOINC admin interface (`contrib-compute/boinc/project.xml`). Install a container app candidate using a trusted stock wrapper and the pullable worker image digest, or a native app candidate using `--kind native --binary`:

```sh
# Container app version (apps/<app>/<version>/<platform>__<plan_class>):
python3 scripts/boinc/install.py --project /srv/boinc/newspapers \
  --wrapper /path/to/docker_wrapper \
  --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST --version 1.0 \
  --platform x86_64-pc-linux-gnu --plan-class docker

# Native app version (apps/usnm_quality/<version>/<platform>, or <platform>__<plan_class> for usnm_qwen):
python3 scripts/boinc/install.py --project /srv/boinc/newspapers \
  --app usnm_quality --kind native --binary /path/to/usnm_quality_1.1_x86_64-pc-linux-gnu \
  --platform x86_64-pc-linux-gnu --version 1.1
```

`contrib-compute/boinc/plan_class_spec.xml` defines both the container plan classes (`docker`, `docker_mt`, `docker_nvidia_cuda`) and the native `usnm_qwen` plan classes (`apple_gpu_8b`, `nvidia_cuda_8b`, `nvidia_cuda_2b`, `mt`). For native `usnm_qwen`, `ocr/boinc/qwen/llama.lock` pins the upstream `llama.cpp` release tag and per-platform archive SHA-256 digests, and the GGUF model templates use `<sticky/>` and `<no_delete/>` so volunteer clients cache models across workunits (`client/client_types.cpp:387`) and `file_deleter` (`sched/file_deleter.cpp:160-171`) never deletes shared model files from `download/`.

The installer refuses an existing version directory. It does not alter running
server configuration. Sign and publish its files using BOINC's normal
code-signing process and `bin/update_versions`. Merge `ocr/boinc/daemons.xml`
into the existing `config.xml` daemons section and restart the project daemons.
This file is a fragment, not a replacement list: retain the stock feeder,
transitioner, file_deleter, db_purge and other standard daemons. The separate
hosting work in [#439](https://github.com/tgoodyear/usnewsmap.com/pull/439)
supplies server deployment and bootstrap configuration; it is not a prerequisite
for self-hosting.

`sample_trivial_validator` accepts results for this trusted-friends pilot; it is
not an accuracy or security check. `batch_collect_assimilator` moves canonical
outputs to `results/0/<workunit>__file_<logical_name>` and records terminal
errors there. Our central collector validates every result before publishing.
Keep the spool until central publication succeeds. A partial or missing spool is
refused; inspect BOINC logs and recover its files rather than accepting a
partial result.

Export a real allocated cohort using its native prefetch manifest, canonical
page-key list, cached scan root, and curated and reference stores. For Azure state
use `--cosmos` in place of `--state-file`, with the same Entra identity and store
settings as ingest. The image flag is the full `sha256:...` digest corresponding
to the installed app. Then submit each exported job:

```sh
target/debug/usnm-boinc submit --root /srv/usnm/boinc-state \
  --project /srv/boinc/newspapers --job JOB_ID
```

#### Optional local page hosting (native OCR pilot only, #783)

The native `usnm-boinc submit` command alone accepts
`--download-base-url https://downloads.example.org/ocr-pages/` for an
independently published origin. The Azure submitting process does not need an
origin mount: the trusted local producer must first publish the exact staged
page as `<origin-root>/<sha256>`, atomically and immutably. Default submission is
unchanged. This does **not** migrate gold, Qwen, quality, Surya, NDLOCR, model,
application, upload or result files; the scheduler, application and database
remain in Azure.

Two-host flow (no origin filesystem mount on Azure):

```sh
# Trusted local producer: receipt records publication, NOT HTTPS readiness.
usnm-boinc prepare-download --page /srv/local/staged-page \
  --download-root /srv/usnm/immutable-ocr-pages
# Azure project server: retains the existing verified staged job/request.
usnm-boinc submit --root /srv/usnm/boinc-state \
  --project /srv/boinc/newspapers --job JOB_ID \
  --download-base-url https://downloads.example.org/ocr-pages/
```

When the submitter also runs on the producer, optional
`--download-root /srv/usnm/immutable-ocr-pages` adds create-only publication
before the same mandatory public probe (and requires `--download-base-url`).
Provision the existing absolute local directory without symlinks in any path
component and without group/world write access. This must be the trusted
producer's writable path, **not** a web-server container's read-only bind mount.
Map that directory exactly to
the public HTTPS prefix, with a valid certificate, anonymous GET and no redirects
or rewriting. In this mode submission verifies the staged SHA-256, publishes a create-only
`<root>/<sha256>` file as read-only `0444`, and fsyncs the file and directory.
An existing object must be identical and read-only; conflicts are never replaced.
Keep the directory operator-controlled and its content immutable; retain files
for outstanding work, retries and rollback, with backup and disk headroom.

Before writing `Submitting`, every attempt streams a fresh HTTP 200 GET,
checking exact size and SHA-256 with the existing scan-size cap and a 120-second
request timeout. DNS resolution has a separate 10-second timeout and runs
outside Tokio's blocking pool so stalled system DNS cannot delay CLI shutdown;
only public
addresses are allowed and pinned, proxies and redirects are disabled. Local
existence is not readiness proof. Unavailable/mismatched downloads leave the
job reserved without invoking stock tools.

Only tiny `request.json` is then staged on the project share. Stock
`create_work --remote_file URL nbytes md5` occupies input position 0; the
request filename remains position 1. At pinned BOINC commit
`22bbebe08e2eaac5d5e88a6d93b88e800cb6341c`,
[`tools/create_work.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/tools/create_work.cpp)
appends remote descriptors in argument order and
[`tools/process_input_template.cpp`](https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/tools/process_input_template.cpp)
names them `jf_<md5>` while preserving the supplied URL verbatim (the URL
basename need not equal that physical name). The unchanged `usnm_ocr_in`
template maps them to logical `page` and `request.json`. Any failure after
write-ahead remains uncertain and requires existing explicit reconciliation.
Allocation, trust, validation, acceptance and publication gates are unchanged.
Before enabling this opt-in, verify ingress from outside the LAN and adequate
upload bandwidth; no service or deployment change is included here. Public
HTTPS ingress is not yet established: the local origin is LAN-only, so
production volunteer submission/cutover remains blocked pending an approved,
persistent public ingress path. Do not relax the HTTPS probe to enable it.

Attach one friend's stock BOINC client to the project. Configure idle-only
processing and conservative preferences, and update the project. After the task
uploads and the assimilator spools it:

```sh
target/debug/usnm-boinc collect --root /srv/usnm/boinc-state \
  --spool /srv/boinc/newspapers/results/0 --job JOB_ID \
  --curated-url CURATED_STORE --out-url OCR_OUT_STORE
```

On Linux, submission gives only the staged download inputs (`page` and
`request.json`) mode `0644`, before their atomic rename and before `create_work`.
These files are meant to be downloaded by volunteers; the separate Apache
identity must be able to read them. The dispatch journal and original input
snapshots retain owner-only access. New download fan-out directories are
created with mode `0755` regardless of umask. Pre-existing download parents
are repaired by adding only missing `0755` bits, preserving other mode bits
such as setgid and group-write; submission does not change directory ownership
or ACLs. Check both file access and traversal with the actual web-server
identity on the deployed filesystem.

The server bridge wrapper submits directly and preserves the CLI's exit status.
It does not scan the download or hand-off trees to broaden permissions after
submission. The hand-off directory (`$HANDOFF_DIR`) stays private: `submit` and
`reconcile` run as the same user, and nothing reads that directory under another
identity. Operator hand-offs therefore need access to the journal owner's files.
Deploy the updated bridge image containing both this CLI and wrapper together.

A `published` bridge entry means its native output pair is in `ocr-out`, not that
it has replaced searchable text. Continue with the existing `ocr-sync` and release.

## Start a bridge step

The deployed server's bridge job (`contrib-compute/infra/boinc/modules/bridge-job.bicep`)
runs one step of `contrib-compute/images/server/bridge-job.sh` per execution, chosen by
`BRIDGE_STEP` and that step's settings. Start it with:

```sh
scripts/start-boinc-bridge.sh --env prod --step <step> [--var NAME=VALUE ...] [--wait-mins N] [--what-if]
```

Don't use `az containerapp job start --env-vars` for this. With any container flag, the
CLI sends an execution template holding one container named after the job, with no image
and only the variables given, so the job's own settings (`KEY_VAULT_NAME`, `DB_HOST`,
`PROJECT_DIR`, ...) are lost. The API's execution template has no volumes or mounts
either, and every step needs the project share at `/project`. The script writes the run's
settings into the job instead (`az containerapp job update --replace-env-vars`: the
template's own settings, `BRIDGE_STEP` and each `--var`; whatever a previous run set is
dropped), then starts it with no override. The next `scripts/provision-boinc.sh` restores
the template's settings. Run one step at a time.

When a waited-for execution (`--wait-mins`) or a `scripts/provision-boinc.sh` lifecycle job
fails, the script prints the execution's last 50 console lines from the stack's Log Analytics
workspace (`ContainerAppConsoleLogs_CL`; operators have Log Analytics Reader on it), polling up
to five minutes for ingestion. To read any execution's log by hand:

```sh
az monitor log-analytics query --workspace <workspace customerId> --analytics-query \
  "ContainerAppConsoleLogs_CL | where ContainerGroupName_s startswith '<execution>-' | project TimeGenerated, Log_s"
```

### Register the apps

Provisioning runs the idempotent `--ensure-project` lifecycle job and then `bin/xadd`.
An app added to
`contrib-compute/boinc/project.xml` later (`usnm_assess`, `usnm_gold`, `usnm_surya`,
`usnm_quality`, `usnm_qwen`) is registered automatically on the next provision. The
`register-apps` step copies the deployed image's `project.xml` into the project and
runs `bin/xadd`; it remains available for targeted recovery:

```sh
scripts/start-boinc-bridge.sh --env prod --step register-apps --wait-mins 15
```

It is safe to repeat. At the pinned BOINC commit (22bbebe), `tools/xadd` looks up each
`<platform>` and `<app>` by name. It inserts the ones that are missing, updates a changed
`user_friendly_name` or `min_version`, prints "already in DB" for the rest, and never
deletes or deprecates anything. The step needs a bridge image with this step in it
(`scripts/build-boinc-images.sh`, then `scripts/provision-boinc.sh`).

### Current-worker OCR 1.5 publication, 17 October 2026

The native app inventory found that Linux OCR 1.3/app 20 and Mac OCR 1.4/apps
23 and 24 still pin worker `80bf`, whereas current export and scoped trust
name worker `247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa`.
Publishing current-worker Gold does not update these OCR payloads. Do not
export a current-worker pilot and assume its signed OCR app matches, or
weaken the worker's identity check.

The normal prepare-only signing workflow produced immutable `usnm_ocr` 1.5
siblings with all three payload signatures verified for each platform.
Publisher repository: `crusnmprodffapzy.azurecr.io/usnm-boinc-appver`.

| Platform              | ACR build | Publisher digest                                                          | Succeeded execution suffix | Native app version |
| --------------------- | --------- | ------------------------------------------------------------------------- | -------------------------- | -----------------: |
| `x86_64-pc-linux-gnu` | `ch7a`    | `sha256:b9bf07591a265cb7b9a260221bcfd719b320cd96e204cc78ec8d7196eec70dd2` | `9rbj04a`                  |                 41 |
| `arm64-apple-darwin`  | `ch7b`    | `sha256:ccdf4a46376776ede3621be447b15e518e15e5594bdf99ad97bcd56feb07c8d2` | `sia7vgx`                  |                 42 |
| `x86_64-apple-darwin` | `ch7c`    | `sha256:74a718918a031125e5103125a1142a35533fc73efe91b525cb8899bca96ee626` | `n7hkbcb`                  |                 43 |

All pin the exact current `247c` worker; existing signed versions remain
unchanged. The desired `images.appver` pin names the final Intel sibling;
it is not a multi-platform publisher. After exact-head Copilot GREEN review,
zero threads, all CI and merge of #762, Linux, ARM and Intel were published
separately and serially. Each execution has prefix
`usnm-boinc-publish-caj-prod-` plus the suffix above and terminated Succeeded
before the next started. Normal production mutex, all-six-idle preflight,
infinite controller lease and expected-previous-image guards protected each
image-only update. Exact comparisons preserved every other template,
configuration and identity property; no broad stack provision was run.

Independent checks verified all three unique active version-105 / `docker`
registrations, exact `247c` Dockerfiles and three payload/signature pairs per
platform. All nine native download URLs returned anonymous HTTP 200 and
byte-identical SHA-256 hashes. Scoped global trust stayed unchanged.
Publication and registration are **not actual OCR-client acceptance**. The
[current-worker native Gold evidence and English-only scoped promotion](ocr-gold-set.md#current-worker-native-canonical-evidence-and-scoped-promotion)
are separate from fresh-page OCR execution, native handoff, release/search and
Intel-client acceptance. No allocation, continuous-feed activation or
backlog-policy exception is authorized by this publisher pin.

### Fresh-page Mac computation, collection and import, 17 October 2026

The normal one-page controller admitted
`sn82014782/1789-04-04/ed-2/seq-1` from base batch `kyu_frenchie`
(LoC version `kyu_frenchie_ver02`) under plan
`boinc-mac-current247c-20261017`. Four earlier ticks correctly held at
617 pending workunits against the normal target of 600. The existing managed
laptop client had 59 successful completed results waiting to report; an
identity-guarded ordinary project update reported them, reducing pending
work to 558. The unchanged allocation/configuration and persisted controller
state then admitted exactly one page. No cap exception, task cancellation,
reprioritization, account change or client-preference change was used.

The existing Mac fetched the page through ordinary scheduling; this was not
a targeted Gold challenge or a relabeled Linux result. Native evidence:

| Field                        | Accepted value                                                     |
| ---------------------------- | ------------------------------------------------------------------ |
| Job                          | `cae8426495087616cdf46bcadbf648406c96c1e24561b81aa99f6d7fcee6b232` |
| Workunit / canonical result  | `16738` / `32119`                                                  |
| Canonical host / app version | `1` / `42` (Apple Silicon OCR 1.5)                                 |
| Outcome / validation / exit  | `1` / `1` / `0`                                                    |
| Assimilation / error mask    | `2` / `0`                                                          |
| Canonical report SHA-256     | `d5ac93f0be4e05258affaddecb642b01e38f4caf31f4fb40dde40e0168f51b6e` |
| Input scan SHA-256           | `64c0a1122e17ced977e1540536acdd88a8fbdeff268192fb634f832de729d958` |
| Output text SHA-256          | `92d54913897d94a50dda409f1610f9c291e88dfe0a557e482ba7e2c88e4d52de` |

The canonical report names current worker `247c`, Tesseract 5.5.0 and
profile `eng` / PSM 3 / DPI 300. Global scoped trust stayed at SHA-256
`3bfc19016cf38b47455faa4ddbe8fbbe27577c410c1d8e74a2ccdd8a0a6a651b`.
Normal controller collection then completed through `spool-out` and
trust-checked handoff execution `usnm-boinc-handoff-caj-prod-71j1xrr`
(Succeeded). The durable dispatch record is `published`; the one-page
controller reports selected 1 / published 1 / collection backlog 0.
This marker means published to `ocr-out`, not to public search.

Normal pickup execution `usnm-ocrs-caj-prod-ye8ldw4` also Succeeded.
Its durable import record
`ocr-local/import/20261017T091059970936Z-2a10320b-0000.json`
records the exact BOINC page part and ALTO as `imported`. The merged part is
`ocr-local/pages/sn82014782/20261017T091059970936Z-2a10320b-0000.parquet`,
SHA-256 `ed7f2968107ced85fec4031c7d327bcf26413c3be9680bb94d4ae702a6e0e86e`.
Its one row names the expected document, node `boinc-cae842649508`,
worker `247c`, `route=boinc@r1` and 8,669 normalized characters. The
normalized text SHA-256 is
`0747212670a1df26014df567c1226180eaee373da196775990aeb55ba99c8d4b`;
the collector applies `normalize_ocr` after validating the raw canonical
text hash, so this is not the raw worker text hash above.

Stored metric-v2 scores detect English for both readings: our damage rate
is `0.20512820512820512`, versus LoC's `0.2841726618705036`, with head-word
counts 341 versus 199. These stored scores clear rule v1's 0.05 margin and
head-count test, but are an estimate: a real release scores the texts again,
checks title languages, revocations and competing candidates, and records
its actual selection. The already-running `pages-v20261017-1` rebuild
started before this import; do not assume its frozen local-OCR inputs
include the new part or restart its owner's writer to force inclusion.

This proves fresh-page Mac computation, native canonical assimilation,
trust-checked handoff and native import. Actual release selection,
new-version search/source credits and Intel-client execution remain open.
Retain the same controller state and journals; never manufacture another
submission or reset the page to fresh.

## Server and maintainer on different hosts

The bridge has no remote-submit API. `submit` and `reconcile` must run on the
BOINC server host where the project tree and database are reachable. `export`
and `collect` can run on the maintainer's host with its existing Entra access.
The original prefetch scan cache stays there: the dispatch root's `jobs/`
already contains the input snapshots the server needs.

For a small pilot, use a manual hand-off over a trusted transfer channel:

1. Export the allocated cohort locally, stop bridge writers for that dispatch
   root, and transfer its complete `jobs/` tree into a fresh server-local dispatch
   root. Preserve file bytes and journal timestamps; do not transfer a live lock
   or regenerate requests. Keep the frozen original as a backup.
2. Make the server copy the sole writable journal. Submit using that root and
   the local BOINC project. If submission becomes uncertain, inspect and
   reconcile on the server before transferring control again.
3. Stop server bridge writers and transfer the updated complete journal tree
   back into a fresh maintainer-local root. Transfer the matching assimilator
   spool files, including any terminal error markers. Retain the server copy
   and spool until publication is confirmed. Run `collect` against this updated
   root and the copied spool, then native `ocr-sync` as usual.
4. Transfer control again only after freezing the current writer. Do not merge
   diverging journals or run both copies concurrently: local `flock` does not
   serialize separate machines. An old backup cannot establish that a job was
   never submitted; inspect the server and preserve uncertain state.

This is a manual single-writer protocol, not tested remote orchestration. Keep
journals on persistent local storage at each stage; Azure Files/SMB lock and
atomic-rename durability have not been verified. Exercise the hand-off on a
small cohort before using the Azure-hosted submission job from #439.

## One page from the scan cache

This is the route for one page (#469) when the scan cache, the services that reserve and
publish work (Cosmos, the lake, `ocr-out`) and the BOINC server share no filesystem. One
blob container, `boinc-handoff`, sits between them. Nothing reads `/tank/ocr` except the scan
fetcher, and nothing asks LoC for a page again: the scan fetcher (CT 211) stays the only LoC
fetcher, and volunteers download the page from our project server.

| Who                                 | Runs                                                | Reads                                     | Writes                                       |
| ----------------------------------- | --------------------------------------------------- | ----------------------------------------- | -------------------------------------------- |
| Scan fetcher, CT 211 (Arc identity) | `scripts/ocr-node/push-boinc-scan.sh` (az only)     | `/tank/ocr/scans`, `/tank/ocr/manifests`  | `scans/`, `manifests/`                       |
| Hand-off job, main environment      | `handoff-job.sh`: `export`, later `collect --trust` | `scans/`, `manifests/`, `spool/`, `gold/` | `dispatch/`, Cosmos lease, ledger, `ocr-out` |
| BOINC bridge job, BOINC environment | `submit-remote`, later `spool-out`                  | `dispatch/jobs/<id>/`                     | `spool/<id>/`, `gold/trust.json`             |

The hand-off job runs in the main Container Apps environment because that environment
already reaches Cosmos and the lake through their private endpoints; neither is opened to
the LAN. It uses the bridge image with the command
`/usr/local/lib/usnm-boinc/handoff-job.sh`. Its dispatch root (the journal `export` and
`collect` share) lives in the container under `dispatch/`: each run copies it down and
writes it back.

`submit-remote` copies `dispatch/jobs/<id>/` (the page, `request.json` and `record.json`)
into the server's own hand-off root. It checks that the record is that request's, that the
request's identity is the job id, and that the page's size and sha256 are the request's.
Then it runs the existing `submit`, which checks the page again and stages it as a BOINC
download. A job the server already has past `reserved` is left alone, so a repeated run
submits nothing. The dispatch root's copy stays `reserved`, which `collect` accepts.

`spool-out` writes the job's spool files (terminal error markers included), the canonical
result's host id (`result.hostid` of the workunit's canonical result, read from the BOINC
database; UNVERIFIED against a live database) and `/project/gold/trust.json` to the
container. The hand-off job's `collect` step reads them back and publishes to `ocr-out`
only for a host that passed the gold check.

### Infrastructure

- the container `boinc-handoff` on the exchange account (`stusnmo<suffix>`,
  `infra/modules/ocr-exchange.bicep`), with the `boinc-handoff/scans/` prefix expiring after
  30 days, like `ocr-out`;
- the BOINC environment's `aca` subnet allowed on the exchange account's firewall, through a
  `Microsoft.Storage.Global` service endpoint on that subnet
  (`contrib-compute/infra/boinc/modules/network.bicep`). The BOINC VNet is in westus2 and the
  exchange account in eastus2, and a plain `Microsoft.Storage` endpoint covers only its own
  region and its pair. A private endpoint is not an option: the main VNet's `privatelink.blob`
  zone would then fail to resolve the exchange account for `ocr-sync`. Storage refuses the
  firewall rule until the endpoint exists, so the rule comes with the hand-off job (below). On
  a new environment: `scripts/provision.sh` with `USNM_BOINC_GROUP` (the group), then
  `scripts/provision-boinc.sh` (the network, the identity and the images), then
  `USNM_BOINC_HANDOFF_IMAGE` and `scripts/provision.sh` again (the job, the rule and the
  grants);
- the hand-off job `usnm-boinc-handoff-caj-<env>` and its identity
  `usnm-boinc-handoff-id-<env>` in the main environment (`infra/modules/boinc-handoff-job.bicep`),
  with its own settings `AZURE_CLIENT_ID`, `HANDOFF_URL`, `COSMOS_ENDPOINT`, `CURATED_URL`,
  `REFERENCE_URL` and `OUT_URL`. Its image is the setting `USNM_BOINC_HANDOFF_IMAGE`
  (`infra/env/<env>.json`): `images.bridge` from `contrib-compute/infra/boinc/env/<env>.json`,
  so a new bridge image means updating both. `scripts/start-boinc-bridge.sh --target handoff`
  starts it with a run's settings. It reads the job name and resource group from the main
  stack's outputs saved by `scripts/provision.sh`, so it doesn't need an
  `infra/names/<env>.json` entry.

The role assignments:

| Identity                                     | Scope                             | Role                                                       |
| -------------------------------------------- | --------------------------------- | ---------------------------------------------------------- |
| Hand-off job (`usnm-boinc-handoff-id-<env>`) | container `boinc-handoff`         | OCR node writer (list, read, create; no delete)            |
| Hand-off job                                 | Cosmos `usnm/ops`                 | Cosmos DB Built-in Data Contributor (the `ocr-sync` lease) |
| Hand-off job                                 | lake `curated`, `ocr-local/` only | `usnm-boinc-ledger-role-<env>` (list, read, create)        |
| Hand-off job                                 | lake `reference`                  | Storage Blob Data Reader                                   |
| Hand-off job                                 | exchange `ocr-out`                | OCR node writer                                            |
| Hand-off job                                 | the shared registry               | AcrPull                                                    |
| BOINC bridge (`usnm-boinc-id-<env>`)         | container `boinc-handoff`         | OCR node writer                                            |
| BOINC bridge                                 | scan archive `scans`              | Storage Blob Data Reader (staging reads it first)          |
| Scan fetcher (`usnm-ocr-node5-arcs-<env>`)   | container `boinc-handoff`         | OCR node writer                                            |

The main stack makes all of them, the bridge's included (it reads the BOINC identity as an
existing resource), so the BOINC stack only adds the service endpoint. The Cosmos, ledger,
reference and `ocr-out` grants are `infra/modules/boinc-handoff-runner.bicep`'s; the
container grants are `ocr-exchange.bicep`'s, and the `scans` grant (with the BOINC subnet on the
archive's firewall) `scan-archive.bicep`'s.

### Runbook

On CT 211, signed in with `az login --identity --allow-no-subscriptions`:

```sh
scripts/ocr-node/push-boinc-scan.sh --account stusnmoffapzy \
  --scan /tank/ocr/scans/keep/<batch>/<lccn>/<date>/<ed>/<seq>.jp2 \
  --manifest /tank/ocr/manifests/<version>/manifest.parquet
```

Then, from the operator's machine, the hand-off job's export. It prints the job id in its
log, and `dispatch/jobs/<id>/` appears in the container:

```sh
scripts/start-boinc-bridge.sh --env prod --target handoff --step export \
  --var HANDOFF_PAGES=<page key> --var HANDOFF_BATCH=<batch> --var HANDOFF_PLAN=<plan id> \
  --var HANDOFF_MANIFEST=<version>/manifest.parquet \
  --var ENGINE_IMAGE=sha256:<images.worker's digest> --wait-mins 30
```

Then the bridge:

```sh
scripts/start-boinc-bridge.sh --env prod --step submit-remote --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff --wait-mins 30
```

When the workunit's result is assimilated (the ops pages show it done):

```sh
scripts/start-boinc-bridge.sh --env prod --step spool-out --var JOB_ID=JOB_ID \
  --var HANDOFF_URL=https://stusnmoffapzy.blob.core.windows.net/boinc-handoff \
  --var HANDOFF_SPOOL=/project/usnmocr/results/0 --wait-mins 15
```

Then the hand-off job's collect (`HOST_ID` only when the server recorded none):

```sh
scripts/start-boinc-bridge.sh --env prod --target handoff --step collect --var JOB_ID=JOB_ID --wait-mins 30
```

It publishes to `ocr-out` for a trusted host and writes the
dispatch root back with the job `published`. An untrusted host's result is held in the
dispatch root's `quarantine/<job>/`. Stop there: `ocr-sync` is the next step, as usual. A terminal BOINC failure
(an `_error` or `_no_output_files` spool) needs neither a host nor the trust list: collect
marks the job failed. Each run writes back only the journal entries it touched; run one
execution at a time all the same. If a write-back keeps failing, the job's log carries the
record (`USNM-HANDOFF-RECORD` lines), since a lost reservation's record can't be made again.

### The scan fetcher's identity (scan-fetch container)

The scan fetcher (deployed by infrastructure tooling) holds the prefetch
manifest and the scans under `/tank/ocr` and pushes them with `push-boinc-scan.sh`. It is an
unprivileged Debian 12 LXC; the Azure Connected Machine agent runs in it like on a VM. Its
Arc machine, `usnm-ocr-node5-arcs-prod` in the environment's group, gives it a
system-assigned identity. It is not an OCR worker and is not in the nodes' Entra group, so
it has none of the queue grants.

Its grants go to the object id in the private setting `USNM_BOINC_HANDOFF_RUNNER`:

| Scope                            | Role                                                       | For                        |
| -------------------------------- | ---------------------------------------------------------- | -------------------------- |
| exchange `boinc-handoff`         | the OCR nodes' writer role (list, read, create; no delete) | the scans and the manifest |
| `usnm-boinc-log-<env>` workspace | Log Analytics Reader (`boinc-handoff-logs.bicep`)          | the bridge jobs' logs      |

Until the hand-off job existed, `export` and `collect` ran here too, with Cosmos, ledger,
reference and `ocr-out` grants (#514); the job took those over and they are gone.

Set up once, from the operator's machine and the Proxmox host, after the BOINC stack is
deployed (the workspace grant needs `usnm-boinc-log-<env>` to exist):

```sh
# On pve-big: the agent, locked down before it connects (no extensions, no
# machine configuration, no incoming connections), and az.
pct exec 211 -- bash -c 'curl -fsSL -o /root/arc.sh https://aka.ms/azcmagent && bash /root/arc.sh'
pct exec 211 -- azcmagent config set extensions.enabled false
pct exec 211 -- azcmagent config set guestconfiguration.enabled false
pct exec 211 -- azcmagent config set incomingconnections.enabled false
# Connect with a short-lived ARM token in a root-only file (never on a command line),
# --resource-group <the environment's group> --resource-name usnm-ocr-node5-arcs-prod
# --tags environment=prod,project=usnewsmap,role=boinc-handoff --config <file>.

# On the operator's machine: the grant.
az resource show -g "$(scripts/settings.sh --env prod --key AZURE_RESOURCE_GROUP)" \
  -n usnm-ocr-node5-arcs-prod --resource-type Microsoft.HybridCompute/machines \
  --query identity.principalId -o tsv
scripts/settings.sh --env prod --key USNM_BOINC_HANDOFF_RUNNER --value <object id>
scripts/provision.sh --env prod --what-if
scripts/provision.sh --env prod
```

In the container, `az login --identity --allow-no-subscriptions` signs in (the
identity has data-plane roles only, no subscription role).

The grant opens no network path: the exchange account takes the home IP through
`USNM_OCR_NODE_IPS`.

## Allocation and recovery

Export filters the manifest while decoding, retaining only selected batch and
page rows (the native decoder still validates all rows). Undetected page
languages use the title's first catalog language from the published reference
snapshot, using the same MARC-to-ISO mapping as native enqueue. BOINC
eligibility is stricter: only titles present in this version's `titles.json` are
eligible. The mutable `catalog/titles.json` cannot make an unpublished title
eligible. Published version IDs must be safe single path segments before any
snapshot objects are read.

Export stages all selected jobs locally before reserving the cohort under one
renewable `ocr-sync` lock. It reads the published metadata and native ledger
once, validates the whole cohort, and writes one create-only queued part for
fresh reservations. If any row conflicts, no new reservations are written. It
verifies that the published version has not changed since staging, checks batch,
version and title against that snapshot, and leaves Japanese titles to their
existing pipeline. It refuses pages with prior native work, except an exact
replay of its own reservation. "Fresh cohort" means different page keys without
prior native work. Both outstanding and completed native work make a page
ineligible; changing the plan, image pin, timeout or job ID does not bypass this
check. The ledger remains outstanding across BOINC processing; no Azure queue
lease is held or renewed. Allocate a fresh small cohort, and exclude it from
overlapping seeder plans: the existing seeder may reconsider old outstanding
work under its own policy. This pilot does not add a global long-lived BOINC
ownership field to that policy.

The local journal has `prepared`, `reserved`, `submitting`, `submitted`,
`published`, and `failed` states. Keep it on persistent local storage and back it
up. An advisory lock serializes bridge processes sharing that directory.

- Repeating export completes a reservation interrupted by a crash, without writing
  another request. A lost cohort-write response or partially updated local
  journals replay the exact existing rows and reserve only still-fresh pages.
  The original request time is retained at native microsecond precision.
- Before `create_work`, submission records `submitting`. After an error or crash
  it never automatically submits again. Inspect BOINC for `usnm_JOB_ID`. Only
  after verifying whether it exists, run `reconcile --root DIR --job ID --found
present` (mark submitted) or `--found absent` (permit submission again).
- The importer snapshots the first accepted bytes before external publication.
  Fixed object names and byte comparisons recover a lost upload response or a
  crash between ALTO and Parquet. Conflicting preexisting bytes stop publication.
  Native import outcomes recover a lost local commit after `ocr-sync` consumed
  the objects. A completed import is never republished from another callback.
- A terminal BOINC error sets `failed`. Preserve the native reservation and
  diagnose it with BOINC's server tools. This first pilot has no automatic
  reallocation. After recovering the same workunit on the server, preserve then
  remove its stale spool error marker and use `reconcile --root DIR --job ID
--found present` to permit collection of its recovered output. A failed job
  cannot be marked absent to create a duplicate workunit. Alternatively arrange
  a new native allocation through the existing pipeline. Do not reset a
  submitted or failed task blindly. There is no supported "re-export this failed
  page with a new BOINC job ID" command. To move the page back to the native
  pool, first ensure BOINC can no longer return or collect it, retain its
  journal and spool for audit, then use the existing [native enqueue/requeue
  procedure](operations.md#ocr-pickup-ocr-sync-296) with an operator-approved
  `ocr-enqueue --include-outstanding` retry for that page. `ocr-requeue` targets
  native dead letters, not BOINC spool error markers. Never delete ledger
  history to make the page appear fresh.
- Keep `keep/` scans. Do not delete `buffer/` scans from a volunteer claim or task
  completion. The exporter snapshots inputs centrally and never deletes originals.
- No in-page checkpointing: restart can repeat a page. BOINC resource estimates
  are initial bounds; calibrate them on real scans. One-hour maximum
  configurable OCR deadline, default 600 seconds, one Tesseract thread, PSM 3,
  DPI 300. The fixed `rsc_fpops_est=1e12` is provisional, so client ETA and
  credit are rough; use `elapsed_ms` from returned manifests across real JPEG
  and JP2 scans to calibrate it. `target_nresults=1` requests one result.
  `max_success_results=3` is a failure ceiling, not a request for three copies;
  stock BOINC cancels unsent copies once it finds a canonical result. These
  retry ceilings remain unchanged pending live recovery measurements. The 1.5
  GiB memory bound is provisional and has only been exercised on a synthetic
  page; peak memory for large historical JP2 decode plus Tesseract is
  unmeasured. Record peak memory and time on the largest intended scans before
  widening enrollment or raising batch volume. A resource limit failure is a
  failed task, not evidence of a usable OCR result.

Result identity and provenance fields and checksums establish transport
consistency, not proof that a volunteer actually ran the declared image. Trusted
friends are a prerequisite; public enrollment requires stronger validation.
Raising quorum alone with `sample_trivial_validator` does not compare OCR
output. A future audit or comparative validator could rerun sampled pages and
compare text, after measuring determinism across supported hosts, or check a
trusted reference set. No hand-transcribed historical accuracy measurement is
implied by the synthetic smoke test. The optional comparative validator is being
developed separately in
[#444](https://github.com/tgoodyear/usnewsmap.com/pull/444); it is not part of
this pilot's default validator or a claim of public-volunteer readiness.

Replicas of one workunit go to different **hosts**
(`one_result_per_host_per_wu`), not different accounts. The project's own hosts
share one account, and with the per-account rule they could take only one of
each quorum-2 pair, so they idled while the second copies waited for another
user. The cost: a volunteer with several hosts can supply both copies of a unit.
Gold units and spot checks (Host trust below) are the guard against that, not
the quorum.

## Live acceptance exercise

Complete one real task on a stock client and server, then test keyboard activity
pause, sleep and reboot, a missing result file, a wrong checksum, an importer
restart and repeat collection. Confirm the wrapper applies `create_args` and all
three outputs are readable by the host client. Include a slot owned by the BOINC
service user with restrictive directory and input modes, not only a directory
owned by the runtime user; confirm `DAC_OVERRIDE` and the file modes work as
intended. Record the server, client, wrapper and image versions and which
platforms were tested. Local native-pipeline tests do not establish BOINC
scheduler or OS-specific idle behavior.

Upstream references:

- [Server setup](https://github.com/BOINC/boinc/wiki/ServerIntro)
- [Docker app cookbook](https://github.com/BOINC/boinc/wiki/Docker-app-cookbook)
- [Code signing](https://github.com/BOINC/boinc/wiki/Code-signing)
- [Batch collector](https://github.com/BOINC/boinc/blob/master/sched/batch_collect_assimilator.cpp)
- [Stock validator result and retry handling](https://github.com/BOINC/boinc/blob/master/sched/validator.cpp)
- [Stock Docker wrapper source](https://github.com/BOINC/boinc/blob/master/samples/docker_wrapper/docker_wrapper.cpp)
- [Stock scheduler plan-class sample](https://github.com/BOINC/boinc/blob/master/sched/plan_class_spec.xml.sample)

## Gold check

For an optional, aggregate-capped client beside an OCR node's existing queue
worker, see [the dev-first OCR-node plan](boinc-ocr-node.md) (#472). It does not
enable or attach production clients.

A volunteer host can score its engine on the public gold bundle
([ocr-gold-set.md](ocr-gold-set.md#volunteer-bundle), #457) and report CER and
WER together with the bundle version. Two steps, because the task container
runs with `--network=none`:

```sh
# On the host, with network: verify the bundle, download the original files it
# names (BLN600's zip from figshare, page images from loc.gov) into a cache, and
# keep each only if its sha256 matches. Downloads are one at a time, 1 s apart.
usnm-boinc gold-stage --bundle usnm-ocr-gold-v1.tar --cache CACHE --work WORK

# Offline: verify the bundle again, check the cache, cut the crops locally, run
# Tesseract on each, score as gold.py does, and write the report.
usnm-boinc gold-run --bundle usnm-ocr-gold-v1.tar --cache CACHE --work WORK \
  --out gold-result.json [--tesseract tesseract] [--psm 3] [--dpi 300] [--limit N]
```

For another engine, replace `--tesseract` with `--engine-cmd /path/to/adapter
--engine-name NAME --engine-version VERSION`. The executable receives exactly
two arguments: the crop image path and its bundle language. It must write only
UTF-8 OCR text to stdout and exit zero. Use an adapter executable for extra
flags; no shell expansion is performed. Each crop has the same timeout; output
is capped at 16 MiB. External commands require Unix; their process group is
killed on completion, failure, timeout and cancellation, including ordinary
adapter subprocesses. Detached sessions are outside this containment (use a
container for untrusted engines). Failed command output (including stderr) is
never copied into the report. The profile is the bundle language; psm and dpi
describe Tesseract only and do not configure an external engine. Name and
version are explicit operator metadata, not independently verified image
identity.

`gold-run` does not download files, but it cannot sandbox an arbitrary
executable. Run the **entire invocation** in a container with `--network=none`
(as for the stock worker), with preinstalled engine and models and a staged
cache; never give the adapter reference text. This option is for local
experiments, not the locked `usnm_gold` BOINC job or its trust reports.

`gold-result.json` holds `bundle_version`, the manifest's sha256, the engine
(name, version, psm, dpi), one row per crop (counts only: reference characters,
character edits, reference words, word edits, the model profile and the time), a
summary per source and overall, the `--limit` if one was given (a limited run is
partial), and the crops that were skipped with the reason (not staged, checksum
mismatch, no installed model, engine error, over the limit). It holds no page
text and no transcription. The project does not ship derived BLN600 crops or LoC
crops: it mirrors the unchanged original files from the publisher and LoC fetch
cache, and the client cuts the crops locally.

### As BOINC work: `usnm_gold`

The third app, `usnm_gold` (#462), sends the same check to hosts as work. It
runs in the same worker image; `gold.json` in the slot selects it. The job and
its checks are `usnm_boinc::gold::job`; the code is `crates/usnm-boinc/src/gold/`.

**Inputs and delivery.** A unit's inputs, in input template order:

| Open name              | From                                                               | On the host  |
| ---------------------- | ------------------------------------------------------------------ | ------------ |
| `gold.json`            | the project server                                                 | per unit     |
| `bundle.tar`           | the project server (shareable files only)                          | sticky, kept |
| `<sha256>.zip`, `.jpg` | the project server's mirror of figshare and tile.loc.gov originals | sticky, kept |

`gold prepare` downloads the originals into the campaign cache and records
their sizes, MD5s and SHA-256s in `prepared.json`. Before creating work,
`gold submit` verifies each cached file against those values and mirrors it
once into the BOINC project's ordinary download hierarchy under a
content-addressed physical name. `create_work` receives those staged filenames,
not `--remote_file` URLs; the volunteer client transfers the project copy,
not the publisher copy. A pre-existing mirror file must match the prepared
manifest or submission stops without replacing it.

The [Figshare v2 record](https://doi.org/10.15131/shef.data.25439023.v2)
identifies CC BY-NC-ND 4.0 and credits Callum Booth, Alan Thomas and Robert
Gaizauskas. The [CC BY-NC-ND 4.0
deed](https://creativecommons.org/licenses/by-nc-nd/4.0/deed.en) and [legal
code](https://creativecommons.org/licenses/by-nc-nd/4.0/legalcode.en) describe
sharing subject to NonCommercial use, appropriate credit and a license link, and
do not permit sharing adaptations. The staged zip is copied unchanged. The
bundle's `licenses/bln600/` directory preserves the source `LICENSE` and
`readme.md`, plus `SOURCE.json` and `ATTRIBUTION.txt`; the bundle is delivered
as a separate sticky input alongside the zip. These files record the published
conditions; the locked v1 `SOURCE.json` is retained as historical source
metadata, not a legal determination or confirmation that a particular deployment
qualifies as NonCommercial. Before enabling a mirror, the operator must verify
the project and hosting use against the source terms. Do not distribute
BLN600-derived crops or re-encoded images. The LoC images are likewise
transferred from the verified mirror.

The tar and originals are marked `<sticky/>` and `<no_delete/>` in the input
template, intended for reuse across a host's gold units. Live client evidence
has not confirmed that the generated work retains these flags or avoids repeat
downloads (see the acceptance record below). All are `<copy_file/>`, because
the container sees only the slot. Their open names match the cache names
`gold-run` reads, so the slot is the cache and the run step needs no network:
the container keeps `--network=none` from `job.toml`. The worker checks every
input's size and sha256 against the job, the manifest's digest and version, the
Tesseract version and base image, and the model bytes of each profile it used,
then writes `gold-result.json`.

**Result.** `gold-result.json` is the report above, run on the whole bundle
(no `--limit`), with a `boinc` block: job id, worker image and base digests,
and the host id the client wrote into the slot's `init_data.xml` (the client's
claim, not proof). Counts only; no text. The output template allows 256 KiB.

**Validation.** One result per unit (`min_quorum` 1): each result measures the
host that ran it, so results from two hosts are not expected to match, and
there is no compare script. `script_validator` runs `usnm_gold_check`
(`usnm-boinc gold check`) on each result against
`<project>/gold/<manifest sha256>.json`, which `gold submit` writes. It
refuses a result with any field outside the schema, a skip reason other than
`no-model:<lang>`, `engine-error` or `not-prepared` (the worker turns its
reasons into these codes), or an engine name other than `usnm-tesseract`, and
refuses it unless:

- it is the report of the unit it was uploaded for (the job id in it matches the
  workunit name in the result file's name), a unit this server made, with that unit's bundle version, manifest
  sha256 and engine (image, base, version, PSM 3, DPI 300), and no `--limit`;
- every crop of the bundle is scored or skipped exactly once;
- each scored crop's reference characters and words equal the counts `gold
prepare` computed from the transcription (a host that read other text fails);
- edit counts and times are in range, and the summary adds up from the crops.

This does not prove which engine ran. As for the other apps, volunteers are
trusted friends.

**Where reports go.** `usnm-boinc gold collect` reads the canonical results
from the `batch_collect_assimilator` spool, checks each again, and writes
`reports.jsonl` (one row per result) and `crops.jsonl` (one row per scored
crop), keyed by bundle version, manifest sha256, engine version, image and
base, workunit and host id, under `<share>/gold/<campaign>/out/`. Nothing new
in Azure: they live on the existing project share.

**Models.** The worker image's model lock decides which crops a host can score;
crops in a language with no locked model are listed as skipped.

**Running a campaign.** With the server deployed with `assessment=true` (`"assessment": true` in
`contrib-compute/infra/boinc/env/<env>.json`, then `scripts/provision-boinc.sh`; the
server runs from the bridge image, which carries the validator), a
`usnm_gold` app version installed (`scripts/boinc/install.py --app usnm_gold
...`), signed, and `bin/update_versions` run, and the bundle tar copied to
`<share>/gold/<campaign>/bundle.tar`:

```sh
# Once per bundle: fetch the originals, one request at a time, 5 s apart, into
# <campaign>/cache on the share; write prepared.json (sizes, md5s, reference
# counts). Stops if any download fails or a checksum differs.
scripts/start-boinc-bridge.sh --env prod --step gold-prepare --var GOLD_CAMPAIGN=v1-tess550 --wait-mins 60
# Make the units (default 20). Re-running skips units already submitted.
scripts/start-boinc-bridge.sh --env prod --step gold-submit --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_ENGINE_IMAGE=sha256:... --var GOLD_ENGINE_BASE=<its TESSERACT_IMAGE> --var GOLD_UNITS=20
# When results are assimilated:
scripts/start-boinc-bridge.sh --env prod --step gold-collect --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_SPOOL=/project/usnmocr/results/<batch>
# Then the trust list for `collect --trust` (see Host trust below), at <share>/gold/trust.json:
scripts/start-boinc-bridge.sh --env prod --step gold-trust --var GOLD_CAMPAIGN=v1-tess550 \
  --var GOLD_MAX_CER=<calibrated-threshold> \
  --var GOLD_MANIFEST_SHA256=<calibrated-manifest-sha256> \
  --var GOLD_LANGUAGE=<source-language> --var GOLD_PROFILE=<model-profile> --wait-mins 10
```

The gold units' batch is 0, so their spool is `/project/usnmocr/results/0`.
These commands describe the code path; no production mirror or campaign is
enabled by this change.

For separately authorized LoC-only transfer acceptance, `gold-mirror-loc`
stages only the 18 original JPEGs pinned in `bundle-v1.lock.json`, without
opening BLN600, fetching upstream data, submitting work, or changing clients:

```sh
scripts/start-boinc-bridge.sh --env prod --step gold-mirror-loc --var GOLD_CAMPAIGN=v1-tess550
```

Use the normal serialized production-job preflight. The command first checks
all selected cache files against prepared size, MD5 and SHA-256, and the locked
URL/SHA-256, and checks existing destinations. It rejects unexpected paths,
symlinks and mismatches, then publishes mode-0664 files atomically using native
`dir_hier_path`. JSON receipts list physical paths and checksums. A partial
publication after an I/O failure is safe to retry; existing matching files are
not replaced. This is not BLN licence clearance or sticky-client acceptance.
Verify anonymous HTTP GET bytes and headers separately; proving client reuse
still requires two units on the same compatible attached host.
In a fresh bridge job, the wrapper writes only `download_dir` and the committed
template's fanout to the container-local config target. It refuses an existing
local config or unexpected shared config symlink, never replaces that symlink,
and never fetches credentials. The server replica's own config is unaffected.

A unit left with a `submitting` marker is handled as for the assessment.

### Host trust for `usnm_ocr`

`usnm_ocr` validates with `sample_trivial_validator`, which accepts any
well-formed result. So `collect` can gate publication on the gold check (#468):
a result counts only when the host that returned it has a gold report for the
same engine image (the worker image digest) on the exact calibrated challenge,
and the page's source language and model profile match the declared calibration,
with every crop scored, no skips, no crop with character edits at least its
reference length, and overall CER at or below a threshold. The per-crop guard
rejects complete loss (including empty output) even when an aggregate hides it;
it is not a guarantee against subtler OCR errors. There is **no default
scientific threshold**: calibrate on the first campaign for the locked image,
bundle, languages and scoring configuration;
record that evidence and pass `--max-cer` (bridge: `GOLD_MAX_CER`) and
`--manifest-sha256` (bridge: `GOLD_MANIFEST_SHA256`), `--language`
(`GOLD_LANGUAGE`) and `--profile` (`GOLD_PROFILE`) explicitly.
Gold currently supports PSM 3 / 300 DPI only; the trust artifact records these,
and different page configurations are refused. The collector records observed
profiles and scoring configuration, and every scored crop in an eligible report must use the declared profile.
A newer valid canonical report with a different/mixed profile or failed crop
quality revokes earlier eligibility.
The operator must establish the declared
source-language scope from the pinned challenge and calibration evidence;
observing an English model profile alone does not establish an English source.
One artifact covers one language/profile/configuration, not every language
supported by its image.

An earlier MVP's 0.10 was not calibrated. Trust format v4 deliberately rejects
old unscoped v1/v2/v3 lists for publication. Gold onboarding logs those legacy
policies and treats every host as untrusted, so it can issue new challenges.
Rerun the updated normal `gold collect` against the original validated canonical
reports to derive counts-only crop-loss and scope metadata, then rebuild trust
with the calibrated manifest and scope; do not edit or relabel legacy lists.
Old tables without those fields and mixed-challenge tables fail explicitly.
Deploying this guard neither chooses a threshold nor promotes hosts.
Only validated canonical quality reports establish or revoke eligibility.
Terminal attempts, including an engine failing every crop without producing
a valid report, are counted separately by the collector; their spool markers
do not carry authoritative host/time attribution and do not automatically revoke
an earlier passing host. Do not infer that attribution from file timestamps or
claim that this quality signal detects terminal failures or malicious hosts.
The handoff spool shares one global `gold/trust.json`; passing a separate local
file does not isolate its policy to one job. Both native and handoff publication,
and independent-host spot replay/comparison, enforce the job scope. English-only
evidence never implicitly authorizes French, Japanese, another model profile or
another page segmentation/DPI configuration.

```sh
# Build the trust list from one or more campaigns' reports.
usnm-boinc gold trust --reports <share>/gold/v1-tess550/out/reports.jsonl \
  [--reports ...] --max-cer CALIBRATED_THRESHOLD \
  --manifest-sha256 CALIBRATED_MANIFEST_SHA256 \
  --language CALIBRATED_LANGUAGE --profile CALIBRATED_PROFILE --out <share>/gold/trust.json
# The canonical result's host, from the project database:
#   select r.hostid from workunit w join result r on r.id = w.canonical_resultid
#   where w.name = 'usnm_JOB_ID';
usnm-boinc collect --root ROOT --spool SPOOL --job JOB_ID \
  --curated-url CURATED_STORE --out-url OCR_OUT_STORE \
  --trust <share>/gold/trust.json --host-id HOST_ID
```

With `--trust`, a result from an untrusted host, or with no `--host-id`, is held
back: `collect` copies its spool files and a `reason.json` to
`<root>/quarantine/<job>/`, publishes nothing, leaves the job's stage as it was
and exits non-zero. Once the host passes a gold unit, rebuild `trust.json` and
run `collect` again. Without `--trust`, `collect` publishes as before;
production collection must use the existing hand-off `collect` step: it always
requires trust and a canonical host for successful work, refusing a missing list
or host. `spool-out` already queries
`workunit.canonical_resultid -> result.hostid`; the hand-off rejects an operator
override conflicting with that host. Only terminal failure markers bypass trust
to mark failure, never to publish. The host id in gold remains the client's
own claim, so this is a quality signal for trusted volunteers, not an identity
check.

Trust is keyed by the **full immutable worker digest**, not a tag, base image or
engine version. The latest report's `completed_at` per host and image wins,
regardless of file order; later failures revoke trust and same-time conflicts
fail closed. Missing CER never grants trust. These timestamps are also client
claims, not protection against malicious volunteers.

`gold submit --target-host HOST` (bridge: `GOLD_TARGET_HOST`) provides a
targeted, write-ahead campaign; its persisted campaign nonce and host
participate in the validated job identity, so fresh roots and hosts cannot
collide in workunit or spool names and retries keep the same identity. Targeted
jobs use protocol v2 and require a **newly built and released worker containing
this support**; do not target old worker images that only understand v1.
Untargeted v1 job bytes and identities remain compatible. Use a fresh campaign
root per host and image. It refuses until the project's active XML config
explicitly has `<enable_assignment>1</enable_assignment>`. This PR does **not**
enable assignment on production: first verify scheduler and assignment behavior
in dev and calibrate the first campaign. Live acceptance of discovery and
issuance for new hosts, canonical host-query acceptance, and periodic spot-check
activation remain rollout blockers; no automatic publication or scheduler
configuration has been deployed. The OFF-by-default [host-trust follow-up
controller](boinc-host-trust-followups.md) now provides bounded automatic host
discovery and first-gold issuance commands, and sampled independent-host OCR
rerun and comparison commands. Dispatch requires an explicit `--execute`; no
timer, trust promotion or assignment enablement is installed.

### Build the gold bundle

The bundle tar can be built inside the VNet, so nothing passes through an
operator's machine. The job needs a bridge image that has the step, so build it first
(`scripts/build-boinc-images.sh`, which rewrites `images.bridge`). Then set
`"goldBundle": true` in `contrib-compute/infra/boinc/env/<env>.json` in the same
commit as the new digest; it is false until then. After
`scripts/provision-boinc.sh`, the stack has the gold bundle job (`usnm-boinc-gold-caj-<env>`, the stack's
output `goldBundleJobName`). It runs the bridge image's `gold-bundle` step with
`GOLD_CAMPAIGN=v1-tess550` set in the job itself, so it needs no override:

```sh
az containerapp job start --subscription <subscription> --resource-group usnm-boinc-rg-prod \
  --name usnm-boinc-gold-caj-prod --query name -o tsv
```

The step (`contrib-compute/images/bridge/gold-bundle.sh`):

1. runs `gold.py public-fetch` for `bln600`, `overproof-ca` (`--pages 20`),
   `newseye-bnf`, `newseye-nlf-fi` and `newseye-nlf-sv` (`--subset validation`)
   and `nzz` into `<share>/gold/build-v1/data`. gold.py keeps its own pacing:
   one request at a time, a second apart, and loc.gov paced as `locfetch` paces
   it. A file that's already downloaded isn't fetched again, so a rerun resumes;
2. runs `gold.py public-convert` for each, with the flags of the 13 October 2026
   runs ([ocr-scoring-guide.md](ocr-scoring-guide.md#fetch-and-convert)): `--count 600 --per-page 1`
   for BLN600, `--count 600 --per-page 50` for Overproof, `--count 60` for the others;
3. runs `usnm-ocr-survey bundle-build --version 1 --seed 457 --per-source 30` with
   BLN600's zip as `--original`, writing its lock beside the bundle, then
   `bundle-verify` on the tar;
4. stops, and installs nothing, unless that lock equals the committed
   `fixtures/ocr-gold/bundle-v1.lock.json`, the tar's sha256 is
   `42520de42c7597bef4ea0ef3508797bfb7db129dd09c31e3d5a2236222be9183` and the
   manifest's is `150e20d75a5357caeeb6eb63911a3d94348e617879f7b0de16c7b1a54670fe3a`;
5. writes `<share>/gold/v1-tess550/bundle.tar` (beside it first, then renamed).
   A bundle already there with that sha256 ends the run at once; one with another
   sha256 is refused. One run at a time: a second run stops on the lock
   `<share>/gold/.build-v1.lock`.

The image carries Pillow 11.3.0 (the CI's version, from its manylinux wheel)
and bsdtar for BLN600's AES zip. The crops are JPEG re-encodes, so a different
Pillow or libjpeg could change the tar's bytes, and step 4 would catch that.
UNVERIFIED: that this build reproduces the lock byte for byte (it was built on
another machine), and how long the run takes on the share. The job allows six
hours.

Then run `gold-prepare` as above.

### Live acceptance record and remaining checks

The production `v1-tess550` campaign completed on 14 October 2026. `gold-collect`
read the batch-0 assimilator spool and wrote six report rows and 540 crop rows.
Hosts 2 and 3 returned four and two valid results respectively; each scored 90
crops and skipped 52 for unavailable language models. Both hosts reported CER
0.0905 and WER 0.2366. The successful reports establish an end-to-end path through
the stock client, worker and server validator, but do not prove every transport
or runtime detail below.

| Acceptance check                                                                             | Evidence and status                                                                                                                                                                                                                                                                                            |
| -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Legacy campaign: publisher fetch, BLN600 MD5 and bridge access to figshare / tile.loc.gov    | **Passed end to end on 14 October:** the campaign prepared publisher inputs and valid tasks scored BLN600 and LoC-backed samples. This predates project mirroring; it is not mirror acceptance evidence.                                                                                                       |
| Wrapper slot contains `init_data.xml`                                                        | **Passed end to end:** accepted reports carried host IDs 2 and 3.                                                                                                                                                                                                                                              |
| Validator finds the expected bundle data                                                     | **Passed in production; mechanism unverified:** `script_validator` accepted all six reports. The run did not establish whether `PROJECT_DIR` and `PROJECT` were present or the fallback/`USNM_GOLD_EXPECTED` path was used.                                                                                    |
| `<sticky/>` / `<no_delete/>` survive `create_work --remote_file`; a second unit reuses files | **Partly verified:** production WUs 1 and 4 retain both flags in their database XML (16 October 2026). No transfer log or before/after cache record established that later units avoided downloads.                                                                                                            |
| Current ordinary-file mirror path                                                            | **Not deployed or live-tested:** `gold submit` verifies originals against `prepared.json` and stages them as ordinary project downloads; repeat-unit cache behavior still needs client transfer evidence.                                                                                                      |
| Real-host duration and resource use                                                          | **Partly measured:** two Gold results in the related live acceptance recorded 163.618 s elapsed each; the host's cgroup limits were 1 CPU, 1.5 GiB memory, no swap and 64 PIDs. Peak memory and disk consumption were not measured, so `rsc_fpops_est=5e12` remains a guess.                                   |
| Volunteer guide on a clean Debian/Ubuntu host                                                | **Debian host readiness passed:** clean Debian 13 x86-64 full VM, official BOINC 8.2.13, Podman 5.4.2 and AppArmor enabled; setup and restart passed mandatory slot, isolation, kernel-limit and pause/resume probes (16 October 2026). No project attached; new task acceptance and Ubuntu remain unverified. |

For a repeatable acceptance record, retain per-host and per-workunit evidence
for these same checks: result upload and validation; the result's non-null host
ID matched to the server's canonical host; client transfer and cache evidence
across two units on one host; mirrored-file hashes against `prepared.json`;
validator success and how it resolved its expected-data directory;
`gold-prepare` completion; task elapsed time, peak memory and disk; and the
clean-host guide's command output. Do not infer cache reuse from multiple
successful tasks or infer peak resource use from cgroup limits. Keep the
production issuance and campaign separate from this documentation checklist.

Volunteers' steps are in [boinc-volunteer.md](boinc-volunteer.md).

### Engine routing

Production selection is shared through `usnm_ocr_contract::routing::EngineKind`
and `EngineRoute { kind, profile }`. `EngineKind::for_language` chooses
`NdlocrLite` (`ndlocr-lite`) for `jpn` and `Tesseract` for every other language.
`usnm_ocr_worker::engine::candidates` exposes engine and profile preferences
(Japanese: `default`); `route(language, installed)` resolves only capabilities
of a Tesseract node, returning an `EngineRoute` or `None`. Sampling's existing
`no_model` counter includes an unavailable selected engine, not just missing
Tesseract weights. There is no Japanese fallback to Tesseract.

`boinc::Engine.kind` carries that selection through job identity, validation,
result matching and the `ocr_engine` family. It defaults to Tesseract and is
omitted when serializing Tesseract jobs, preserving existing JSON, identity
hashes and ledger tags. Tesseract-only BOINC workers reject other kinds before
checking models or starting Tesseract. The dedicated NDLOCR-Lite worker
validates `job.engine.kind`, its pinned image, version, profile and model
digest; use `usnm-boinc ndlocr stage/submit/collect`, not the native Tesseract
exporter. For on-prem contract adapters, the hello's family must match the
selected engine and its models must pass the catalog's existing production gate.
The historical survey-only NDLOCR-Lite entry remains separate from the pinned
production entry. Dashboard routes retain the engine and version as well as the
profile.
