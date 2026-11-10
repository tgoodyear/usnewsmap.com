# macOS BOINC quick start

Donate your Mac's spare time to historical-newspaper OCR at
<https://boinc.usnewsmap.com/>. You install the official BOINC client, join the
project in BOINC Manager, and run one helper script that checks the Mac and sets
its limits. You don't need Docker, Podman or a virtual machine.

This is a pilot. Work goes out in small batches, so a joined Mac can sit idle
for a while.

## What you need

- An Apple silicon Mac (M1 or later) or an Intel Mac, on a current macOS.
- For GPU work, an Apple silicon Mac with at least 8 GB of memory. The app runs
  on the Mac's own GPU, so there is nothing to install for it. Intel Macs do CPU
  work only.
- Free disk that BOINC may use: 2 GiB for the quality app and 18 GiB for the
  Qwen GPU app (about 2.1 and 19.3 GB as Finder counts).
- A plugged-in Mac. BOINC doesn't compute on battery.

## Install BOINC and join

1. Install the **recommended** macOS BOINC client from
   <https://boinc.berkeley.edu/download.php> (8.2.11 on 10 November 2026). One
   download works on Apple silicon and Intel. Don't use a development or beta
   build.
2. Open **BOINC Manager** once. Its first-run window may offer to add a project;
   you can do that now or in the next step.
3. [Join the project](README.md#join-the-project).

## Run the helper

1. Download the repository as a ZIP from
   <https://github.com/tgoodyear/usnewsmap.com> (the green **Code** button, then
   **Download ZIP**) and unzip it, or `git clone` it.
2. Open **Terminal** and change to the unzipped folder.
3. Preview what the helper sees. This only reads the Mac and prints what it
   would change:

   ```sh
   python3 boinc/scripts/setup_mac_volunteer.py
   ```

   The first run of `python3` may offer to install Apple's command line
   developer tools; accept, then run the command again. The output is JSON:
   check that `project_attached` is `true`, and read `eligible_apps`.

4. Apply it:

   ```sh
   python3 boinc/scripts/setup_mac_volunteer.py --apply
   ```

   This writes BOINC's computing preferences so a task can keep its model in
   memory: up to 75% of the CPU cores and 75% of memory while you use the Mac,
   85% of memory while it is idle, and never on battery. These limits apply to
   every BOINC project on the Mac; if you run others, review them in Manager's
   **Options → Computing preferences** afterwards. The helper then asks the
   project for work. It runs as your normal user; if it reports a permission
   error on the BOINC Data folder, run the same command with `sudo`.

## Check that it works

Run the preview again any time; `project_attached` should be `true`. In
Manager's **Event Log** (Advanced view, **Tools → Event Log**), a **Scheduler
request completed** line means you are joined and waiting for a batch. On an
Apple silicon Mac the Event Log also lists the GPU at startup; without that line
the Mac gets CPU work only.

[Check that it works](README.md#check-that-it-works) says what success looks
like, and [Pause, stop or leave](README.md#pause-stop-or-leave) how to stop. The
container apps, which need Podman, are in
[macOS container apps](README.md#macos-container-apps).

### CPU quality tasks blocked by a concurrency limit

Check `projects/boinc.usnewsmap.com/app_config.xml` in BOINC's data directory. A
`project_max_concurrent` of `1` applies to all of the project's apps, so one Qwen
task can keep CPU quality work from starting indefinitely. The helper doesn't
create or remove this limit.

To allow one Qwen task alongside one quality task, back up the file first. An
otherwise empty configuration can be:

```xml
<app_config>
  <project_max_concurrent>2</project_max_concurrent>
  <app>
    <name>usnm_qwen</name>
    <max_concurrent>1</max_concurrent>
  </app>
  <app>
    <name>usnm_quality</name>
    <max_concurrent>1</max_concurrent>
  </app>
</app_config>
```

Don't overwrite other app settings or raise an intentional GPU limit. Reload it
with Manager's **Options → Read config files**, without restarting or aborting
tasks, then check the Event Log for configuration errors and the Tasks view for
a CPU task running beside at most one Qwen task. To roll back, restore the
backup and read the configuration again; don't reset the project.
