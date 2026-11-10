# Linux BOINC quick start

Donate your computer's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. You install BOINC, join the project in BOINC
Manager, and set limits. The native apps don't need Docker, Podman or a CUDA toolkit.

This is a pilot. Work goes out in small batches, so a joined computer can sit
idle for a while. Linux GPU work hasn't been verified end to end on a volunteer
host; ask the project before setting up a whole cluster for it.

## What you need

- 64-bit Linux on an Intel or AMD CPU. 64-bit ARM gets CPU work only and is
  not yet verified on a volunteer host.
- BOINC 8.2.11 or newer for GPU work. Some distributions ship an older client;
  check with `boinc --version` and use
  [BOINC's own packages](https://boinc.berkeley.edu/linux_install.php) if yours
  is too old.
- For GPU work, an NVIDIA, AMD or Intel GPU with at least 7 GB of video memory
  and a driver BOINC can detect, plus two free CPU cores per GPU task
  ([app versions](../apps/quality.md#app-versions-and-plan-classes),
  [AMD](../apps/amd-gpu.md), [Intel](../apps/intel-gpu.md)). Intel GPUs need
  Mesa's Vulkan driver and Intel's OpenCL runtime (`mesa-vulkan-drivers` and
  `intel-opencl-icd` on Debian and Ubuntu); AMD GPUs need an AMD Vulkan driver
  and OpenCL runtime. Without a GPU, the computer gets CPU work from the quality
  app.
- Free disk that BOINC may use: 2 GiB for the quality app and 18 GiB for the
  Qwen GPU app (about 2.1 and 19.3 GB as most disk tools count).

## Install BOINC and join

1. Install the client and Manager: on Debian and Ubuntu,
   `sudo apt install boinc-client boinc-manager`; on Fedora and RHEL,
   `sudo dnf install boinc-client boinc-manager`. The client runs as a service
   under its own unprivileged `boinc` user.
2. Open **BOINC Manager**. If it can't connect to the local client, add
   yourself to the `boinc` group (`sudo usermod -aG boinc "$USER"`), log out and
   back in, and try again.
3. [Join the project](README.md#join-the-project) and
   [set limits](README.md#set-limits).

On a headless machine or cluster node, use
[Command-line attach](README.md#command-line-attach) instead of Manager.

## Check that it works

```sh
boinccmd --get_project_status
boinccmd --get_tasks
boinccmd --get_messages 0
```

The first lists `https://boinc.usnewsmap.com/`. A **Scheduler request
completed** line for usnewsmap in the messages means you are joined and waiting
for a batch. For GPU work, the messages at startup should name your GPU; without
that line the computer gets CPU work only. If `boinccmd` is refused, run it as
the `boinc` user or from the data directory.
[Check that it works](README.md#check-that-it-works) says what success looks
like, and [Pause, stop or leave](README.md#pause-stop-or-leave) how to stop.

The container apps (`usnm_ocr`, `usnm_gold`, `usnm_ndlocr`) need Podman and the
repository's rootless profile: see
[Linux container apps](README.md#linux-container-apps).
