# The feeder's restart bound and a stale-slot check

**Status:** measured; the bound is in the server image's source, not deployed · **Date:** November 2026 · **Measured on:** CT 230's server and database journals and the feeder log, 21 October to 5 November 2026; `bin/show_shmem` and the `app` table on 5 November, read-only · **Code:** `contrib-compute/images/server/boinc-daemons.sh`, `feeder-slot-guard.py`; [Feeder-only refresh](../boinc/feeder-refresh.md#the-loops-restart-bound) · **Issues:** #1462

## The question

#1462 left two things after the [feeder refresh runbook](../boinc/feeder-refresh.md): whether the running feeder's slot allocation is stale now, and what bound the server image's supervisor loop should put on restarting a feeder that keeps stopping. The loop ran `bin/start --cron` every five minutes with no limit, so a feeder that failed every start was started again every five minutes for as long as nobody looked.

## What we did

- Counted the feeder's starts in its log (`Executing command: feeder` lines) and the loop's restarts in the server unit's journal (`Starting daemon: feeder` from `bin/start --cron`), and timed the database's restarts from its journal.
- Compared the running feeder's cache with the slot map a new feeder would build. `bin/show_shmem` gave the feeder's apps, weights and cached results; a read-only `SELECT` gave the `app` rows. Reproducing BOINC's `weighted_interleave` over the slots gave each slot's app, and each cached result was checked against its slot. No lock, signal or write.
- Chose a rolling window: at most three restarts of a stopped feeder in any hour, then a hold that an operator releases ([the bound](../boinc/feeder-refresh.md#the-loops-restart-bound)). A count of consecutive failures was rejected because it never holds a feeder that runs for a few minutes after each start; a growing back-off because it never stops trying and lengthens the outage after the cause is fixed. The loop doesn't create `stop_maintenance`, which the runbook keeps for operators.

## What we measured

| What                                                         | Value                                                 | How                                                                    |
| ------------------------------------------------------------ | ----------------------------------------------------- | ---------------------------------------------------------------------- |
| Feeder starts, 21 October 12:42 UTC to 5 November            | 18                                                    | `Executing command: feeder` lines in `log_<host>/feeder.log`           |
| Of those, restarts by the supervisor loop                    | 1, on 3 November; that feeder still ran on 5 November | `Starting daemon: feeder` in `journalctl -u usnm-boinc-server.service` |
| `[CRITICAL]` lines in the feeder log                         | 0                                                     | `grep`                                                                 |
| Database restarts, stop to ready for connections             | 3: 6, 23 and 73 seconds                               | `journalctl -u usnm-boinc-mysql.service`                               |
| Active apps; the feeder's `apps:` list                       | 5; the same 5, in `id` order, with the same weights   | `show_shmem` against `SELECT id, name, deprecated, weight FROM app`    |
| Cached results; results in a slot whose expected app differs | 1,648 of 4,096 slots; 0                               | the reproduced interleave against `show_shmem`'s slots                 |

So no allocation is stale today, and nothing warrants a refresh. A healthy project restarts its feeder almost never, so three restarts an hour is far above normal. Each database restart took less than one five-minute cycle, so it costs the loop at most one restart, and an explicit database restart also restarts the server, whose unit `Requires=` the database's, which clears the count. A feeder that fails every start is held 15 minutes after the loop first finds it stopped, inside one 1200-second `maintenance_delay`.

The numbers don't show a deterministic start failure: none happened in the period, so the 15 minutes is reasoned from the loop's five-minute cycle, not observed. The slot comparison is one moment; the next registry change or reread can make a feeder stale.

## What is left

- The maintainer confirms or changes the default (`USNM_FEEDER_RESTART_LIMIT`, `USNM_FEEDER_RESTART_WINDOW`). Its cost: a database outage longer than about 15 minutes that doesn't restart the server leaves the project held after the database returns, until an operator removes the marker.
- The bound takes effect only with a new server image.
- The slot comparison above is the core of the runbook's proposed read-only `--plan-refresh`, which needs its own issue.
