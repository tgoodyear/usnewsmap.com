# Contribute compute to usnewsmap OCR (BOINC)

Run OCR and quality checks on historical newspapers with BOINC on Linux, macOS or Windows. Tasks process page images, extracted text or word crops. The project checks returned outputs before using them in search or quality audits.

Status: a pilot for trusted friends. The project server is up and work goes out only in small hand-picked cohorts. Every step below that has not been tried on a real host is marked UNVERIFIED. Operator docs: [boinc.md](boinc.md) and [boinc-quality.md](boinc-quality.md).

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
Fluent text can still misread a scan; outputs need checks, and not every task
changes the public index.

### What you contribute, and what you control

BOINC downloads tasks and applications, runs them, and returns results and
diagnostics automatically. **Participation includes production and experimental
tasks, including Qwen**, through the same account and client. Your CPU/GPU and
resource choices apply to both; experiments need no separate checkbox.
Operators control campaign rollout.

Processing uses electricity, produces heat and fan noise, and consumes disk
space and bandwidth. Models can require several gigabytes of downloads.
Use only a computer you own or have permission to use. BOINC lets you limit
resources, run only while idle, pause, or select **No new tasks** while existing
work finishes. Local computing preferences affect other BOINC projects too.

This pilot may have no work or supported app for your device. You can install
BOINC and register or attach later. Start with the
[Linux quick start](boinc-linux.md), [Windows quick start](boinc-windows.md),
[macOS setup](#12-macos-arm64-apple-darwin-apple-silicon-and-x86_64-apple-darwin-intel)
or the [Linux reference](#11-linux-x86_64-pc-linux-gnu-and-aarch64-unknown-linux-gnu),
then review [resources, network and privacy](#4-resources-network-and-privacy).

## 1. Set up and check the host

The project ships two kinds of BOINC app versions:

- **Native apps** (`usnm_quality` on CPU, and `usnm_qwen` on GPU or multi-thread CPU): signed platform executables that the stock BOINC client runs directly in the task slot with no container runtime required.
- **Container apps** (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, and the `usnm_qwen` container fallback on Linux): signed `docker_wrapper` app versions that run a pinned OCI worker image inside rootless Podman (or Docker).

Choose the section below for your operating system (`Linux`, `macOS`, or `Windows`).

### 1.1 Linux (`x86_64-pc-linux-gnu` and `aarch64-unknown-linux-gnu`)

Linux supports both `x86_64-pc-linux-gnu` (AMD/Intel 64-bit) and `aarch64-unknown-linux-gnu` (64-bit ARM, `UNVERIFIED` on live volunteer hosts).

#### Native CPU app (`usnm_quality`)

`usnm_quality` runs directly as a static Linux executable (`x86_64-unknown-linux-musl` or `aarch64-unknown-linux-musl`) under any stock BOINC client (`boinc-client` 8.2.13+ recommended). It requires **no container runtime**: neither Podman nor Docker is needed if your host only runs native apps.

#### Native GPU and multi-thread CPU app (`usnm_qwen`)

`usnm_qwen` runs `llama-server` directly on the host to transcribe deterministic word crops with Qwen3-VL:

- **NVIDIA CUDA (`nvidia_cuda_8b` and `nvidia_cuda_2b`, on `x86_64-pc-linux-gnu`)**: requires a proprietary or open NVIDIA Linux driver supporting CUDA 12+. Hosts with at least 7,000 MB of free VRAM receive `nvidia_cuda_8b` (Qwen3-VL-8B-Instruct at `Q4_K_M`, ~5.38 GiB total with projector); hosts with at least 3,000 MB of free VRAM receive `nvidia_cuda_2b` (Qwen3-VL-2B-Instruct at `Q8_0`, ~2.12 GiB total).
- **Multi-thread CPU (`mt`, on `x86_64-pc-linux-gnu` and `aarch64-unknown-linux-gnu`)**: runs on hosts with 4 to 8 usable CPUs without a supported GPU, using the 2B `Q8_0` model.
- **Sticky model caching (`<sticky/><no_delete/>`)**: the GGUF model and multimodal projector files (`usnm_qwen_model_<sha256>`) are marked `<sticky/>` and `<no_delete/>` in the workunit input templates. Your BOINC client downloads the model weights once into the project directory (`projects/boinc.usnewsmap.com/`) and reuses them across subsequent tasks without re-downloading.

#### Container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`, and `usnm_qwen` container fallback)

Use a dedicated BOINC service on Linux (`x86_64-pc-linux-gnu` or `aarch64-unknown-linux-gnu`) with enough RAM for the task profile (at least 4 GiB is recommended for the default 1536 MiB `usnm_ocr` profile; at least 8–12 GiB for 6 GiB `usnm_surya` or `usnm_qwen` container tasks). The rootless Podman profile below needs systemd, cgroup v2, Python 3.9 or later, Podman, and a stock BOINC client version 8.2.13 or later. The stock wrapper supports Podman directly; a Docker daemon and Docker-group membership are not required. BOINC still calls the scheduler plan classes `docker`, `docker_mt`, and `docker_nvidia_cuda` and logs `Docker: version ... (podman)`.

A successful container start does not verify resource limits or pausing. On RHEL, Podman accepted CPU and memory flags without a usable cgroup, and SELinux blocked the slot mount. The startup preflight checks kernel limits and slot access; failure prevents BOINC startup. See the [live measurement](notes/2026-10-14-boinc-volunteer-preflight.md).

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

The official BOINC package must give its unprivileged service user a non-overlapping subordinate UID/GID allocation. The helper verifies an allocation exists; it does not invent ranges that could overlap another user. It also refuses a root BOINC service, cgroup v1, nonstandard or symlinked configuration files, or queued or running work. For an existing client, select **No new tasks** and finish and report its tasks before reconfiguration.

##### Preview, install, then attach

From a current checkout of this repository, preview the setup and then install it:

```sh
python3 scripts/boinc/setup_volunteer.py

# After reviewing the preview:
sudo python3 scripts/boinc/setup_volunteer.py --apply
```

The helper accepts both `x86_64` and `aarch64` Linux hosts, detects the service's existing data directory, and uses the published worker digest in this checkout's `contrib-compute/infra/boinc/env/prod.json` (`images.worker`). It does not move data. The default service is `boinc-client.service`; use `--service` if the installed package names it differently. `--data-dir` and `--image` can override the detected directory and released image, but the directory must still be the service's existing working directory and the image must use its full immutable digest.

The helper preserves account data and unrelated settings, backs up the preference and configuration XML it changes, and sets a conservative pilot profile: one BOINC CPU, no GPUs, idle-only processing after five minutes, no battery use, 50% memory allowance, and no extra work buffer. Apps stay in memory while suspended so the wrapper can pause their container. CPU-load suspension is disabled because the client can count whole-machine load against its simulated single CPU; the idle policy and actual per-container CPU limit remain active.

Installation enables the BOINC user's systemd session at boot and configures Podman to use that session for cgroups. It makes the service's cgroups writable (`ProtectControlGroups=false`); the unprivileged UID still limits which cgroups it can modify. It allows read-only home-directory traversal while keeping the data directory writable and retaining the service's other restrictions. The service unsets `INVOCATION_ID`, so Podman starts each container's monitor (conmon) in its own scope in the BOINC user's session rather than in the service's cgroup. A BOINC restart then pauses and later resumes running tasks instead of killing their monitors and losing the exit status (seen on node 4 on 16 October 2026). Runtime files survive service restarts. Only the slots directory receives a persistent `container_file_t` label when SELinux is enabled; SELinux remains enabled and the account and configuration files outside slots retain their labels. The helper does not grant Docker access or change other users' Podman settings. If this dedicated account already belongs to `docker`, an administrator can remove that unused access.

Each service's private runtime directory is
`/run/user/<uid>/usnm-boinc-<service-without-.service>`, mode `0700`.
Keeping it under `/run/user/<uid>/` lets Debian's packaged `pasta` AppArmor
profile open Podman's network namespace during stock BOINC's hello-world
capability probe. No AppArmor policy change or rootful runtime is required.
Older profiles used `/run/usnm-boinc-*`; drain and report all work before
rerunning setup to change paths. Never switch a running task's runtime path.

The preflight (`scripts/boinc/volunteer_preflight.py`) runs in `ExecStartPre` as the service user, with the same service restrictions the client will have. It uses a disposable slot, the pinned worker image, and the task's `job.toml` runtime options (`ocr/boinc/job.toml` by default, or any app's job file via `--job-toml`) to check:

- a mode-0600 input can be read through the same mount the wrapper uses, and a mode-0644 output is readable by the host;
- the **kernel** cgroup v2 files (`cpu.max`, `memory.max`, `memory.swap.max`, `pids.max`) match the limits requested by the selected `job.toml` (1 CPU, 1536 MiB memory, no extra swap, and 64 processes for `ocr/boinc/job.toml`; multi-CPU `--cpus`, 6 GiB memory, no extra swap, 1024 PIDs, and `--gpus all` device visibility when testing `ocr/boinc/surya/job-*.toml` or `ocr/boinc/qwen/job-*.toml` with `--job-toml`);
- the root filesystem is read-only and the container has no network routes;
- Podman actually enters paused and running states, and its probe can be removed.

To run the preflight manually against a multi-CPU (6 GiB) or GPU `job.toml` profile (`usnm_surya` or `usnm_qwen` container fallback):

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

Measured profiles are RHEL 10.2 x86-64, SELinux enforcing, BOINC 8.2.13, Podman
5.8.2; and a clean Debian 13 x86-64 full VM, AppArmor enabled, official BOINC
8.2.13, Podman 5.4.2 (16 October 2026, #469). The Debian setup and subsequent
service restart passed the real slot, network and root-filesystem isolation,
pause and resume, and kernel-limit probes: `cpu.max=100000 100000`,
`memory.max=1610612736`, `memory.swap.max=0`, `pids.max=64`. The host was not
attached to a project; this establishes setup readiness, not a new
task-validation or Gold cache reuse result. An unprivileged nested LXC failed
`newuidmap` and was not counted as full-host acceptance. Ubuntu,
`aarch64-unknown-linux-gnu` hosts, and other distributions remain UNVERIFIED
until the same live checks are recorded. Docker remains an existing pilot
option; this helper and its rootless guarantees apply to Podman only.

### 1.2 macOS (`arm64-apple-darwin` Apple Silicon and `x86_64-apple-darwin` Intel)

Install the official **recommended stable** universal macOS BOINC client
(currently **8.2.11**) from [BOINC's Mac downloads](https://boinc.berkeley.edu/download_all.php?platform=mac).
Do not install a development client just to volunteer. Upstream 8.2.12 and
8.2.13 are Linux hotfix releases, not Mac container prerequisites; stable
8.2.11 already includes `Run_Podman`. Apple Silicon Macs report
`arm64-apple-darwin` as their primary platform; Intel Macs report
`x86_64-apple-darwin`.

#### Native CPU app (`usnm_quality`)

`usnm_quality` is published as a native Mach-O executable for both `arm64-apple-darwin` and `x86_64-apple-darwin`. It runs directly in the BOINC slot directory with **no Podman installation or Linux VM required**.

#### Native GPU and CPU app (`usnm_qwen`)

- **Apple Silicon Metal GPU (`apple_gpu_8b` on `arm64-apple-darwin`)**: Macs with Apple Silicon (`<gpu_type>apple</gpu_type>`, Metal support) and at least **8 GiB of unified memory** (~7,000 MB reported available by BOINC) receive the native `apple_gpu_8b` app version. It runs a Metal-enabled `llama-server` directly on macOS and caches the 8B `Q4_K_M` GGUF model and multimodal projector (~5.38 GiB) in the BOINC project directory via `<sticky/><no_delete/>`.
- **Multi-thread CPU fallback (`mt` on `arm64-apple-darwin` and `x86_64-apple-darwin`)**: Macs with 4–8 CPU threads that do not use the Metal GPU class run the native `mt` version with the 2B `Q8_0` model (~2.12 GiB sticky download).
- Because Podman Machine on macOS does not pass the Apple Metal GPU into its Linux guest VM, Mac GPU work is always routed to the native `apple_gpu_8b` app rather than a container.

#### Container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`, `usnm_surya`)

The standard `usnm_ocr` container app on macOS requires Podman (Docker Desktop
is not supported by BOINC's macOS wrapper integration) and stable BOINC
**8.2.11+**, which includes the setuid
`/Library/Application Support/BOINC Data/Run_Podman` helper. Older deployed
scheduler rules may reject 8.2.11 until the server release containing this
version-gate correction is deployed. This does not change the separately
documented Linux multi-CPU and GPU pilot requirements or task resource limits.

1. **Install Podman** (via Podman Desktop, Homebrew or the installer). The stable client recognizes `/opt/podman/bin/podman`, `/usr/local/bin/podman`, and Apple Silicon Homebrew's `/opt/homebrew/bin/podman`. Restart BOINC normally after installation so its cached runtime-path detection is refreshed; finish and report existing tasks first.
2. **Inspect and size BOINC's own VM**, not a separate VM belonging to your login account. The installed `Run_Podman` helper runs Podman as `boinc_project`, with configuration and data under `/Library/Application Support/BOINC podman`. Plain `podman machine init` in your Terminal account does **not** configure the runtime BOINC uses:

   ```sh
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine list
   ```

   BOINC normally creates and starts its VM at startup and omits container
   capability from scheduler requests until it is running
   (`client/cs_scheduler.cpp`). If no BOINC-owned VM exists, a bounded **ordinary
   OCR** profile tested with stable 8.2.11 is 2 CPUs, 4 GiB RAM, and 30 GiB disk.
   With the client stopped and no outstanding tasks, initialize it through the
   same helper:

   ```sh
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine init --cpus 2 --memory 4096 --disk-size 30 -v /Library:/Library
   "/Library/Application Support/BOINC Data/Run_Podman" "$(command -v podman)" machine start
   ```

   Do not reinitialize an existing machine. To resize one, first finish and
   report all tasks and stop the client, then use the helper's `machine stop`,
   `machine set --cpus 2 --memory 4096`, and `machine start` commands before
   reopening Manager. Never stop the VM under running tasks.

   Budget **1.5 GiB per concurrent ordinary OCR task plus VM overhead**; the
   scheduler sees the Mac's host RAM, not just the VM allocation. A conservative
   pilot uses one project task at a time via
   `<project_max_concurrent>1</project_max_concurrent>` inside the project's
   existing `app_config.xml` `<app_config>` element, followed by Manager's
   **Read config files** command. Preserve any existing app configuration.
   Higher concurrency requires proportionately more guest RAM and host headroom.
   This small OCR profile is **not** a Surya profile: the current Surya CPU job
   requires **12 GiB**, so the previous 8 GiB example was insufficient. Separate
   multi-CPU and GPU admission and publication requirements still apply.

3. **Verify `Run_Podman` pause and resume**: upstream `docker_wrapper` release 22 failed to resume paused containers on macOS because `Run_Podman` re-joins its arguments into a second shell command, so the unquoted `|` in `--format "{{.Names}}|{{.Status}}"` was parsed as a shell pipe (`client/Run_Podman.cpp`, `docker_wrapper.cpp` `get_container_state`). Our Mac app versions build `docker_wrapper` from the pinned BOINC commit with `scripts/boinc/patch-mac-wrapper.py` applied so `--format` pipe quoting survives `Run_Podman` and pause and resume work ([details](notes/2026-10-15-boinc-cross-platform-distribution.md)). This is a project application fix, not a replacement for the installed BOINC client or helper. After attaching, verify in Activity Monitor and the helper's `podman ps` output that suspending and resuming a task in BOINC Manager pauses and unpauses the container cleanly without duplicate-name errors.
4. **Status**: runtime readiness was measured on Computer 1 (M1/16 GiB) with the unmodified stable **BOINC 8.2.11**, Homebrew **Podman 6.1.4**, and the BOINC-owned ARM VM above. Live client RPC reported that runtime after an idle-client restart. The pinned ARM OCR worker enforced `cpu.max=100000 100000`, `memory.max=1610612736`, `memory.swap.max=0`, `pids.max=64`, `NoNewPrivs=1`, and only `DAC_OVERRIDE`. This is **not** end-to-end acceptance: a reserved server cohort must still complete signed download, execution, client-controlled pause and resume, upload, validation, and bridge collection; Intel Mac acceptance remains unverified.

### 1.3 Windows (`windows_x86_64`)

The short version is the [Windows quick start](boinc-windows.md); this section
is the full reference. **Start here for native NVIDIA GPU volunteering.** Use the same one-time setup
for experimental Qwen and production work: one official BOINC client, one
account, one project attachment, and the usual BOINC Manager controls.
Participation includes both experimental and production work under your normal
CPU, GPU and resource choices; cohort selection and signed application versions
are server-side. There is no experimental script flag, alternate client,
`app_info.xml`, manual runtime or model installation, or remove-and-reattach
step.

#### One PowerShell entrypoint

Get a current reviewed checkout of this repository, open **64-bit PowerShell as
your regular user**, and change to its root directory. Windows PowerShell 5.1
(included with Windows) or PowerShell 7 is supported. First preview:

```powershell
.\scripts\boinc\setup_windows_volunteer.ps1
```

The default only reads local host and client information (including `nvidia-smi`
if installed). It does not contact the project, download software, start BOINC,
edit preferences, or attach a project. After reviewing the preview:

```powershell
# Same command whether BOINC already exists or needs installing:
.\scripts\boinc\setup_windows_volunteer.ps1 -Apply -InstallBoinc
```

Later, `-Status` is a read-only local check: it reports whether the PC is
attached and how many usnewsmap tasks it holds, by state, using only
`boinccmd` in the data directory. It opens nothing and contacts no server.

`-InstallBoinc` permits an installation **prompt**, not a silent install. An
existing eligible client is reused. If you already installed BOINC, `-Apply`
alone suffices. A nonstandard program location can be supplied with
`-BoincDirectory 'D:\Applications\BOINC'`; the existing registry data directory
is preserved. For a fresh installation, hardware is checked first but
model-space verification waits until you select the real data directory in the
installer's Advanced options; a full default C: volume does not block choosing
D:. Existing installations check their current data directory. Disk queries
follow the actual directory's backing volume, including NTFS mount points and
junctions, rather than assuming the drive-letter root. The helper refuses to
upgrade a running older client or Manager: finish and report existing work, then
exit them first. It never kills or restarts BOINC.

For a new or older installation, you must type `YES` before the download and
again before execution. The helper downloads the official recommended **8.2.11
x64** installer (about 17 MB), checks its pinned SHA256 **and** Windows
Authenticode trust and publisher `BOINC`, then opens the interactive installer
with its own UAC prompt. Do not elevate the whole PowerShell session. In
**Advanced** options keep **Service install** off: Windows service-mode BOINC
cannot use the GPU. Leave BOINC protected-application execution at its secure
default; do not disable it. At **Finish**, verify **Launch BOINC Manager** is
unchecked. The helper explicitly passes `LAUNCHPROGRAM=0` to the signed
installer's MSI (the upstream default is `1`), including upgrades, so the
Manager and client cannot auto-launch before the separate Manager-consent
prompt. Leave any optional WSL installer unchecked as well. Review all installer
options; finish and reboot if requested before continuing. The helper removes
its installer download afterward and does not change execution policy, firewall
rules or TLS validation. A hash or signature failure stops without execution; do
not bypass it.

Installer sources:
[official recommended downloads](https://boinc.berkeley.edu/download.php),
[official version feed](https://boinc.berkeley.edu/download_all.php?xml=1),
and the matching
[Microsoft winget package hash](https://github.com/microsoft/winget-pkgs/blob/master/manifests/u/UCBerkeley/BOINC/8.2.11/UCBerkeley.BOINC.installer.yaml).
Pins were checked against the official download on 2026-10-15. A changed upstream
file requires a reviewed pin update, not disabling verification.

Windows PowerShell 5.1 can default to `Restricted`, which blocks `.ps1` files
even when they are local and have no Mark-of-the-Web. Inspect the effective policy
first with `Get-ExecutionPolicy` and `Get-ExecutionPolicy -List`. Only if the
effective policy is `Restricted` and neither `MachinePolicy` nor `UserPolicy` is
set, you can permit this reviewed checkout **for this PowerShell window only**:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned
```

Then run the command above in that same window. This does not persist after the
window closes. Do not lower a stricter local policy. If a downloaded checkout's
reviewed script still has Mark-of-the-Web, `Unblock-File` may be used on that
single reviewed file if permitted. Do not set a global execution-policy bypass
or work around an enforced Group Policy; ask your administrator instead.

#### Prerequisites and current availability

- Use Intel/AMD **x64 Windows 11**, or Windows 10 22H2 with current ESU security
  updates. This helper does not support Windows ARM, 32-bit Windows, older desktop
  Windows, or Windows Server.
- Install or update the **official NVIDIA Windows driver**, not a CUDA toolkit.
  The native CUDA 12.4 runtime is delivered with the signed BOINC app; the
  helper conservatively requires driver **551.61 or newer** and at least **3,000
  MiB currently free VRAM**. Close GPU-heavy applications if needed. AMD/Intel
  GPUs are not supported by the native NVIDIA CUDA classes. Hardware checks are
  not a guarantee that a particular older GPU and runtime combination can
  execute work.
- Have at least **6 GiB host RAM** and allow **12 GiB disk** for a cold 2B model
  run, or **18 GiB** if any supported NVIDIA GPU has at least 7,000 MiB total
  VRAM and could later qualify for an 8B task. The preview uses total memory
  across all driver-supported NVIDIA GPUs for disk headroom, not just whichever
  adapter currently has free VRAM; GPU memory use can change after onboarding.
  The data volume must have the reported free space, and BOINC's own disk
  allowance must also permit it. Prefer a desktop with comfortable spare RAM and
  VRAM rather than using your last available resources.
- Intended classes: `nvidia_cuda_8b` with at least 7,000 MB GPU RAM, or
  `nvidia_cuda_2b` with at least 3,000 MB; actual scheduling also depends on
  BOINC's GPU and memory report and resource preferences. CPU participation is
  **optional**. GPU-only volunteers disable **Use CPU** in normal project
  preferences. To knowingly onboard a CPU-only or unsupported-GPU host use
  `-AllowCpuOnly` with the same command; this only relaxes the local GPU check,
  does not enable CPU work, and does not promise a published CPU app. Ask the
  operator about `usnm_quality` or the multi-threaded Qwen CPU class before
  expecting work.
- **Release status (2026-10-16): native Windows Qwen 1.3 is signed and
  published; live GPU, pause and validation acceptance and automatic
  failure-report acceptance remain unverified.** Native CUDA supports stable
  BOINC **8.2.11+**. Operators open only bounded workunit cohorts while
  validating it. A successful attachment can safely wait for eligible work,
  without another experiment preference update. Do not install a development
  client to work around a closed cohort. No WSL2, Podman, Docker, VM, or CUDA
  toolkit is needed for native apps.

#### Account, attachment, and normal BOINC controls

The helper asks permission before opening **BOINC Manager**, which may start its
local client and resume existing projects. It then walks you through the same
normal Manager flow used for production:

1. Set conservative **computing preferences**: CPU and GPU only while idle, no
   battery use, and reviewed CPU, memory and disk limits. Local Manager
   preferences affect **all projects**; do not overwrite settings blindly.
2. At **<https://boinc.usnewsmap.com/>**, create your own account or sign in to
   your existing one. Review normal **project preferences**, including CPU and
   GPU choices. Participation includes production and experimental tasks; no
   Qwen-specific enrollment or repeated profile update is needed. Existing
   experiment-flag values are ignored after the project-participation policy
   rollout. [Project
   preferences](https://boinc.usnewsmap.com/usnmocr/prefs.php?subset=project)
   still control CPU and GPU choices for the host's venue. Never enable a GPU
   you do not want to use. Do not share an account or key with another
   volunteer. Operators bound pilot campaigns server-side.
3. In Manager choose **Tools → Add project** (or **Add project/account
   manager**), enter `https://boinc.usnewsmap.com/`, and sign in with that same
   account. Passwords go only into Manager or the HTTPS account site, never into
   this script, shell history, command arguments, or a support ticket. The
   helper neither creates accounts nor handles keys or passwords. It
   intentionally uses Manager for attachment rather than a credential-bearing
   `boinccmd` command. If already attached, **do not remove and re-add** the
   project.
4. Return to PowerShell and press Enter for its **read-only local attachment
   check**. This uses `boinccmd --get_project_status` in the existing data
   directory without passing a GUI RPC password or printing raw client output.
   For local-client or authorization errors, open Manager on the local computer
   or repair the official installation or user access; do not expose RPC to the
   network or weaken data-directory permissions.
5. In Manager's **Event Log**, confirm the NVIDIA GPU is detected and look for
   **Scheduler request completed**. Use normal project **Update** after changing
   account preferences. No work can mean unpublished apps, a closed cohort, or
   resource or preference limits; it does not mean you need WSL. For connection
   errors check DNS, clock, firewall or proxy and the HTTPS site. For login
   errors recover your account on that site; never post account keys.

BOINC automatically downloads signed app versions and shared sticky models, runs
tasks, and uploads and reports results for **both experimental and production
work**. Use Manager's ordinary suspend and resume controls and resource
preferences for both. Use **No new tasks** to stop accepting work and let
existing tasks finish and report. Do not sideload executables or models, create
`app_info.xml`, or delete sticky cached models
(`C:\ProgramData\BOINC\projects\boinc.usnewsmap.com\` by default) to switch
cohorts. Nothing must be reconfigured or reattached when the server graduates an
app version.

If a task fails, leave its normal BOINC report and diagnostic path intact; this
onboarding does not ask volunteers to harvest or manually upload log files or
share account keys. The Manager Event Log is for local troubleshooting, not a
required support-ticket attachment.

**UNVERIFIED live Windows acceptance:** local attachment and an HTTP 200
response are not scheduler acceptance or useful results. A real authorized host
still must complete download and run, GPU detection, pause and resume, result
upload and report, server-side validation and collection. The automated mocked
PowerShell tests check the helper's contract, not this end-to-end acceptance.

#### Optional container apps (not required for native Windows volunteering)

The separate container apps (`usnm_ocr`, `usnm_assess`, `usnm_gold`,
`usnm_surya`) use BOINC's WSL2 `boinc-buda-runner` Podman integration with an
appropriate **8.2.13+** client, and remain **UNVERIFIED** on a live Windows
volunteer host. That is a different app capability, not a prerequisite or a
workaround for native Qwen availability. The native onboarding helper does not
enable or install it.

## 2. Attach to the project

**Windows volunteers:** the [PowerShell/BOINC Manager flow
above](#13-windows-windows_x86_64) already covers this step without putting
credentials on a command line. Do not repeat the command-line account-key
examples below.

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

`boinccmd --get_messages 0` then shows `Master file download succeeded` and `Scheduler request completed`. For **container apps only**, the client also logs `Docker: version ... (podman)` at start-up; a missing runtime prevents container work, not native CPU or GPU apps.

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
- The BLN600 zip, 68 MB, and 18 page images, served by the BOINC project from its verified mirror. Your client checks each file and keeps it as a sticky input.

The input template marks all of these `<sticky/>` and `<no_delete/>` (written by `usnm-boinc gold submit`). On 14 October 2026, production hosts returned multiple valid Gold tasks, but no client transfer log was retained to confirm that a later task reused each file without downloading it again; see the [live acceptance record](boinc.md#live-acceptance-record-and-remaining-checks). By taking gold tasks you use these datasets under their licences: BLN600 under CC BY-NC-ND 4.0; Overproof transcriptions under CC BY-NC-SA 4.0, with Library of Congress page images; NewsEye under CC BY 4.0; NZZ under the licence in `licenses/nzz/` in the bundle. Each source's licence and attribution files are in `licenses/<source>/` in the bundle. The NC licences allow non-commercial use only. Nothing derived from BLN600 is shared: your host cuts the crops itself, and nothing it cuts leaves the machine.

`gold-result.json` holds, per crop: reference characters, character edits, reference words, word edits, the model profile and the time; then a summary per source, the crops it skipped and why, the engine version, the bundle version and manifest checksum, the job id and your BOINC host id. It holds no page text and no transcription. Counts are enough to compare engines across hosts, and the licences (BLN600's No-Derivatives terms in particular) rule out sending anything else. Crops in a language the worker image has no model for are listed as skipped.

**How gold results will be used.** The OCR app's validator accepts any well-formed result. It doesn't compare text between hosts yet ([boinc.md](boinc.md#connect-a-stock-boinc-project)). A gold result shows how a host's engine does on known text, and the plan is to use it to decide whether to trust that host's OCR. That is a tracked follow-up and is not built yet.

## 4. Resources, network and privacy

Per container task, from `ocr/boinc/job.toml` (`usnm_ocr`, `usnm_assess`, `usnm_gold`, and container `usnm_quality`): one CPU, 1536 MiB of memory with no extra swap, at most 64 processes, a read-only root filesystem, a 512 MiB `/tmp`, all capabilities dropped except `DAC_OVERRIDE`, and `--network=none`. `usnm_surya` and container `usnm_qwen` (`ocr/boinc/surya/job-*.toml`, `ocr/boinc/qwen/job-*.toml`) use 6 GiB of memory with no extra swap, at most 1024 processes, `--cpus` from the plan class (`4`–`8` for `docker_mt`, `2` plus `--gpus all` for `docker_nvidia_cuda`), a read-only root filesystem, and `--network=none`. Every container sees only the task's slot directory. Native `usnm_quality` runs single-threaded on the slot's files with no network calls; native `usnm_qwen` spawns `llama-server` bound exclusively to `127.0.0.1` on an ephemeral loopback port.

Network, step by step:

| Step                                                        | Who connects                               | To where                                          |
| ----------------------------------------------------------- | ------------------------------------------ | ------------------------------------------------- |
| scheduler requests, downloads of inputs, uploads of results | the BOINC client                           | `https://boinc.usnewsmap.com/`                    |
| worker image (container apps only)                          | Podman (or Docker), started by the wrapper | the registry named in the app version             |
| gold originals                                              | the BOINC client                           | the BOINC project's download server               |
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
- **Client will not start.** Read `journalctl -u boinc-client.service`. A slot or SELinux error needs the slots-only label; missing limits need a working user systemd session, cgroup v2 and writable delegated cgroups. Re-run the setup preview and fix its diagnostic. Do not disable the preflight to accept work.
- **Tasks fail at once.** Check the event log for a runtime or slot error. A hello-world check alone is insufficient; the startup preflight exercises the real task limits and mount.
- **Upload fails.** An upload error is not a completed acceptance test even if the scheduler receives a completion report. Preserve a local result when diagnosing, stop requesting new tasks, and report the upload-server message to the project operator.
- **Tasks fail after a while.** A page may need more than 1536 MiB, or the engine may have crashed. Either is a failed task, which the project sees ([boinc.md](boinc.md#allocation-and-recovery)). If every task fails, detach and report it.
- **Gold downloads fail.** Make sure your network allows the BOINC project's download server. The 14 October production reports predate the project mirror and only establish that publisher-sourced inputs passed the worker's hash checks; they do not establish mirror or sticky-cache acceptance. See the [live acceptance record](boinc.md#live-acceptance-record-and-remaining-checks).

## Detach

```sh
boinccmd --project https://boinc.usnewsmap.com/ detach
```

The client stops its tasks for this project and deletes the project's files, sticky files included. To delete your account too, use the project website.

## Remove the managed profile

First select **No new tasks** and let BOINC finish and report all queued and running work before stopping the service. Stopping BOINC with unfinished work can leave task containers paused in the profile's managed Podman storage. Removing the drop-in also removes `CONTAINERS_STORAGE_CONF` and `XDG_RUNTIME_DIR`, so restarting cannot find those containers and may recreate tasks while leaving the originals stranded. Once all work is reported, stop BOINC, remove `/etc/systemd/system/boinc-client.service.d/zz-usnm-volunteer.conf`, and restore the XML state recorded on first installation in `/etc/usnm-boinc-volunteer/boinc-client.service/backups/` (substitute the service name if different): copy back each backed-up XML file, or if `<file>.absent` is present because the file did not exist before installation, delete that XML file (or remove only this profile's settings if you added others afterward). Run `systemctl daemon-reload` before starting the client. Account data and container images are retained. Only disable the user's lingering session or remove its slots-label rule when that user or slots tree no longer needs them. Do not remove the preflight while continuing to rely on this profile's resource and isolation guarantees.

## Qwen disk requirements

Qwen's container CPU (2B) and GPU (8B) releases require **32 GiB** and **48 GiB
free** respectively, not the old 2 GiB bound. Native 2B and 8B workunits
advertise 12 GiB and 18 GiB instead. These conservative cold-cache admission
bounds also apply to warm hosts; they include extraction overlap, workspace and
reserve, not just model files. Keep image graph storage and extraction scratch
on BOINC's filesystem so its scheduler reports include that consumption. The
managed Podman profile does this and rejects insufficient free disk before
pulling or accepting work. Do not prune or reset a volunteer's existing cache to
make room automatically. See [Qwen disk accounting and
acceptance](boinc-quality.md#qwen-disk-admission-and-cold-cache-acceptance-559)
for metadata sources, exact byte bounds, native shared-model layout and live
acceptance limitations.
