# Windows BOINC quick start

Donate your PC's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. One helper script checks your PC, installs the
official BOINC client if needed, and walks you through joining. You don't need WSL, Docker, Podman or a CUDA toolkit.

This is a pilot. Work goes out in small batches, so a joined PC can sit idle for
a while.

## What you need

- Windows 11, or Windows 10 22H2 with current updates, on an Intel or AMD
  64-bit PC. Windows on ARM and Windows Server are not supported.
- For GPU work, an AMD GPU with at least 7 GB of video memory and a current AMD
  graphics driver, which provides Vulkan ([AMD](../apps/amd-gpu.md)). No Windows
  version has been released for NVIDIA or Intel GPUs yet; both get GPU work on
  Linux ([app versions](../apps/quality.md#app-versions-and-plan-classes)). On a
  PC without an eligible AMD GPU, add `-AllowCpuOnly` to the preview and apply
  commands below (not to `-Status`) to offer CPU time instead. CPU work is
  optional and not always available.
- 6 GB of RAM and 12 GiB of free disk, or 18 GiB with an eligible AMD GPU
  (about 12.9 and 19.3 GB as most disk tools count).

## Run the helper

1. Download the repository as a ZIP from
   <https://github.com/tgoodyear/usnewsmap.com> (the green **Code** button, then
   **Download ZIP**) and extract it, or `git clone` it.
2. Open **PowerShell** as your normal user (not as administrator) and change to
   the extracted folder.
3. Preview what the helper sees. This reads your PC only; it changes nothing and
   doesn't use the network:

   ```powershell
   .\boinc\scripts\setup_windows_volunteer.ps1
   ```

   If Windows refuses to run the script, run `Get-ExecutionPolicy -List`. When
   the effective policy is `Restricted` and both `MachinePolicy` and
   `UserPolicy` are `Undefined`, allow scripts for this window only and try
   again:

   ```powershell
   Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned
   ```

   If the policy is anything else, don't lower it; ask whoever manages the PC. If the script is still blocked because the ZIP came from the internet,
   unblock that one file:
   `Unblock-File .\boinc\scripts\setup_windows_volunteer.ps1`.

4. Run it for real:

   ```powershell
   .\boinc\scripts\setup_windows_volunteer.ps1 -Apply -InstallBoinc
   ```

   The helper asks you to type `YES` before it downloads the official BOINC
   installer and again before it runs it, and checks the installer's checksum
   and signature first. In the installer, open **Advanced** and keep **Service
   install** off (a service can't use the GPU), and at **Finish** leave **Launch
   BOINC Manager** unchecked. If BOINC 8.2.11 or newer is already installed, the
   helper reuses it; an older one is upgraded after you exit it.

5. The helper asks before opening **BOINC Manager**. There,
   [set limits](README.md#set-limits) and
   [join the project](README.md#join-the-project). Then press Enter in
   PowerShell, and the helper confirms the PC is attached.

## Check that it works

Any time later, from the same folder:

```powershell
.\boinc\scripts\setup_windows_volunteer.ps1 -Status
```

It reports whether the PC is attached and how many usnewsmap tasks it holds, by
state. "No tasks" with a **Scheduler request completed** line in Manager's
**Event Log** (Advanced view) means you are joined and waiting for a batch. If
the GPU isn't listed in the Event Log at startup, update the GPU driver and
check that BOINC wasn't installed as a service.

[Check that it works](README.md#check-that-it-works) says what success looks
like, and [Pause, stop or leave](README.md#pause-stop-or-leave) how to stop. The
[Windows helper details](README.md#windows-helper-details) cover the helper's
options and the installer pin.
