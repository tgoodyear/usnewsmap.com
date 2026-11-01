# OCR output copy on tank

These files copy `curated/ocr-local/` from the lake to a dataset on `tank` every day. The lake's copy is one Standard_LRS account, and this output takes real work to make again. `requests/` is left out: it's rewritten in place, and it can be made again from the `ocr-requests` container.

It runs on the search log copy's host, as that host's identity and user. It reads nothing from `tank` except its own dataset. The BOINC server never mounts this dataset.

## Files

| File                          | What it does                                                                                    |
| ----------------------------- | ----------------------------------------------------------------------------------------------- |
| `usnm-ocr-local-copy.service` | Runs `usnm-ingest ocr-local-copy`, with write access to the dataset only                        |
| `usnm-ocr-local-copy.timer`   | Starts the service at 03:30 UTC every day, with up to 20 minutes' delay, and after a missed run |
| `test_units.py`               | Checks the units against the command's flags. Runs in CI and locally                            |

The command is `crates/usnm-ingest/src/cli/ocr_local_copy.rs` and the logic `crates/usnm-ingest/src/ocr_local_copy.rs`.

## Setup

1. **The dataset and mount.** home-infra's `searchlog-copy-ct.sh --ocr-local-copy` creates `tank/azure/lake-alto` (`zstd-3`, 128K records), its snapshots (daily 14, weekly 8, monthly 12) and the bind mount.
2. **The grant.** `USNM_OCR_LOCAL_COPY` is on in `infra/env/prod.json`, so a provision creates it. `infra/modules/ocr-local-copy.bicep` allows read and list on `ocr-local/` only.
3. **The binary.** Install a reviewed `usnm-ingest` as for the search log copy.
4. **Seed.** On the host holding `tank`, copy what's already there, so only the gap crosses the network:

   ```sh
   rsync -a --ignore-existing --chmod=F440,D750 --chown=102100:102101 \
     --exclude requests/ <existing ocr-local copy>/ /tank/azure/lake-alto/
   ```

5. **First run.** Check the seeded bytes against the lake's Content-MD5, then enable the timer. `--verify` lists the lake, reads only the local copy, and copies nothing:

   ```sh
   sudo -u usnm-searchlog sh -c 'set -a; . /etc/usnm/ocr-local-copy.env; set +a; USNM_CURATED_URL=/var/empty USNM_REFERENCE_URL=/var/empty USNM_STATE_FILE=/var/lib/usnm-ocr-local-copy/unused-state.json usnm-ingest ocr-local-copy --to /tank/azure/lake-alto --status /var/lib/usnm-ocr-local-copy/status.json --verify'
   systemctl enable --now usnm-ocr-local-copy.timer
   ```

   Objects the seed didn't cover are reported as missing, and fail this check once they're older than 48 hours. The timer's run copies them; `systemctl start usnm-ocr-local-copy.service` does it sooner. Read `mismatched_objects` in the status file: only those need a person.

## The alert

A run fails, and the unit with it, when an object is still missing, different or unverified more than 48 hours after it was written. Only `--verify` reports unverified objects: those listed with no Content-MD5, which it can't check. `/var/lib/usnm-ocr-local-copy/status.json` has `ok`, the counts, the first overdue paths and every path that differs. home-infra's watcher reads it each hour.

A copy that differs is left alone. Move it aside by hand and run the copy again. The daily run checks sizes only, but it reads `mismatched_paths` from the last status file and hashes those copies again, so a same-sized mismatch stays reported until it's fixed. That reads local files only. Keep the status file: deleting it forgets them.
