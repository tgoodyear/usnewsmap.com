# OCR bundles: several pages in one `usnm_ocr` workunit

Issue [#1059](https://github.com/tgoodyear/usnewsmap.com/issues/1059). Without bundles, `usnm_ocr`
sends one page per workunit, so every page pays for its own container start, scheduler request,
download, upload, report, validation and assimilation. A bundle puts up to 32 pages of one
queue into one workunit. Only the BOINC unit is shared: each page keeps its claim, export,
ledger reservation, dispatch record, hand-off spool, trust and gold check, and publication.

Bundling is off until a controller plan sets `pages_per_unit`. Without it, the controller,
bridge and worker take the same single-page path as before.

## Measurement

Measured on 24 October 2026 from CT 230's database and spool (read-only): 975 successful
`usnm_ocr` results over 14 days, each BOINC `elapsed_time` set against the worker's own
Tesseract time (`elapsed_ms` in its `result.json`).

| Host | CPU                  | Results | Median elapsed (s) | Median Tesseract (s) | Median overhead (s) | Overhead share |
| ---- | -------------------- | ------: | -----------------: | -------------------: | ------------------: | -------------: |
| 1    | Apple M1, 8 threads  |     373 |               66.8 |                 54.5 |                 9.5 |            15% |
| 2    | i7-7700K, 12 threads |     371 |              281.7 |                272.8 |                11.9 |             4% |
| 10   | i7-7700K, 2 threads  |     149 |              117.7 |                102.8 |                13.2 |            13% |
| 9    | i7-1185G7, 6 threads |      82 |               50.4 |                 40.8 |                 9.5 |            18% |
| All  |                      |     975 |               87.1 |                 75.8 |                10.8 |             7% |

In-task overhead is a nearly fixed 9.5–13 s a task. That leaves out what happens outside the
task (scheduler, transfers, report, validation, assimilation and one hand-off record per
workunit). The chosen sizes are 10 pages a unit for `ocr-bulk` and 1 for `ocr-priority`, so
priority latency is unchanged. A bulk bundle runs about 9–10 minutes on hosts 1 and 9 and
about 45 minutes on host 2. Staged scans average 5.2 MiB, so a 10-page bundle is about
55 MiB to download.

## Layout

The app and plan class stay the same (`usnm_ocr`, `docker`). A bundle uses its own templates
and its workunit is named `usnm_ocrb_<bundle id>`, which `^usnm_[0-9a-f]{64}$` (a single
page) never matches. The bundle id is the sha256 of the bundle version and the member job ids
in order (`usnm_ocr_contract::boinc::bundle`).

|                             | File              | What it holds                                                                                                                       |
| --------------------------- | ----------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| In (`usnm_ocr_bundle_in`)   | `pages`           | the scans, back to back                                                                                                             |
|                             | `ocr_bundle.json` | each page's unchanged `Job` and its scan's offset in `pages`                                                                        |
| Out (`usnm_ocr_bundle_out`) | `results.json`    | one entry per page: `ok` with the page's standard `ResultManifest` and the offsets of its text and ALTO, or `error` with the reason |
|                             | `texts`           | the pages' texts, back to back (16 MiB limit)                                                                                       |
|                             | `layouts`         | the pages' ALTO files, back to back (256 MiB limit)                                                                                 |

A bundle holds 2 to 32 pages with one worker image, engine version, page deadline and queue.
Its scans total at most 512 MiB.

## The worker

`usnm-boinc work` sees `ocr_bundle.json` and runs `bundle::work`. It sets up Tesseract once,
then reads the pages in order. For each page it checks the scan's sha256, the models and the
route, as for a single page. After each page it saves that page's outputs and
`ocr_checkpoint.json` in the slot directory and writes `fraction_done`. A task the client
restarts resumes at the next page. It ignores a checkpoint from another bundle, or one whose
saved outputs changed. A page that fails (Tesseract error, output over its limit, bad ALTO,
wrong scan, refused model) is returned as `error`, and the rest still run. Only an unusable
unit fails the whole task: a missing or short `pages`, an invalid manifest, or no Tesseract.

## The server

- **Submission.** `submit-remote` takes `JOB_UNITS=<id>+<id>...,<id>` and
  `BUNDLE_APP_VERSION`. It pulls every page from the hand-off container as before, then runs
  `usnm-boinc submit-bundle` for the ones still reserved. That command writes the bundle's
  inputs under `<hand-off root>/bundles/<id>/` and into the download tree. It moves every
  member to `submitting` with `bundle` set on its server record, then runs stock `create_work`
  with `--app_version_num` set to the version whose worker reads bundles, so an older worker
  never gets one. Delay bound and priority come from the queue as for a single page;
  `rsc_fpops_est`, `rsc_fpops_bound` and `rsc_disk_bound` scale with the bundle. A group whose
  scans pass 512 MiB is cut into more bundles. A group of one, and a page taken out of an
  earlier bundle (`unbundled`), go as single-page units.
- **Inventory.** `batch-inventory` also reads `usnm_ocrb_` workunits and spools and reports
  them as their pages. A bundle workunit's pending and existing state, and its spool once it
  is assimilated, go to each page whose server record names it. So the controller's counts,
  holds and checks still work page by page.
- **Spool-out.** For a bundled page (`put-spool --root`), `spool-out` reads that page's entry
  from the bundle's assimilated files, reading only that page's ranges of `texts` and `layouts`, checks it against the page's
  job, and writes the
  standard `spool/<id>/usnm_<id>__file_{result.json,text.txt,layout.xml}`, with the bundle's
  canonical host as the page's host. The hand-off `collect`, the trust and gold checks, and
  publication don't change.
- **Partial failure.** The page is requeued if its entry is `error`, if its bytes don't match
  its manifest, or if the whole bundle failed on BOINC (`_error` or `_no_output_files`).
  `spool-out` puts it back to `reserved` on the server, marks it `unbundled`, and writes
  `spool/<id>/requeued`. The hand-off `collect` sees only that marker, changes nothing and
  succeeds. The dispatch record stays `reserved`, so the next tick submits the page again as a
  single-page unit. If that unit fails too, the page ends as a terminal failure, as it would
  have before.
- **Reconciliation.** An uncertain bundle submission leaves every member `submitting`, and
  the controller stops as it does for a page. `usnm-boinc status` names the bundle's
  workunit. Reconciling any member reconciles all of them; `--found absent` also takes them
  out of the bundle.
- **Validation.** `sample_trivial_validator` and `batch_collect_assimilator` are unchanged.
  Per-page validation is the hand-off `collect`'s, as before.

## The controller

Two optional fields in the controller config:

```json
"pages_per_unit": {"ocr-bulk": 10, "ocr-priority": 1},
"bundle_app_version": 106
```

`pages_per_unit` takes 1 to 32 per queue; a queue left out gets 1. `bundle_app_version` is
required once any value is over 1. The tick groups the pages it would submit by queue (from
each job's `reason`) and splits each group into units of at most that size. Admission stays
counted in pages. Both fields are part of the configuration's hash, so turning bundles on
makes a new plan, with fresh state.

Before it submits bundles, the tick checks `bundle_app_version` against the inventory's
`ocr_app_versions` (#1687): the `usnm_ocr` versions not deprecated in the database, and the
app's `min_version`. It refuses, after collection and before any submission, when the pinned
version is held, isn't published, or is outranked on a platform where it is published, since
the scheduler would never send those units. Single-page units carry no pin and aren't checked.

## Rollout runbook (waits for the maintainer's go)

Claim each production step on #529 first, and release the claim afterwards. Times are US
Eastern.

1. **Merge and build.** After this PR merges (crate changes roll the prod API), build
   the worker image from `boinc/apps/Containerfile` and the bridge image from
   `boinc/images/bridge/Containerfile` at the merge commit. Record the worker
   image's full digest.
2. **Release the worker as a new `usnm_ocr` app version** (boinc/docs/app-release.md):
   for every platform the current version 105 has (`x86_64-pc-linux-gnu`,
   `arm64-apple-darwin`, `x86_64-apple-darwin`), with the signing host, `--what-if` first.
   Use the next version number (106 unless one is already taken; check the `app_version`
   table). Since #1687, publish it held and release it as in
   [Rotating a plan onto a new worker version](#rotating-a-plan-onto-a-new-worker-version).
   Then check the feeder sends 106 for a single-page unit. The new worker runs
   single-page units exactly as 105 did, so nothing else changes yet.

   **Host trust starts over with the new worker image.** `gold/trust.json` lists trusted hosts
   under the full worker digest ([gold-check.md](../boinc/docs/gold-check.md)), so until a host
   passes gold on the new image, `collect` holds its results. Before the server image is built,
   add the new digest to `boinc/project/gold-engine-platforms.json` with each architecture's
   `USNM_TESSERACT_IMAGE` (read from the published image configs). Release a `usnm_gold`
   version whose Dockerfile pins the same worker. The English threshold carries over only if a
   project host scores challenge `d8cd7279…` with the same per-crop edit counts on both images
   ([boinc-native-ocr.md](boinc-native-ocr.md), "Threshold carry-over"). Then point the plan's
   `gold_onboarding` at a campaign prepared for the new image, with `engine_base` set to its
   amd64 engine. Gold onboarding matches hosts by the campaign's exact engine base, and the
   controller takes one scope per language and profile, so it never reaches arm64 hosts.
   Calibrate those with a separate campaign on the arm64 engine, submitted and promoted by
   hand, as `current247c-arm-calibration-20261017` was for `247c`.

3. **Server.** Stop CT 211's controller timer and wait for any running tick to finish. Move
   CT 230 to the new bridge image, as in the 24 October image switch on #529 (stop the server,
   `refresh-project-code.py apply` with a backup, point `SERVER_IMAGE` at it, start, check
   `get_project_config` and the daemons). Install main's `onprem-step.sh` as
   `usnm-boinc-step`. Then:
   - `batch-inventory` from the new image must still list the in-flight single-page jobs as
     before. Run `usnm-boinc-step` `batch-inventory` and compare `pending_jobs` with the
     previous tick's.
   - The bundle templates go into `<project>/templates/` on the first `JOB_UNITS` submission
     (`submit-remote` copies them).
4. **Hand-off image.** `collect` must understand `get-spool`'s `requeued` before any bundle can
   fail. With `--onprem-handoff collect` (#967), collect runs from CT 230's `SERVER_IMAGE`, so
   step 3 covers it. Prod has had no Azure hand-off job since 6 November 2026 (#308); only an
   environment whose settings still name `USNM_BOINC_HANDOFF_IMAGE` needs it set to the new
   bridge image.
5. **Canary plan.** Start a new plan on CT 211, `queue-feed-<n+1>`, once the old plan has
   drained or is left to drain under its own state. Only the plan name, image and the fields
   below change; keep everything else as in the live config:

   - `image`: the new worker image digest from step 1;
   - `pages_per_unit`: `{"ocr-bulk": 10, "ocr-priority": 1}`;
   - `bundle_app_version`: the version from step 2;
   - `pages_per_host`: 20 (two bundles a host) and `max_pending`: 80, so a 10-page unit
     doesn't hold all of admission;
   - `batch_size`: 20, so one tick claims and submits two bulk units.

   Run one `--apply` tick by hand with the timer stopped. Check that it submits
   `usnm_ocrb_` workunits, that a host picks one up, that `fraction_done` advances page by
   page in its BOINC Manager, and that the result spools, splits, collects and publishes each
   page. Then resume the timer.

6. **Watch.** Pages per hour, `collection_held`, and requeued pages (the server's records with
   `unbundled`, and `requeued` markers under `spool/`). To roll back, start a plan without
   `pages_per_unit`. Bundles already in flight finish and collect under the new images.
   Rolling back the images too would strand them, so let them drain first.

## Rotating a plan onto a new worker version

A plan pins its bundles to `bundle_app_version`. Publishing a newer `usnm_ocr` version makes
the feeder stop loading the pinned one, so the plan's unsent bundles stop going out. Host
trust is keyed by worker digest, so it starts over on a new image. On 9 November both
happened during one roll (#1687, #529). Since then a new `usnm_ocr` or `usnm_gold` version is
published held, and released in the rotation step below
([app-release.md](../boinc/docs/app-release.md#held-versions)). Claim each production step on
#529 first. Steps in this order:

1. **Publish held.** Release the new `usnm_ocr` and `usnm_gold` versions for every platform
   the current ones have, with `--hold` (`--what-if` first). Hosts see no change: the
   versions are deprecated and the live plan keeps its pin.
2. **Server.** Switch CT 230 to a server image built from a commit that has this change and
   binds the new worker in `gold-engine-platforms.json`. A gold release refuses without the
   binding. An image without this change has no `ocr_app_versions` in its inventory (the
   controller only warns) and no `app-version-hold.php` (copy it in with `podman cp`).
3. **Rotation window.** On the feed monitor host, defer the alerts that a rotation trips:

   ```sh
   python3 /opt/usnm-feed-monitor/feed_status.py --rotation-start \
     --state-dir /var/lib/usnm-boinc-feed-health --minutes 180 --reason "<#529 claim>"
   ```

4. **Gold first.** Release the new `usnm_gold` versions with `app-version-hold.php --action
   release`. In the same step, point the live plan's `gold_onboarding` entry at the new worker:
   its `campaign` prepared for the new image, `engine_base` its amd64 engine, and `image` the
   new worker digest (`sha256:...`). Without `image` the scope onboards onto the plan's own
   `image`, the old worker. `gold_onboarding` is outside the allocation digest, so this isn't a
   new plan, and the plan's `image` stays the old one:

   ```json
   "gold_onboarding": [
     {"campaign": "current8404-amd64-calibration-20261109", "language": "eng", "profile": "eng",
      "engine_base": "sha256:942f5e02…", "image": "sha256:840487b2…"}
   ]
   ```

   Calibrate arm64 hosts by hand, as before. Wait until `gold-promote` has added the project's
   hosts on the new digest in `gold/trust.json`. Meanwhile the live plan keeps feeding OCR on
   the old version, and every collected result comes from a host trusted on the old image.
   Gold units already out for the old image are refused from the release on; let them expire.
5. **Rotate OCR.** Stop CT 211's controller timer between ticks, then, together:
   1. `app-version-hold.php --action min-version --min-version <old pin>`. The feeder then
      keeps the old version loaded beside the new one, so the old plan's unsent bundles still
      go out while it drains.
   2. `app-version-hold.php --action release` for each new `usnm_ocr` app version. It refuses
      while a change would strand unsent units pinned to a version.
   3. Start the new plan: the live config with only the plan name, `image` and
      `bundle_app_version` changed (`gold_onboarding` stays as step 4 set it). Archive the old
      plan to drain.
   4. Run one `--apply` tick by hand, check that it submits bundles and that a host takes one,
      then turn the timer back on.
6. **Close the window** once ticks are clean and `cpu_shortage` is healthy:
   `feed_status.py --rotation-end --state-dir /var/lib/usnm-boinc-feed-health`.
7. **Later.** Once no workunit pinned to the old version is unsent or in progress, raise
   `min_version` to the new pin, or leave it. The hold tool refuses a raise that strands work.

Rollback: hold the new `usnm_ocr` versions again, which brings the old one back, and revert to
the old plan. A held version stays published.

### What fires during a rotation

| Check | Fires when | During a rotation | With the window |
| --- | --- | --- | --- |
| `collection` | a source is unknown for 3 minutes | the controller's timer stopped, or a tick by hand, for more than 15 minutes ("no recent completed controller tick") | a missing controller tick doesn't count; any other unknown source still fires |
| `cpu_shortage` | own clients have more idle CPU slots than eligible unsent results for 30 minutes | no refill while the timer is stopped, or while ticks fail | held until the window ends, then fires if still true |
| `usnm_ocr_no_work` | OCR has a backlog and no new workunit for an hour | the same | held until the window ends |
| `controller_tick` | two failed ticks in a row | collect stops each tick on a result from a host not yet trusted on the new image | not covered: step 4 is what prevents it |

The window lasts at most 240 minutes, and every other check fires as usual inside it. A host
with only an older version installed no longer makes the eligibility check unavailable
(`collection` fired on 9 November for that): the collector counts it for the version the
scheduler would send it.

## Not changed

Claims, the export, the ledger, the hand-off record and its publication, trust and gold checks,
the validators and assimilators, the plan class, and every other app. Priority pages still go
one per unit unless `ocr-priority` is set above 1.
