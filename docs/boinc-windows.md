# Windows BOINC quick start

Donate your PC's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. One helper script checks your PC, installs the
official BOINC client if needed, and walks you through joining. Nothing else to
install: no WSL, Docker, Podman or CUDA toolkit.

**Availability:** this is a limited pilot. Work goes out in small batches, so a
joined PC can sit idle for a while. That is normal.

## 1. What you need

- Windows 11, or Windows 10 22H2 with current updates, on an Intel or AMD
  64-bit PC. Windows on ARM and Windows Server are not supported.
- For GPU work, an NVIDIA GPU with the official NVIDIA driver 551.61 or newer
  and at least 3 GB of free video memory. AMD and Intel GPUs can only do CPU
  work.
- 6 GB of RAM and 12 GB of free disk, or 18 GB if your GPU has 7 GB or more of
  video memory.

## 2. Run the helper

1. Download the repository as a ZIP from
   <https://github.com/tgoodyear/usnewsmap.com> (the green **Code** button,
   then **Download ZIP**) and extract it, or `git clone` it.
2. Open **PowerShell** as your normal user (not as administrator) and change to
   the extracted folder.
3. Preview what the helper sees. This reads your PC only; it changes nothing
   and does not use the network:

   ```powershell
   .\scripts\boinc\setup_windows_volunteer.ps1
   ```

   If Windows refuses to run the script, allow scripts for this window only
   and try again:

   ```powershell
   Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned
   ```

   If it is still blocked because the ZIP came from the internet, unblock that
   one file: `Unblock-File .\scripts\boinc\setup_windows_volunteer.ps1`.

4. Run it for real:

   ```powershell
   .\scripts\boinc\setup_windows_volunteer.ps1 -Apply -InstallBoinc
   ```

   The helper asks you to type `YES` before it downloads the official BOINC
   installer and again before it runs it. It verifies the installer's checksum
   and signature first. In the installer, open **Advanced** and keep **Service
   install** off (a service cannot use the GPU), and at **Finish** leave
   **Launch BOINC Manager** unchecked. If BOINC is already installed, the
   helper skips this and reuses it.

5. The helper then asks before opening **BOINC Manager** and prints the steps
   to follow there (next section). When you have done them, press Enter and it
   confirms the PC is attached.

## 3. Join the project in BOINC Manager

1. **Options → Computing preferences**: compute only while the PC is idle and
   not on battery, and set CPU, memory and disk limits you are comfortable
   with. These apply to every BOINC project on this PC.
2. Create an account at <https://boinc.usnewsmap.com/> (or sign in to yours).
   In **Project preferences** choose whether to use the GPU, the CPU or both.
3. In Manager, **Tools → Add project**, enter `https://boinc.usnewsmap.com/`
   and sign in with that account. Type your password only in Manager or on the
   project website, never into the script or a support message.

If the PC is already attached, do not remove and re-add the project.

## 4. Check that it works

Any time later, from the same folder:

```powershell
.\scripts\boinc\setup_windows_volunteer.ps1 -Status
```

It reports whether the PC is attached and how many usnewsmap tasks it holds,
by state. "No tasks" with a **Scheduler request completed** line in Manager's
**Event Log** (Advanced view) means you are joined and waiting for a batch. A
task that downloads, runs and uploads is a success; its validation shows under
**Tasks** on your account page.

If the GPU is not listed in the Event Log at startup, update the NVIDIA driver
and check that BOINC was not installed as a service.

## 5. Pause, stop or leave

- **Pause:** Manager's **Activity** menu, or the project's **Suspend** button.
- **Stop taking work:** select the project and click **No new tasks**; running
  tasks finish and report.
- **Leave:** select the project and click **Remove**. Delete your account on
  the website if you also want that gone.

BOINC downloads signed apps and models by itself and returns results and
diagnostics. Never copy in executables or models or edit `app_info.xml`; if a
task fails, BOINC reports it and the project sees it.

The [full volunteer reference](boinc-volunteer.md#13-windows-windows_x86_64)
has the helper's options, the installer pins, container apps on Windows, and
[what the project sees from your PC](boinc-volunteer.md#4-resources-network-and-privacy).
