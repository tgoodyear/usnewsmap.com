# Feeder-only refresh (#1462)

This page defines when an operator may restart only the BOINC feeder so that its `--allapps` slot
allocation is rebuilt from the active app registry, how to do it, and what must stay manual. It is
the boundary asked for in [#1462](https://github.com/tgoodyear/usnewsmap.com/issues/1462).
Detection ([#1464](https://github.com/tgoodyear/usnewsmap.com/pull/1464),
[Allocation drift detection](../apps/app-release.md#allocation-drift-detection))
is merged but not deployed. Nothing on this page restarts anything by itself. Production rollout,
the first live refresh and any automatic repair need the maintainer's approval.

In short:

- Refresh only on a confirmed `drift` from the guard, onto a registry that is the intended one, with
  the maintainer's approval. Never on `unknown`, `maintenance`, a failed check or `match`.
- One `SIGHUP` to the verified feeder PID, one `bin/start --cron`, and at most one retry after a
  transient cause is fixed, with the supervisor loop held once a start fails. No loops, no
  `SIGKILL`, no whole-server restart.
- While no feeder is ready, every new scheduler request is told to come back after
  `maintenance_delay`: 20 minutes, or an hour on a server image that doesn't set it.
- Verify with readbacks: the new process, `show_shmem`'s apps and weights against the database, the
  guard, slot shares, bounded replicas in the cache and a natural assignment to an eligible host.

BOINC references are to the pinned commit `22bbebe` (`BOINC_COMMIT` in
[`boinc/images/base/Containerfile`](../../../boinc/images/base/Containerfile)),
which the server image builds. Commands assume the on-prem layout: the `usnm-boinc-server`
container on CT 230 and the project at `/project/usnmocr`.

## Why `reread_db` is not enough

With `--allapps` (the feeder's `<cmd>` in [`config.xml.template`](../../../boinc/project/config.xml.template)),
the feeder builds two tables once, at startup, from the apps it has just read: `app_indices` maps
each of the `shmem_work_items` cache slots to an _index_ into the shared-memory app array, and
`enum_sizes` gives each index its share of `feeder_query_size` ([feeder.cpp][feeder] `main`). The interleave, `weighted_interleave`, gives
each app about `slots × weight / sum of weights` slots.

Touching `reread_db` makes the running feeder clear the whole cache and read the tables again
([feeder.cpp][feeder] `check_reread_trigger`, [sched_shmem.cpp][sched_shmem] `SCHED_SHMEM::init`), but it does not rebuild
`app_indices`, `enum_sizes` or the feeder's own app count. Every pass still fills slot `i` with work
for `ssp->apps[app_indices[i]]`, with that index's startup query budget (`get_job_from_db` and `scan_work_array` in
[feeder.cpp][feeder]). So after a reread that changed the active
apps:

- an app added since startup gets no slots;
- after a retirement, each later app moves up one index and inherits the slots and query budget
  of the app before it, and indices past the new end point at an empty entry (app ID 0) whose
  slots never fill;
- a weight change alters nothing.

The guarded feeder from #1464 records the app IDs and weights it allocated from and latches
`drift` when a reread loads different ones. Only a new feeder process rebuilds the allocation.
"Feeder-only" means replacing that one process and nothing else.

## Who owns the feeder

- The stock `bin/start` starts the feeder from the `<daemons>` list in `config.xml`
  ([`config.xml.template`](../../../boinc/project/config.xml.template)). It
  double-forks, takes an exclusive `lockf` lock on `pid_<host>/feeder.lock`, writes
  `pid_<host>/feeder.pid` and `exec`s the command, so the feeder itself holds the lock ([`start`][start] `run_daemon`, `lock_file`). A second start
  finds the lock held and starts nothing.
- The feeder is not a supervisord program. `[program:boinc]` in
  [`supervisord.conf`](../../../boinc/images/server/supervisord.conf) runs `boinc-daemons.sh`, which runs `bin/start` once and then `bin/start --cron` every 300
  seconds as `boincadm`, unless `stop_maintenance` or its own restart hold is present
  ([`boinc-daemons.sh`](../../../boinc/images/server/boinc-daemons.sh) `main`).
  That loop is the feeder's supervisor: a feeder that exits is started again within five minutes,
  as BOINC expects from cron (`DB connection lost` in [feeder.cpp][feeder], `--cron` in [`start`][start]).
  It restarts a stopped feeder at most three times an hour, then holds the project
  ([The loop's restart bound](#the-loops-restart-bound)). This procedure adds no loop to it, and
  holds it once a start fails ([Hold the supervisor loop](#hold-the-supervisor-loop)).
- `pid_<host>` is on the container's own disk, `${USNM_PID_ROOT:-/var/lib/usnm-boinc}/pid_<host>`,
  and linked into the project
  (`local_pid_dir` in [`boinc-daemons.sh`](../../../boinc/images/server/boinc-daemons.sh)). The
  guard's snapshot, `usnm-feeder-slots.json`, and the loop's hold marker,
  `usnm-feeder-restart-hold`, are always in that local directory, even where the wrapper keeps an
  older `pid_<host>` directory in the project instead of the link. The commands below use the local
  path.
- The stock stop is `SIGHUP`. The handler only sets a flag; the feeder exits at its next
  `check_stop_daemons`, which runs every second while it sleeps and after every pass. On that exit,
  `cleanup_shmem` clears `ready` and removes the segment ([sched_util_basic.cpp][sched_util_basic],
  [feeder.cpp][feeder] `feeder_loop`). `SIGTERM` and `SIGKILL` skip that
  cleanup. `bin/stop` (`bin/start --stop`) sends `SIGHUP` too, but to every daemon, and disables
  the project ([`start`][start] `stop_daemon`, `command_disable_stop`).

None of these is a feeder-only refresh:

- Starting `bin/feeder` by hand. It would not hold `feeder.lock`, so the next `--cron` starts a
  second feeder; the second removes the first's segment at startup and the first keeps filling a
  cache no scheduler can reach ([feeder.cpp][feeder] `main`).
- `supervisorctl restart boinc`. It restarts the wrapper, whose plain `bin/start` clears
  `stop_sched` (`command_enable_start` in [`start`][start]) but leaves the running feeder alone, since the
  feeder is not the wrapper's child and holds its lock.
- Restarting `usnm-boinc-server.service`. That restarts Apache, every daemon and the container.

## The loop's restart bound

Before each `bin/start --cron`, `boinc-daemons.sh` checks the feeder: `pid_<host>/feeder.pid` must
name a live process called `feeder` that is not a zombie. If it doesn't, and `stop_daemons` is
absent (a project stopped with `bin/stop` is not restarted anyway), the run counts as a restart and
the wrapper logs `the feeder is not running; restart k of N in Ws` to the server unit's journal.
If `N` restarts already fall within the last `W` seconds, it skips the run instead, creates the
marker `usnm-feeder-restart-hold` in the local `pid_<host>` directory
(`held_at=<epoch> restarts=<n> window_s=<W>`) and logs `holding the project until ... is removed`
(`held` in [`boinc-daemons.sh`](../../../boinc/images/server/boinc-daemons.sh)).
The wrapper runs as root in a directory `boincadm` can write, so it only ever creates a new file
there. If anything already occupies the path, such as a link, or the write fails, it logs
`could not write` and stays held without a marker until the container restarts; the guard then
reports `unknown` for the stopped feeder. Remove whatever occupies the path before that restart.
Only the feeder is counted. The first `bin/start` and a run that finds the feeder running don't
count, and the count starts again after each hold.

While the marker exists, the loop treats it like `stop_maintenance`: it starts no daemon and runs
no periodic task, but stops nothing that is running, and a restart of the wrapper waits for it.
The guard reports `unknown` with reason `feeder_restart_hold` without querying anything, so the host
unit fails: a held project is an incident. The loop never creates `stop_maintenance` itself. The
marker is on the container's own disk, so a container start clears it. Otherwise only an operator
removes it, once the cause is found and fixed, as `boincadm` in the server container:

```sh
host=$(python3 -c 'import socket; print(socket.gethostname().split(".")[0])')
rm "${USNM_PID_ROOT:-/var/lib/usnm-boinc}/pid_$host/usnm-feeder-restart-hold"
```

The next run, within five minutes, starts the feeder with a fresh count.

The default bound is `N` = 3 restarts in any `W` = 3600 seconds. A feeder that fails every start
is started three times, five minutes apart, and held at the next run, 15 minutes after the loop
first found it stopped: within one `maintenance_delay` (1200 s), the back-off every scheduler request
gets while no feeder is ready. A rolling window, unlike a count of consecutive failures, also holds a
feeder that starts, runs for a while and dies. On CT 230 between 21 October and 5 November 2026
the loop restarted the feeder once, and each database restart took less than one five-minute cycle
([the measurements](../../notes/2026-11-05-feeder-restart-bound.md)). The cost is that a database
outage of more than about 15 minutes that doesn't restart the server leaves the project held after
the database returns, until an operator removes the marker.

`USNM_FEEDER_RESTART_LIMIT` (0 to 9999; 0 holds as soon as the loop finds the feeder stopped) and
`USNM_FEEDER_RESTART_WINDOW` (1 to 999999 seconds) in the server container's environment change
the bound. The wrapper logs any other value and keeps the default. Like any setting, a change is a
reviewed `-e` flag in
[`usnm-boinc-server.service`](../../../boinc/scripts/onprem/usnm-boinc-server.service).

## When a refresh is warranted

The guard's statuses (`feeder-slot-guard.py`, its exit code in brackets):

| Status (exit)                                             | Meaning                                                                                                                                                                                                                                     | Action                                                                                                                                                                                     |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `match` (0)                                               | The allocation equals the active registry.                                                                                                                                                                                                  | No refresh. A match does not show that eligible work exists or reaches a host: look at plan-class bounds, app versions, `one_result_per_host_per_wu` and the enumeration budget instead.   |
| `maintenance` (0)                                         | `stop_maintenance` is present; nothing was checked.                                                                                                                                                                                         | No refresh. Someone holds the project.                                                                                                                                                     |
| `unknown` (2)                                             | The check could not be made: `stale_snapshot`, `snapshot_process_mismatch`, `feeder_reload_in_progress`, `feeder_snapshot_uncertain`, `database_unavailable`, `not_allapps_feeder`, `observation_changed`, `feeder_restart_hold` and so on. | No refresh. A feeder that is starting, reloading or stalled, or a database that is down, is not fixed by a restart. Find the reason; a persistent `unknown` is an incident, not a trigger. |
| `drift` (1), reason `registry_changed`                    | The active registry differs from the startup allocation. The report lists the added, removed and changed app IDs.                                                                                                                           | Candidate, if the registry is the intended one.                                                                                                                                            |
| `drift` (1), reason `reload_changed_app_order_or_weights` | A reread loaded different apps or weights. Latched until the feeder restarts.                                                                                                                                                               | Candidate.                                                                                                                                                                                 |

A refresh is warranted only when all of these hold:

1. Two consecutive runs of the host's guard timer, five minutes apart, report the same `drift`
   reason and app IDs (elsewhere, two consecutive `boinc_feeder_slot_guard` events from the
   image's own watch), and `pid_<host>/feeder.pid` names the same feeder at both.
2. The drift comes from a known, reviewed change to the app table whose rows before the change were
   saved, and the current registry is the one the project should run. If the change was a mistake,
   undo the change instead. With `registry_changed` and no reread since, restoring the rows returns
   the guard to `match` without a restart. A latched `reload_changed_app_order_or_weights` needs the
   refresh even after the rows are restored.
3. The current registry meets the [preconditions](#preconditions), because it is what the new
   feeder will allocate from.
4. The maintainer has approved the window, including the
   [supervisor hold](#hold-the-supervisor-loop) if a start fails. While the feeder is down,
   scheduler requests are told to come back in 20 minutes, or an hour on an older image
   ([below](#what-the-scheduler-does-during-the-restart)).

This procedure covers only a feeder from the guarded image. A feeder started before that image was
deployed has no snapshot, so the guard can only report `unknown` for it and steps 1 and 4 cannot
pass. Restarting it with `bin/start --cron` would also start the same unguarded `bin/feeder`, which
the server replaces only when the container starts ([Before first use](#before-first-use)). Such a
feeder is not refreshed with this procedure: it is replaced when the guarded image is deployed,
which is the maintainer's rollout decision. Evidence that it is stale can support that decision:
`show_shmem`'s `apps:` list differs from the active rows, or the feeder log has a reread after its
start and the change's own record puts an active-registry change between the two. The feeder runs
at `-d 2`, which drops BOINC's normal-level lines such as `Starting` and
`Found trigger file reread_db`. Its log does show each start as one
`Executing command: feeder ...` line from `bin/start`, and each scan of the tables, at a start or a
reread, as five `have apps for` lines from `scan_tables`. The first five `have apps for` lines after
the last `Executing command` line are that start's own scan; any later group of five is a reread. Cached results whose app differs from the slot's app in the
current registry's interleave confirm the second case (reproduce `weighted_interleave` by hand; no tool computes it yet). On
5 November 2026 that comparison found none on CT 230
([the measurements](../../notes/2026-11-05-feeder-restart-bound.md)).

## Preconditions

All must hold just before the stop. If any fails, stop: there is no refresh.

Coordination:

- A coordination claim on #529 and #1462 names the operator, the window, the evidence directory
  and the maintainer's approval.
- The project mutex, `/run/usnm-boinc-step.lock` on CT 230, is held through the restart and
  the readbacks of step 4. While it is held, bridge steps that take it fail fast, or wait at most
  `USNM_BOINC_STEP_LOCK_WAIT_SECS` (up to 300 s) where that is set. That covers
  `register-apps` (`bin/xadd`), publication through the step and steps that other hosts start,
  so tell whoever runs them. The Qwen auto-cohort controller's tick exits 75
  ([`onprem-step.sh`](../../../boinc/scripts/onprem-step.sh),
  [`usnm-qwen-cohort-auto.service`](../../../boinc/scripts/usnm-qwen-cohort-auto.service)).
- Registry writers that don't take that lock are paused for the whole window, verification
  included: retirement and rollback scripts such as `app_tidy.php`
  ([Retiring an app](server-and-apps.md#retiring-an-app)), and any direct SQL on
  `app.deprecated`, `app.weight` or `app.homogeneous_redundancy`.
- `reread_db` writers that don't change apps are paused too, because each reread empties the whole
  cache and spoils the readbacks:
  [`publish-appver.sh`](../../../boinc/images/appver/publish-appver.sh) (its
  `.usnm-publish.lock` directory must be absent),
  [`hold-legacy-qwen.php`](../../../boinc/scripts/hold-legacy-qwen.php) and
  [`quality_corpus_runner.py`](../../../boinc/scripts/quality_corpus_runner.py).

Project state:

- `bin/status --verbose` says `BOINC is ENABLED` and shows every daemon that is not disabled as
  `running` and `locked`. `bin/start --cron` also starts any other stopped daemon, so a stopped
  daemon must be explained
  first ([`start`][start] `run_daemons`, `command_cron_start`).
- None of these exists in the project directory: `stop_maintenance` (pauses the supervisor loop),
  `stop_sched` (scheduler maintenance), `stop_daemons` (stops every daemon, and `bin/start`
  removes it, which would undo someone else's stop: `remove_stop_daemons` in [`start`][start]) or `reread_db` (a pending reread). Nor is there a restart hold
  marker (`$restart_hold` in step 1): the loop has [held itself](#the-loops-restart-bound), and why
  comes first.
- Exactly one feeder: `pgrep -u boincadm -x feeder` prints one PID, the PID in
  `pid_<host>/feeder.pid`. Its command line is `feeder -d 2 --allapps --purge_stale 5`, and the
  guard's snapshot names the same PID and start ticks.
- The database answers the guard's verified-TLS query (no `database_unavailable`).

The registry the new feeder will load (`WHERE deprecated=0`,
`SCHED_SHMEM::scan_tables` in [sched_shmem.cpp][sched_shmem]):

- At most 10 active apps; an 11th stops the feeder at startup. The other tables are capped as
  well: at most 49 active platforms, 99 loaded app versions (counted from the database as in
  step 1) and 9 multi-host assignments (`MAX_APPS` and the other limits in
  [sched_shmem.h][sched_shmem_h]).
- Every active app has `homogeneous_redundancy = 0` and `n_size_classes` of at most 1. An HR
  misconfiguration stops the feeder (`hr_init` in [feeder.cpp][feeder]), and a missing size
  census file ends the table scan early, a failure that the startup path ignores. The project uses no HR ([Feeder
  visibility](../apps/app-release.md#feeder-visibility)).
- No unsent result belongs to a workunit with a nonzero `hr_class` (step 1's last query returns 0).
  Without HR there should be none. The feeder's stale purge, which runs all the time, clears such a
  class and adds a replica (`purge_stale` in [feeder.cpp][feeder]), which would change replication
  while step 5 is judging it.
- The sizing invariant in [Feeder
  visibility](../apps/app-release.md#feeder-visibility) holds for the current
  weights: every positive-weight app's `floor(0.5 + feeder_query_size × weight / sum)` (`enum_sizes` in
  [feeder.cpp][feeder]) is at least twice `shmem_work_items`, Qwen's budget is at least four
  times its ready baseline, and Qwen's slot share covers that baseline. A registry that fails this
  needs a reviewed configuration change first, not a refresh.
- The current `app` rows are saved in the evidence directory (step 1).

Memory: the new feeder creates its segment while requests already attached may still map the old
one, which the kernel frees only at the last detach ([lib/shmem.cpp][shmem] `destroy_shmem`). Allow
for two segments of `sizeof(SCHED_SHMEM) + shmem_work_items × sizeof(WU_RESULT)` for a few seconds; Feeder visibility estimates about 380 MB
each at 4,096 slots.

## What the scheduler does during the restart

The scheduler runs as plain CGI (`ScriptAlias` in [`apache-project.conf.template`](../../../boinc/images/server/apache-project.conf.template)),
so each request is a new process that attaches to the segment by key (`attach_to_feeder_shmem` in
[sched_main.cpp][sched_main]).

1. **Old feeder exits.** `cleanup_shmem` clears `ready`, detaches and removes the segment.
   Removal hides the key at once, but the segment stays mapped in every process attached to it.
2. **In-flight requests finish on the old cache.** A request already past the `ready` check keeps
   working. To send a cached result it reserves the slot under the semaphore, reads the result from
   the database again and requires it to be unsent, then marks it sent with an update that only
   succeeds if the state hasn't changed (`result_still_sendable` in [sched_check.cpp][sched_check],
   `DB_RESULT::mark_as_sent` in [boinc_db.cpp][boinc_db]). The database, not the cache, prevents a double send.
3. **No segment.** A new request cannot attach. It gets `Server error: feeder not running`, with
   `<project_is_down/>` and a `request_delay` of `maintenance_delay` (`send_message` in
   [sched_main.cpp][sched_main]). [`config.xml.template`](../../../boinc/project/config.xml.template)
   sets it to 1200 seconds, so that client waits
   20 minutes before asking again ([Client retry behaviour](#client-retry-behaviour)). A server
   image built before that runs with BOINC's default of 3600 seconds ([sched_config.cpp][sched_config]), an hour; `grep maintenance_delay /project/usnmocr/config.xml` in the
   server container finds nothing on such an image.
4. **New segment, not yet ready.** The new feeder creates the segment with `ready` false
   and sets it after its first fill pass (`feeder_loop`). A request that attaches in between waits
   up to 10 seconds for `ready`, then gets the same reply (`attach_to_feeder_shmem`).
5. **Semaphore.** The new feeder destroys and re-creates the semaphore at startup. Requests look it up by key on every lock and unlock and
   ignore errors ([lib/synch.cpp][synch], `lock_sema` in [sched_send.cpp][sched_send]), so a request still on the old segment can briefly lose or skew slot exclusion. `SEM_UNDO`
   restores the count when that request exits, and the database check in step 2 still prevents a
   double send.

`stop_sched` would not help: it returns the same `maintenance_delay` and only widens the window. What keeps the gap short is starting the
new feeder as soon as the old one exits, instead of waiting up to five minutes for the supervisor
loop. `maintenance_delay` itself is not part of this procedure. It changes only with a server image,
because a new value changes the rendered `config.xml` and so the SHA-256 the Qwen auto-cohort
controller pins ([boinc-intel-gpu.md](../apps/intel-gpu.md)).

### Client retry behaviour

A BOINC client that gets a project-down reply sets its next scheduler request to now +
`request_delay` (`handle_scheduler_reply` in [cs_scheduler.cpp][cs_scheduler]). It then counts the
reply as a failed request and backs off by min(60 s × 2ⁿ, 4 h) × U(0.5, 1), where n is the number of
failed requests to the project in a row, capped at 10 (`project_rpc_backoff` in
[scheduler_op.cpp][scheduler_op], `calculate_exponential_backoff` in [client_state.cpp][client_state]).
`set_min_rpc_time` keeps the later of the two times ([project.cpp][project]), so the client waits max(`maintenance_delay`, backoff):

| Failures in a row | Backoff                 | Wait at 1200 s | Wait at 3600 s (older image) |
| ----------------- | ----------------------- | -------------- | ---------------------------- |
| 1 to 4            | 1–2 min, up to 8–16 min | 20 min         | 60 min                       |
| 5                 | 16–32 min               | 20–32 min      | 60 min                       |
| 6                 | 32–64 min               | 32–64 min      | 60–64 min                    |
| 7                 | 64–128 min              | 64–128 min     | 64–128 min                   |
| 8 to 10           | 2–4 h                   | 2–4 h          | 2–4 h                        |

The eleventh failure in a row is the exception to that rule. It clears the time the reply just set,
schedules a fresh fetch of the project's master page and starts n again at 1, so that client waits
only 1–2 minutes, `maintenance_delay` or not (`project_rpc_backoff`). A
successful request sets n back to 0 (`handle_scheduler_reply`), so a
client that asked during a short gap comes back once, 20 minutes later. If the server can't be
reached over HTTP at all, there is no reply and no `request_delay`, and only the backoff applies
(`rpc_failed` in [scheduler_op.cpp][scheduler_op]).

## Procedure

Run as root on CT 230, then as `boincadm` in the server container. Keep every readback in the
evidence directory. It holds result and workunit IDs, so it stays on the server and never goes into
the repository.

### 1. Take the project mutex and record the before state

```sh
# CT 230, as root. The lock is held until this shell exits.
flock --exclusive --wait 120 /run/usnm-boinc-step.lock \
  podman exec -it --user boincadm usnm-boinc-server bash
```

In that shell (create `/project/ops` as root first if `boincadm` cannot write to it):

```sh
cd /project/usnmocr
host=$(python3 -c 'import socket; print(socket.gethostname().split(".")[0])')
restart_hold="${USNM_PID_ROOT:-/var/lib/usnm-boinc}/pid_$host/usnm-feeder-restart-hold"
evidence=/project/ops/feeder-refresh-$(date -u +%Y%m%dT%H%M%SZ)
mkdir "$evidence"
python3 /usr/local/lib/usnm-boinc/feeder-slot-guard.py --project /project/usnmocr | tee "$evidence/guard-before.json"
bin/status --verbose | tee "$evidence/status-before.txt"
ls -ld stop_maintenance stop_sched stop_daemons reread_db .usnm-publish.lock "$restart_hold" 2>&1 | tee "$evidence/markers-before.txt"
bin/show_shmem > "$evidence/shmem-before.txt"
ipcs -m | tee "$evidence/ipcs-before.txt"
cp "pid_$host/usnm-feeder-slots.json" "$evidence/snapshot-before.json"
# The snapshot stores each weight as the hex of its IEEE double.
snap() { python3 -c 'import json, struct, sys; r = json.load(open(sys.argv[1])); print(r["pid"], r["start_ticks"], [(i, struct.unpack(">d", bytes.fromhex(w))[0]) for i, w in r["apps"]])' "$1"; }
pid=$(cat "pid_$host/feeder.pid")
ticks=$(awk '{print $22}' "/proc/$pid/stat")
echo "$pid $ticks" | tee "$evidence/feeder-before.txt"
pgrep -u boincadm -x feeder
tr '\0' ' ' < "/proc/$pid/cmdline"; echo
snap "$evidence/snapshot-before.json"
```

The readbacks must show: `drift` in `guard-before.json`; `BOINC is ENABLED` with every enabled daemon
`running` and `locked`; `No such file or directory` for every marker; `ready: 1` in
`shmem-before.txt`; one feeder PID, equal to `$pid`, with the expected command line; and a snapshot
whose `pid` and `start_ticks` equal `$pid` and `$ticks`.

Then save the registry with read-only queries, run the way `entrypoint.sh` runs `mysql`
(`--defaults-extra-file` naming a 0600 option file, `--ssl --ssl-verify-server-cert
--ssl-ca=/usr/local/share/usnm/mysql-ca.pem`) with the runtime user from `config.xml`. Never put a
password on a command line, and delete the option file afterwards. Save the output as
`$evidence/app-before.txt`:

```sql
SELECT id, name, deprecated, weight, homogeneous_redundancy, n_size_classes, min_version FROM app ORDER BY id;
SELECT COUNT(*) FROM platform WHERE deprecated=0;
SELECT COUNT(*) FROM assignment WHERE multi<>0;
-- The app versions the new feeder will load, selected as scan_tables does.
SELECT COUNT(*) FROM app_version v
  JOIN app a ON a.id = v.appid AND a.deprecated = 0
  JOIN platform p ON p.id = v.platformid AND p.deprecated = 0
  WHERE v.deprecated = 0 AND (
    (a.min_version <> 0 AND v.version_num >= a.min_version)
    OR (a.min_version = 0 AND NOT EXISTS (
      SELECT 1 FROM app_version newer
        WHERE newer.appid = v.appid AND newer.platformid = v.platformid AND newer.deprecated = 0
          AND newer.plan_class = BINARY v.plan_class AND newer.version_num > v.version_num)));
SELECT COUNT(*) FROM result r JOIN workunit w ON w.id = r.workunitid
  WHERE r.server_state = 2 AND w.hr_class <> 0;
```

Check the active rows against the preconditions and against the reviewed change. The app-version
count follows `scan_tables`: on active
platforms, every version at or above an app's nonzero `min_version`, otherwise the newest in each
plan class. It must be at most 99. Don't count from `shmem-before.txt`: that is the old feeder's
last scan, which misses later changes.

### 2. Stop the feeder

Check the identity again immediately before signalling, then send one `SIGHUP`:

```sh
lines=$(wc -l < "log_$host/feeder.log")
date -u +%FT%TZ | tee -a "$evidence/stop-time.txt"
[ "$(cat "pid_$host/feeder.pid")" = "$pid" ] && [ "$(awk '{print $22}' "/proc/$pid/stat")" = "$ticks" ] && kill -HUP "$pid"
```

Wait at most two minutes for it to exit:

```sh
for i in $(seq 24); do [ "$(awk '{print $22}' "/proc/$pid/stat" 2>/dev/null)" = "$ticks" ] || break; sleep 5; done
tail -n +"$((lines + 1))" "log_$host/feeder.log" | grep 'GOT STOP SIGNAL'
```

The `grep` shows that the feeder caught the signal (`stop_signal_handler` in
[sched_util_basic.cpp][sched_util_basic]); the loop above shows that it exited. The feeder exits after its
current pass, normally within seconds. If it is still running after two
minutes, do not send another signal, `SIGTERM` or `SIGKILL`. The flag is set, and it will exit
when its current database call returns. Check the database and the feeder log, and stay with it:
the supervisor loop starts a new feeder within five minutes of the exit if you don't. If it still
hasn't exited after 10 minutes, escalate to the maintainer. `SIGTERM` and `SIGKILL` skip
`cleanup_shmem`, leaving a segment marked ready that nothing refills, so they need the
maintainer's approval.

### 3. Start the feeder once

```sh
attempt=1
bin/start --cron 2>&1 | tee "$evidence/start-$attempt.txt"
```

Expect exactly one `Starting daemon: feeder -d 2 --allapps --purge_stale 5` line and no other
daemon. Any periodic task that is due runs as well, as it would in the loop's own run. If the
output is `start is currently running!`, the supervisor loop is
starting daemons right now: don't run it again, go to step 4. If it prints no `Starting daemon:` line
at all, the loop started the feeder between steps 2 and 3, which is just as good: go to step 4.

Never use plain `bin/start` (clears `stop_sched`), `bin/stop` or
`--disable` (stops every daemon and disables the project), `supervisorctl`
or the systemd unit.

### 4. Verify the start

Within two minutes:

```sh
newpid=$(cat "pid_$host/feeder.pid"); echo "$newpid"
pgrep -u boincadm -x feeder
tr '\0' ' ' < "/proc/$newpid/cmdline"; echo
# From the last feeder start since the stop: only the feeder running now, whoever started it.
from=$(awk -v after="$lines" 'NR > after && /Executing command: feeder / {n = NR} END {print n ? n : after + 1}' "log_$host/feeder.log")
tail -n +"$from" "log_$host/feeder.log" > "$evidence/feeder-log-after-$attempt.txt"
grep -E 'Executing command: feeder |have apps for' "$evidence/feeder-log-after-$attempt.txt"
grep -F '[CRITICAL]' "$evidence/feeder-log-after-$attempt.txt"
bin/show_shmem > "$evidence/shmem-after-$attempt.txt"
grep -E '^(ready|max_wu_results):' "$evidence/shmem-after-$attempt.txt"
sed -n '/^apps:/,/^app versions:/p' "$evidence/shmem-after-$attempt.txt"
ipcs -m | tee "$evidence/ipcs-after-$attempt.txt"
bin/status --verbose | tee "$evidence/status-after-$attempt.txt"
```

Each attempt keeps its own files, and an earlier failed attempt's log lines stay out of a later
attempt's readback. Expect:

- A new PID, different from `$pid`, as the only feeder, with the same command line.
- One `Executing command: feeder -d 2 --allapps --purge_stale 5` line, then the startup scan's five
  `have apps for` lines. The `[CRITICAL]` `grep` prints nothing. At `-d 2` the feeder doesn't log
  its app or slot counts (its `read ... apps` line is `MSG_NORMAL`), so they come from `show_shmem`.
- `ready: 1` and `max_wu_results: 4096`.
- The `apps:` list is the active rows in `id` order: same IDs, `hr: 0`, and the same weights to the
  two decimals `show_shmem` prints (`SCHED_SHMEM::show` in [sched_shmem.cpp][sched_shmem]). A
  position in this list is the app index the allocation uses. `scan_tables` reads the table without
  an `ORDER BY`, normally giving ascending `id`; any other order is a finding.
- One segment with key `0x1111aaaa` in `ipcs -m`. The old segment may linger with key
  `0x00000000` and status `dest` until its last request finishes; it must be gone within minutes.
- In `status-after-$attempt.txt`, the feeder `running` and `locked` with the new PID, and every
  other daemon with the same PID as in `status-before.txt`: only the feeder restarted.

At least two minutes after the start (the snapshot is refreshed about once a minute and is
accepted for three):

```sh
python3 /usr/local/lib/usnm-boinc/feeder-slot-guard.py --project /project/usnmocr | tee "$evidence/guard-after.json"
cp "pid_$host/usnm-feeder-slots.json" "$evidence/snapshot-after.json"
snap "$evidence/snapshot-after.json"
cat "pid_$host/feeder.pid"
```

Expect `match` with `allocation_matches_registry`, a snapshot naming `$newpid`, and the same
`$newpid` still running: a feeder whose first `update_av_scales` fails exits after it has set
`ready` (`update_av_scales failed` in [feeder.cpp][feeder]). Then record the PID and leave the shell to release
the mutex:

```sh
echo "$newpid" | tee "$evidence/feeder-after.txt"
exit
```

The host's guard unit may have recorded failed runs (`unknown`) during the window; that is
expected, so note them in the evidence.

If anything in this step fails, go to [Failed start and retry](#failed-start-and-retry).

### 5. Verify visibility and assignment

The mutex is released, but the registry writers stay paused until this step is done. Run these
checks in a new shell without the lock, restoring the values from steps 1 and 4:

```sh
# CT 230, as root. No lock this time.
podman exec -it --user boincadm usnm-boinc-server bash
```

```sh
cd /project/usnmocr
host=$(python3 -c 'import socket; print(socket.gethostname().split(".")[0])')
evidence=/project/ops/feeder-refresh-<the time stamp from step 1>
newpid=$(cat "$evidence/feeder-after.txt")
```

Allow at least two purge rotations before judging the cache (`--purge_stale 5` is five minutes:
`purge_stale_time` in [feeder.cpp][feeder]), and at least
`maintenance_delay` (20 minutes, or an hour on an older image) for clients that hit the window to
come back.

1. **Slot shares.** Count the cached results per app:

   ```sh
   bin/show_shmem | awk '$1 ~ /^[0-9]+$/ {n[$2]++} END {for (a in n) print a, n[a]}'
   ```

   `show_shmem` cuts app names to 12 characters (`SCHED_SHMEM::show`).
   No app may hold more than its share, about `4096 × weight / sum`. An app whose unsent backlog
   exceeds its share should be close to it. `weighted_interleave` gives the exact numbers, and the
   proposed helper would check every slot's app against it.

2. **Bounded replicas visible.** For the currently admitted bounded cohort, list the unsent
   replicas (`SELECT id, workunitid FROM result WHERE workunitid IN (...) AND server_state=2`) and
   check that their IDs appear in `show_shmem`'s `result ID` column within two rotations.
3. **Assignment to an eligible host.** A host with a matching app version that has not run another
   replica of the workunit (`one_result_per_host_per_wu` in
   [`config.xml.template`](../../../boinc/project/config.xml.template)) receives one
   of those replicas on its own next request. For those IDs,
   `SELECT id, hostid, server_state, sent_time FROM result WHERE id IN (...)` shows
   `server_state=4` with a `sent_time` after the stop time. Don't restart clients or force requests
   on volunteer hosts.
4. **Distinct-host validation.** When both replicas return, they came from different hosts and the
   workunit validated, as release verification requires
   ([Feeder visibility](../apps/app-release.md#feeder-visibility)).
5. **Load.** The feeder's CPU time and memory (`ps -o pid,etime,time,rss -p "$newpid"`), the
   database's load, and the `feeder not running` lines in `log_<host>/scheduler.log` since the
   stop time, compared with the same hours a day earlier.

If the replicas stay out of the cache for three rotations while the app's slots are full of other
work, or stay unsent while eligible hosts ask for work, the allocation is not the problem. Stop
and report: another refresh would not help. As #1462 found, a correct allocation with no replica
the requesting host may take is not a feeder fault.

## Failed start and retry

A start has failed if, within two minutes of step 3, there is no new live feeder, `ready` is not 1,
the feeder log has a `[CRITICAL]` line, `show_shmem`'s `apps:` list is not the active rows,
or the new PID disappears or changes before the guard reports `match`. Every error below ends the
feeder, except perhaps the census one. One that gets past creating the segment removes it on the
way out (`atexit(cleanup_shmem)`), so no ready segment is left, every request gets the
project-down reply ([Client retry behaviour](#client-retry-behaviour)), and the supervisor loop
tries `bin/start --cron` again every five minutes until it is held.

| Feeder log                                                 | Source                                       | Kind                                                                                                                                                                    |
| ---------------------------------------------------------- | -------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `boinc_db.open`, `DB connection lost`                      | [feeder.cpp][feeder]                         | Transient: the database.                                                                                                                                                |
| `update_av_scales failed`                                  | [feeder.cpp][feeder] `feeder_loop`           | A database write after `ready`. Transient once; deterministic if again.                                                                                                 |
| `Can't parse config.xml`                                   | [feeder.cpp][feeder] `main`                  | Deterministic: configuration.                                                                                                                                           |
| `The SCHED_SHMEM structure is too small for the ... table` | [sched_shmem.cpp][sched_shmem] `overflow`    | Deterministic: the registry (too many apps, versions, platforms).                                                                                                       |
| `HR type ... out of range` and the other HR messages       | [feeder.cpp][feeder] `hr_init`               | Deterministic: the registry.                                                                                                                                            |
| `can't destroy shmem`                                      | [feeder.cpp][feeder] `main`                  | Deterministic: the segment belongs to another user.                                                                                                                     |
| `can't create shmem`                                       | [feeder.cpp][feeder] `main`                  | Deterministic: memory or kernel limits.                                                                                                                                 |
| `Missing size census file`, and `show_shmem` lists no apps | [sched_shmem.cpp][sched_shmem] `scan_tables` | Deterministic: the registry. The scan stops before any app is counted, and allocating slots over no apps is undefined: the feeder may crash or stay up serving nothing. |

### Hold the supervisor loop

As soon as a start has failed, hold the loop so that it makes no attempts of its own:

```sh
touch stop_maintenance
echo "hold $(date -u +%FT%TZ)" | tee -a "$evidence/hold-time.txt"
```

`boinc-daemons.sh` checks the marker before each `bin/start --cron` and skips the run while it
exists, and a container start waits for it
(`maintenance` in [`boinc-daemons.sh`](../../../boinc/images/server/boinc-daemons.sh)). It
stops nothing that is running. While it is in place, the loop starts no daemon and runs no periodic
task, and the guard reports `maintenance`, which doesn't fail the host unit. The hold is part of
the approved window. Record the loop's own attempts made before the hold: every
`Executing command: feeder` line in the feeder log after `$lines` that your `bin/start --cron`
didn't make, which match the wrapper's `restart k of N` lines in
`journalctl -u usnm-boinc-server.service` on CT 230. There are few, because the loop runs every five
minutes and the hold follows the failure at once. If the loop has already held itself
([The loop's restart bound](#the-loops-restart-bound)), add `stop_maintenance` all the same and copy
`$restart_hold` into the evidence directory.

Release the hold only once a feeder passes step 4 after the hold: run step 4 up to the guard
check, then release it and run the guard check, which reports `maintenance` while the hold is in
place:

```sh
rm stop_maintenance
rm -f "$restart_hold"
echo "release $(date -u +%FT%TZ)" | tee -a "$evidence/hold-time.txt"
```

If the guard then reports anything but `match`, or the rest of step 4 fails, put the hold back at
once with the hold commands above and escalate. If a check fails before the release, or the failure
needs the maintainer, the hold stays until they decide.

### The rules

- One refresh allows at most **one** operator retry: one more `bin/start --cron`, run once with
  `attempt=2` and the hold in place, and only after the cause is fixed and confirmed. For the
  database, confirm it with a read-only query such as `SELECT COUNT(*) FROM app WHERE deprecated=0`
  over step 1's verified-TLS connection, saved to `$evidence/db-check.txt`. The guard can't confirm
  it: during the hold it reports `maintenance` before it queries anything
  (`check` in [`feeder-slot-guard.py`](../../../boinc/images/server/feeder-slot-guard.py)).
  Then repeat step 4. If a feeder is already running by
  then (the loop started it before the hold), don't retry: verify that feeder with step 4 and
  `attempt=2`. Never script a retry, loop, or watch and restart.
- A deterministic failure is never retried as it is. If the registry is the cause, apply
  [Rollback](#rollback) with the hold in place; the retry follows that. Otherwise tell the
  maintainer at once, because the project sends no work while the feeder is down. A whole-server
  restart and an image rollback are the maintainer's decisions.
- A feeder that stayed up after a deterministic failure (the census case) is stopped with one
  `SIGHUP` as in step 2, after the registry is restored and before the retry. Step 2 checks `$pid`
  and `$ticks`, which still name the original feeder, so first capture the failed feeder's identity
  and check that it is `$newpid` and the only feeder:

  ```sh
  pid=$(cat "pid_$host/feeder.pid")
  ticks=$(awk '{print $22}' "/proc/$pid/stat")
  echo "$pid $ticks" | tee "$evidence/feeder-failed.txt"
  [ "$pid" = "$newpid" ] && pgrep -u boincadm -x feeder
  ```

  Then run step 2. If it does not exit, escalate as in step 2.

- If the retry fails too, keep the hold and escalate.
- So during a refresh the feeder is started only by step 3, by the loop's attempts before the
  hold and by the one retry. Nothing else starts it until the hold is released.

## Rollback

A refresh changes no database row, project file or queue. It replaces one process and its cache.
There is no way back to the old process or its stale map, and no reason to want one. What can be
rolled back is the registry the new feeder allocated from:

- If the new allocation is unwanted (the registry change itself was wrong), or the registry made the
  start fail: restore the `app` rows to what the previous feeder started with. After a failed start,
  do it with the [hold](#hold-the-supervisor-loop) in place, so that the loop cannot start a feeder
  on a half-restored registry. Use the change's own saved rows (for example a retirement's journal,
  as in [Retiring an app](server-and-apps.md#retiring-an-app)) and check them against the `apps`
  IDs and weights in `snapshot-before.json`.
- After a failed start, the one retry then starts the feeder on the restored registry. After a
  successful but unwanted refresh, run one more feeder-only refresh with this procedure: the same
  preconditions, readbacks and retry limit, and the maintainer's approval. Touching `reread_db`
  is not a substitute.
- A start that fails for any other reason has no rollback here; escalate as above.

## Never automate without approval

The guard and its timer stay read-only reporters. None of the following may be automated, chained
to a guard result (including through a systemd `OnFailure=` or a supervisor event), or done
without the maintainer's approval for that occasion:

- Signalling, stopping or starting the feeder or any other daemon, on any guard status; any retry
  beyond the single manual one; `SIGTERM` or `SIGKILL`.
- Touching `reread_db` as a repair.
- Changing the app table (`deprecated`, `weight`, HR fields), `config.xml` (`feeder_query_size`,
  `shmem_work_items`, `maintenance_delay`) or the `config.xml` SHA-256 the auto-cohort controller
  pins.
- Creating or removing `stop_maintenance` (other than the manual failure hold inside an approved
  window), creating `stop_sched` or `stop_daemons`, running `bin/stop`, or disabling the project.
- `supervisorctl restart boinc`, restarting `usnm-boinc-server.service`, or changing
  `SERVER_IMAGE`.
- Cancelling, resetting or reassigning results; widening plan-class bounds; weakening
  `one_result_per_host_per_wu` or quorum.
- Anything on volunteer hosts.

The only automation this design allows is notification: the failed guard unit. A read-only
`--plan-refresh` mode for `feeder-slot-guard.py`, which would print the checks of steps 1 and 4 as
one report, has been proposed but not built. The loop's [restart bound](#the-loops-restart-bound) is no
repair either: it only stops the loop's own restarts, and an operator releases it.

## Before first use

- The guarded server image (#1464) must be deployed and its feeder started at a coordinated
  boundary, so that the guard has a baseline
  ([Feeder allocation guard](onprem.md#feeder-allocation-guard-1462)). The server copies
  `bin/feeder` from the image only when the container starts
  (`project_tree` in [`entrypoint.sh`](../../../boinc/images/server/entrypoint.sh)), so
  that deployment is a server restart and outside this procedure.
- The first live refresh needs the maintainer's approval. Its step 5 is the rollout verification
  #1462 asks for: live app IDs and weights, query and slot sizing, HR, bounded replicas in the
  cache, assignment to an eligible host, distinct-host validation and load.

[feeder]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/feeder.cpp
[sched_shmem]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_shmem.cpp
[sched_shmem_h]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_shmem.h
[start]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/start
[sched_util_basic]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_util_basic.cpp
[sched_main]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_main.cpp
[sched_check]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_check.cpp
[boinc_db]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/db/boinc_db.cpp
[sched_send]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_send.cpp
[sched_config]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/sched/sched_config.cpp
[shmem]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/lib/shmem.cpp
[synch]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/lib/synch.cpp
[cs_scheduler]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/cs_scheduler.cpp
[scheduler_op]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/scheduler_op.cpp
[client_state]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/client_state.cpp
[project]: https://github.com/BOINC/boinc/blob/22bbebe08e2eaac5d5e88a6d93b88e800cb6341c/client/project.cpp
