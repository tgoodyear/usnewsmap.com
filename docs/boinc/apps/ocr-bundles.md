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
2. **Release the worker as a new `usnm_ocr` app version** (docs/boinc/apps/app-release.md):
   for every platform the current version 105 has (`x86_64-pc-linux-gnu`,
   `arm64-apple-darwin`, `x86_64-apple-darwin`), with the signing host, `--what-if` first.
   Use the next version number (106 unless one is already taken; check the `app_version`
   table). Since #1687, publish it held and release it as in
   [Rotating a plan onto a new worker version](#rotating-a-plan-onto-a-new-worker-version).
   Then check the feeder sends 106 for a single-page unit. The new worker runs
   single-page units exactly as 105 did, so nothing else changes yet.

   **Host trust starts over with the new worker image.** `gold/trust.json` lists trusted hosts
   under the full worker digest ([gold-check.md](../server/gold-check.md)), so until a host
   passes gold on the new image, `collect` holds its results. Before the server image is built,
   add the new digest to `boinc/project/gold-engine-platforms.json` with each architecture's
   `USNM_TESSERACT_IMAGE` (read from the published image configs). Release a `usnm_gold`
   version whose Dockerfile pins the same worker. The English threshold carries over only if a
   project host scores challenge `d8cd7279…` with the same per-crop edit counts on both images
   ([boinc-native-ocr.md](native-ocr.md), "Threshold carry-over"). Then point the plan's
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

## Line jobs: the `docker_lines` plan class

Cherokee line jobs (#1621) need a newer worker than the live plan's. Publishing that worker as the
next `docker` version outranks the live one: stock `sched_shmem.cpp` loads, per app, platform and
plan class, only the highest undeprecated version (unless the app's `min_version` is set), so the
units pinned to the older number go unsent (#1687, 9 November). Line jobs instead get their own
`usnm_ocr` and `usnm_gold` versions under the `docker_lines` plan class
(`boinc/project/plan_class_spec.xml`). It loads beside `docker` and outranks nothing.

How the scheduler at BOINC `22bbebe0` picks a version for a unit (`sched/sched_version.cpp`,
`get_app_version`), and what that means here:

- **A pinned unit gets only its pinned number.** The request's cache of best versions is checked
  first. When the cached version's number differs from the unit's, the code is

  ```cpp
  if (wu.app_version_num) {
      if (bavp->avp->version_num != wu.app_version_num) {
          break;
      }
  }
  ```

  and the scan that follows skips every other number:

  ```cpp
  if (wu.app_version_num) {
      if (av.version_num != wu.app_version_num) {
          continue;
      }
  } else {
      if (av.deprecated) {
          continue;
      }
  }
  ```

  So a unit pinned to 106 is sent with 106 whatever the host's best version is, and a line
  job pinned to the `docker_lines` version only with that one.

- **An unpinned unit gets the fastest feasible version.** Both classes would be feasible on the
  same host, so `docker_lines` admits only batch 1621 (`min_batch` = `max_batch` = 1621;
  `plan_class_spec.cpp`, `wu_is_infeasible_for_plan_class`). Live single-page and gold units are
  batch 0, so `app_plan` refuses `docker_lines` for them and they keep the `docker` version.
- **The cache can defer an unpinned unit for one request.** If a line unit is looked at first in a
  request, its version is cached, and a later unpinned unit of the same app is handed that cached
  version. The plan class's batch check (`wu_is_infeasible_custom`) then refuses it, and it waits
  for the next request. Pinned units are not affected: they rescan, as above.
- **A host must have a feasible version for every pinned unit it is offered.** When a pinned unit
  finds none, `get_app_version` caches "no version" for the whole app, and every later unit of
  the app in that request is refused (`if (!bavp->present) return NULL;`). So `docker_lines`
  has exactly `docker`'s host gates (`boinc/project/test_config.py` checks this). Restricting it
  to a few hosts would make every other host skip its `docker` work whenever it met a line unit.
- **Targeted units never enter the feeder's array.** `--target_host` makes an assignment, and
  `sched_assign.cpp` sends it only to that host. Line jobs and their gold are submitted targeted
  at hosts calibrated on the line worker, so no other host ever looks at them.

`usnm-boinc` puts every line job, retries included, in batch 1621 (`LINE_BATCH`), pinned
(`--app-version-num`, bridge `LINE_APP_VERSION`) and optionally targeted (`--target-host`,
bridge `LINE_TARGET_HOST`). Gold for the line worker takes `gold submit --docker-lines-version N`
(bridge `GOLD_DOCKER_LINES_VERSION`): pinned, because the live `docker` gold version also
admits batch 1621, and the gold routing hook refuses a unit sent with another worker. Their
results are in `results/1621`, so give `GOLD_SPOOL` and `HANDOFF_SPOOL` that directory.

The feed monitor (`feed_collect.supply`) treats the two classes as one CPU profile: both ask
for one CPU, and each unit counts only for the version that can serve it. Targeted units count
for none.

Release the two apps with `--plan-class docker_lines --hold` and a version number neither app
has used, so a pin can't match a deprecated `docker` row: 1.8 (108) for the Cherokee roll.
Release them with `app-version-hold.php --action release` once the server image has the class.
They are their own platform and plan class group, so the release strands nothing pinned to
106 and needs no `min_version`; the hold tool's check confirms that.

## Running Cherokee line jobs (#1621)

The steps after the release, from the plan on #1621. Live since 11 November 2026:

| App         | Version   | `app_version` rows                                                          |
| ----------- | --------- | --------------------------------------------------------------------------- |
| `usnm_ocr`  | 1.8 (108) | 91 `x86_64-pc-linux-gnu`, 92 `arm64-apple-darwin`, 94 `x86_64-apple-darwin` |
| `usnm_gold` | 1.8 (108) | 93 `x86_64-pc-linux-gnu`                                                    |

Both run worker `sha256:840487b2aaa5a696dae61ded8fdcf0217cdf683e24c14e50ef0e728406aa122c`
(`840487b2…` below), which the server image binds in `gold-engine-platforms.json`. Its
Tesseract base is `sha256:942f5e0261c0cf465261515440972c1ff213ad0043831d32732895bf9c5839e5` on
x86-64 and `sha256:e27b699ef219d4fdf291d6eb2f44f49e21f838d2abf92e0b22733bc2378f36ef` on arm64.

Every step runs as root in CT 230 through `usnm-boinc-step` (`boinc/scripts/onprem-step.sh`),
which needs `--wait-mins`. Claim each production step on #529 first, and release the claim
afterwards. CT 211's controller never touches these jobs: they belong to their own plan, and
`batch_scheduler.py` skips every journal of another plan. So each step below is run by hand.
Nothing here changes the live OCR plan, its pin (106) or the `docker` versions.

**Before the first step**, CT 230 needs:

- `<enable_assignment>1</enable_assignment>` in the project's `config.xml`, since the gold units
  and the line jobs are targeted (`gold submit` refuses without it). Set on 11 November.
- `usnm-boinc-step` from a main that has the line-job export (`HANDOFF_LINE_READ`, the staged
  ALTO and manifests, the `line-report` step) and the line trust scope (#1753). Install it as
  `onprem.md` describes.
- A hand-off image with the same `usnm-boinc` and `handoff-job.sh`: a `SERVER_IMAGE` built from
  that main, or an overlay named in `/etc/usnm-boinc/handoff-image`
  ([onprem.md](../server/onprem.md), "To ship a hand-off fix without a new server image"). The
  bridge's `gold-trust` step needs the same `usnm-boinc` in the server image.

**The approved values** (the maintainer, 11 November). The evidence for each is in #1730:

| Value                                         | Approved                                                                                      |
| --------------------------------------------- | --------------------------------------------------------------------------------------------- |
| Line scope `max_cer` (`GOLD_MAX_CER`)         | `0.08734328574534386`, `eng`/`eng`'s                                                          |
| Line scope challenge (`GOLD_MANIFEST_SHA256`) | `d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f`, the English challenge     |
| Line scope evidence (`GOLD_LINE_EVIDENCE`)    | the line in step 2 below                                                                      |
| Hosts                                         | 2, 9 and 10                                                                                   |
| Plan id (`HANDOFF_PLAN`)                      | `chr-lines-1621`                                                                              |
| Test batch                                    | the 5 pages in step 3                                                                         |
| Whether the 29 test-set pages are read        | yes, in the later exports; they are not in the 5-page test batch                              |
| When to stop                                  | any `line_changed` verdict, any collect failure, or fewer than 4 of the 5 test pages admitted |

### 1. Gold on the line worker

`gold/trust.json` lists no host under `840487b2…` in any scope (checked 11 November: `eng`/`eng`
lists hosts under `247c131a…` and `dac9c123…` only). So hosts 2, 9 and 10 each need a gold
report on `840487b2…`. All three are x86-64 Linux containers, so the Linux-only `usnm_gold`
108 fits them. The reports are read in the English page configuration (`eng`/`eng`, PSM 3,
300 DPI), and they admit the hosts to the line scope only (step 2), never to `eng`/`eng`.

**One fresh campaign root per host.** The first `gold submit --target-host` for a root refuses
one that already has units ("cannot target a pre-existing campaign; use a fresh campaign
root", `gold::submit::run`). Make one root per host under the share's `gold/` directory, named
`[a-z0-9-]`, 41 characters at most, for example `lines8404-amd64-h2-20261112`. Copy
`bundle.tar`, `prepared.json` and `cache/` into it from
`current8404-amd64-calibration-20261109` (`cp -a`). Don't run `gold-prepare`: it downloads the
bundle's originals again, and LoC is refusing our requests. `gold submit` checks `bundle.tar`
against `prepared.json` and each cached original before it makes any unit; if it refuses, stop.

**Recovering a partial submit.** Never make a new root for a host whose submit failed part way:
that makes a second set of units for the host. Run the same submit again on the original root;
it skips units already marked `submitted` and the retry is allowed once the root has its
`assignment.json`. A unit left with a `submitting` marker is skipped (the run counts it as
uncertain) until someone checks the server for its workunit, `usnm_gold_<job id>`: remove the
marker if there is none, or rename it to `submitted` if there is.

**Submit**, once per host, pinned to the `docker_lines` gold version (batch 1621):

```sh
usnm-boinc-step --env prod --target bridge --step gold-submit --wait-mins 30 \
  --var GOLD_CAMPAIGN=lines8404-amd64-h2-20261112 \
  --var GOLD_ENGINE_IMAGE=sha256:840487b2aaa5a696dae61ded8fdcf0217cdf683e24c14e50ef0e728406aa122c \
  --var GOLD_ENGINE_BASE=sha256:942f5e0261c0cf465261515440972c1ff213ad0043831d32732895bf9c5839e5 \
  --var GOLD_TARGET_HOST=2 --var GOLD_DOCKER_LINES_VERSION=108
```

`GOLD_UNITS` defaults to 20 and `GOLD_ENGINE_VERSION` to 5.5.0.

**Collect** each root once its units are assimilated. The results are in `results/1621`:

```sh
usnm-boinc-step --env prod --target bridge --step gold-collect --wait-mins 30 \
  --var GOLD_CAMPAIGN=lines8404-amd64-h2-20261112 \
  --var GOLD_SPOOL=/project/usnmocr/results/1621 \
  --var GOLD_LANGUAGE=eng --var GOLD_PROFILE=eng
```

### 2. The line trust scope

A line-job scope lists its own hosts (#1753): those whose gold reports pass in the page
configuration its `line_basis` names. `gold-trust` with `GOLD_LINE_SCOPE` builds it from the
reports and writes only that entry of `gold/trust.json`. The top-level fields and every other
scope, `eng`/`eng` included, stay byte for byte as they were. `GOLD_CAMPAIGNS` adds the other
hosts' roots to the same build, so one run covers all three.

Back up the list first:

```sh
cp -p /srv/usnm-boinc/project/gold/trust.json \
  /srv/usnm-boinc/project/gold/trust.json.pre-chr-lines-$(date -u +%Y%m%dT%H%M%SZ)
```

Then, with the approved values:

```sh
usnm-boinc-step --env prod --target bridge --step gold-trust --wait-mins 30 \
  --var GOLD_CAMPAIGN=lines8404-amd64-h2-20261112 \
  --var GOLD_CAMPAIGNS=lines8404-amd64-h9-20261112,lines8404-amd64-h10-20261112 \
  --var GOLD_MAX_CER=0.08734328574534386 \
  --var GOLD_MANIFEST_SHA256=d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f \
  --var GOLD_LANGUAGE=eng --var GOLD_PROFILE=eng \
  --var GOLD_LINE_SCOPE=chr/chr_phoenix \
  --var "GOLD_LINE_EVIDENCE=#1621 chr_phoenix line route: test set v2 silver CER 10.2% vs LoC 18.0% (report 2026-11-cherokee step 3); e2e 9 Nov: 3 pages, 700 of 734 lines took the model, 3 admitted by line-merge-v1; hosts by English gold on 840487b2"
```

`usnm-boinc` loads and validates the list before it writes, and refuses a line policy in
place of a page scope. Check what changed: everything but the `chr` entry must equal the
backup.

```sh
cd /srv/usnm-boinc/project/gold
diff <(jq -S 'del(.policies[] | select(.scope.language == "chr"))' trust.json) \
  <(jq -S . trust.json.pre-chr-lines-<stamp>)
jq '.policies[] | select(.scope.language == "chr")' trust.json
```

The `chr` entry must list hosts 2, 9 and 10 under `840487b2…`, PSM 13. If anything else
differs, stop and report. Don't `mv` the backup over the list: it is a whole-file snapshot, and
`gold-promote` or a hold may have changed the list since. Keep both copies. `spool-out` copies the share's
`gold/trust.json` to the hand-off container, so the next `spool-out` makes the change live
for collection.

### 3. Stage the pages

[`phoenix-pages.jsonl`](../../reports/2026-11-cherokee/phoenix-pages.jsonl) lists the 324
_Phoenix_ pages: each page's batch, ALTO sha256, the lines `export --line-read chr` reads,
whether test set v2 holds it, and the local copy of its scan (LoC's full-size IIIF JPEG) by
sha256. It was made from LoC's two OCR archives and the scans already on our storage, without
fetching anything ([the report](../../reports/2026-11-cherokee/report.md), "Reproducing").

| Batch                   | Pages | With lines | With lines and a local scan | Of them in the test set |
| ----------------------- | ----: | ---------: | --------------------------: | ----------------------: |
| `dlc_misctopsn83020866` |    84 |         66 |                          66 |                      13 |
| `dlc_misctopsn83020874` |   240 |        125 |                         124 |                      16 |

The 133 pages without lines would be skipped by `export` anyway. One page with lines,
`sn83020874/1829-03-04/ed-1/seq-2`, has no local scan; it waits until a scan can be fetched.
The 161 pages with lines outside the test set are the pages the model's real training lines
were cut from, so `line-report` on them isn't held-out evidence.

An export takes up to 100 pages of one batch. So, in order:

1. **Test batch**, 5 pages outside the test set (approved 11 November): `sn83020866/1828-03-06/ed-1/seq-1`,
   `sn83020866/1828-03-06/ed-1/seq-3`, `sn83020866/1828-03-20/ed-1/seq-1`,
   `sn83020866/1828-03-20/ed-1/seq-2` and `sn83020866/1828-03-20/ed-1/seq-3` (648 lines).
2. The other 61 pages of `dlc_misctopsn83020866` (13 of them in the test set).
3. `dlc_misctopsn83020874` in two exports of 62 (124 pages, 16 of them in the test set).

The production run reads the 29 test-set pages too (approved 11 November). They stay out of
the test batch, and `line-report` on them is production output, not a held-out score.

`usnm-boinc line-stage` writes one export's staging tree on the host that holds the scan
copies and the unpacked ALTO. It checks each scan and ALTO against the page list, gives each
scan a sidecar in the scan fetcher's layout (its `user_agent` says it is a local copy), and
writes a prefetch manifest of the pages. It prints the export's four `HANDOFF_` values:

```sh
usnm-boinc line-stage --pages docs/reports/2026-11-cherokee/phoenix-pages.jsonl \
  --select <page keys, one per line> --scans <scan copies> [--scans ...] \
  --loc-alto <the archives' ocr.xml, unpacked> --manifest-name chr-lines-1621-<n> --out <new dir>
```

Send the tree to CT 230 from that host, never through a workstation:

```sh
tar -C <tree> -c keep loc-alto manifests |
  <to CT 230> usnm-boinc-step --env prod --target scans --step receive --wait-mins 10
```

The receive step takes only that layout. Staged files are pruned 4 days after they arrive, so
export and submit each staged batch within 4 days.

### 4. Export the line jobs

```sh
usnm-boinc-step --env prod --target handoff --step export --wait-mins 30 \
  --var HANDOFF_LINE_READ=chr --var HANDOFF_PLAN=chr-lines-1621 \
  --var ENGINE_IMAGE=sha256:840487b2aaa5a696dae61ded8fdcf0217cdf683e24c14e50ef0e728406aa122c \
  --var HANDOFF_BATCH=<from line-stage> --var HANDOFF_MANIFEST=<from line-stage> \
  --var HANDOFF_PAGES=<from line-stage> --var HANDOFF_SCANS=<from line-stage> \
  --var EXPECTED_HANDOFF_URL=<HANDOFF_URL from handoff.env>
```

Every `--target handoff` step needs `--var EXPECTED_HANDOFF_URL=<HANDOFF_URL from handoff.env>`
(this one, `collect` and `line-report`): with the local hand-off backend (`/handoff-state`),
`usnm-boinc-step` refuses the step without it, and it also refuses a value that differs from
the host's setting.

With `HANDOFF_LINE_READ`, the step mounts the staging area and reads each page's scan, its
LoC ALTO and the manifest from there; nothing comes from the hand-off container. It runs
`usnm-boinc export --line-read chr --loc-alto ...` against Cosmos (`COSMOS_ENDPOINT` in
`handoff.env`), writes the new journal entries back with their `line-read/` sidecars, and
prints the job ids. Each line job keeps LoC's ALTO beside it (`lines::LOC_ALTO_FILE`).

**If the export fails after it reserved some jobs**, don't run it again as it stands: a second
run can make a different `queued_at` for the same job, which the ledger rejects, and can
overwrite the first run's journal. A failed export prints no job ids, so find the ones it
reserved in the hand-off store. With the local backend (`HANDOFF_URL=/handoff-state`, the path
inside the step's container), the store is the host's `${USNM_BOINC_HANDOFF_ROOT:-/srv/usnm-boinc/handoff-state}`.
With an Azure `HANDOFF_URL`, copy the journals down first, as root on the host, with
`usnm-boinc handoff get --from-url <HANDOFF_URL> --prefix dispatch --to <new dir>` (the command
`handoff-job.sh` uses), and work in `<new dir>`, which then holds `jobs/`. Then, in the store's `dispatch/` directory
(or in `<new dir>`):

```sh
jq -r 'select(.plan == "chr-lines-1621") | "\(.job.id) \(.stage) \(.job.message.page)"' jobs/*/record.json
```

Each line is a job id, its stage and its page. Keep the jobs of this export's pages, including
any still at `prepared`. Run the export again with the same variables plus `--var HANDOFF_JOBS=<those job ids>`, so the existing
records load. If a write-back failed and the log has `USNM-HANDOFF-RECORD` lines, restore the
records from them first.

### 5. Submit, collect and check

Run the test batch through this step and check it before any other export. Then do the same
for each export.

**Submit** each host's share of the jobs, pinned to `usnm_ocr` 108 and targeted
(`submit_ocr_job` in `bridge-job.sh`). `JOB_IDS` takes up to 100 jobs, and every job in the
call goes to the one `LINE_TARGET_HOST`. Split each export's jobs about evenly over hosts 2,
9 and 10:

```sh
usnm-boinc-step --env prod --target bridge --step submit-remote --wait-mins 30 \
  --var HANDOFF_URL=<HANDOFF_URL from handoff.env> \
  --var JOB_IDS=<id>,<id>,... --var LINE_APP_VERSION=108 --var LINE_TARGET_HOST=2
```

The submit takes each job's page from the staging area (`handoff pull --scans`).

**Spool out** each job whose workunit is assimilated. This also copies `gold/trust.json` to the
hand-off container:

```sh
usnm-boinc-step --env prod --target bridge --step spool-out --wait-mins 30 \
  --var HANDOFF_URL=<HANDOFF_URL from handoff.env> \
  --var HANDOFF_SPOOLS=<id>=/project/usnmocr/results/1621,<id>=/project/usnmocr/results/1621
```

(or `JOB_ID` and `HANDOFF_SPOOL=/project/usnmocr/results/1621` for one job).

**Collect** through the hand-off step. It fetches each line job's `line-read/` sidecars
(`handoff get-line-read`), and `collect --trust` imports the result:

```sh
usnm-boinc-step --env prod --target handoff --step collect --wait-mins 60 \
  --var EXPECTED_HANDOFF_URL=<HANDOFF_URL from handoff.env> --var JOB_IDS=<id>,<id>,...
```

Import (`import` in `lib.rs`) merges the readings into LoC's page (`lines::merge_in`). It
writes the counts and rule `line-merge-v1`'s verdict to the job's `line-merge.json`
(`lines::admission`, `usnm_ocr_local::rule::line_merge`). The rule admits a page when every
line the job didn't read is LoC's text byte for byte, and at least half the lines it read took
the model's reading (`MIN_MODEL_SHARE`). An admitted page is published to `ocr-out` with
LoC's text kept as `text_loc`. Otherwise the result is kept under `accepted/`, the record is
marked `held` with `line-merge-v1: line_changed` or `line-merge-v1: model_share`, and
`collect` succeeds. A result from a host the line scope doesn't list goes to
`quarantine/<job>/`, and `collect` fails.

**Check** with the hand-off `line-report` step. It brings each job's record, accepted result and
`line-read/` down and runs `usnm-boinc line-report` on them. It only reads, and only as root on
the host:

```sh
usnm-boinc-step --env prod --target handoff --step line-report --wait-mins 30 \
  --var EXPECTED_HANDOFF_URL=<HANDOFF_URL from handoff.env> --var JOB_IDS=<id>,<id>,...
```

It prints one JSON line per collected line job: `stats` (lines read, model, fallbacks by
reason), `verdict` (rule v1, for comparison only), `line_merge` (`rule`, `verdict` `admit` /
`line_changed` / `model_share`, `cherokee`, `model`), and `ours`/`loc` language and damage. A
last line sums `pages`, `lines_read`, `lines_model`, `lines_fallback`, the `line-merge-v1`
verdicts (`line_merge_verdicts`) and the rule v1 `verdicts`. Record each batch's summary on
#1621 before the next export. Stop at any `line_changed`: it means a line the job didn't read
changed, which is a bug, not a reading. For comparison, the e2e run of 9 November read 734
lines on 3 test-set pages; 700 took the model's reading and all 3 pages were admitted.

### 6. Into the index

`ocr-sync` (its timer in CT 215) imports BOINC's `ocr-out` parts into `curated/ocr-local/`
([onprem-ocr.md](../../operations/onprem-ocr.md), "OCR pickup: ocr-sync (#296)"). A release with
`--ocr-local` admits a line job's page by `line-merge-v1` rather than rule v1
(`usnm_ocr_local::rule::judge_row`). The _Phoenix_ batches are already published, and the weekly
delta applies `ocr-local` only to the batches it indexes. So the pages reach search at a
[full rebuild](../../operations/rebuild-and-index-storage.md#full-rebuild-on-prem)
(`scripts/onprem/release.sh`).

Open, and not needed for the run: whether these pages wait for the next planned full rebuild
or get one of their own. The proposal is to wait.

After the rebuild, open the results as a draft PR: step 4 of
`docs/reports/2026-11-cherokee/report.md` (#1622).

**Rollback.** Hold the 108 versions with `app-version-hold.php` (above), once nothing pinned to
them is unfinished. To drop the line scope, remove only the `chr/chr_phoenix` entry from the current
`gold/trust.json`: write the edited list to a new file beside it with `jq`, check that every
other entry equals the current list's, then `mv` it over. Don't restore the
`trust.json.pre-chr-lines-*` snapshot as a whole; it would undo hosts admitted and holds set
since. Results already published stay published. A published page can be taken out of later releases
with a create-only `curated/ocr-local/revoked/<stamp>.json` naming its `engine` or `run`;
published text stays until a full rebuild ([onprem-ocr.md](../../operations/onprem-ocr.md),
"Our OCR in releases (#296)").

## Rotating a plan onto a new worker version

A plan pins its bundles to `bundle_app_version`. Publishing a newer `usnm_ocr` version makes
the feeder stop loading the pinned one, so the plan's unsent bundles stop going out. Host
trust is keyed by worker digest, so it starts over on a new image. On 9 November both
happened during one roll (#1687, #529). Since then a new `usnm_ocr` or `usnm_gold` version is
published held, and released in the rotation step below
([app-release.md](app-release.md#held-versions)). Claim each production step on
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

4. **Gold first.** Release the new `usnm_gold` versions with
   `app-version-hold.php --action release`. In the same step, point the live plan's
   `gold_onboarding` entry at the new worker:
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
      while a change would strand unfinished units pinned to a version.
   3. Start the new plan: the live config with only the plan name, `image` and
      `bundle_app_version` changed (`gold_onboarding` stays as step 4 set it). Archive the old
      plan to drain.
   4. Run one `--apply` tick by hand, check that it submits bundles and that a host takes one,
      then turn the timer back on.
6. **Close the window** once ticks are clean and `cpu_shortage` is healthy:
   `feed_status.py --rotation-end --state-dir /var/lib/usnm-boinc-feed-health`.
7. **Later.** Once every workunit pinned to the old version has finished, raise
   `min_version` to the new pin, or leave it. The hold tool refuses a raise that strands work.

**Rollback.** Once the new plan has submitted bundles pinned to the new version, holding that
version would strand them, and `app-version-hold.php` refuses. So, in this order:

1. With the controller timer stopped, put the old plan back as the live plan and archive the
   new one to drain. The old plan's pin is still loaded, because `min_version` is still the old
   pin from step 5.1: leave it there.
2. Leave the new `usnm_ocr` versions released, so the new plan's outstanding bundles go out and
   drain. Turn the timer back on.
3. Once every workunit pinned to the new version has finished, hold the new versions. Finished
   means no unsent or in-progress result, nothing awaiting validation or a retry, and
   assimilated; a bundle still in progress can fail and need a replacement on the new version.
   The hold tool's stranding check counts all of these, so preview first, then apply, with the
   check on (no `--accept-stranded`):

   ```sh
   podman exec usnm-boinc-server php /usr/local/lib/usnm-boinc/app-version-hold.php \
     --project /project/usnmocr --journal-dir /project/usnmocr/ops-app-versions \
     --app-version ID --action hold
   podman exec usnm-boinc-server php /usr/local/lib/usnm-boinc/app-version-hold.php \
     --project /project/usnmocr --journal-dir /project/usnmocr/ops-app-versions \
     --app-version ID --action hold --apply
   ```

   If it still refuses, its preview names the version, the platforms and the count of
   unfinished workunits (`stranded_workunits`): wait and run it again. A held version stays
   published.

### What fires during a rotation

| Check              | Fires when                                                                       | During a rotation                                                                                                   | With the window                                                               |
| ------------------ | -------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| `collection`       | a source is unknown for 3 minutes                                                | the controller's timer stopped, or a tick by hand, for more than 15 minutes ("no recent completed controller tick") | a missing controller tick doesn't count; any other unknown source still fires |
| `cpu_shortage`     | own clients have more idle CPU slots than eligible unsent results for 30 minutes | no refill while the timer is stopped, or while ticks fail                                                           | held until the window ends, then fires if still true                          |
| `usnm_ocr_no_work` | OCR has a backlog and no new workunit for an hour                                | the same                                                                                                            | held until the window ends                                                    |
| `controller_tick`  | two failed ticks in a row                                                        | collect stops each tick on a result from a host not yet trusted on the new image                                    | not covered: step 4 is what prevents it                                       |

The window lasts at most 240 minutes, and every other check fires as usual inside it. A host
with only an older version installed no longer makes the eligibility check unavailable
(`collection` fired on 9 November for that): the collector counts it for the version the
scheduler would send it.

## Not changed

Claims, the export, the ledger, the hand-off record and its publication, trust and gold checks,
the validators and assimilators, the plan class, and every other app. Priority pages still go
one per unit unless `ocr-priority` is set above 1.
