# macOS BOINC quick start

Donate your Mac's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. You install the official BOINC client, join the
project in BOINC Manager, and run one helper script that checks the Mac and
sets sensible limits. Nothing else to install: no Docker, Podman or virtual
machine.

**Availability:** this is a limited pilot. Work goes out in small batches, so a
joined Mac can sit idle for a while. That is normal.

## 1. What you need

- An Apple Silicon Mac (M1 or later) or an Intel Mac, on a current macOS.
- 2 GB of free disk for the CPU app. For the GPU app, an Apple Silicon Mac
  with at least 8 GB of memory and 12 GB of free disk; it runs on the Mac's
  own GPU, so there is nothing to install for it. Intel Macs do CPU work only.
- A plugged-in Mac. BOINC does not compute on battery.

## 2. Install BOINC and join the project

1. Install the **recommended** macOS BOINC client from
   <https://boinc.berkeley.edu/download.php> (version 8.2.11 at the time of
   writing). One download works for both Apple Silicon and Intel. Do not use a
   development or beta build.
2. Open **BOINC Manager** once. Its first-run window may ask you to add a
   project; you can do that here or in step 4.
3. Create an account at <https://boinc.usnewsmap.com/> (or sign in to yours).
   In **Project preferences** choose whether to use the GPU, the CPU or both.
4. In Manager, **Tools → Add project**, enter `https://boinc.usnewsmap.com/`
   and sign in with that account. Type your password only in Manager or on the
   project website, never into a script or a support message.

If the Mac is already attached, do not remove and re-add the project.

## 3. Run the helper

1. Download the repository as a ZIP from
   <https://github.com/tgoodyear/usnewsmap.com> (the green **Code** button,
   then **Download ZIP**) and unzip it, or `git clone` it.
2. Open **Terminal** and change to the unzipped folder.
3. Preview what the helper sees. This only reads the Mac and prints what it
   would change:

   ```sh
   python3 scripts/boinc/setup_mac_volunteer.py
   ```

   The first run of `python3` on a Mac may offer to install Apple's command
   line developer tools; accept, then run the command again. The output is
   JSON: check that `project_attached` is `true` and read `eligible_apps`.

4. Apply it:

   ```sh
   python3 scripts/boinc/setup_mac_volunteer.py --apply
   ```

   This writes BOINC's computing preferences so a task can hold its model in
   memory: up to 75% of the CPU cores and 75% of memory while you use the Mac,
   85% of memory while it is idle, and never on battery. These limits apply to
   every BOINC project on this Mac; if you run others, review them in
   Manager's **Options → Computing preferences** afterwards. The helper then
   asks the project for work. It runs as your normal user; if it reports a
   permission error on the BOINC Data folder, run the same command with
   `sudo`.

## 4. Check that it works

Run the preview again any time:

```sh
python3 scripts/boinc/setup_mac_volunteer.py
```

`project_attached` should be `true`. In Manager's **Event Log** (Advanced
view, **Tools → Event Log**), a **Scheduler request completed** line means you
are joined and waiting for a batch. A task that downloads, runs and uploads is
a success; its validation shows under **Tasks** on your account page. On an
Apple Silicon Mac the Event Log also lists the GPU at startup; without that
line the Mac gets CPU work only.

## 5. Pause, stop or leave

- **Pause:** Manager's **Activity** menu, or the project's **Suspend** button.
- **Stop taking work:** select the project and click **No new tasks**; running
  tasks finish and report.
- **Leave:** select the project and click **Remove**. Delete your account on
  the website if you also want that gone.

BOINC downloads signed apps and models by itself and returns results and
diagnostics. Never copy in executables or models or edit `app_info.xml`; if a
task fails, BOINC reports it and the project sees it.

The [full volunteer reference](boinc-volunteer.md#12-macos-arm64-apple-darwin-apple-silicon-and-x86_64-apple-darwin-intel)
has the helper's options, the container apps that need Podman, and
[what the project sees from your Mac](boinc-volunteer.md#4-resources-network-and-privacy).
