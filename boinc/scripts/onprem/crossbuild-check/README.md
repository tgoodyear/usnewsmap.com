# Native OCR cross-build check

[#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418). Once a day, `native_crossbuild_check.py` takes 10 pages that Windows native hosts returned in the last 24 hours (seeded by the date, spread evenly over hosts), reads each scan again with the Linux build, and compares the two texts. No agent is involved.

Per page it records the CER of the Windows text against the Linux text and the character ratio Windows/Linux. Before reading, it refuses to run unless `usnm_tesseract` is a recorded Linux build's `engine_base` and each model is the locked one. A returned result that doesn't match its dispatch request (job id, scan sha256, engine), its text checksum or UTF-8, or whose scan isn't the named bytes, is reported as `invalid`, not as a build difference. A missing `spool/` or `dispatch/jobs/` is an error that records nothing, and a job is sampled once. The window is the result's `completed_at`. A page is flagged at a CER over 10%, a word-multiset F1 under 0.9, a ratio outside 0.9 to 1.1, a scan whose bytes aren't the ones the job named, or a Linux read that failed. It writes `crossbuild-<date>.json` and `state.json` to `/var/lib/usnm-crossbuild-check/`, prints one summary line to the journal, and exits 3 if any page was flagged.

The Linux read is the worker's: `usnm_tesseract <scan> <base> -l <profile> --psm 3 --dpi 300 txt alto` with `OMP_THREAD_LIMIT=1` and the locked models, from the Linux build's own `usnm_tesseract` (compare its sha256 with `engine_base` in `native-build-trust.json`; the report records it). Two pages are read at a time, one thread each, niced, in a 200% CPU quota.

## What to expect

The same engine and models differ by 1% to 8% per page across builds, mostly page layout and not characters (the 30-crop challenge gave CER 7.19% on Windows against 7.16% on Linux). Measured on 13 Nov 2026, Windows `fd620775...` against the Linux engine `21877bc3...`: CER 0.9%, 1.3%, 2.1%, 4.5% and 8.0% (a page with a segmentation miss), with character ratios of 0.99 to 1.01. So the limits are loose: ratio 0.9 to 1.1, word F1 0.9 (insensitive to column order), CER 10%. Each daily JSON records the CER, F1 and ratio distributions (`summary.*_distribution`); tighten the limits after two weeks of them.

## Escalation

- A flagged day: with `TRACKING_ISSUE` set the script appends the flagged pages to that issue through `gh` (needs a `gh` login or `GH_TOKEN` for the service user; failure is logged and not fatal).
- Two flagged days in a row: the summary line ends `PAUSE-REVIEW`. That is the cue to ask the Compute Coordinator to pause the native plan. The check pauses nothing.

## Install

Runs on CT 211 (usnm-scanfetch, on pve-big), which has `/tank/ocr/scans` locally. As root:

1. `useradd --system usnm-crossbuild`. Copy `native_crossbuild_check.py`, `boinc/project/native-build-trust.json` and `ocr/engines/tesseract/models.lock` to `/opt/usnm-crossbuild/`. Put the Linux native artifact's `usnm_tesseract` (sha256 `21877bc3...`) in `/opt/usnm-crossbuild/linux-build/` and the locked models there as `tessdata/<model>.traineddata` (at least `eng`), plus `tessdata/configs/alto` (`tessedit_create_alto 1`) and `tessdata/configs/txt` (`tessedit_create_txt 1`). The service user needs read access to all of it.
2. `/etc/usnm-crossbuild-check.env`:
   ```
   HANDOFF_DIR=<directory with spool/<job>/ and dispatch/jobs/<job>/ for native-feed-5's collected results>
   SCANS_DIR=/tank/ocr/scans
   TRACKING_ISSUE=1911
   CER_LIMIT=0.10
   F1_LIMIT=0.90
   ```
   `HANDOFF_DIR` has the layout the bridge's `spool-out` step writes (`spool/<id>/host_id`, `usnm_<id>__file_result.json`, `usnm_<id>__file_text.txt`) next to the dispatch records (`dispatch/jobs/<id>/request.json`). Read it from where the plan's collect step leaves it; the check opens no new path.
3. Copy the two units and `usnm-crossbuild.slice` to `/etc/systemd/system/`, then `systemctl daemon-reload && systemctl enable --now usnm-crossbuild-check.timer` (06:30 daily, Nice=19, CPUQuota=200%). Try it with `systemctl start usnm-crossbuild-check.service` and read `journalctl -u usnm-crossbuild-check`. `gh` for the issue comments needs a login or `GH_TOKEN` for the service user; without it the check still writes its JSON.

## Second instance: Linux replication check

A same-build check of Linux native while Linux results have no other cross-check: `usnm-crossbuild-linux.service` and `.timer` (07:00 daily) sample plan native-feed-4 results from host 13 whose `engine.image` starts with `4d6e2a3b` and re-read them with the identical Linux build (`/opt/usnm-crossbuild`, the same `usnm_tesseract`). The same build should reproduce its own text almost exactly, so the limits are tight: CER at most 0.01, ratio 0.98 to 1.02, word F1 at least 0.98.

Instances differ only in selector, limits, state and tag (the script takes each as a flag, with an `XB_*` env fallback: `--image`/`XB_IMAGE`, `--host`/`XB_HOSTS`, `--plan`/`XB_PLAN`, `--cer-limit`, `--f1-limit`, `--ratio-min`, `--ratio-max`, `--label`). With no selector the script samples the Windows builds, as above. `--image` takes a digest prefix and must match a build in `native-build-trust.json`.

|                                                    | Windows                          | Linux replication                |
| -------------------------------------------------- | -------------------------------- | -------------------------------- |
| Units                                              | `usnm-crossbuild-check`          | `usnm-crossbuild-linux`          |
| Time                                               | 06:30                            | 07:00                            |
| Env file                                           | `/etc/usnm-crossbuild-check.env` | `/etc/usnm-crossbuild-linux.env` |
| State dir (`state.json`, `crossbuild-<date>.json`) | `/var/lib/usnm-crossbuild-check` | `/var/lib/usnm-crossbuild-linux` |
| Syslog tag                                         | `usnm-crossbuild-check`          | `usnm-crossbuild-linux`          |
| Summary line starts                                | `crossbuild <date>`              | `crossbuild [linux] <date>`      |

Both services run in `usnm-crossbuild.slice` (200% CPU, 4 GB in all), so overlapping runs share the two-core budget instead of adding up. Streaks (`PAUSE-REVIEW` after two flagged days) and the checked-job list live in each state dir, so they never mix. The Linux instance's `/etc/usnm-crossbuild-linux.env`:

```
HANDOFF_DIR=<directory with spool/ and dispatch/jobs/ for native-feed-4's collected results>
SCANS_DIR=/tank/ocr/scans
TRACKING_ISSUE=1911
CER_LIMIT=0.01
F1_LIMIT=0.98
RATIO_MIN=0.98
RATIO_MAX=1.02
IMAGE_PREFIX=4d6e2a3b
HOST_ID=13
PLAN=native-feed-4
```

Install (after the handoff pre-step below): the first install step above already provides `/opt/usnm-crossbuild`; add the env file, copy `usnm-crossbuild.slice`, `usnm-crossbuild-linux.service` and `.timer` to `/etc/systemd/system/`, then `systemctl daemon-reload && systemctl enable --now usnm-crossbuild-linux.timer`, and try `systemctl start usnm-crossbuild-linux.service` and `journalctl -t usnm-crossbuild-linux`. The handoff directory is filled by the pre-step drop-in below.

Tests: `python3 -m unittest boinc/scripts/test_native_crossbuild_check.py`.

## Handoff pre-step (CT 211)

The check reads `HANDOFF_DIR`; on CT 211 a pre-step fills it from the Azure `boinc-handoff` blob store before each run. `fetch_handoff.py` downloads with `az storage blob download-batch --auth-mode login`, that is the machine's own managed identity (the one the BOINC controller reads the store with). No SAS, keys or connection strings, and nothing is written to the store. It downloads every `spool/*/*__file_result.json` manifest, keeps those whose `engine.image` is selected and completed within `--days` (newest first, up to `--max-jobs`), then copies each kept job's `spool/<job>/` files and `dispatch/jobs/<job>/request.json` into `--dest`, replaced as a whole.

Selection: with no `--image`, the Windows builds in `native-build-trust.json`; with `--image <prefix>`, the recorded builds that digest prefix matches (any platform); with `--host <id>` (repeatable), only jobs whose `spool/<job>/host_id` matches. The handoff data has no plan name, so there is no plan selector: the image and host pair identify the plan.

Install, as root, after the units are in place (copy `fetch_handoff.py` to `/opt/usnm-crossbuild/` and run `az login --identity` once as root so `/root/.azure` holds the identity):

1. Copy each instance's drop-ins into `/etc/systemd/system/`: `usnm-crossbuild-check.service.d/handoff.conf`, `usnm-crossbuild-check.timer.d/eastern.conf`, `usnm-crossbuild-linux.service.d/handoff.conf` and `usnm-crossbuild-linux.timer.d/eastern.conf`. The `handoff.conf` files add `ExecStartPre=+` (root, outside the unit's sandbox; `AZURE_CONFIG_DIR=/root/.azure` and, for that step only, `HOME=/root` so `az` finds the identity while the check itself keeps the service user's `HOME` for `gh`) and `TasksMax=128` for `az`'s threads. The Windows instance fetches into `/var/lib/usnm-crossbuild/handoff` and the Linux one (`--image 4d6e2a3b --host 13`, last 2 days, at most 30 jobs) into `/var/lib/usnm-crossbuild-linux/handoff`. Each `eastern.conf` resets `OnCalendar` and sets 06:30 and 07:00 in `America/New_York`, whatever the host's time zone.
2. Set `HANDOFF_DIR` in each env file to the same directory as its drop-in's `--dest` (`/var/lib/usnm-crossbuild/handoff` for the Windows instance, `/var/lib/usnm-crossbuild-linux/handoff` for the Linux one).
3. `systemctl daemon-reload && systemctl restart usnm-crossbuild-check.timer usnm-crossbuild-linux.timer`; check `systemctl list-timers 'usnm-crossbuild*'`.
