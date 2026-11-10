# The server and the apps

How app versions, plan classes and daemons go onto a stock BOINC project, the single-page submit and collect commands, and the server settings that keep scans and retire apps. The project's own server is described in [BOINC server on-prem](onprem.md).

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
upstream sample is supplied in `boinc/apps/plan_class_spec.xml`: copy it into a
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

Register the application in the BOINC admin interface (`boinc/project/project.xml`). Install a container app candidate using a trusted stock wrapper and the pullable worker image digest, or a native app candidate using `--kind native --binary`:

```sh
# Container app version (apps/<app>/<version>/<platform>__<plan_class>):
python3 boinc/scripts/install.py --project /srv/boinc/newspapers \
  --wrapper /path/to/docker_wrapper \
  --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST --version 1.0 \
  --platform x86_64-pc-linux-gnu --plan-class docker

# Native app version (apps/usnm_quality/<version>/<platform>, or <platform>__<plan_class> for usnm_qwen):
python3 boinc/scripts/install.py --project /srv/boinc/newspapers \
  --app usnm_quality --kind native --binary /path/to/usnm_quality_1.1_x86_64-pc-linux-gnu \
  --platform x86_64-pc-linux-gnu --version 1.1
```

`boinc/project/plan_class_spec.xml` defines both the container plan classes (`docker`, `docker_lines` (Cherokee line jobs, batch 1621 only; `docs/boinc/apps/ocr-bundles.md`), `docker_mt`, `docker_mt_2b`, `docker_nvidia_cuda`, `docker_nvidia_cuda_8b`, `docker_nvidia_train`, `docker_intel_vulkan`, `docker_intel_opencl_4b`) and the native `usnm_qwen` plan classes (`mt`, `apple_gpu_8b`, `nvidia_cuda_8b`, `nvidia_cuda_2b`, `nvidia_vulkan_8b`, `nvidia_vulkan_6gb_8b`, `nvidia_vulkan_2b`, `intel_vulkan_8b`, `amd_vulkan_8b`). For native `usnm_qwen`, `boinc/apps/qwen/llama.lock` pins the upstream `llama.cpp` release tag and per-platform archive SHA-256 digests, and the GGUF model templates use `<sticky/>` and `<no_delete/>` so volunteer clients cache models across workunits (`client/client_types.cpp`) and `file_deleter` (`sched/file_deleter.cpp`) never deletes shared model files from `download/`.

The installer refuses an existing version directory. It does not alter running
server configuration. Sign and publish its files using BOINC's normal
code-signing process and `bin/update_versions`. Merge `boinc/apps/daemons.xml`
into the existing `config.xml` daemons section and restart the project daemons.
This file is a fragment, not a replacement list: retain the stock feeder,
transitioner, file_deleter (with `--preserve_wu_files`, see [Scans are never
deleted](#scans-are-never-deleted)), db_purge and other standard daemons.

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

### Optional local page hosting (#783)

The native `usnm-boinc submit` command alone accepts
`--download-base-url https://downloads.example.org/ocr-pages/` for an
independently published origin. The BOINC server's submitting process does not need an
origin mount: the trusted local producer must first publish the exact staged
page as `<origin-root>/<sha256>`, atomically and immutably. Default submission is
unchanged. This does **not** migrate gold, Qwen, quality, Surya, NDLOCR, model,
application, upload or result files; the scheduler, application and database
run on the on-prem host.

Two-host flow (no origin filesystem mount on the BOINC host):

```sh
# Trusted local producer: receipt records publication, NOT HTTPS readiness.
usnm-boinc prepare-download --page /srv/local/staged-page \
  --download-root /srv/usnm/immutable-ocr-pages
# BOINC project server (on-prem): retains the existing verified staged job/request.
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
upload bandwidth. No public HTTPS ingress for such an origin exists (UNVERIFIED
on 10 November 2026; nothing in the repository configures one), so this stays
off in prod. Do not relax the HTTPS probe to enable it.

`boinc/scripts/file-host.conf` is an nginx site for a dedicated, isolated origin
container with a **read-only** bind mount at `/srv/usnm-boinc-downloads`,
published by a separate trusted writer. It listens on port 8080 and is not a
public HTTPS endpoint: put a separately configured TLS ingress in front that
preserves the path. It serves only `GET` and `HEAD` of a lowercase
64-character SHA-256 filename, with no listings, uploads or symlinks, single
byte ranges, at most four concurrent transfers per address and sixteen overall,
each limited to 2 MiB/s. Never mount the scan cache, project share, result
quarantine or credentials into it, and never expose the LAN scan server, which
also takes deletes and manifest writes. Before a real workunit uses the origin,
fetch the exact URL from outside the LAN over HTTPS and check its size,
SHA-256 and a ranged request, then watch a real volunteer transfer and result
return. App bundles, model files and result uploads need their own signed,
authenticated paths; this origin doesn't cover them.

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

## Scans are never deleted

From 8 November 2026 no scan in `download/` is deleted, and a file fetched from
LoC or another external host is not fetched again while a local copy exists.
Two settings carry this on the server:

- `file_deleter` runs as `file_deleter -d 2 --preserve_wu_files`
  (`boinc/project/config.xml.template`). For each finished workunit it sets
  `file_delete_state` to done in the database without removing the input files
  from `download/` (`sched/file_deleter.cpp` at 22bbebe0, the
  `preserve_wu_files` branch). Output files under `upload/` are still deleted, and
  `db_purge` still purges the rows. Because the flag is read by the daemon, not
  from each workunit, it also covers workunits created before the templates
  below changed, whose `xml_doc` has no `<no_delete/>`.
- Every input template (`boinc/apps/*_in`) marks every `file_info`
  `<no_delete/>`, which `file_deleter` honours per file
  (`sched/file_deleter.cpp`). This is a second guard: a `file_deleter`
  started without the flag still skips files of workunits created from these
  templates. It does not change what volunteers' clients do with the files;
  `<sticky/>` does that, and the page files stay non-sticky.

`antique_file_deleter` stays disabled. `boinc/project/test_config.py`
(`NoScanDeletion`) fails if either guard is dropped.

`config.xml` is rendered from the template on every container start, so the live
server gets the flag when its container restarts on an image built after this
change. The input templates in `<project>/templates/` are copied there when an
app version is installed (`boinc/scripts/install.py`) or by the bridge steps that
copy their own templates before submitting; otherwise copy `boinc/apps/*_in` in
by hand after backing up the old ones. Until both are in place,
keep `file_deleter` off by hand. After the restart, `pgrep -a file_deleter` in
the container should print the command with `--preserve_wu_files`.

To free space later, delete only output files or move whole scan trees to other
kept storage; do not remove `--preserve_wu_files`.

## Retiring an app

To stop an app's work without losing anything, deprecate it rather than delete it.
Save its `app` and `app_version` rows first, and coordinate a feeder-only refresh:
the `--allapps` slot map is fixed at feeder startup, so touching `reread_db` alone
does **not** reallocate slots after an app is deprecated or restored. Set
`deprecated=1` on the app and its versions, then touch `reread_db` and refresh
the feeder at the coordinated boundary described in
[Feeder visibility](../apps/app-release.md#feeder-visibility),
following [Feeder-only refresh](feeder-refresh.md).
Results already sent can still be reported, and workunits and signed versions stay.
Rollback restores the saved `deprecated` values and needs the same reload and
coordinated feeder refresh. Before you start, check that no controller plan, timer
or feed still stages work for the app.
`usnm_surya` was retired this way on 2 November 2026; its
[retirement notes](../apps/surya.md#retirement-2-november-2026) list the steps and the
state before and after.

`usnm_assess`, `usnm_iris` and `usnm_qwen_train` were retired the same way later on
2 November 2026 (#529), when two app names were also corrected:

- Before: no result in progress. Every `usnm_iris` and `usnm_qwen_train` version was
  already deprecated, and `usnm_assess` had none and never had a workunit. Iris's last
  workunit was from 18 October, training's from 15 October. No timer, feed or controller
  plan staged any of them. `usnm_qwen`'s `intel_vulkan_8b` class replaced the Iris pilot.
- `usnm_qwen_train`'s two unsent results were cancelled with `bin/cancel_jobs --name`.
- `deprecated=1` on the three apps (`project.xml` says the same), then `reread_db`.
- New names for `usnm_quality` (tasks are bundles of about 200 pages that measure the scan
  and compare texts) and `usnm_qwen` (GPU versions only, one page per task). `bin/xadd`
  updates a changed `user_friendly_name`, so the branch's `project.xml` was copied into the
  project and `bin/xadd` run, as the `register-apps` step does.
- The six validator and assimilator daemons of the three apps keep running with nothing to
  do, and so do Surya's two. They come from `config.xml`, rendered at container start (`render_config` in
  `boinc/images/server/lib.sh`), and changing the live copy would change the
  `config.xml` SHA-256 the Qwen auto-cohort controller pins. #1449 drops all eight from
  `render_config`. They leave the status page when the server runs an image built from it. The
  same image also sets `maintenance_delay` to 1200 seconds ([Client retry
  behaviour](feeder-refresh.md#client-retry-behaviour), #1462). That
  restart changes the pin (`CONFIG_SHA256` in `/etc/usnm-boinc/qwen-cohort-auto.env`) once for both, so
  disable `usnm-qwen-cohort-auto.timer` before it, and update the pin after it
  ([Intel and NVIDIA GPUs](../apps/intel-gpu.md)).

The state before is saved under `/project/ops/app-tidy-20261102/` with the script that made
the change. Rollback, in the server container as `boincadm`: run
`php /project/ops/app-tidy-20261102/app_tidy.php --project /project/usnmocr --journal /project/ops/app-tidy-20261102 --project-xml /project/ops/app-tidy-20261102/project.xml.new --rollback`.
It restores the app names and `deprecated` flags, the two workunits' `error_mask` and their
results' state, and the previous `project.xml`, then touches `reread_db`. That trigger
does not itself refresh the slot map; coordinate the feeder-only boundary after a rollback
([Feeder-only refresh](feeder-refresh.md#rollback)).

Retiring an app no longer leaves its daemons running. Restoring `usnm_surya`, `usnm_assess`,
`usnm_iris` or `usnm_qwen_train` now also needs its daemons and validator links back in
`render_config`, and a new server image.
