# Contribute compute to usnewsmap OCR (BOINC)

Run OCR and OCR quality checks on historical newspaper pages with BOINC on Linux, macOS or Windows. The project checks every returned result before it uses it in search or in quality audits.

Status: a pilot. The project server is up, and work goes out in small batches that the project picks by hand, so an attached computer can sit idle for a while. Steps not yet tried on a real volunteer host are marked UNVERIFIED. Operator documentation: [BOINC](../README.md) and [OCR quality apps](../apps/quality.md).

## Why volunteer?

Searchable newspapers help genealogists find family names, historians trace
stories and students study changes in language and public discussion.
[US News Map](https://usnewsmap.com/) maps where and when search matches were
printed, using scans from
[Chronicling America](https://www.loc.gov/collections/chronicling-america/),
the Library of Congress and National Endowment for the Humanities newspaper
collection.

Search depends on optical character recognition (OCR), which turns images into
text. Faded ink, damaged paper, unusual typefaces and complex columns cause
errors that hide names and passages from search. Your computer transcribes pages
or checks existing OCR. Some tasks test methods such as Qwen on image crops.
A model can produce fluent text that misreads the scan, so outputs are checked, and not every task
changes the public index.

### What you contribute and what you control

BOINC downloads tasks and applications, runs them, and returns results and
diagnostics automatically. You get production and experimental tasks, Qwen
included, through the same account and client, under the same CPU/GPU and
resource choices; there is no separate opt-in. The project decides which
campaigns run.

Processing uses electricity, produces heat and fan noise, and uses disk space
and bandwidth. Models can mean several gigabytes of downloads. Use only a
computer you own or have permission to use. BOINC lets you limit resources, run
only while idle, pause, or select **No new tasks** while existing work finishes.
Local computing preferences affect other BOINC projects too.

## Get started

The pilot may have no work, or no app, for your device yet. You can install BOINC and attach anyway; it waits for work.

| Platform                                | Start with                                    | Helper script                                                                                                    |
| --------------------------------------- | --------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| Linux, x86-64 or 64-bit ARM             | [Linux quick start](linux.md)                 | None for the native apps. `boinc/scripts/setup_volunteer.py` sets up the [container apps](#linux-container-apps) |
| macOS, Apple silicon or Intel           | [macOS quick start](macos.md)                 | `boinc/scripts/setup_mac_volunteer.py`                                                                           |
| Windows x64                             | [Windows quick start](windows.md)             | `boinc/scripts/setup_windows_volunteer.ps1`                                                                      |
| Proxmox VE host, as an unprivileged LXC | [Proxmox VE container](#proxmox-ve-container) | `boinc/scripts/pve-boinc-ct.sh`                                                                                  |

The helpers are in this repository: download it as a ZIP from <https://github.com/tgoodyear/usnewsmap.com> (**Code**, then **Download ZIP**) or `git clone` it. The Linux, macOS and Windows helpers print a preview and change nothing until you add `--apply` (`-Apply` on Windows).

## Join the project

The master URL is `https://boinc.usnewsmap.com/`. It is fixed for the life of the project.

1. Create an account at <https://boinc.usnewsmap.com/>, or sign in to yours. Account creation is open, with no invitation code (checked 14 October 2026). In **Project preferences** choose whether the project may use your CPU, your GPU or both. Leave the GPU off if you don't want it used.
2. In BOINC Manager, choose **Tools → Add project**, enter `https://boinc.usnewsmap.com/` and sign in with the same account. Type your password only in Manager or on the project website, never into a script, a command line or a support message. Don't share an account or key with another volunteer.
3. If the computer is already attached, don't remove and re-add the project. After you change preferences, select the project and click **Update**.

A headless Linux host attaches from the command line instead: see [Command-line attach](#command-line-attach).

## Set limits

In Manager, **Options → Computing preferences**: compute only while the computer is idle and not on battery, and set CPU, memory and disk limits you are comfortable with. They apply to every BOINC project on the computer. On a headless host, set the same limits under **Computing preferences** on the project website (**Project preferences** only choose CPU and GPU work). Website limits apply to every host on your account; limits set locally in Manager take precedence.

BOINC's disk limit must leave room for the apps you run: 2 GiB for `usnm_quality` and 18 GiB for the Qwen GPU app ([Disk](#disk)).

## Check that it works

```sh
boinccmd --get_project_status   # lists https://boinc.usnewsmap.com/
boinccmd --get_tasks            # this project's tasks and their state
boinccmd --get_messages 0       # the event log
```

In Manager the same information is in the **Projects** and **Tasks** tabs and the **Event Log** (**Tools → Event Log**). A **Scheduler request completed** line for usnewsmap means the computer is joined and waiting for work; no tasks is normal during the pilot. A task that downloads, runs and uploads is a success, and its validation shows under **Tasks** on your account page. For GPU work the event log names the GPU at startup; without that line the computer gets CPU work only.

If `boinccmd` is refused, run it from the BOINC data directory, where the client's GUI RPC password file is; never copy that password into a command.

The **Tasks** link on your account page lists your task history and checks that the tasks are yours. **Privacy limitation:** BOINC also accepts a `hostid` on the results page without checking that the signed-in user owns that host, so another signed-in volunteer with a host ID can view its task history.

## Pause, stop or leave

- **Pause:** Manager's **Activity** menu, or select the project and click **Suspend** (`boinccmd --project https://boinc.usnewsmap.com/ suspend`).
- **Stop taking work:** select the project and click **No new tasks** (`... nomorework`). Running tasks finish and report.
- **Leave:** select the project and click **Remove** (`... detach`). The client stops the project's tasks and deletes its files, sticky files included. To delete your account too, use the project website.

BOINC downloads signed apps and models itself and returns results and diagnostics. Never copy in executables or models, create `app_info.xml`, or delete sticky model files to change what you get. BOINC reports failed tasks to the project, so leave the report alone. You don't need to collect log files, and nobody from the project needs your account key.

## What you will run

Native apps run directly on the host. Container apps run a pinned OCI worker image, named in the app version, under Podman (or Docker).

| App            | Runtime                                                                                                                                                                    | What a task does                                                                                                                                               | Downloads                                                                     | Returns                                               |
| -------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- | ----------------------------------------------------- |
| `usnm_quality` | Native CPU on all five platforms, or container (`docker`)                                                                                                                  | Measures a page's scan and compares its existing OCR readings: estimated accuracy, completeness, findability and scan features ([quality](../apps/quality.md)) | `page.jpg`, `loc.xml`, `texts.json`, `quality.json` and a sticky `places.txt` | `result.json`, `confusions.json`, `diag.txt`          |
| `usnm_qwen`    | Native GPU: `nvidia_vulkan_8b`, `nvidia_vulkan_6gb_8b`, `amd_vulkan_8b` and `intel_vulkan_8b` on Linux x86-64, `amd_vulkan_8b` on Windows, `apple_gpu_8b` on Apple silicon | Transcribes word crops of a page with Qwen3-VL through `llama-server` ([quality](../apps/quality.md))                                                          | `page.jpg`, `qwen.json`, and the sticky GGUF model and projector              | `readings.json`, `result.json`, `diag.txt`            |
| `usnm_ocr`     | Container (`docker`)                                                                                                                                                       | OCR of a newspaper page with Tesseract 5.5.0 and the project's model lock                                                                                      | The page image and a job file                                                 | `text.txt`, ALTO XML (`layout.xml`) and `result.json` |
| `usnm_gold`    | Container (`docker`)                                                                                                                                                       | Scores your engine on public hand-transcribed text ([gold check](#the-gold-check))                                                                             | The gold bundle and originals                                                 | `gold-result.json`, counts only                       |
| `usnm_ndlocr`  | Container, CPU                                                                                                                                                             | Japanese OCR with NDLOCR-Lite ([NDLOCR-Lite](../apps/ndlocr-lite.md))                                                                                          | The page image and `ndlocr.json`                                              | `text.txt`, `layout.xml`, `result.json`               |

The [app versions table](../apps/quality.md#app-versions-and-plan-classes) has each GPU class's requirements: from 5,500 MB of GPU memory (`nvidia_vulkan_6gb_8b`) to 8,192 MB (`apple_gpu_8b`, unified memory), most classes 7,000 MB, and two free CPU cores per task. The native Qwen CPU class (`mt`), the native CUDA classes and the Qwen container versions have had no live version since 2 November 2026. `usnm_assess`, `usnm_surya`, `usnm_iris` and `usnm_qwen_train` were retired on 2 November 2026 and send no work.

The project uses the OCR text from `usnm_ocr`. It is published as ALTO and Parquet and goes through the same import and release as the project's other OCR.

Some tasks hold several pages: a `usnm_quality` task up to 500, a `usnm_qwen` task up to 32. The apps save a checkpoint in the task's slot after each page (`usnm_qwen` after each crop), so a task BOINC stops and restarts resumes where it left off.

### The gold check

`usnm_gold` uses bundle version 1 ([OCR gold set](../../ocr/gold-set.md#volunteer-bundle)):

- `bundle.tar`, 9.6 MB, from the project server: crops and transcriptions the licenses allow us to share, with their license files.
- The BLN600 zip, 68 MB, and 18 page images, served by the project from its verified mirror. Your client checks each file and keeps it as a sticky input.

By taking gold tasks you use these datasets under their licenses: BLN600 under CC BY-NC-ND 4.0; Overproof transcriptions under CC BY-NC-SA 4.0, with Library of Congress page images; NewsEye under CC BY 4.0; NZZ under the license in `licenses/nzz/` in the bundle. Each source's license and attribution files are in `licenses/<source>/` in the bundle. The NC licenses allow non-commercial use only. Nothing derived from BLN600 is shared: your host cuts the crops itself, and nothing it cuts leaves the machine.

`gold-result.json` holds, per crop: reference characters, character edits, reference words, word edits, the model profile and the time; then a summary per source, the crops it skipped and why, the engine version, the bundle version and manifest checksum, the job id and your BOINC host id. It holds no page text and no transcription. Counts are enough to compare engines across hosts, and the licenses (BLN600's No-Derivatives terms in particular) rule out sending anything else. Crops in a language the worker image has no model for are listed as skipped.

The OCR app's validator accepts any well-formed result, so agreement comes from the gold check: the [host-trust collector](../server/gold-check.md#host-trust-for-usnm_ocr) publishes a host's `usnm_ocr` text only under a calibrated gold policy for its exact worker image, source language, model profile, PSM and DPI, and holds unsupported jobs in quarantine. It is a quality signal for trusted volunteers; it doesn't verify identity or protect against a malicious host. The [live acceptance record](../server/gold-check.md#live-acceptance-record-and-remaining-checks) lists what has and hasn't been confirmed on real hosts, sticky-file reuse included.

## Resources, network and privacy

Per container task, from `boinc/apps/job.toml` (`usnm_ocr`, `usnm_gold` and container `usnm_quality`): one CPU, 1536 MiB of memory with no extra swap, at most 64 processes, a read-only root filesystem, a 512 MiB `/tmp`, all capabilities dropped except `DAC_OVERRIDE`, and `--network=none`. The multi-CPU and GPU profiles (`boinc/apps/qwen/job-*.toml`) allow 6 GiB with no extra swap and 1024 processes, with the same isolation. Every container sees only the task's slot directory. Native `usnm_quality` runs single-threaded on the slot's files with no network calls; native `usnm_qwen` starts `llama-server` bound to `127.0.0.1` on an ephemeral port.

| Step                                                        | Who connects                               | To where                                           |
| ----------------------------------------------------------- | ------------------------------------------ | -------------------------------------------------- |
| Scheduler requests, downloads of inputs, uploads of results | The BOINC client                           | `https://boinc.usnewsmap.com/`                     |
| Worker image (container apps only)                          | Podman (or Docker), started by the wrapper | The registry named in the app version              |
| Gold originals                                              | The BOINC client                           | The project's download server                      |
| The task itself                                             | Nobody                                     | No network inside containers; loopback only native |

What leaves your machine: the result files in [What you will run](#what-you-will-run), and what any stock BOINC client sends its project (host description, account details). The project stores your email address, name, IP address and account key. Privacy is on by default, and you can delete your account ([BOINC design](../design.md), Privacy). Web server and scheduler logs keep IP addresses for 30 days.

## Disk

The Qwen workunits ask BOINC for 18 GiB of free disk for the 8B model (`boinc/apps/usnm_qwen_8b_in`) and 12 GiB for the 2B model, which has no live version. The Qwen container versions asked for 32 GiB (2B) and 48 GiB (8B). These bounds cover the model files plus a cold cache, extraction overlap, workspace and reserve, and they apply to warm hosts too. The sticky model set itself is 5.38 GiB for 8B `Q4_K_M` and 2.12 GiB for 2B `Q8_0`, downloaded once into `projects/boinc.usnewsmap.com/` and reused. `usnm_quality` keeps the campaign's sticky `places.txt`, and the gold check about 78 MB of sticky files plus the 18 page images, copied into the slot of each running gold task.

Keep container image storage and extraction scratch on BOINC's filesystem so the client's reports count them; the managed Podman profile does this and refuses work when free disk is short. Don't prune a cache to make room. [Qwen disk admission](../apps/quality.md#qwen-disk-admission-and-cold-cache-acceptance-559) has the exact byte bounds and the shared-model layout.

## Command-line attach

Check that the server is up:

```sh
curl --silent --show-error --fail --output /dev/null --write-out '%{http_code}\n' https://boinc.usnewsmap.com/
# expect 200
```

On a headless host or cluster node, attach with the repository's account-key helper rather than putting a key on the command line. Put your **weak account key** from the account website in a file only you can read (mode `0600`), then preview:

```sh
sudo python3 boinc/scripts/client_account.py \
  --data-dir /var/lib/boinc \
  --expected-user-id YOUR_NUMERIC_USER_ID \
  --expected-user-name 'YOUR_ACCOUNT_NAME' \
  --key-file /secure/path/weak-account-key
```

Review the preview, then repeat with `--apply`. Use the service's real data directory (some packages use `/var/lib/boinc-client`). Run one client per node with its own data directory; on a batch scheduler, run only inside an allocation you are authorized for and stop BOINC when it ends.

`boinccmd --project_attach https://boinc.usnewsmap.com/ ACCOUNT_KEY` also works, but leaves the key in shell history and the process list. After attaching, `boinccmd --get_messages 0` shows `Master file download succeeded` and `Scheduler request completed`. With the container profile, the client also logs `Docker: version ... (podman)` at startup; a missing runtime stops container work only.

On a headless host or in a container, the client may log `Suspending computation - CPU is busy` because it counts the whole machine's load. Set `<suspend_cpu_usage>0</suspend_cpu_usage>` in `global_prefs_override.xml` in the data directory and run `boinccmd --read_global_prefs_override`.

## Linux container apps

The native apps need none of this. The container apps (`usnm_ocr`, `usnm_gold`, `usnm_ndlocr`, container `usnm_quality`) need a dedicated BOINC service with rootless Podman and enough RAM: at least 4 GiB for the 1536 MiB profile. The profile below needs systemd, cgroup v2, Python 3.9 or later, Podman, and a stock BOINC client 8.2.13 or later. The stock wrapper supports Podman directly, so no Docker daemon or `docker` group membership is needed. BOINC still names the plan classes `docker`, `docker_mt` and `docker_nvidia_cuda` and logs `Docker: version ... (podman)`.

A container that starts doesn't prove that its limits or pausing work. On RHEL, Podman accepted CPU and memory flags without a usable cgroup, and SELinux blocked the slot mount. The startup preflight checks kernel limits and slot access, and a failure stops BOINC from starting ([live measurement](../../notes/2026-10-14-boinc-volunteer-preflight.md)).

### Install prerequisites

Get a recent client from [BOINC's Linux packages](https://boinc.berkeley.edu/linux_install.php), then check `boinc --version`. Some distributions ship a client older than 8.2.13. Upstream packages may put `boinc` and `boinccmd` in `/usr/local/bin`; use their full paths if needed.

On Debian or Ubuntu:

```sh
sudo apt-get install podman uidmap dbus-user-session python3 ca-certificates
```

On RHEL:

```sh
sudo dnf install podman shadow-utils python3 ca-certificates policycoreutils-python-utils
```

The BOINC package must give its unprivileged service user a subordinate UID/GID range that overlaps no other user's. The helper checks that one exists; it doesn't create ranges. It also refuses a root BOINC service, cgroup v1, nonstandard or symlinked configuration files, and queued or running work. On an existing client, select **No new tasks** and let its tasks finish and report first.

### Preview and install the profile

From a checkout of this repository:

```sh
python3 boinc/scripts/setup_volunteer.py               # preview
sudo python3 boinc/scripts/setup_volunteer.py --apply  # after reviewing it
```

The helper accepts `x86_64` and `aarch64` hosts, finds the service's existing data directory, and uses the worker image digest released in this checkout's `boinc/infra/boinc/env/prod.json` (`images.worker`). It doesn't move data, and it doesn't attach the project: [join](#join-the-project) in Manager afterwards. The default service is `boinc-client.service`; use `--service` if your package names it differently. `--data-dir` and `--image` override the detected directory and image, but the directory must still be the service's working directory and the image must use its full digest.

The helper keeps account data and unrelated settings, backs up each preference and configuration file it changes, and sets a conservative profile: idle-only processing after five minutes, no battery use, a 50% memory allowance and no extra work buffer. Apps stay in memory while suspended so the wrapper can pause their container. CPU-load suspension is off because the client can count whole-machine load against its share; the idle policy and the per-container CPU limit still apply. GPUs stay off unless you pass `--gpu`.

Installation enables the BOINC user's systemd session at boot and has Podman use it for cgroups. It makes the service's cgroups writable (`ProtectControlGroups=false`); the unprivileged UID still limits which cgroups it can change. It allows read-only traversal of home directories, keeps the data directory writable and keeps the service's other restrictions. The service unsets `INVOCATION_ID`, so Podman starts each container's monitor (conmon) in its own scope in the BOINC user's session rather than in the service's cgroup; a BOINC restart then pauses and resumes running tasks instead of killing their monitors and losing their exit status (seen on 16 October 2026). With SELinux enabled, only the slots directory gets a persistent `container_file_t` label; SELinux stays enforcing. The helper grants no Docker access and doesn't change other users' Podman settings. If the BOINC account already belongs to `docker`, an administrator can remove that unused access.

Each service's private runtime directory is `/run/user/<uid>/usnm-boinc-<service name without .service>`, mode `0700`. Under `/run/user/<uid>/`, Debian's packaged `pasta` AppArmor profile can open Podman's network namespace during stock BOINC's capability probe, with no AppArmor change or rootful runtime. Older profiles used `/run/usnm-boinc-*`; drain and report all work before rerunning setup to change the path, and never switch a running task's runtime path.

### NVIDIA GPU containers

GPU containers are opt-in, and no container GPU version is live (see [What you will run](#what-you-will-run)). To prepare a host anyway:

1. Install the NVIDIA driver and the [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html).
2. Generate the CDI spec that Podman's `--gpus all` uses: `sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml`.
3. Install with a GPU job profile, so every start of the service checks that a container can see the GPU:

   ```sh
   sudo python3 boinc/scripts/setup_volunteer.py --apply --gpu \
     --job-toml "$PWD/boinc/apps/job.toml" --job-toml "$PWD/boinc/apps/qwen/job-gpu.toml"
   ```

### Startup preflight

`boinc/scripts/volunteer_preflight.py` runs in `ExecStartPre` as the service user, with the restrictions the client will have. Using a disposable slot, the pinned worker image and each `--job-toml` profile (`boinc/apps/job.toml` by default), it checks that:

- a mode-0600 input is readable through the mount the wrapper uses, and a mode-0644 output is readable by the host;
- the kernel's cgroup v2 files (`cpu.max`, `memory.max`, `memory.swap.max`, `pids.max`) match the profile: 1 CPU, 1536 MiB, no extra swap and 64 processes for `boinc/apps/job.toml`; the profile's `--cpus`, 6 GiB, no extra swap, 1024 processes and `--gpus all` device visibility for `boinc/apps/qwen/job-*.toml`;
- the root filesystem is read-only and the container has no network routes;
- Podman pauses and resumes the container, and can remove it.

It doesn't add `:z` or `:Z` to hide a broken slots label, and the installer rejects conflicting service overrides. The first run may pull the worker image and the hello-world image stock BOINC uses to detect a runtime. Setup and preflight need no account key. To run it by hand against another profile:

```sh
sudo -u boinc python3 boinc/scripts/volunteer_preflight.py \
  --data-dir /var/lib/boinc \
  --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST \
  --job-toml boinc/apps/qwen/job-cpu.toml
```

Inspect the result with `sudo journalctl -u boinc-client.service --no-pager -n 30`; expect `BOINC host preflight passed`. The preflight doesn't cover the server side; after attaching, check one task's upload, validation and collection.

### Measured hosts

On 16 October 2026 (#469) the profile was measured on RHEL 10.2 x86-64 (SELinux enforcing, BOINC 8.2.13, Podman 5.8.2) and on a clean Debian 13 x86-64 VM (AppArmor enabled, official BOINC 8.2.13, Podman 5.4.2). On Debian, setup and a service restart passed the slot, network and root-filesystem isolation, pause and resume, and kernel-limit probes (`cpu.max=100000 100000`, `memory.max=1610612736`, `memory.swap.max=0`, `pids.max=64`). That host wasn't attached to the project, so this shows setup readiness only. An unprivileged nested LXC failed `newuidmap` and doesn't count. Ubuntu, `aarch64` hosts and other distributions are UNVERIFIED. Docker remains an option, but the helper and its rootless guarantees cover Podman only.

### Remove the managed profile

First select **No new tasks** and let BOINC finish and report all queued and running work. Stopping BOINC with unfinished work can leave task containers paused in the profile's Podman storage, and removing the drop-in also removes `CONTAINERS_STORAGE_CONF` and `XDG_RUNTIME_DIR`, so a restarted client can't find those containers and may start the tasks again beside them.

Once all work is reported:

1. Stop BOINC and remove `/etc/systemd/system/boinc-client.service.d/zz-usnm-volunteer.conf`.
2. Restore the files backed up on first installation in `/etc/usnm-boinc-volunteer/boinc-client.service/backups/` (substitute your service name): copy back each XML file, and where a `<file>.absent` marker shows the file didn't exist before, delete it (or remove only this profile's settings if you added others since).
3. Run `systemctl daemon-reload`, then start the client.

Account data and container images stay. If nothing else needs them, disable the user's lingering session or remove its slots-label rule.

## Proxmox VE container

`boinc/scripts/pve-boinc-ct.sh` creates an unprivileged Debian 13 (trixie) LXC on a Proxmox VE host as a BOINC client. Run it on the host as `root`; `--what-if` prints the plan without changing anything:

```sh
boinc/scripts/pve-boinc-ct.sh --ctid 101 --name boinc-volunteer-2 --what-if
boinc/scripts/pve-boinc-ct.sh --ctid 101 --name boinc-volunteer-2 --authenticator-file PATH
```

It creates the container with `--features nesting=1,keyctl=1`, installs Docker and `boinc-client` 8.2.13 or later from Berkeley's Debian trixie repository, writes `global_prefs_override.xml`, and attaches with the key in `--authenticator-file`, or with a new account named after `--account-name` (default: `--name`). The project validates each workunit on two hosts of different accounts (`<one_result_per_user_per_wu/>`), so give each container meant to validate the other's work its own account. `--help` lists the size, storage and network options.

## macOS container apps

The native apps need none of this. The container apps on macOS need Podman (BOINC's macOS wrapper doesn't support Docker Desktop) and the stable BOINC client 8.2.11 or later, which includes the setuid helper `/Library/Application Support/BOINC Data/Run_Podman`. Upstream 8.2.12 and 8.2.13 are Linux hotfix releases. Podman's Linux VM can't use the Mac's Metal GPU, so Mac GPU work always goes to the native `apple_gpu_8b` app.

1. **Install Podman** (Podman Desktop, Homebrew or the installer). The client recognizes `/opt/podman/bin/podman`, `/usr/local/bin/podman` and Homebrew's `/opt/homebrew/bin/podman`. Finish and report existing tasks, then restart BOINC so it finds the runtime.
2. **Size BOINC's own VM**, not one in your login account. `Run_Podman` runs Podman as `boinc_project`, with configuration and data under `/Library/Application Support/BOINC podman`; `podman machine init` in your own Terminal account doesn't configure it. Inspect it:

   ```sh
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine list
   ```

   BOINC normally creates and starts the VM at startup, and leaves container capability out of its scheduler requests until the VM runs. If there is none, a profile tested with 8.2.11 for ordinary OCR is 2 CPUs, 4 GiB RAM and 30 GiB disk. With the client stopped and no tasks outstanding:

   ```sh
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine init --cpus 2 --memory 4096 --disk-size 30 -v /Library:/Library
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine start
   ```

   Don't reinitialize an existing machine. To resize one, finish and report all tasks, stop the client, then use the helper's `machine stop`, `machine set --cpus 2 --memory 4096` and `machine start`. Never stop the VM under running tasks.

   Budget 1.5 GiB per concurrent OCR task plus VM overhead; the scheduler sees the Mac's RAM, not the VM's. To run one project task at a time, put `<project_max_concurrent>1</project_max_concurrent>` in the project's `app_config.xml` and use Manager's **Read config files** (see the [macOS quick start](macos.md#cpu-quality-tasks-blocked-by-a-concurrency-limit) for how that limit interacts with native work).

3. **Check pause and resume.** Upstream `docker_wrapper` release 22 couldn't resume paused containers on macOS: `Run_Podman` re-joins its arguments into a shell command, so the `|` in `--format "{{.Names}}|{{.Status}}"` became a pipe. The project's Mac app versions build `docker_wrapper` from the pinned BOINC commit with `boinc/scripts/patch-mac-wrapper.py` applied ([details](../../notes/2026-10-15-boinc-cross-platform-distribution.md)). After attaching, suspend and resume a task in Manager and check with the helper's `podman ps` that the container pauses and unpauses without duplicate-name errors.

Runtime readiness was measured on an M1 Mac with 16 GiB, stable BOINC 8.2.11, Homebrew Podman 6.1.4 and the VM above: the ARM OCR worker ran with `cpu.max=100000 100000`, `memory.max=1610612736`, `memory.swap.max=0`, `pids.max=64`, `NoNewPrivs=1` and only `DAC_OVERRIDE`. A full task through download, pause and resume, upload, validation and collection is UNVERIFIED, and so is any Intel Mac.

## Windows helper details

`boinc/scripts/setup_windows_volunteer.ps1` runs in 64-bit Windows PowerShell 5.1 or PowerShell 7, as your normal user. Its parameters:

- No parameters: a preview. It reads local host and client information, including the display adapters in the registry, and contacts nothing.
- `-Apply`: walks you through setup with an existing BOINC 8.2.11 or later. Add `-InstallBoinc` to allow it to install or upgrade BOINC.
- `-Status`: a read-only local check of the attachment and of this project's tasks by state, using `boinccmd` in the data directory.
- `-AllowCpuOnly`: onboard a PC without an eligible AMD GPU. It relaxes only the local GPU check; it doesn't turn on CPU work or promise a CPU app.
- `-BoincDirectory 'D:\Applications\BOINC'`: a nonstandard program directory. The registry's data directory is kept.

The helper needs x64 Windows 11, or Windows 10 22H2 with current security updates; not Windows on ARM, 32-bit Windows or Windows Server. It requires 6 GiB of RAM and 12 GiB free on the BOINC data volume, or 18 GiB with an eligible GPU. For GPU work it looks for an AMD GPU with at least 7,000 MB of video memory and the driver's `vulkan-1.dll` (`amd_vulkan_8b`, [AMD](../apps/amd-gpu.md)), the only Windows GPU class with a live version. It can't see free video memory; a task needs 7,000 MiB free when it starts. Disk checks follow the data directory's real volume, mount points and junctions included. For a fresh installation, the disk check waits until you pick the data directory in the installer.

With `-InstallBoinc`, you type `YES` before the download and again before the installer runs. The helper downloads the official 8.2.11 x64 installer (about 17 MB), checks its pinned SHA-256 and its Authenticode signature from publisher `BOINC`, and opens the interactive installer, which asks for administrator rights itself; don't run PowerShell as administrator. In the installer's **Advanced** options keep **Service install** off (a service can't use the GPU) and leave protected application execution on. The helper passes `LAUNCHPROGRAM=0`, so neither Manager nor the client starts before it asks. Leave any optional WSL component unchecked, and reboot if asked. The helper deletes the installer afterwards and changes no execution policy, firewall rule or TLS setting. It never upgrades, kills or restarts a running client: exit BOINC first. A hash or signature failure stops it; don't bypass it. The pin matches the [official download](https://boinc.berkeley.edu/download.php) and the [winget manifest](https://github.com/microsoft/winget-pkgs/blob/master/manifests/u/UCBerkeley/BOINC/8.2.11/UCBerkeley.BOINC.installer.yaml) as checked on 15 October 2026; a new upstream file needs a reviewed pin update.

It then asks before opening BOINC Manager, prints the [join steps](#join-the-project) and, when you press Enter, checks the attachment with `boinccmd --get_project_status` in the data directory, without an RPC password on the command line. For a local RPC or permission error, open Manager on that PC or repair the installation; don't expose RPC to the network or loosen the data directory's permissions.

Windows PowerShell 5.1 can default to the `Restricted` policy, which blocks local `.ps1` files; the [Windows quick start](windows.md#run-the-helper) says when and how to allow the script for one window.

One RX 9070 XT on Windows has run `amd_vulkan_8b` ([AMD](../apps/amd-gpu.md)); other cards and drivers are unmeasured, and the helper's automated tests mock the host. The container apps on Windows need BOINC's WSL2 `boinc-buda-runner` with Podman and a client 8.2.13 or later. They are UNVERIFIED on a volunteer host, and the helper doesn't set them up.

## Troubleshooting

- **No tasks.** Work goes out in small batches; there may be none. Run `boinccmd --project https://boinc.usnewsmap.com/ update` and read the event log. No work can also mean no app for your platform yet, or resource or preference limits.
- **Connection or login errors.** Check DNS, the clock, any firewall or proxy, and that <https://boinc.usnewsmap.com/> opens. Recover a lost login on the project website; never post account keys.
- **Client won't start (container profile).** Read `journalctl -u boinc-client.service`. A slot or SELinux error needs the slots-only label; missing limits need a working user systemd session, cgroup v2 and writable delegated cgroups. Rerun the setup preview and fix what it reports. Don't disable the preflight to get work.
- **Tasks fail at once.** Check the event log for a runtime or slot error.
- **Upload fails.** A completion report without an upload isn't a success. Select **No new tasks**, keep the local result, and report the upload server's message to the project.
- **Tasks fail after a while.** A page may need more than the task's memory, or the engine crashed. The project sees every failed task ([allocation and recovery](../server/allocation-and-recovery.md#allocation-and-recovery)). If every task fails, detach and report it.
- **Gold downloads fail.** Make sure your network allows the project's download server.
