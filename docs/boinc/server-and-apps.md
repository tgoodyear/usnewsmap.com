# The server and the apps

Part of the [BOINC batch pilot guide](README.md).

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

## Moving the project share

The project directory (`/project`: uploads, downloads, staging, results) is one Azure Files share mounted by the server and every job. On the pay-as-you-go Standard account (`usnmboincst…`, TransactionOptimized) it cost about $21 in operations in four days of October 2026: 51M operations, 83% of them open, close and stat. Capacity was $0.61. A provisioned v2 SSD share (`usnmboincv2st…`, `PremiumV2_LRS`) bills only for its provisioned size, about $0.10/GiB a month with 3,000 + 1 IOPS per GiB included, and nothing per operation (#917).

`projectShare` in `contrib-compute/infra/boinc/env/<env>.json` moves the share in stages, each a pull request and a `scripts/provision-boinc.sh` run (claimed on #529):

1. **`copy`.** Creates the provisioned account, share and private endpoint, plus the share copy job `usnm-boinc-sharecopy-caj-<env>` and its own identity. That identity has Storage File Data Privileged Reader on the old account, Storage File Data Privileged Contributor on the new one, and pull rights on the registry.
   - The job runs AzCopy (pinned by version and SHA-256 in `main.bicep`): `azcopy sync https://<old account>.file.core.windows.net/project https://<new account>.file.core.windows.net/project --delete-destination=true`, as the job's identity, both over their private endpoints.
   - Over REST, BOINC's symlinks are the small `mfsymlinks` files that store them, so they copy byte for byte, and the new share's normal mount reads them as symlinks again. Listing the old share over the SMB mount instead took 107 to 156 minutes a pass.
   - Start it (`az containerapp job start`) while BOINC runs; repeat runs send only what changed. Files being written at that moment (logs, `run_state`, the web cache) fail and the execution shows Failed; that's expected while live.
   - The server isn't touched in this stage.
2. **Cutover.** Make the tree still:
   - stop the quality wave job and the batch controller in CT 211, and wait for running bridge executions;
   - as `boincadm` in `/project/usnmocr`: `touch stop_maintenance`, `bin/stop`, then `touch stop_sched stop_upload`. `stop_maintenance` keeps the server image's startup wrapper (`boinc-daemons.sh`) from running `bin/start`, which would reopen the scheduler on the new revision before it is checked. The scheduler then answers "temporarily shut down for maintenance" and the upload handler refuses uploads with a transient error that clients retry. Apache keeps running, so the liveness probe (`get_project_config.php`) stays healthy;
   - wait a minute for requests already in flight to finish.

   Then start the copy job once more and wait for it to succeed: every file copies while nothing writes. Set **`provisioned`** and provision with `BOINC_MAINTENANCE=1`, which lets the final health check accept the closed scheduler. `provision-boinc.sh` refuses this while a copy is running, or when the latest copy didn't succeed within two hours. After reading a pass's failed transfers, `BOINC_COPY_ACCEPTED=<execution name>` accepts that pass when the only failures are files the server regenerates on start (`run_state_*.xml`, `html/cache`).
   - The environment storage for `/project` is `project-v2` on the new account (`project` on the old one), so the change produces a new server revision and no replica keeps the old mount.
   - Check that the new revision is the only one running and that `/project` on it is the new account (`grep " /project " /proc/mounts`). Then, in `/project/usnmocr`, remove `stop_maintenance`, `stop_sched` and `stop_upload`, and `bin/start` as `boincadm` (the wrapper also starts it within a minute). A server image older than the `stop_maintenance` check runs `bin/start` as it boots, so it reopens the scheduler on its own; the October 2026 cutover ran on such an image, and only `stop_upload` was left to remove. In the October 2026 cutover the outage was 35 minutes, 24 of them the final copy.
   - Run `provision-boinc.sh` once more without `BOINC_MAINTENANCE`: the strict health check now passes, and nothing else changes.
   - Start everything else again, re-apply GPU admission, and relaunch the quality wave (`launch-corpus`).

3. **`provisioned-final`**, after a few days without trouble: the stack deletes the old account and its endpoint.

To roll back before stage 3, stop the server the same way (`stop_maintenance`, `bin/stop`, `stop_sched`, `stop_upload`), copy back what was written to the provisioned share after the cutover, set `standard` and provision: the storage name changes back to `project`, so again a new revision replaces the running one. Then remove the three files and `bin/start`.
