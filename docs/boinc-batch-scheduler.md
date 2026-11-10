# BOINC volunteer batch controller

Issue [#525](https://github.com/tgoodyear/usnewsmap.com/issues/525). The controller chains the cache push, hand-off export, bridge `submit-remote`, assimilator `spool-out` and trust-checked hand-off `collect`. It runs one bounded tick, not a daemon, and does nothing remotely without `--apply`. Prod runs it from a pinned checkout ([below](#on-prem-controller-host-ct-211)).

A failed subprocess leaves `last-command-failure.json` (mode `0600`, replaced atomically) in the private state directory: step label, timestamp, exit code or exception class, and the last 16,384 characters of each output stream (partial timeout output included), never arguments. Only the latest failure is kept; a later success doesn't erase it, and a failure to write it is reported. Telemetry never carries subprocess output. The file is sensitive: read it on the controller host, not in GitHub, CI or telemetry, and check its timestamp and label before tying it to a tick. Nothing retries automatically.

## Recovering a contract-invalid canonical output

BOINC validity doesn't prove native output integrity. Don't patch a checksum in `result.json`, replace output with empty files, reset the journal, or release a failed page to rediscover the same workunit. Trust holds, missing files and transport errors aren't permanent failures. Once the exact job, canonical result and assimilator spool are reconciled, the local-only disposition is:

```sh
usnm-boinc reject-output --root "$dispatch_root" --spool "$canonical_spool" \
  --job "$job" --expect-result-sha256 "$result_sha256" \
  --expect-text-sha256 "$text_sha256" --expect-alto-sha256 "$alto_sha256"
```

Hashes come from the reviewed canonical bytes (empty files included). It rejects only invalid result JSON or a failed native provenance, checksum or UTF-8 contract; it refuses missing or changed files, accepted snapshots, bundles, unsubmitted jobs and valid outputs. It keeps the bytes and a content-free receipt in `quarantine/<job>/rejected/` before marking the job `failed`. Replays are idempotent; conflicting receipts or bytes fail.

For a remote hand-off, back up both dispatch journals and the canonical files under the exclusive operator and controller locks, then on a private copy:

1. Compare bridge `handoff/jobs/<job>/record.json` with collection `dispatch/jobs/<job>/record.json` (the latter can stay reserved while the bridge is submitted; don't advance it). Identity, plan, stamp and ledger must match, with a confirmed submitted workunit, no bundle and no uncertain submission; a mismatch needs reconciliation, not disposition.
2. Use the submitted bridge record; fetch collection `dispatch/jobs/<job>/accepted/` into it and check the bridge's. Any accepted manifest makes the command refuse; never discard it. Keep existing quarantine files so a retry can't overwrite conflicting evidence.
3. Write the failed record to both records above and the quarantine to collection `dispatch/quarantine/<job>/` (via `handoff put`) and the bridge's quarantine path; verify all hashes before resuming.

`--dead-letter-failed` then sends only that page to `ocr-deadletter` with the journal's reason, keeping the reservation and scientific database. Confirm settlement and a clean next tick; a lost lease needs reconciliation. Requeue is never automatic. Before settlement, roll back by restoring the journals under the same locks; after it, never restore a submitted journal or replay work: keep the receipt and reconcile the dead letter and reservation.

## Allocation and boundaries

An explicit cohort is page keys from one prefetch manifest snapshot, one base batch and one native plan, reserved for this controller (not also an on-prem node plan); the exporter still reserves the ledger and refuses conflicts. LoC batch names are case-sensitive (`curiv_joshuaTree`, `lu_Cypher`); use the base batch, not `_verNN`. Other pages stay on-prem work unless the controller reads the queues ([Queue-fed work](#queue-fed-work)). It never fetches from LoC, changes scanfetch, rolls the worker image, issues gold outside [onboarding](#gold-onboarding-901), overrides trust, attaches clients or retries terminal failures.

The host needs Python 3.9+, Azure CLI, `jq`, Bash, an Azure login and the checkout's environment outputs. Bridge steps run through `boinc/scripts/start-boinc-bridge.sh` or, with `step_command`, `usnm-boinc-step` on the BOINC host ([boinc-onprem.md](boinc-onprem.md), cutover step 5). Server and bridge images need `batch-inventory`; keep the immutable worker pin. Don't deploy server or bridge images, or run the controller, while another release owner is changing them.

An explicit cohort's configuration is operator-owned JSON outside version control; replace the example's illustrative values and `image` digest with the approved pin. `push_command` is a trusted literal argv prefix for `scripts/ocr-node/push-boinc-scan.sh` (from another host, e.g. `ssh -o BatchMode=yes <user>@<host> pct exec <ct> -- …`); it gets `--scan`, `--manifest`, account and root options and only reads cached scans and sidecars.

```json
{
  "schema_version": 1,
  "env": "dev",
  "account": "exchange123",
  "plan": "volunteer-cohort-1",
  "batch": "example_batch",
  "manifest": "snapshot-1/manifest.parquet",
  "image": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "push_command": ["/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"],
  "scans_root": "/tank/ocr/scans",
  "manifests_root": "/tank/ocr/manifests",
  "batch_size": 10,
  "max_pending": 40,
  "pages_per_host": 2,
  "pages": [
    {
      "page": "sn12345678/1900-01-01/ed-1/seq-1",
      "scan": "/tank/ocr/scans/keep/example_batch/sn12345678/1900-01-01/ed-1/seq-1.jp2"
    }
  ]
}
```

Each scan must be the page's manifest scan with sidecar (the exporter checks). Up to 10,000 pages, 1 to 100 per tick. The hash pins allocation, limits and image: don't edit configuration or state mid-cohort. Finish or drain, reconcile failures and get operator approval before migrating the configuration or allocating another cohort. A fixed page list with a page another plan's journal is for is refused before any state or export, naming the page and that plan; keep the original plan's journals and choose other pages, historical cohorts included. A queue-fed plan passes such a page over instead ([below](#a-page-two-plans-claim-1685)).

### On-prem controller host (CT 211)

Prod runs the queue-fed priority controller in the scan-fetch container (#866) as its Arc identity, the `USNM_BOINC_HANDOFF_RUNNER` that pushes scans. `USNM_BOINC_CONTROLLER_ONPREM` (`infra/modules/boinc-controller.bicep`) grants it subscription Reader and a custom role on the main and BOINC groups to read, update and start the hand-off and bridge jobs (so it can act as their identities, a risk accepted for this host). On the host:

- `/opt/usnm`: main pinned to one commit ([Moving the controller checkout](#moving-the-controller-checkout-1412));
- `/root/.config/usnewsmap/prod/.env`: non-secret outputs;
- `/etc/usnm/boinc-priority-controller.json`: `claim_queues ["ocr-priority"]`, `claim_languages ["eng", "deu"]` (widened to `srp` by `--claim-language srp` since 7 November), `lease_hours` 36;
- `/var/lib/usnm-priority-controller`: state;
- push command `/opt/usnm/scripts/ocr-node/push-boinc-scan.sh` (local).

`usnm-boinc-priority-controller.service` runs one tick with `--apply`, `--server-status-url https://boinc.usnewsmap.com/server_status.php?xml=1` (without per-app status every app's work counts as OCR, which can block priority claims while unrelated GPU queues are full) and `--max-global-pending 3000` in drop-in `usnm-boinc-priority-controller.service.d/batch.conf` (2000 until 3 November, [#1474](https://github.com/tgoodyear/usnewsmap.com/issues/1474)). It sets `Environment=USNM_SETTINGS_HOME=/root/.config/usnewsmap`, since a system service needn't inherit `HOME` and the hand-off launcher then can't find its local environment file (the bridge launcher's stack discovery still works); verify hand-off `--what-if` under systemd, not only a shell. `usnm-boinc-priority-controller.timer` uses `OnBootSec=5min` and `OnUnitInactiveSec=5min`; events are in `journalctl -u usnm-boinc-priority-controller.service`. Stop the timer and let a tick finish before any configuration change or maintenance.

Keep `max_pending: 40`, `pages_per_host: 2`, `batch_size: 10` (admission `min(40, active_hosts * 2)` under the global cap). A status fetch or parse failure stops the tick. Tune with runtime flags, never by changing the configuration hash or resetting state; shorten ticks with [`--refill-cap`](#refill-cap-1447).

**Other apps' budget (#1474).** Stranded `usnm_qwen` replicas fill any cap, so `usnm_ocr` stops with `blocked_by: "global_safety_cap"`. `--other-app-budget B` counts other apps' unassimilated workunits (the inventory's `pending_workunits_by_app`) toward the cap only up to `B`, which must be below a cap set explicitly (flag or `max_global_pending` in the configuration), leaving the cohort `cap - B` less collection backlog. The count is per app, not the residual after OCR's named workunits, so unlisted OCR work counts in full. OCR plans only (Surya refuses it). Under a budget, all apps' work counted in full may not pass the feeder's 4,096 slots (`shmem_work_items`, reason `global_hard_cap`, which no cap setting lifts). Server images without the count discount nothing (`other_app_workunits: null`). Neither the pinned checkout nor the server image has it yet. To enable:

1. Roll the server image past it ([release pipeline](boinc-onprem.md#reviewed-resumable-release-pipeline-551)) and [move the checkout](#moving-the-controller-checkout-1412), in either order.
2. Add e.g. `--max-global-pending 3000 --other-app-budget 2500` to `ExecStart` (leaves `usnm_ocr` 500: the 300 high watermark in #1474 plus margin; the budget is the operator's choice).
3. Dry-run (shows `other_app_budget`; an invalid budget fails), `systemctl daemon-reload`, run one tick.
4. Check `other_app_workunits` (not `null`), `global_pending_counted`, `global_hard_cap`, `backpressure_reason` before starting the timer. Removing the flag reverts.

### Moving the controller checkout (#1412)

Moving `/opt/usnm` deploys the controller (`boinc/scripts/batch_scheduler.py`, `scripts/ocr-node/push-boinc-scan.sh`, `boinc/scripts/start-boinc-bridge.sh` with `scripts/lib/`, settings in `infra/env/` and `boinc/infra/boinc/env/`). `onprem-step.sh` and the images deploy separately. Keep one commit of main, no local edits; until #1412 closes it is an older commit with a hand-placed `batch_scheduler.py`.

1. **Read the range**; deploy first anything a commit needs on `step_command` or in an image, and note changes to what `state.json` or journals hold:

   ```sh
   cd /opt/usnm
   git fetch origin
   git status --short   # anything listed is a local edit or stray file: step 3 saves it
   git log --oneline HEAD..<commit> -- boinc/scripts/batch_scheduler.py \
     scripts/ocr-node/push-boinc-scan.sh boinc/scripts/start-boinc-bridge.sh scripts/lib/ \
     infra/env/ boinc/infra/boinc/env/
   ```

2. **Test** while the timer runs the old checkout. The dry run reads only configuration; its `config_sha256` must match `state.json`'s or it stops with "immutable allocation changed":

   ```sh
   mkdir /root/usnm-pin-<commit>
   git -C /opt/usnm archive <commit> | tar -x -C /root/usnm-pin-<commit>
   cd /root/usnm-pin-<commit>
   python3 -m unittest boinc/scripts/test_batch_scheduler.py \
     boinc/images/server/test_batch_inventory.py
   python3 boinc/scripts/batch_scheduler.py --config /etc/usnm/boinc-priority-controller.json \
     --state-dir /nonexistent <the drop-in's other flags, without --apply>
   ```

   Then check both versions own the same jobs in the same stages, from a state copy taken between ticks (files only, since the journal cache is the last full listing a tick made, not the container now; never run a tick against the copy, even `--drain`, since a tick acts on the live feed):

   ```sh
   cp -a /var/lib/usnm-priority-controller /root/usnm-pin-state-<date>
   python3 -B - /opt/usnm /root/usnm-pin-<commit> /root/usnm-pin-state-<date> <<'PY'
   import hashlib, importlib.util, json, sys
   from pathlib import Path

   *trees, state_dir = sys.argv[1:]
   config_path = Path("/etc/usnm/boinc-priority-controller.json")
   # A tick writes journal-snapshot.json only after a complete download, and removes it first
   # when it downloads again, so the cache must hold exactly the files it lists.
   snapshot = Path(state_dir, "journal-snapshot.json")
   assert snapshot.exists(), "no complete journal snapshot; copy the state again after a clean tick"
   expected = json.loads(snapshot.read_text())["files"]
   cache = Path(state_dir, "journal-cache")
   files = {str(path.relative_to(cache)): path for path in cache.rglob("*") if path.is_file()}
   assert set(files) == set(expected), "journal cache doesn't match its snapshot"
   records = []
   for name, path in sorted(files.items()):
       data = path.read_bytes()
       assert hashlib.sha256(data).hexdigest() == expected[name], f"journal cache changed: {name}"
       records.append(json.loads(data))
   seen = []
   for tree in trees:
       spec = importlib.util.spec_from_file_location("controller", f"{tree}/boinc/scripts/batch_scheduler.py")
       controller = importlib.util.module_from_spec(spec)
       spec.loader.exec_module(controller)
       config = controller.load_config(config_path)
       state = json.loads(Path(state_dir, "state.json").read_text())
       assert state["config_sha256"] == controller.digest(config), "configuration digest differs"
       controller.QueueFeed(config, state, None, None).restore()
       owned = controller.owned_records(config, json.loads(json.dumps(records)))
       seen.append(sorted((record["job"]["id"], record["stage"]) for record in owned.values()))
   print(len(records), "journals,", len(seen[0]), "owned:", "same" if seen[0] == seen[1] else "DIFFERENT")
   PY
   ```

3. **Move** detached; start the timer only after a clean tick, otherwise roll back (step 5):

   ```sh
   systemctl stop usnm-boinc-priority-controller.timer
   systemctl is-active usnm-boinc-priority-controller.service   # repeat until neither active nor activating
   cd /opt/usnm
   git rev-parse HEAD                                            # the previous pin
   sha256sum boinc/scripts/batch_scheduler.py                    # the previous file
   git diff --binary HEAD > /root/usnm-checkout-<date>.diff \
     && git reset -q --hard                                      # staged and unstaged edits, if any
   mkdir /root/usnm-checkout-<date>.untracked                    # move each `??` path here
   git checkout --detach <commit>
   git status --short                                            # must print nothing
   sha256sum boinc/scripts/batch_scheduler.py
   systemctl start usnm-boinc-priority-controller.service        # one tick by hand
   journalctl -u usnm-boinc-priority-controller.service -n 50 --no-pager
   systemctl start usnm-boinc-priority-controller.timer
   ```

4. **Record** both commits, both sums, the diff path, the empty status and the first tick on the issue; add this to the drop-in and `systemctl daemon-reload`:

   ```ini
   # /opt/usnm at <commit> since <date> (#<issue>). Revert: <previous commit>, /root/usnm-checkout-<date>.diff.
   ```

5. **Roll back**: stop the timer and wait for a running tick as in step 3. Never roll back the state directory; if step 1 showed state or journal changes, plan the reconcile before moving:

   ```sh
   cd /opt/usnm
   git checkout --detach <previous commit>
   git apply /root/usnm-checkout-<date>.diff                     # if the previous checkout had local edits
   sha256sum boinc/scripts/batch_scheduler.py                    # must match the recorded previous file
   systemctl start usnm-boinc-priority-controller.timer
   ```

### Batched steps (#932)

Runtime flags (no hash change) put a tick's pages in one execution per step instead of one per page (an Azure Container Apps execution cost 1 to 2 minutes, about 40 minutes per 10-page tick, 20 October 2026; on-prem steps start none):

- `--batch-submit`: `submit-remote` with `JOB_IDS=<id>,...`; stops at the first failing job, reconciled next tick before any other submission.
- `--batch-spool-out`: `spool-out` with `HANDOFF_SPOOLS=<id>=<spool>,...`.
- `--batch-collect`: hand-off `collect` with `JOB_IDS=<id>,...`.
- `--collect-parallel N`: with `--batch-collect`, N jobs at once (`COLLECT_PARALLEL`, 1 to 8, default 1, #1447; serial is about 2.5 s a job), sharing one token cache ([boinc-onprem.md](boinc-onprem.md#hand-off-steps-on-prem)). Needs an image and host `usnm-boinc-step` that allow it.
- `--rest-journals`: list and fetch journals over the storage REST API with az's token (needs an az login), 16 at a time, instead of one `az` process per call (a 32,000-blob dispatch listing: 25 s and 20 s CPU with az, 2 s this way). Changed journals are fetched singly up to 2,000 (`az` does one `download-batch` past 32). Lists use delimiter `t`, so `accepted/` and `request.json` collapse to prefixes while every hex job id's `/record.json` keeps its ETag (#308), cutting list egress about 71% (1.08 MB to 0.31 MB per digit, 6 November 2026). The re-list ETag consistency checks are unchanged.
- `--batch-export`: hand-off `export` for up to 20 base batches per execution (more go in several; without the flag a queue-fed tick runs one execution per batch), `HANDOFF_BATCHES` giving each page's batch in `HANDOFF_PAGES` order (requested pages span many batches, #1055).

Spool-out, collect and export carry on past a failing job and fail at the end, so one held page doesn't hold up the others; the tick then stops. Each job keeps its single-job checks. Add a flag to `ExecStart` only once the image taking its variable is live (bridge image for submit and spool-out, hand-off image for collect and export), with the timer stopped. Each page is still its own workunit: several pages per workunit is configuration (`pages_per_unit`, `bundle_app_version`; #1059, [boinc-ocr-bundles.md](boinc-ocr-bundles.md)), so turning it on makes a new plan.

### Refill cap (#1447)

`--refill-cap N` (1 to 500, at most `batch_size`, which is also at most 500) caps pages claimed, exported and submitted per tick; collection still takes `batch_size`. Every bridge and hand-off step takes at most 100 pages, jobs or doc ids, so the controller runs claim, scan push, export, submit, spool-out, collect, release and dead-letter in steps of at most 100, and never splits a bundle across submit steps. A failed spool-out or collect step doesn't stop the others; once every step has been tried, the first error is raised. A claim step that takes fewer pages than it asked for ends the claim, and if a later claim step fails, the controller keeps the claims the earlier steps made. A full refill took 11 to 13 minutes on the 5-minute timer (November 2026; `submit` about 3.5 s a job). A smaller `batch_size` would also shrink collection, leaving results that stop the refill (`collection_backlog`), and changes the hash. Refilling still runs to the high watermark, over more ticks. Shown as `refill_cap`. It needs a checkout with the flag; set it like any flag: stop the timer, wait for a running tick to end, edit `ExecStart`, `systemctl daemon-reload`, one tick, start the timer.

### On-prem claim, collect and export (#967)

Journal metadata is cached as an account-bound snapshot in the state directory. Every read lists all names and ETags; an identical listing reuses validated records, and up to 32 changes are fetched incrementally after retained files are rechecked by digest (more, or another account, does a full refresh). Directory symlinks are rejected. A download invalidates the marker first and must match a second ETag listing before publishing a snapshot; bad or changed local records fail closed. The cache is never a claim or acknowledgement; don't restore state from it.

The hand-off steps default to the Azure hand-off job, which prod no longer has (#308, 6 November 2026), so prod passes `--onprem-handoff claim`, `collect` and `export` (runtime flags). These send the steps via `step_command` to `usnm-boinc-step --target handoff` on the BOINC host ([boinc-onprem.md](boinc-onprem.md#hand-off-steps-on-prem)); `claim` brings `release` and `dead-letter`. They need `step_command` and the host's `/etc/usnm-boinc/handoff.env`; dry runs show `onprem_handoff`. `texts-export` runs with `boinc/scripts/texts-export-onprem.sh` on a host with a local curated copy. With `export` on-prem, the push sends scans straight to the BOINC host (`usnm-boinc-step --target scans --step receive`, a tar into `/srv/usnm-boinc/project/handoff-scans`), so no scan goes through Azure (#1116).

### Retirement evidence (#1116)

Summaries carry `timings.execution_routes`: the launcher actually used (`onprem` or `azure`), failed launches included, scan pushes apart from journal and lease operations, with no argv, credentials, page text or scan paths. A non-queue scan push still uses the Azure container. To audit a window:

```sh
journalctl --all -u usnm-boinc-priority-controller.service --since "$START" --until "$END" \
  -o json --no-pager > controller-journal.jsonl
python3 boinc/scripts/handoff-window.py --journal controller-journal.jsonl \
  --since "$START_UNIX_SECONDS" --until "$END_UNIX_SECONDS"
```

Use the same UTC bounds, at least 24 hours apart, in quiet gaps (`--all` keeps fields over 4096 bytes). Each invocation needs a start marker, one terminal summary and a finish marker inside the window. The audit rejects failures, held or backlog results, alerts, Azure launches or pushes, missing route counters, plan changes, duplicate ticks, gaps over 20 minutes, native systemd failure records (including by `UNIT`), and windows without real on-prem claim, collect, export, push (`timings.scan_pages` > 0) and new publications. Exit 0 means only that this controller-routing telemetry window passes.

The on-prem launcher fails fast on the project mutex. For coordinated rollouts only, `USNM_BOINC_STEP_LOCK_WAIT_SECS=120` in the controller's SSH forced-command environment waits for a concurrent producer (0 to 300 s, below `--wait-mins`, from the runtime budget; a timeout never starts a container without the mutex). Never stop a producer, remove the lock or edit a plan to avoid contention. Deploy the reviewed launcher before setting this, keep the previous launcher and forced command for rollback, and start a clean-day window only after the rollout.

### Production priority canary

On 19 October 2026 page `sn99066040_1865-08-12_ed-1_seq-2` (plan `priority-feed-1`, job `a9ba35b339d479442d0efcfeefa9bbab2f31ab4989b88db569cd3c0c0f0761bb`) went from `ocr-enqueue --reason user` (04:36:52 UTC) to `imported` (06:28:01): claimed 05:10:14, returned by host 10 05:18:33, published 06:25:06. Collection first held it for missing exact-image gold; one targeted challenge (CER 0.0716 against 0.08734328574534386) put hosts 1, 8 and 10 in the v5 trust list with no override. Calibration and a throttling wait dominated; it isn't a latency guarantee. Bounded scanning promises neither ownership nor a fixed tick count, and admission doesn't make the returning host calibrated, so keep gold onboarding and fail-closed quarantine.

### Gold onboarding (#901)

A result from a host without gold evidence for its scope is held, and hand-off `collect` stops the tick on it. With `gold_onboarding` the controller calibrates hosts itself:

```json
"gold_onboarding": [
  {"campaign": "current247c-amd64-calibration-20261017", "language": "eng", "profile": "eng",
   "engine_base": "sha256:<the worker's TESSERACT_IMAGE digest>",
   "interval_minutes": 60, "max_hosts": 10}
]
```

`campaign` must be the scope's calibrated challenge, whose `prepared.json` `manifest_sha256` matches `gold/trust.json` (eng/eng, worker 247c: manifest `d8cd7279…`); otherwise `gold-promote` refuses it. `engine_base` is `sha256:` or `name@sha256:`. A due scope runs two bridge steps before collection:

1. `gold-onboard` gives each untrusted known host one targeted unit for the scope, challenge and `image` (one outstanding per host, `max_hosts` per pass). Units count against the global cap like the refill (uncollected journals of every plan, recovered reservations, unexported selections), shared across scopes; at zero room the pass is skipped (`gold-promote` still runs, with the configured `max_hosts`). They count as in flight for the refill, and under `--other-app-budget` count in full that tick and as other-app work in later ticks. Failed units are retried; one scored over the threshold is a verdict. Hosts no loaded gold app version can reach are skipped and printed (#1418), since such a unit logs "App version for assigned WU not found" and holds cap space until its deadline. Loaded versions are the latest per platform and plan class, or all from `min_version`. From the `host` row, a host qualifies when:
   - OS and CPU vendor give the version's platform. A host with no platform is judged on the rest; one with no `misc` is judged without client version and Docker;
   - the client is within `min_core_version` and `max_core_version`;
   - the plan class is in `plan_class_spec.xml` and its bounds and `<docker/>` hold (a runtime in `misc`, on Windows in an allowed WSL distro, without `dont_use_docker` or `dont_use_wsl`);
   - the version packages the job's engine (`Dockerfile` `FROM` its image, or a native manifest named for it) and `engine_base` is bound for the platform in `gold-engine-platforms.json`.

   `usnm_gold` is container-only, so hosts without Docker or Podman (5 and 13) get nothing; a host that installs one is picked up on its next scheduler request. Units already outstanding for skipped hosts are left as they are.

2. `gold-promote` (`usnm-boinc host-trust promote`) judges each untrusted host's evidence against the scope's `max_cer` and `manifest_sha256` and adds passing hosts. It never changes a threshold or challenge or removes a host. Each promotion goes in `host-trust/promotions.jsonl` with workunit and result ID, and a recorded workunit is never reused. Records are written before the trust change and committed after; an uncommitted one is finished only if `trust.json` is unchanged and the threshold and challenge are current, and otherwise settled without adding the host, with the ledger reread under the controller lock and the list version rechecked under the trust lock.

The same tick publishes a newly trusted host's held results. A scope is due `interval_minutes` (10 to 1440, default 60) after its last run, or after an hour while results are held if that is sooner. A run takes about 12 minutes on CT 230, so a shorter held wait put it on every tick and pushed ticks past `controller_tick`'s 15-minute limit. Its steps are timed as the tick's `gold` phase; runs are recorded before their steps, so failures wait for the next due time. A failed step adds `gold_onboarding_failed` without stopping the tick. Tick and `boinc_batch_failure` lines report `gold_onboarding`, `collection_held` and `oldest_held_seconds`. Drain ticks skip gold. An entry's optional `image` (a `sha256:` worker digest, default the plan's `image`) is the worker it onboards and promotes hosts onto, so a plan can calibrate its hosts on a new worker before it rotates onto it (#1687). It's outside the allocation digest; up to four scopes, one per language and profile. Over ssh the controller may set only `GOLD_CAMPAIGN`, `GOLD_LANGUAGE`, `GOLD_PROFILE`, `GOLD_ENGINE_IMAGE`, `GOLD_ENGINE_BASE`, `GOLD_ENGINE_VERSION` and `GOLD_MAX_HOSTS`. `gold-promote` needs a bridge image whose `bridge-job.sh` has that step.

### Separate hand-off runtime

The Azure hand-off job (main resource group, apart from the BOINC server and bridge) ran `claim`, `export`, `collect` and `dead-letter` until 6 November 2026 (#308); no `infra/env/*.json` sets `USNM_BOINC_HANDOFF_IMAGE` now, and prod runs those steps [on the BOINC host](boinc-onprem.md#hand-off-steps-on-prem). Setting that image would bring the job back (`boincOnPrem` grants and the shared exchange storage must stay either way); its last pin was `sha256:b4f2f00b2128b7eb32206f210e0673be7f9edc7017133e1684e493ed2922b877`. Its identity had Storage Queue Data Message Processor and Message Sender on `ocr-priority` and `ocr-bulk` only (`handoffQueueIds` in `infra/modules/ocr-exchange.bicep`; no Update Message, ADR-0015). To revive it: provision through #529, set `AZURE_SUBSCRIPTION_ID`, `AZURE_RESOURCE_GROUP` and `BOINC_HANDOFF_JOB` from the main-stack outputs, and change pins image-only with the jobs idle, verifying nothing else changed.

## One tick and backpressure

1. Take a kernel lock and an infinite blob lease on `boinc-handoff/scheduler/controller.lock`; a second controller fails closed. Manual bridge operations ignore the lease, so **one serialized operator owns the jobs**.
2. Run or wait for `batch-inventory`: bridge journal, complete assimilator outputs, and over verified database TLS the active hosts and each app's unassimilated workunits (`pending_workunits`, one read). It writes only `scheduler/inventory.json`, with a per-tick nonce against stale snapshots. Hand-off journals come as metadata in one `az` batch or [`--rest-journals`](#batched-steps-932).
3. Collect up to `batch_size` results even with no hosts or a full queue. Never override host IDs or bypass `--trust`. Partial outputs wait; duplicate spool directories fail closed.
4. Admit. The cohort is runnable `usnm_ocr` work (`app` / `platform` / `resource` only `usnm_ocr` / `all` / `cpu`, matching `Transport.submit()` and the accounting), under one global cap (`max_global_pending` / `--max-global-pending`, up to 100,000 workunits including collection backlog) shared with `usnm_qwen`, `usnm_quality` and `usnm_surya`.
   - `parse_server_status` reads each app's `<unsent>` and `<in_progress>` (`server_status.php?xml=1`, `--server-status-url`, `--server-status-file` or `inventory.json`).
   - Headroom (`cohort_pending`, `other_app_pending`, `global_pending`) is in workunits; `*_pending_results` are diagnostics.
   - Watermarks (`low_watermark`, `high_watermark`, flags of the same name, or under `cohorts.<cohort>` / `cohort`): at `cohort_pending <= low_watermark` with active hosts it refills (`refilling: true`) to `high_watermark` (default `min(max_pending, active_hosts * pages_per_host)`), then holds (`cohort_hysteresis`) until back at the low mark; at the high mark the reason is `cohort_high_watermark`.
   - `backpressure_reason`: `"global_safety_cap"` (at `max_global_pending`), `"global_hard_cap"` (all work at 4,096 under a budget), `"cohort_high_watermark"`, `"cohort_hysteresis"`, `"collection_backlog"` (includes assimilated jobs with partial outputs), `"no_active_hosts"`, `"draining"`. The [budget](#on-prem-controller-host-ct-211) and the 4,096 bound (`FEEDER_SHMEM_SLOTS`) are counted in workunits while the feeder holds results. An uncertain submission blocks allocation.
5. Persist selections before push or export, then push, export and submit at most `batch_size` (or [`--refill-cap`](#refill-cap-1447)) within all limits, recovered reservations included; headroom includes other plans' assimilated, uncollected backlog. Export moves only selected scans, sidecars, journal entries and the manifest snapshot; collection only one journal, its `accepted/` snapshot and result and trust files (accepted bytes are reused even if spool bytes change). Reruns recover from the hand-off and bridge records. Never replay `create_work` for a `submitting` record; a submitted reservation is never submitted twice. A SQL workunit with a missing or stale bridge journal needs reconciliation.
6. Emit `boinc_batch_tick`: selected, published and newly published counts, pages/hour, `cohort_pending`, `other_app_pending`, `global_pending`, `low_watermark`, `high_watermark`, `max_global_pending`, `refilling`, `app_queues`, `global_pending_counted` (what the cap counts), `other_app_workunits`, `other_app_budget`, `global_hard_cap` (4,096 under a budget, else `null`), oldest pending age, collection backlog, `ownership_conflicts` (claimed pages passed over, [#1685](#a-page-two-plans-claim-1685)), `backpressure`, `backpressure_reason`, `starvation_alert` and `alerts` (`ownership_conflict` with the count and first pages while any are passed over; `cohort_starvation` when active hosts face `0` unsent or `<= low_watermark` runnable `usnm_ocr` work; `zero_work_starvation` when scheduler logs or status show hosts getting zero tasks). Errors emit `boinc_batch_failure` and exit nonzero without command output or credentials. Past 80% of a lifetime cap (`MAX_JOBS` for dispatch journals, claims and bridge journals; the server's `existing_jobs_limit` and `spool_entries_limit`) each tick writes a `boinc_batch_cap_warning` (`cap`, `count`, `limit`, `at`) to stderr and carries on: [archive](#archive-drained-history-1483) first (#1483). Only newer server images report `spool_entries` and `spool_entries_limit`.

Collection backlog goes first. A terminal, failed or held collection stops the tick with state intact; fix it in its own workflow and rerun. Targets count page workunits, not replicas.

## Queue-fed work

With `"source": "queue"` the controller takes pages from the exchange's Azure Storage queues `ocr-bulk` and optionally `ocr-priority` (#296). Since the direct worker's retirement (#966) it's their only consumer.

```json
{
  "schema_version": 1, "source": "queue", "env": "dev", "account": "exchange123",
  "plan": "queue-feed-1", "manifest": "snapshot-1/manifest.parquet", "image": "sha256:...",
  "push_command": ["/opt/usnm/scripts/ocr-node/push-boinc-scan.sh"],
  "scans_root": "/tank/ocr/scans", "manifests_root": "/tank/ocr/manifests",
  "batch_size": 10, "max_pending": 40, "pages_per_host": 2,
  "claim_queues": ["ocr-bulk"], "lease_hours": 96
}
```

No `batch` or `pages`; `claim_queues` and `lease_hours` are required. Bulk workunits get a three-day `delay_bound`; `reason` `user` pages (`ocr-priority`) get one day and priority 10 (#866), which the feeder's `priority desc, workunitid` order serves first (no app uses homogeneous redundancy). Claiming `ocr-priority`, set `lease_hours` 36.

```
ocr-bulk (and ocr-priority) --claim--> claims/<doc_id>.json in boinc-handoff
  -> push the page's cached scan -> export --claims -> submit
  -> BOINC -> spool-out -> collect (ocr-out) -> delete the queue message
```

### The claim

`claim` (`usnm-boinc claim pull`) receives up to `CLAIM_MAX` messages hidden for `lease_hours` (default 96; Azure allows seven days), holding the `ocr-sync` lock across ledger read and receive (producers write the row first). A message is used only if:

- the ledger has the page outstanding with the same batch, reason and language;
- the batch is published at that version, the title isn't Japanese and an installed Tesseract model covers the language;
- the page is on the newest fetched-scans list (volunteers never fetch from LoC);
- with `claim_languages` (`CLAIM_LANGUAGES`, `--language`), its language is listed (#866). Set it to the languages `trust.json` has a policy for, or results outside it are held all lease. Deploy a hand-off image that knows `CLAIM_LANGUAGES` first; an older one ignores it and claims every language. `--claim-language LANG` (repeatable) adds a language to `claim_languages` for the claim step; as a flag it leaves the allocation digest alone, so no drain or new plan. It only widens an explicit list, refuses a code already listed, and the dry run's plan event shows the languages used. The controller runs `--claim-language srp` for the Serbian hold ([note](notes/2026-11-07-serbian-trust-hold.md)); remove the flag to stop claiming it (pages already claimed are still collected).

A held scope (#835, German until calibrated) may stay listed: `collect` keeps the result unpublished, marks the record `held` and succeeds ([gold check](../boinc/docs/gold-check.md#host-trust-for-usnm_ocr)). Held jobs count as settled and are reported as `scope_held`; the message returns after its lease and a later claim keeps the job. Held journals and spool outputs count toward the caps; `dispatch --include-held` [archives them](#archive-held-pages-835). After calibration, collect them by hand (`restore-held` first if archived).

Answered pages are deleted; other unusable messages are sent back (copied, original deleted). Queues are read in `claim_queues` order sharing one `CLAIM_MAX`. Bulk queues are peeked and skipped if the front is unusable; priority queues scan past it. Each queue has its own receive budget (`--max-receives`, default 3 × the claim limit, #1057); so a tick receives at most the budget times the number of queues; a repeated sent-back message ends that queue's scan. The step logs claimed, sent-back and deleted counts per queue. Each used message becomes `claims/<doc_id>.json` (text, pop receipt, lease end, ledger row), uploaded before the summary `scheduler/claims.json` even if the run fails, because the pop receipts exist nowhere else; a message whose claim file couldn't be written returns when its hide ends; `claims/` expires after 30 days.

### The rest of the tick

The tick claims `min(batch_size, target - pending)` (or the [refill cap](#refill-cap-1447)) less unstarted claims, saving each to state before `push-boinc-scan.sh --keep-page <batch>/<lccn>/<date>/<edition>/<seq>`, which finds `.jp2` or `.jpg` in the fetcher's keep set, then its buffer set (requested pages, #1055), uploading a buffer scan under its keep name. A page without a scan is released and dropped; with `--skip-truncated-scans` a cut-short JP2 is [dead-lettered](#a-page-whose-jp2-loc-holds-cut-short-1457). Export runs per batch with `HANDOFF_CLAIMS=1`, building each job from the claim (batch version from the row, language from the message, scan matching its sidecar), writing no ledger row and checking under the `ocr-sync` lock that the row is still outstanding (`ocr_local::boinc::adopt`). `collect` imports against that row.

### A page that fails for good

The Tesseract page deadline (`engine.timeout_secs`, export `--timeout-secs`) defaults to 1800 s, max 3600; it was 600 s before 25 October (#1159). It's part of the job identity. When a workunit's error mask is set, `collect` marks the journal `failed` and the tick stops. With `--dead-letter-failed` (#1154) the controller runs hand-off `dead-letter` (`JOB_IDS`): a v2 `permanent` letter on `ocr-deadletter` with the journal's `error`, message deleted, claim `ended: dead_lettered`. The controller records the job in its state and passes over its journal, which stays in `dispatch/`, and `ocr-sync` drains the letter. Needs the on-prem host's Message Sender role on `ocr-deadletter`. The claim and the failed journal stay with the plan on purpose: the superseding dead letter and the deadline retry below both start from them. So if a producer feeds the page again as a new message and another plan claims it, the two plans collide; [reconcile](#a-page-two-plans-claim-1685) gives the page back once the first plan is draining.

### A page whose JP2 LoC holds cut short (#1457)

Some LoC JP2s are truncated at the source (a box or tile-part past the end, or no EOC marker) yet match LoC's checksums. The fetcher no longer files them and workers fail them unread ([boinc-quality.md](boinc-quality.md#undecodable-source-scans)), but older cached ones reach the push. `--skip-truncated-scans` (off by default) pushes with `push-boinc-scan.sh --skip-truncated`, which checks headers of a `.jp2` matching its sidecar (`quality_feed.py jp2-check`) and on truncation prints `truncated <page> <reason>` and exits 3. A scan that doesn't match its sidecar isn't checked: it's pushed with a warning and export's sidecar check stops it. The controller then runs `dead-letter` with `SOURCE_TRUNCATED_DOCS=<doc_id>:<message sha256>` (`usnm-boinc claim dead-letter --source-truncated …`), which refuses if the claim is now another message and otherwise sends `source_truncated: LoC's JP2 is cut short at its source; not OCRed (#1457)` and deletes the message. The page is listed in `jq .source_truncated <state dir>/state.json` for reporting to LoC. A failed step is retried first next tick, even while draining or out of room. A page pushed before the flag was on isn't checked when export re-pushes it. Without the flag such a page is exported, the worker fails it unread, and it needs `--dead-letter-failed`. It needs a hand-off image and host `onprem-step.sh` that take `SOURCE_TRUNCATED_DOCS`, the Message Sender role, and `python3` with `boinc/scripts/onprem/quality-feed/` beside the push script.

### A retried page claimed after its dead letter

A dead-lettered page can be queued again with a new `queued_at` (#1417). Its new claim conflicts with the old one and stops every tick ("claimed a page already in this plan with a different message"); `JOB_IDS`, `usnm-boinc retry` and `release` can't settle it. As root on the BOINC host (the controller's ssh key can't set `SUPERSEDES_FAILED_JOB`):

```sh
# Dry run: checks, prints the newer message's sha256 and changes nothing.
usnm-boinc-step --env prod --target handoff --step dead-letter \
  --var SUPERSEDES_FAILED_JOB=<the dead-lettered job's id> --wait-mins 10
# Compare the sha256 with the claim summary's message_sha256 for the page, then confirm it.
usnm-boinc-step --env prod --target handoff --step dead-letter \
  --var SUPERSEDES_FAILED_JOB=<the dead-lettered job's id> \
  --var CONFIRM_MESSAGE_SHA256=<the newer message's sha256> --wait-mins 10
```

(`usnm-boinc claim dead-letter --supersedes-failed-job <id> --curated-url <curated> [--confirm-message-sha256 <sha256>]`.) It checks the failed job is queue-fed and not a retry, the claim is live, outstanding and for the same page, batch and language with a later `queued_at`, and the ledger's latest outcome is that job's `permanent` letter. It writes `dispatch/jobs/<failed job>/superseding/<message digest>.json`, sends one v2 `permanent` letter (`.sent` marker), deletes the message and marks the claim `ended: dead_lettered` (a rerun prints `already ended`). If the lease had lapsed (`dead_letter_lease_lost`) or the marker wasn't written, rerun after the next claim pull. A rerun never resends a letter `ocr-sync` has drained, but an undrained letter with no `.sent` marker is sent again; both copies settle the same attempt. Each later attempt needs the same step.

#### Retrying it with another page deadline

To run the attempt instead, record a longer deadline as root on the BOINC host (the controller's ssh key can't run `deadline-retry`):

```sh
# Dry run: checks, prints the newer message's sha256 and changes nothing.
usnm-boinc-step --env prod --target handoff --step deadline-retry \
  --var SUPERSEDES_FAILED_JOB=<the dead-lettered job's id> --var TIMEOUT_SECS=3600 --wait-mins 10
# Compare the sha256 with the claim summary's message_sha256 for the page, then confirm it.
usnm-boinc-step --env prod --target handoff --step deadline-retry \
  --var SUPERSEDES_FAILED_JOB=<the dead-lettered job's id> --var TIMEOUT_SECS=3600 \
  --var CONFIRM_MESSAGE_SHA256=<the newer message's sha256> --wait-mins 10
```

(`usnm-boinc claim deadline-retry --supersedes-failed-job <id> --timeout-secs <s> --curated-url <curated> [--confirm-message-sha256 <sha256>]`.) The deadline must be 1 to 3600 s, not the failed job's own, and must not rebuild an earlier job of the page (e.g. 600, 3600, 600 s), and no superseding dead letter may have been started for the message. While the record exists, the superseding dead letter refuses the same job and message. It writes `dispatch/jobs/<failed job>/deadline-retry/<message sha256>.json`; a rerun prints `already recorded`, and deleting it undoes the retry until the controller takes the claim. When a pull next returns the message (lease lapse, or `release` with `HANDOFF_DOCS=<doc id>`), the tick replaces the claim (with no record it stops as before; with an unreadable or invalid one it keeps the pull's other claims, then stops), records `deadline_retries`, logs `boinc_batch_deadline_retry`, and export (`HANDOFF_DEADLINE_RETRIES`, `usnm-boinc export --deadline-retry <failed job>`) builds a new job with the recorded deadline, re-pushing the scan (the host prunes staged copies after four days; the hand-off container's `scans/` expire after 30). Roll out the hand-off image, host `onprem-step.sh` and controller from one revision first: an older host script refuses `HANDOFF_DEADLINE_RETRIES`, so export fails closed; `archive_history.py` leaves such pages (`unresolved_retry_lineage`).

### A page two plans claim (#1685)

A queue-fed tick passes over a claimed page that another plan's journal is for. It exports, submits and collects nothing for that page, logs `boinc_batch_ownership_conflict` on stderr (the page, the other plans and this plan's own journal stage) when it first sees it in a tick, and reports `ownership_conflicts` and an `ownership_conflict` alert in `boinc_batch_tick`. The plan's own journals of the page are still checked: an image or allocation mismatch, two roots or a branching retry still stop the tick. Nothing in the controller resolves the conflict by itself, because either plan may be the one whose job counts.

The usual cause is a draining plan's stranded page ([above](#drain-without-changing-the-allocation)). `boinc/scripts/archive_history.py reconcile` gives the draining plan's stranded claimed pages back, by kind (`--kind`, repeatable; all three by default):

| Kind            | The draining plan's claimed page                                                                                                                                                                            |
| --------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `dead-lettered` | its only journal is a failed root in `dead_lettered`, with no retry, no `deadline_retries` entry, no `supersedes` on the claim and no `superseding/` or `deadline-retry/` record                            |
| `journal-less`  | has no journal of the plan (claims with `source_truncated` are left to the tick)                                                                                                                            |
| `unsubmitted`   | its only journal is a `reserved` root, not held by a scope, with no `spool/` output and no bridge journal (the bridge writes one, at `submitting`, before `create_work`; `crates/usnm-boinc/src/submit.rs`) |

Anything else is listed under `skipped` with its reason and left alone, as are bridge journals, the BOINC database, the queues and `ocr-deadletter`. Each page's claim and selection, and its job's `held_since` and `dead_lettered` entries, go to `<archive dir>/reconcile/<NNNN>/controller/state-entries.json` first; then the job's blobs are copied, checked and deleted as `dispatch` does (`controller_step` and `handoff_step`). The page's queue message isn't touched: a dead-lettered page's message is gone already, and the others come back when their lease runs out, for a live plan to claim.

Before anything changes, every plan given (the draining one and each `--check-config`) must accept the journals and its own state as they will be, with `owned_records` as a tick runs it, no page passed over. The dry run shows both (`owned_records.before`, `owned_records.after`) and the pages two plans claim (`collisions`). A page whose draining job reached BOINC isn't given back, so a live plan that claimed it still refuses and the apply stops: collect it first, or settle it by hand.

1. `systemctl stop usnm-boinc-priority-controller.timer`; wait until `systemctl is-active usnm-boinc-priority-controller.service` is inactive or failed. Every plan's tick runs from that service, so no plan ticks meanwhile.
2. On the BOINC host, list the bridge journals into a file and copy it to the controller host: `ls /srv/usnm-boinc/project/handoff/jobs > bridge-jobs.txt`. Take it after step 1, so no submission is missed.
3. Dry-run, naming every plan on the hand-off container (each `--check-config` with its `--check-state-dir`, in order):

   ```sh
   python3 /root/usnm-archive/archive_history.py reconcile \
     --config /etc/usnm/boinc-priority-controller.<draining plan>.json \
     --state-dir /var/lib/usnm-priority-controller.<draining plan> \
     --check-config /etc/usnm/boinc-priority-controller.json \
     --check-state-dir /var/lib/usnm-priority-controller \
     --bridge-jobs bridge-jobs.txt --archive-dir /srv/usnm-archive
   ```

   Read `found`, `pages`, `skipped`, `collisions` and `owned_records.after` (every plan `ok`).

4. Apply with the same arguments and `--apply --confirm-controller-paused --confirm-plan-draining`, plus the dry run's counts as `--expect-dead-lettered N --expect-journal-less N --expect-unsubmitted N` (one per kind asked for). It holds every plan's `controller.lock` and the controller lease, plans again and refuses if the counts differ or a plan would still refuse. The receipt is `receipts/reconcile.json`, with each plan's `owned_records` afterwards; a run stopped half way resumes when rerun.
5. Start the timer and check the next tick reports `ownership_conflicts: 0`.

`--kind dead-lettered --kind journal-less` needs no bridge listing. The archive is the only copy of what was given back; back it up with the controller state. To undo, restore the blobs from `handoff/` and the entries from `controller/state-entries.json` with the controller paused, before any plan claims the pages again.

### The queue message's life

| What happens                                    | The message                                                                     |
| ----------------------------------------------- | ------------------------------------------------------------------------------- |
| Claimed                                         | Hidden for the lease                                                            |
| `collect` published the page                    | Deleted by `collect` (`usnm-boinc claim finish`)                                |
| Delete failed, or the lease ended first         | Visible; a claim deletes it as answered, or a node rereads it (newest row wins) |
| The job failed or was never collected           | Visible after the lease; the controller stops on terminal failures              |
| JP2 cut short at LoC (`--skip-truncated-scans`) | Dead-lettered and deleted before export (`SOURCE_TRUNCATED_DOCS`)               |
| Same message after its lease, job pending       | Same claim, hidden again; a different message stops the feed                    |
| Second delivery within one pull                 | Deleted; the first claim keeps its pop receipt                                  |

Delivery is at-least-once; `ocr-sync` and the ledger tolerate a page read twice. Claims carry no search text.

### Rollout order and edge cases

- Queue-fed journal records carry the claimed ledger row; deploy hand-off and bridge images from one revision (#529).
- A page re-queued after a lapse (the seeder re-queues after 14 days) or a dead letter is a new job; two jobs for one page stop the controller ([settle it](#a-retried-page-claimed-after-its-dead-letter)).
- If a node imports a page whose claim lapsed, `collect` marks the BOINC job `published` with a `superseded` note and publishes nothing (an error for explicit cohorts).
- A claim that fails after taking messages leaves claim files, which the controller records before stopping.

## Surya waves

A plan with `"app": "usnm_surya"` keeps a Surya campaign's queue topped up ([#968](https://github.com/tgoodyear/usnewsmap.com/issues/968), [boinc-surya.md](boinc-surya.md)), with no manifest, scan push, OCR queue, hand-off job or journal; stage the quality campaign's pages first (`quality-stage`). `usnm_surya` was retired on 2 November 2026 ([boinc-surya.md](boinc-surya.md#retirement-2-november-2026)); the plan kind stays for reruns.

```json
{
  "schema_version": 1,
  "env": "prod",
  "plan": "surya-wave1",
  "app": "usnm_surya",
  "low_watermark": 100,
  "high_watermark": 400,
  "server_status_url": "https://boinc.usnewsmap.com/server_status.php?xml=1",
  "step_command": ["ssh", "-o", "BatchMode=yes", "usnm-boinc-step@boinc.example"],
  "surya": {
    "campaign": "surya-wave1",
    "quality_campaign": "quality-2026",
    "engine_image": "sha256:<64 hex>",
    "engine_llama": "sha256:<64 hex>",
    "engine_weights": "sha256:<64 hex>",
    "timeout_secs": 600,
    "stage_pages": 50,
    "submit_units": 200
  }
}
```

Names are `[a-z0-9-]` (`SURYA_CAMPAIGN`, `QUALITY_CAMPAIGN`). Optional: `timeout_secs` (60 to 14400, default 600), `stage_pages` (1 to 500, default 50), `submit_units` (1 to 2000, default 200); watermarks are required. At or below `low_watermark` unsent, a tick runs `surya-stage-crops` (`SURYA_MAX=stage_pages`, `SURYA_TIMEOUT_SECS`, three engine pins) then `surya-submit` with `SURYA_MAX` = min(`submit_units`, (`high_watermark` − unsent) / 2), each unit being two results; with under two results of room it runs nothing. `--drain` runs nothing, and without `--apply` it prints the steps a tick would run at an empty queue. A status without the app or a failed step stops it. There's no Azure lease and state isn't hash-bound, but engine pins are bound to the campaign; a new engine needs a new `campaign`. `surya-collect` stays manual. Over ssh the key may set only `SURYA_CAMPAIGN`, `QUALITY_CAMPAIGN`, the `SURYA_ENGINE_*` pins, `SURYA_MAX` and `SURYA_TIMEOUT_SECS` on `surya-stage-crops`, and `SURYA_CAMPAIGN` and `SURYA_MAX` on `surya-submit`.

## Validation, scheduling and recovery

### Drain without changing the allocation

`--drain --apply` ([#833](https://github.com/tgoodyear/usnewsmap.com/issues/833)) still locks, inventories and collects through the trust gates, but selects, pushes, exports and submits nothing (`draining: true`). Remove it to resume. It cancels no BOINC work and releases no reservations.

A draining plan therefore strands three kinds of claimed page for good: jobs it exported but never submitted, claims it never exported, and pages it dead-lettered. Their messages come back on the queue, and a live plan that claims one collides with the draining plan ([#1685](https://github.com/tgoodyear/usnewsmap.com/issues/1685)). When a plan is retired to `--drain`, dry-run [`archive_history.py reconcile`](#a-page-two-plans-claim-1685) to list them, and give them back; jobs it already submitted are left to be collected.

To stop bulk seeding as well, first save the `USNM_OCR_SEED_*` values, then clear `USNM_OCR_SEED_PLAN` (now `fetched-eng-20261106`) in `infra/env/<env>.json`: `scripts/azure/settings.sh --env prod --key USNM_OCR_SEED_PLAN --unset`; `scripts/onprem/ingest-env.sh` passes it to the on-prem pickup. Keep `ocr-sync` running (imports, dead letters, the user priority feed) and coordinate manual enqueue or requeue commands. Don't move or purge queue messages. Before giving drained pages to BOINC, reconcile their ledger state and match image, model and trust scope.

Offline validation (no Azure or SSH calls, no state):

```sh
python3 boinc/scripts/batch_scheduler.py \
  --config /opt/usnm-controller/cohort.json \
  --state-dir /opt/usnm-controller/state
python3 -m unittest -v boinc/scripts/test_batch_scheduler.py \
  boinc/images/server/test_batch_inventory.py
```

For a new plan: run one approved `--apply` tick, check reservations, host IDs, trust, published ALTO and Parquet and `ocr-sync`, confirm a second tick duplicates nothing, then schedule it like prod's [timer](#on-prem-controller-host-ct-211).

State and journals are durable recovery state. After a crash the lease stays held: stop the schedule, inspect executions and journals, reconcile `submitting` jobs, and only once the former owner is confirmed stopped may the serialized operator break the stale `scheduler/controller.lock` lease. Never delete journals, reset state or break a live lease. A missing reply doesn't mean `create_work` failed.

The inventory fails closed above 10,000 journals, 100,000 spool entries or the workunit cap (default 50,000, `USNM_MAX_EXISTING_WORKUNITS` in the BOINC host's `bridge.env`, 1 to 200,000, reported as `existing_jobs_limit`; older server images: 10,000). BOINC keeps workunits `delete_delay_hours` (168) after they finish (#1483). The cap covers rows (`existing_workunits`) and pages (`existing_jobs`); the 80% warning uses the larger.

### Archive drained history (#1483)

`boinc/scripts/archive_history.py` moves the oldest fully published pages' history out of every capped store into a retained archive, a batch at a time, deleting nothing it hasn't copied and checked. Without `--apply` each step is a dry run with no lock and no writes. Run in order per batch:

| Step       | Host                       | Stores                                                                                                                                       |
| ---------- | -------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `dispatch` | controller host            | state's `claims`, `selected`, `held_since`, `dead_lettered`, then blobs `dispatch/jobs/<id>/`, `dispatch/quarantine/<id>/` and `spool/<id>/` |
| `bridge`   | BOINC host                 | bridge journals `$HANDOFF_DIR/jobs/<id>/` and spool outputs under `results/`                                                                 |
| `db`       | where mysql reaches the DB | finished `workunit` rows and their `result` rows                                                                                             |

A page qualifies when its journals form one retry chain ending `published`, last written at least `--min-age-hours` (24) ago, and state agrees (claimed for that batch in the current plan; in an older plan not claimed, selected or in a fixed plan's page list). Oldest first, `--pages` (1,000) per batch; fixed page lists never. Prepared, reserved, submitting, submitted and failed (including dead-lettered) pages stay. State goes before blobs. Each blob is copied, its sha256 kept in `handoff-sha256/`, and deleted only if unchanged. `claims/<doc_id>.json` and the `ocr-out` ledger stay. The state's `published` count drops by the pages archived, so `newly_published` stays right while `pages_per_hour` falls. Each step writes its plan (`manifest.json`, `bridge-plan.json`, `db-plan.json`) first and a receipt in `receipts/` last, and resumes when rerun with the same arguments; a changed blob stops it. Dry run and apply check that the same pages need work before and after the batch, and the apply refuses otherwise. The `dispatch` apply holds the controller's lock file (refusing if missing: wrong `--state-dir`) and lease; a killed apply leaves the lease held, broken as for a crashed controller once the process is confirmed gone.

Deleting blobs needs `blobs/delete` on `boinc-handoff`: the controller identity (`boincHandoffRunner`) has it via the archiver role (`USNM_BOINC_HELD_ARCHIVE`, `infra/modules/ocr-exchange.bicep`). Otherwise the apply stops with HTTP 403 `AuthorizationPermissionMismatch`; rerun as an operator with at least Storage Blob Data Contributor on `<account resource ID>/blobServices/default/containers/boinc-handoff`. A broader role an operator already holds (e.g. Storage Blob Data Owner) also works, but don't grant one for this. The tool's only `az` call, to the first `az` on its `PATH`, is:

```sh
az account get-access-token --resource https://storage.azure.com/ \
  --query '{token:accessToken,expires:expires_on}' -o json
```

`expires_on` must be a number: `az account get-access-token --resource https://storage.azure.com/ --query 'type(expires_on)' -o tsv` prints `number` from Azure CLI 2.54.0 on. If not, upgrade the CLI rather than substitute `expiresOn`: the tool parses `expires` with `int()`, so a date string stops the step. It asks again 10 minutes before expiry, so rewrite the token file before then. For an operator token:

1. On the controller host, create root-owned 0700 `/root/usnm-archive` with a 0700 `azshim/az` that prints a 0600 `/root/usnm-archive/.maint-token.json` only for exactly the call above and otherwise exits non-zero; it never calls the real `az`.
2. Mint the token on a workstation with the same command and pipe it over `ssh` into that file, unprinted; refresh it by `mv` from a 0600 temp file.
3. Run with `PATH=/root/usnm-archive/azshim:$PATH python3 archive_history.py …` only; never change `PATH` in a profile or for the service.
4. Delete the token, the wrapper and any copies afterwards (e.g. on any jump host).

The maintainer approves each live run:

1. `systemctl stop usnm-boinc-priority-controller.timer`; wait until `systemctl is-active usnm-boinc-priority-controller.service` is inactive or failed.
2. Put `archive_history.py` beside the controller's `batch_scheduler.py` (e.g. `/root/usnm-archive`) and dry-run `dispatch`; read `pages.eligible`, `batch` and `outstanding_pages.unchanged`. Allow about 7 MB a page on an archive dataset off the root filesystem.

   ```sh
   python3 /root/usnm-archive/archive_history.py dispatch \
     --config /etc/usnm/boinc-priority-controller.json \
     --state-dir /var/lib/usnm-priority-controller \
     --archive-dir <archive dir>
   ```

3. Apply with `--apply --confirm-controller-paused` (receipt in `<archive dir>/batches/<NNNN>/receipts/dispatch.json`).
4. Copy `manifest.json` and `receipts/dispatch.json` unchanged to `/srv/usnm-boinc/project/archive/batches/<NNNN>/` on the BOINC host, using the same `<NNNN>` (the step refuses another batch's receipt or a `bridge-plan.json` from another manifest), and `archive_history.py` from the same checkout to `/srv/usnm-boinc/project/archive/`. Run `bridge` as root; it holds `/run/usnm-boinc-step.lock`, `/run/usnm-boinc-handoff.lock` and `$HANDOFF_DIR/.lock`, so other bridge steps fail meanwhile, and archives a bundle's output only when none of its pages remain. It refuses a `--handoff-dir` or `--results` that is a symlink: name the directory it points to. `/srv/usnm-boinc/project/archive` is its own mount on the BOINC host, not the host's root filesystem: the bridge moves each batch's journals and results there (3–7 GB a batch), and the Qwen feed pauses while the host has less than 50 GB free (`storage_backpressure`). In the on-prem deployment it is the ZFS dataset `bulk8/usnm-boinc-archive` on pve-big over NFS; the bridge copies across filesystems when a hard link can't be made.

   ```sh
   python3 /srv/usnm-boinc/project/archive/archive_history.py bridge \
     --batch-dir /srv/usnm-boinc/project/archive/batches/<NNNN> \
     --handoff-dir /srv/usnm-boinc/project/handoff \
     --results /srv/usnm-boinc/project/usnmocr/results
   # then the same with --apply --confirm-controller-paused
   ```

5. Run `db` in `usnm-boinc-server` with a 0600 mysql defaults file for a user that can delete from `workunit` and `result` (remove it afterwards); the bridge receipt must name the batch's manifest. In 500-workunit transactions it locks (`SELECT ... FOR UPDATE`), exports to `db/chunk-<NNNN>-attempt-<N>/` and deletes only workunits assimilated (2) with inputs deleted (2) whose results are over (5) with outputs deleted (2); the rest are `deferred` to `db_purge` and still count toward the workunit cap. A stop before `COMMIT` rolls the chunk back and the resume exports it again; after `COMMIT` the export counts as deleted.

   ```sh
   podman exec usnm-boinc-server python3 /project/archive/archive_history.py db \
     --batch-dir /project/archive/batches/<NNNN> \
     --mysql-defaults-file <defaults file> --database <DB_NAME from the server's settings>
   # then the same with --apply --confirm-controller-paused
   ```

6. Repeat until well under the cap, start the timer, and watch a tick end without `dispatch inventory limit/consistency failure`.

The archive is the only copy; back it up with the controller state.

#### Archive held pages (#835)

`dispatch --include-held` also archives held jobs (`reserved` with `held`) whose chain ends in the held job, in the current plan, with a message sha256 on the claim and both `dispatch/quarantine/<job>/` and `spool/<job>/` present. The manifest marks them `"held": true`; they leave an `archived_held` entry, don't lower `published`, and keep their queue message hidden on redelivery (a different message stops the tick). Ticks report `scope_held_archived` and warn at 80% of `MAX_ARCHIVED_HELD` (200,000). One hour is enough age:

```sh
python3 /root/usnm-archive/archive_history.py dispatch --include-held --min-age-hours 1 \
  --config /etc/usnm/boinc-priority-controller.json \
  --state-dir /var/lib/usnm-priority-controller \
  --archive-dir /srv/usnm-archive
```

`usnm-boinc-held-archive.timer` on the controller host runs this daily at 08:30 UTC ([`held-archive.sh`](../boinc/scripts/onprem/held-archive.sh), unit files in `boinc/scripts/onprem/`):

1. Stop the controller timer and wait for a tick (exit if it was already stopped).
2. Bridge any batch with a dispatch receipt but no `receipts/bridge.json`.
3. Apply up to 10 batches (`--include-held --min-age-hours 1 --pages 1000`) into `/srv/usnm-archive`, each bridged via `usnm-boinc-step --target archive --step bridge --var ARCHIVE_BATCH=<NNNN>`.
4. Restart the controller timer and a tick, however it ended.

No db step (#1587). It logs one `boinc_held_archive` line (`journalctl -u usnm-boinc-held-archive`), and feed health watches it (`--maintenance-unit usnm-boinc-held-archive.service`; failures as `controller_tick`, #1441; no missed-tick alarm until 15 minutes after). Keep the BOINC host's `archive_history.py` in step. A failure fails the unit. 1,000 pages take about 15 minutes, with no refill meanwhile; pause with `systemctl stop usnm-boinc-held-archive.timer`, and a run stopped half way resumes on the next one.

To publish held pages after calibration, restore them with the controller paused (`blobs/write` only):

```sh
python3 /root/usnm-archive/archive_history.py restore-held --batch <NNNN> \
  --config /etc/usnm/boinc-priority-controller.json \
  --state-dir /var/lib/usnm-priority-controller \
  --archive-dir /srv/usnm-archive
# then the same with --apply --confirm-controller-paused; --page <page> restores one page
```

It verifies sha256, uploads with `If-None-Match: *` and the archived Content-MD5 (journal last), refuses if `MAX_JOBS` would be passed (`claims_after`, `journals_after`, `fits`), restores state entries and writes `receipts/restore-held-<time>.json`. Then collect with hand-off `collect` and `JOB_IDS`.
