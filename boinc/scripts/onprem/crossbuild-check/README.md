# Native OCR cross-build check

[#1418](https://github.com/tgoodyear/usnewsmap.com/issues/1418). Once a day, `native_crossbuild_check.py` takes 10 pages that Windows native hosts returned in the last 24 hours (seeded by the date, spread evenly over hosts), reads each scan again with the Linux build, and compares the two texts. No agent is involved.

Per page it records the CER of the Windows text against the Linux text and the character ratio Windows/Linux. A page is flagged at a CER over 1%, a ratio outside 0.9 to 1.1, a scan whose bytes aren't the ones the job named, or a Linux read that failed. It writes `crossbuild-<date>.json` and `state.json` to `/var/lib/usnm-crossbuild-check/`, prints one summary line to the journal, and exits 3 if any page was flagged.

The Linux read is the worker's: `usnm_tesseract <scan> <base> -l <profile> --psm 3 --dpi 300 txt alto` with `OMP_THREAD_LIMIT=1` and the locked models, from the Linux build's own `usnm_tesseract` (compare its sha256 with `engine_base` in `native-build-trust.json`; the report records it). Two pages are read at a time, one thread each, niced, in a 200% CPU quota.

## What CER to expect

The 1% default is the design's. Windows against Linux on whole pages measured on 13 Nov 2026 (build `fd620775...` against the Linux build's own engine, `usnm_tesseract` sha256 `21877bc3...`): CER 0.9%, 1.3%, 2.1%, 4.5% on four pages and 8.0% on one with a segmentation miss, with character ratios of 0.99 to 1.01. The builds match on the challenge crops (CER 7.19% against 7.16%) and differ on page layout, so a 1% limit will flag most days. `CER_LIMIT` sets it; pick it from a few weeks of reports. The ratio band is the sharper test for a lost region.

## Escalation

- A flagged day: with `TRACKING_ISSUE` set the script appends the flagged pages to that issue through `gh` (needs a `gh` login or `GH_TOKEN` for the service user; failure is logged and not fatal).
- Two flagged days in a row: the summary line ends `PAUSE-REVIEW`. That is the cue to ask the Compute Coordinator to pause the native plan. The check pauses nothing.

## Install

On the host that has the Linux build, the handoff directory and the scans (read-only):

1. `useradd --system usnm-crossbuild`; copy `native_crossbuild_check.py` and `boinc/project/native-build-trust.json` to `/opt/usnm-crossbuild-check/`; put the Linux build's `usnm_tesseract` and a `tessdata/` (the locked models and a `configs/` with `alto` and `txt`) in `/opt/usnm-crossbuild-check/linux-build/`.
2. `/etc/usnm-crossbuild-check.env`:
   ```
   HANDOFF_DIR=<directory with spool/ and dispatch/jobs/>
   SCANS_DIR=<directory holding keep/>
   TRACKING_ISSUE=<issue number>
   CER_LIMIT=0.01
   ```
3. Copy the two units to `/etc/systemd/system/`, then `systemctl daemon-reload && systemctl enable --now usnm-crossbuild-check.timer`. Try it once with `systemctl start usnm-crossbuild-check.service` and read `journalctl -u usnm-crossbuild-check`.

Tests: `python3 -m unittest boinc/scripts/test_native_crossbuild_check.py`.
