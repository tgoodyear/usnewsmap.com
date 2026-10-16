# Contribute compute to usnewsmap OCR (BOINC)

Volunteers run OCR and OCR-quality checks on historical newspaper pages from Chronicling America (Library of Congress) on their own Linux, macOS, and Windows machines through a stock BOINC client. Each task reads one page image (or its extracted texts and word crops) and returns structured outputs. Those outputs go into the site's search index and quality audit after the project's own checks.

Status: a pilot for trusted friends. The project server is up and work goes out only in small hand-picked cohorts. Every step below that has not been tried on a real host is marked UNVERIFIED. Operator docs: [boinc.md](boinc.md) and [boinc-quality.md](boinc-quality.md).

## 1. Set up and check the host

The project ships two kinds of BOINC app versions:

- **Native apps** (`usnm_quality` on CPU, and `usnm_qwen` on GPU or multi-thread CPU): signed platform executables that the stock BOINC client runs directly in the task slot with no container runtime required.
- **Container apps** (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, and the `usnm_qwen` container fallback on Linux): signed `docker_wrapper` app versions that run a pinned OCI worker image inside rootless Podman (or Docker).

Choose the section below for your operating system (`Linux`, `macOS`, or `Windows`).

### 1.1 Linux (`x86_64-pc-linux-gnu` and `aarch64-unknown-linux-gnu`)

Linux supports both `x86_64-pc-linux-gnu` (AMD/Intel 64-bit) and `aarch64-unknown-linux-gnu` (64-bit ARM, `UNVERIFIED` on live volunteer hosts).

#### Native CPU app (`usnm_quality`)

`usnm_quality` runs directly as a static Linux executable (`x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`) under any stock BOINC client (`boinc-client` 8.2.13+ recommended). It requires **zero container runtime**—neither Podman nor Docker is needed if your host only runs native apps.

#### Native GPU and multi-thread CPU app (`usnm_qwen`)

`usnm_qwen` runs `llama-server` directly on the host to transcribe deterministic word crops with Qwen3-VL:

- **NVIDIA CUDA (`nvidia_cuda_8b` and `nvidia_cuda_2b`, on `x86_64-pc-linux-gnu`)**: requires a proprietary/open NVIDIA Linux driver supporting CUDA 12+. Hosts with at least 7,000 MB of free VRAM receive `nvidia_cuda_8b` (Qwen3-VL-8B-Instruct at `Q4_K_M`, ~5.38 GiB total with projector); hosts with at least 3,000 MB of free VRAM receive `nvidia_cuda_2b` (Qwen3-VL-2B-Instruct at `Q8_0`, ~2.12 GiB total).
- **Multi-thread CPU (`mt`, on `x86_64-pc-linux-gnu` and `aarch64-unknown-linux-gnu`)**: runs on hosts with 4 to 8 usable CPUs without a supported GPU, using the 2B `Q8_0` model.
- **Sticky model caching (`<sticky/><no_delete/>`)**: the GGUF model and multimodal projector files (`usnm_qwen_model_<sha256>`) are marked `<sticky/>` and `<no_delete/>` in the workunit input templates. Your BOINC client downloads the model weights once into the project directory (`projects/boinc.usnewsmap.com/`) and reuses them across subsequent tasks without re-downloading.

#### Container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, and `usnm_qwen` container fallback)

Use a dedicated BOINC service on Linux (`x86_64-pc-linux-gnu` or `aarch64-unknown-linux-gnu`) with enough RAM for the task profile (at least 4 GiB is recommended for the default 1536 MiB `usnm_ocr` profile; at least 8–12 GiB for 6 GiB `usnm_surya` / `usnm_qwen` container tasks). The rootless Podman profile below needs systemd, cgroup v2, Python 3.9 or later, Podman, and a stock BOINC client version 8.2.13 or later. The stock wrapper supports Podman directly; a Docker daemon and Docker-group membership are not required. BOINC still calls the scheduler plan classes `docker`, `docker_mt`, and `docker_nvidia_cuda` and logs `Docker: version ... (podman)`.

A container starting successfully does not establish that its resource limits or pause work. On a real RHEL host, Podman accepted CPU/memory flags without a usable cgroup, and SELinux blocked the wrapper's slot mount. The startup preflight below checks actual kernel limits and slot access and refuses to start BOINC on failure. See the [live measurement](notes/2026-10-14-boinc-volunteer-preflight.md).

##### Install prerequisites

Get a recent client from the [BOINC project's packages](https://boinc.berkeley.edu/linux_install.php), then check `boinc --version`. Some distribution clients are older than the required 8.2.13. Upstream packages may put `boinc` and `boinccmd` in `/usr/local/bin`; use their full paths if needed.

On Debian/Ubuntu, install the runtime and user-session prerequisites:

```sh
sudo apt-get install podman uidmap dbus-user-session python3 ca-certificates
```

On RHEL, install the runtime and SELinux policy-management tools:

```sh
sudo dnf install podman shadow-utils python3 ca-certificates policycoreutils-python-utils
```

The official BOINC package must give its unprivileged service user a non-overlapping subordinate UID/GID allocation. The helper verifies an allocation exists; it does not invent ranges that could overlap another user. It also refuses a root BOINC service, cgroup v1, nonstandard/symlinked configuration files, or queued/running work. For an existing client, select **No new tasks** and finish/report its tasks before reconfiguration.

##### Preview, install, then attach

From a current checkout of this repository, preview the setup and then install it:

```sh
python3 scripts/boinc/setup_volunteer.py

# After reviewing the preview:
sudo python3 scripts/boinc/setup_volunteer.py --apply
```

The helper accepts both `x86_64` and `aarch64` Linux hosts, detects the service's existing data directory, and uses the published worker digest in this checkout's `contrib-compute/infra/boinc/env/prod.json` (`images.worker`). It does not move data. The default service is `boinc-client.service`; use `--service` if the installed package names it differently. `--data-dir` and `--image` can override the detected directory and released image, but the directory must still be the service's existing working directory and the image must use its full immutable digest.

The helper preserves account data and unrelated settings, backs up the changed preference/configuration XML, and sets a conservative pilot profile: one BOINC CPU, no GPUs, idle-only processing after five minutes, no battery use, 50% memory allowance, and no extra work buffer. Apps stay in memory while suspended so the wrapper can pause their container. CPU-load suspension is disabled because the client can count whole-machine load against its simulated single CPU; the idle policy and actual per-container CPU limit remain active.

Installation enables the BOINC user's systemd session at boot and configures Podman to use that session for cgroups. It makes the service's cgroups writable (`ProtectControlGroups=false`); the unprivileged UID still limits which cgroups it can modify. It allows read-only home-directory traversal while keeping the data directory writable and retaining the service's other restrictions. Podman's container monitors run with the container, and runtime files survive service restarts. Only the slots directory receives a persistent `container_file_t` label when SELinux is enabled; SELinux remains enabled and the account/config files outside slots retain their labels. The helper does not grant Docker access or change other users' Podman settings. If this dedicated account already belongs to `docker`, an administrator can remove that unused access.

The preflight (`scripts/boinc/volunteer_preflight.py`) runs in `ExecStartPre` as the service user, with the same service restrictions the client will have. It uses a disposable slot, the pinned worker image, and the task's `job.toml` runtime options (`ocr/boinc/job.toml` by default, or any app's job file via `--job-toml`) to check:

- a mode-0600 input can be read through the same mount the wrapper uses, and a mode-0644 output is readable by the host;
- the **kernel** cgroup v2 files (`cpu.max`, `memory.max`, `memory.swap.max`, `pids.max`) match the limits requested by the selected `job.toml` (1 CPU, 1536 MiB memory, no extra swap, and 64 processes for `ocr/boinc/job.toml`; multi-CPU `--cpus`, 6 GiB memory, no extra swap, 1024 PIDs, and `--gpus all` device visibility when testing `ocr/boinc/surya/job-*.toml` or `ocr/boinc/qwen/job-*.toml` with `--job-toml`);
- the root filesystem is read-only and the container has no network routes;
- Podman actually enters paused and running states, and its probe can be removed.

To run the preflight manually against a multi-CPU / 6 GiB or GPU `job.toml` profile (`usnm_surya` or `usnm_qwen` container fallback):

```sh
sudo -u boinc python3 scripts/boinc/volunteer_preflight.py \
  --data-dir /var/lib/boinc \
  --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:FULL_DIGEST \
  --job-toml ocr/boinc/qwen/job-cpu.toml
```

It does not add `:z` or `:Z` to hide a broken slots label. The installer also rejects conflicting service overrides. Each restart repeats the checks; a failure prevents BOINC startup. The initial run may pull the worker image and the upstream hello-world image used by stock BOINC runtime detection. No account key is required by setup or preflight.

Inspect the result:

```sh
sudo journalctl -u boinc-client.service --no-pager -n 30
# expect: BOINC host preflight passed ...
```

This is host readiness, not server acceptance. After attaching, still verify one task's output upload, validation, and collection. A scheduler completion report or exit code 0 alone is insufficient.

The measured profile is RHEL 10.2 x86-64, SELinux enforcing, BOINC 8.2.13, and Podman 5.8.2. Debian/Ubuntu installation, `aarch64-unknown-linux-gnu` hosts, and other distributions remain UNVERIFIED until the same live checks are recorded. Docker remains an existing pilot option; this helper and its rootless guarantees apply to Podman only.

### 1.2 macOS (`arm64-apple-darwin` Apple Silicon and `x86_64-apple-darwin` Intel)

Install the official universal macOS BOINC client version **8.2.13 or later** (tested locally with `8.2.15`) from [BOINC's Mac downloads](https://boinc.berkeley.edu/download_all.php?platform=mac). Apple Silicon Macs report `arm64-apple-darwin` as their primary platform; Intel Macs report `x86_64-apple-darwin`.

#### Native CPU app (`usnm_quality`)

`usnm_quality` is published as a native Mach-O executable for both `arm64-apple-darwin` and `x86_64-apple-darwin`. It runs directly in the BOINC slot directory with **no Podman installation or Linux VM required**.

#### Native GPU and CPU app (`usnm_qwen`)

- **Apple Silicon Metal GPU (`apple_gpu_8b` on `arm64-apple-darwin`)**: Macs with Apple Silicon (`<gpu_type>apple</gpu_type>`, Metal support) and at least **8 GiB of unified memory** (~7,000 MB reported available by BOINC) receive the native `apple_gpu_8b` app version. It runs a Metal-enabled `llama-server` directly on macOS and caches the 8B `Q4_K_M` GGUF model and multimodal projector (~5.38 GiB) in the BOINC project directory via `<sticky/><no_delete/>`.
- **Multi-thread CPU fallback (`mt` on `arm64-apple-darwin` and `x86_64-apple-darwin`)**: Macs with 4–8 CPU threads that do not use the Metal GPU class run the native `mt` version with the 2B `Q8_0` model (~2.12 GiB sticky download).
- Because Podman Machine on macOS does not pass the Apple Metal GPU into its Linux guest VM, Mac GPU work is always routed to the native `apple_gpu_8b` app rather than a container.

#### Container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`)

Container apps on macOS require Podman (Docker Desktop is not supported by BOINC's macOS wrapper integration) and BOINC 8.2.13+, which installs the setuid `/Library/Application Support/BOINC Data/Run_Podman` helper.

1. **Install Podman** (via Podman Desktop or Homebrew/installer so `podman` is available in `/opt/podman/bin/podman` or standard `PATH`).
2. **Size the Podman Machine VM**: for multi-CPU or 6 GiB workloads (`usnm_surya`, or running tasks comfortably alongside `usnm_ocr`), initialize or resize the Podman machine with at least 4 CPUs, 8 GiB RAM, and 30 GiB disk:

   ```sh
   podman machine init --cpus 4 --memory 8192 --disk-size 30
   podman machine start
   ```

   Note that the BOINC client also manages its own Podman VM through `Run_Podman` at startup and omits Docker/Podman capability from scheduler requests until that VM is running (`client/cs_scheduler.cpp`).

3. **Verify `Run_Podman` pause/resume**: upstream `docker_wrapper` release 22 failed to resume paused containers on macOS because `Run_Podman` re-joins its arguments into a second shell command, causing the unquoted `|` in `--format "{{.Names}}|{{.Status}}"` to be parsed as a shell pipe (`client/Run_Podman.cpp`, `docker_wrapper.cpp` `get_container_state`). Our Mac app versions build `docker_wrapper` from the pinned BOINC commit with `scripts/boinc/patch-mac-wrapper.py` applied so `--format` pipe quoting survives `Run_Podman` and pause/resume works ([details](notes/boinc-buda-arm-evaluation.md)). After attaching, verify in Activity Monitor / `podman ps` that suspending and resuming a task in BOINC Manager pauses and unpauses the container cleanly without duplicate-name errors.
4. **Status**: `UNVERIFIED` end-to-end on live volunteer Mac hosts until a reserved server cohort completes download, execution, pause/resume, upload, validation, and bridge collection.

### 1.3 Windows (`windows_x86_64`)

Install a stock 64-bit Windows BOINC client (**8.2.13 or later**) from [BOINC's Windows downloads](https://boinc.berkeley.edu/download_all.php?platform=win).

#### Native CPU (`usnm_quality`) and native GPU/CPU (`usnm_qwen`) apps

- **`usnm_quality`** and **`usnm_qwen`** run directly as native Windows `.exe` binaries (`windows_x86_64`) with **no WSL2 or Podman required**.
- **`usnm_qwen` plan classes on Windows**:
  - `nvidia_cuda_8b`: NVIDIA GPU with CUDA 12+ driver and at least 7,000 MB free VRAM (8B `Q4_K_M` model).
  - `nvidia_cuda_2b`: NVIDIA GPU with CUDA 12+ driver and at least 3,000 MB free VRAM (2B `Q8_0` model).
  - `mt`: 4 to 8 CPU threads (2B `Q8_0` model fallback).
- Model weights (`usnm_qwen_model_<sha256>`) use `<sticky/><no_delete/>` and are cached once in the BOINC project directory (`C:\ProgramData\BOINC\projects\boinc.usnewsmap.com\`).
- Status: `UNVERIFIED` on a live Windows volunteer host.

#### Container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`)

- Container apps on `windows_x86_64` run through the standard BOINC Windows installer's bundled WSL2 `boinc-buda-runner` Podman distribution (`boinc-buda-runner-wsl-installer.exe`). `docker_wrapper` on Windows opens a pipe-connected shell inside the `boinc-buda-runner` WSL2 distro and runs Podman commands there (`docker_wrapper.cpp` `wsl_init`, `lib/wslinfo.cpp`).
- Ensure Windows Subsystem for Linux 2 (WSL2) is enabled and that the BOINC installer's `boinc-buda-runner` WSL distribution is installed and detected in the BOINC Event Log (`WSL: distro boinc-buda-runner ...`). GPU container workloads (`docker_nvidia_cuda`) additionally require a Windows NVIDIA driver with WSL2 CUDA passthrough (`check_wsl_gpu`, `min_boinc_wsl_distro_version` 6).
- Status: `UNVERIFIED` on a live Windows volunteer host.

## 2. Attach to the project

The master URL is `https://boinc.usnewsmap.com/`. It is fixed for the life of the project.

Check that the server is up:

```sh
curl --silent --show-error --fail --output /dev/null --write-out '%{http_code}\n' https://boinc.usnewsmap.com/
# expect: 200. Anything else: the project isn't open yet; stop here.
```

Create an account (the client prints the account key), or get the key of an existing account. Account creation is open; the server asks for no invitation code (checked 2026-10-14):

```sh
boinccmd --create_account https://boinc.usnewsmap.com/ YOUR_EMAIL YOUR_PASSWORD YOUR_NAME
boinccmd --lookup_account https://boinc.usnewsmap.com/ YOUR_EMAIL YOUR_PASSWORD
```

BOINC Manager can add the project using its URL and your account login. For a command-line client, attach with the key:

```sh
boinccmd --project_attach https://boinc.usnewsmap.com/ ACCOUNT_KEY
boinccmd --get_project_status   # expect: an entry whose master URL is https://boinc.usnewsmap.com/
```

`boinccmd --get_messages 0` then shows `Master file download succeeded` and `Scheduler request completed`. The client also logs `Docker: version ... (podman)` at start-up for this profile; if that line is missing, the scheduler sends this host no work.

Set idle-only processing and conservative limits in your BOINC preferences, as the pilot asks. On a headless host or a container, the client may log `Suspending computation - CPU is busy` because it sees the whole machine's load; set `<suspend_cpu_usage>0</suspend_cpu_usage>` in `/var/lib/boinc/global_prefs_override.xml` and run `boinccmd --read_global_prefs_override`.

To view your task history, log in to the project website with the same account and open your account page's **Tasks** link. The account-scoped list checks ownership. **Privacy limitation:** BOINC also accepts a `hostid` on the results page without checking that the signed-in user owns that host, so another signed-in volunteer with a host ID can view its task history.

## 3. What you will run

Work goes out in explicit cohorts of 1 to 100 pages that the project selects by hand ([boinc.md](boinc.md#boundaries)), so it's normal to see no tasks for a while. Container apps pull a pinned OCI worker image named in the app version; native apps (`usnm_quality` and `usnm_qwen`) run directly on the host without a container runtime.

| App            | Runtime                                                                                                                  | What a task does                                                                                                                             | Downloads                                                                                              | Returns                                                                                                         |
| -------------- | ------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| `usnm_ocr`     | Container (`docker`)                                                                                                     | OCR of one newspaper page with Tesseract 5.5.0 and the project's model lock                                                                  | the page image and a job file, from the project server                                                 | the page's text (`text.txt`), its layout as ALTO XML (`layout.xml`) and `result.json` (checksums, engine, time) |
| `usnm_assess`  | Container (`docker`)                                                                                                     | reads one page twice, as JP2 and as JPEG, to compare the two formats ([boinc-assessment.md](boinc-assessment.md)); two hosts get each page   | the JP2, the JPEG (OCR units) and a job file, from the project server                                  | both texts and `result.json` (outcome, exit code, time, memory per format)                                      |
| `usnm_gold`    | Container (`docker`)                                                                                                     | scores your engine on public hand-transcribed text ([gold check](boinc.md#gold-check))                                                       | see below                                                                                              | `gold-result.json`: counts only                                                                                 |
| `usnm_surya`   | Container (`docker_mt`, `docker_nvidia_cuda`)                                                                            | OCR of one newspaper page with Surya 2 ([boinc-surya.md](boinc-surya.md))                                                                    | the page image and `surya.json`, from the project server                                               | `text.txt`, `lines.json`, `result.json` and `diag.txt`                                                          |
| `usnm_quality` | Native CPU (all 5 platforms) or Container (`docker`)                                                                     | estimates per-page OCR accuracy, completeness, findability and scan features across existing readings ([boinc-quality.md](boinc-quality.md)) | `page.jpg`, `loc.xml`, `texts.json`, sticky `places.txt` and `quality.json`                            | `result.json`, `confusions.json` and `diag.txt`                                                                 |
| `usnm_qwen`    | Native (`apple_gpu_8b`, `nvidia_cuda_8b`, `nvidia_cuda_2b`, `mt`) or Container (`docker_mt_2b`, `docker_nvidia_cuda_8b`) | transcribes deterministic word crops with Qwen3-VL via `llama-server` ([boinc-quality.md](boinc-quality.md))                                 | `page.jpg`, `qwen.json`, and (native only) sticky `<sticky/><no_delete/>` GGUF model + projector files | `readings.json`, `result.json` and `diag.txt`                                                                   |

The OCR text from `usnm_ocr` is what the project uses. It is published as ALTO and Parquet and goes through the same import and release as the project's own OCR nodes.

**The gold check, `usnm_gold`.** Bundle version 1 ([ocr-gold-set.md](ocr-gold-set.md#volunteer-bundle)):

- `bundle.tar`, 9.6 MB, from the project server: crops and transcriptions the licences allow us to share, with their licence files.
- The BLN600 zip, 68 MB, from figshare, and 18 page images from tile.loc.gov. Your BOINC client fetches these from the publishers, not from us, and checks each one's size and md5.

The input template marks all of these `<sticky/>` (written by `usnm-boinc gold submit`), so your client downloads each once per host and reuses it for later gold tasks (UNVERIFIED on a live server, #469). By taking gold tasks you use these datasets under their licences: BLN600 under CC BY-NC-ND 4.0; Overproof transcriptions under CC BY-NC-SA 4.0, with Library of Congress page images; NewsEye under CC BY 4.0; NZZ under the licence in `licenses/nzz/` in the bundle. Each source's licence and attribution files are in `licenses/<source>/` in the bundle. The NC licences allow non-commercial use only. Nothing derived from BLN600 is shared: your host cuts the crops itself, and nothing it cuts leaves the machine.

`gold-result.json` holds, per crop: reference characters, character edits, reference words, word edits, the model profile and the time; then a summary per source, the crops it skipped and why, the engine version, the bundle version and manifest checksum, the job id and your BOINC host id. It holds no page text and no transcription. Counts are enough to compare engines across hosts, and the licences (BLN600's No-Derivatives terms in particular) rule out sending anything else. Crops in a language the worker image has no model for are listed as skipped.

**How gold results will be used.** The OCR app's validator accepts any well-formed result. It doesn't compare text between hosts yet ([boinc.md](boinc.md#connect-a-stock-boinc-project)). A gold result shows how a host's engine does on known text, and the plan is to use it to decide whether to trust that host's OCR. That is a tracked follow-up and is not built yet.

## 4. Resources, network and privacy

Per container task, from `ocr/boinc/job.toml` (`usnm_ocr`, `usnm_assess`, `usnm_gold`, and container `usnm_quality`): one CPU, 1536 MiB of memory with no extra swap, at most 64 processes, a read-only root filesystem, a 512 MiB `/tmp`, all capabilities dropped except `DAC_OVERRIDE`, and `--network=none`. `usnm_surya` and container `usnm_qwen` (`ocr/boinc/surya/job-*.toml`, `ocr/boinc/qwen/job-*.toml`) use 6 GiB of memory with no extra swap, at most 1024 processes, `--cpus` from the plan class (`4`–`8` for `docker_mt`, `2` plus `--gpus all` for `docker_nvidia_cuda`), a read-only root filesystem, and `--network=none`. Every container sees only the task's slot directory. Native `usnm_quality` runs single-threaded on the slot's files with no network calls; native `usnm_qwen` spawns `llama-server` bound exclusively to `127.0.0.1` on an ephemeral loopback port.

Network, step by step:

| Step                                                        | Who connects                               | To where                                          |
| ----------------------------------------------------------- | ------------------------------------------ | ------------------------------------------------- |
| scheduler requests, downloads of inputs, uploads of results | the BOINC client                           | `https://boinc.usnewsmap.com/`                    |
| worker image (container apps only)                          | Podman (or Docker), started by the wrapper | the registry named in the app version             |
| gold originals                                              | the BOINC client                           | figshare (BLN600) and tile.loc.gov                |
| the task itself                                             | nobody                                     | no network inside containers; loopback-only local |

What leaves your machine: the result files in the table in section 3, and what any stock BOINC client sends its project (host description, account details). The project stores your email address, name, IP address and account key. Privacy is on by default, and you can delete your account ([design](../contrib-compute/docs/design.md), Privacy). Web server and scheduler logs keep IP addresses for 30 days.

Disk: the gold check keeps about 78 MB of sticky files plus the 18 page images for as long as you stay attached, and each running gold task copies them into its slot. `usnm_quality` keeps the campaign's sticky `places.txt` (`usnm_quality_places_<sha256>`). Native `usnm_qwen` keeps the sticky `<sticky/><no_delete/>` GGUF model and multimodal projector files (`usnm_qwen_model_<sha256>`) in the project directory: 5.38 GiB for the 8B `Q4_K_M` set (`apple_gpu_8b` and `nvidia_cuda_8b`) or 2.12 GiB for the 2B `Q8_0` set (`nvidia_cuda_2b` and `mt`).

## 5. Check that it works

```sh
boinccmd --get_tasks              # tasks with project URL https://boinc.usnewsmap.com/ and their state
boinccmd --get_messages 0 | grep --ignore-case usnewsmap   # the event log for this project
```

Success: a task moves from downloading to running to uploaded, and the event log shows no error for it. The project's website lists your host and tasks under your account (stock BOINC pages).

## 6. Troubleshooting

- **No tasks.** Work goes out in small cohorts; there may be none queued. Run `boinccmd --project https://boinc.usnewsmap.com/ update` and check the event log.
- **Client will not start.** Read `journalctl -u boinc-client.service`. A slot/SELinux error needs the slots-only label; missing limits need a working user systemd session, cgroup v2 and writable delegated cgroups. Re-run the setup preview and fix its diagnostic. Do not disable the preflight to accept work.
- **Tasks fail at once.** Check the event log for a runtime or slot error. A hello-world check alone is insufficient; the startup preflight exercises the real task limits and mount.
- **Upload fails.** An upload error is not a completed acceptance test even if the scheduler receives a completion report. Preserve a local result when diagnosing, stop requesting new tasks, and report the upload-server message to the project operator.
- **Tasks fail after a while.** A page may need more than 1536 MiB, or the engine may have crashed. Either is a failed task, which the project sees ([boinc.md](boinc.md#allocation-and-recovery)). If every task fails, detach and report it.
- **Gold downloads fail.** The client fetches from figshare and tile.loc.gov; make sure your network allows both. UNVERIFIED: that the client follows figshare's redirect.

## Detach

```sh
boinccmd --project https://boinc.usnewsmap.com/ detach
```

The client stops its tasks for this project and deletes the project's files, sticky files included. To delete your account too, use the project website.

## Remove the managed profile

First select **No new tasks** and let BOINC finish and report all queued and running work before stopping the service. Stopping BOINC with unfinished work can leave task containers paused in the profile's managed Podman storage; removing the drop-in also removes `CONTAINERS_STORAGE_CONF` and `XDG_RUNTIME_DIR`, so restarting cannot find those containers and may recreate tasks while leaving the originals stranded. Once all work is reported, stop BOINC, remove `/etc/systemd/system/boinc-client.service.d/zz-usnm-volunteer.conf`, and restore the XML state recorded on first installation in `/etc/usnm-boinc-volunteer/boinc-client.service/backups/` (substitute the service name if different): copy back each backed-up XML file, or if `<file>.absent` is present because the file did not exist before installation, delete that XML file (or remove only this profile's settings if you added others afterward). Run `systemctl daemon-reload` before starting the client. Account data and container images are retained. Only disable the user's lingering session or remove its slots-label rule when that user/slots tree no longer needs them. Do not remove the preflight while continuing to rely on this profile's resource/isolation guarantees.

## Qwen disk requirements

Qwen's container CPU/2B and GPU/8B releases require respectively **32 GiB** and
**48 GiB free**, not the old 2 GiB bound. Native 2B/8B workunits advertise 12/18 GiB
instead. These conservative cold-cache admission bounds also apply to warm
hosts; they include extraction overlap/workspace/reserve, not just model files.
Keep image graph storage and extraction scratch on BOINC's filesystem so its
scheduler reports include that consumption. The managed Podman profile does
this and rejects insufficient free disk before pulling or accepting work.
Do not prune/reset a volunteer's existing cache to make room automatically.
See [Qwen disk accounting and acceptance](boinc-quality.md#qwen-disk-admission-and-cold-cache-acceptance-559)
for metadata sources, exact byte bounds, native shared-model layout and live
acceptance limitations.
