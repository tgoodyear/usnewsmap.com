#!/usr/bin/env python3
"""Read-only BOINC feed monitor; --config runs live collection and durable evaluation.

--delivery-test prints one synthetic line for the end-to-end notification check
(infra/modules/boinc-feed-alerts.bicep); it reads and writes no state.
The original explicit-input CLI remains an offline diagnostic, not monitoring.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time

REFILL_WARN_S = 6 * 3600
TICK_LIMIT_S = 15 * 60
# Failed or slow controller ticks in a row before controller_tick fires.
TICK_FAILURES_TO_FIRE = 2
# A tick still running past this may never finish, so it fires at once (#1698). One still running
# past TICK_LIMIT_S only waits: an hourly 20-minute tick fired every hour. A tick that finishes
# over TICK_LIMIT_S counts towards the two in a row, as a failure does.
TICK_HANG_S = 2 * TICK_LIMIT_S
# Failed or slow runs within the last hour that fire controller_tick even with good runs between
# them (#1698): a third of a five-minute timer's runs. An hourly slow tick is one.
TICK_FAILURES_PER_HOUR = 4
# Runs in a row that failed with the same error: a sticky failure, which the next tick won't clear.
STICKY_FAILURES = 3
# A plan's tick may wait up to 1800 s for the host's tick lock (controller-tick.sh --lock-wait-secs)
# while its unit already reads as running, and the journal doesn't tell the wait from the run, so a
# plan's running tick is a hang only past the wait plus the live plan's limit.
PLAN_HANG_S = 1800 + TICK_HANG_S
PLAN_CHECK_PREFIX = "plan_"        # plan_discovery and plan_tick:<id>, kept out of `checks`
# feed_collect.PLAN_STATES: the states plan_discovery counts, each a fixed word.
PLAN_STATES_SEEN = ("active", "paused", "draining", "drained", "idle", "unknown")
# The controller's own signals (#1698), opt-in with "controller_signals": true in the config.
CONTROLLER_SIGNAL_CHECKS = ("controller_cap", "ownership_conflict", "controller_progress", "tick_trend")
SIGNAL_FIELDS = ("caps", "ownership_conflicts", "progress", "tick_p90_s")  # feed_collect.SIGNAL_FIELDS
CAP_FIRE_PERCENT = 85              # of any lifetime cap the controller warns about (it warns past 80)
PROGRESS_STALL_S = 2 * 3600        # nothing published by a live plan with pending work
# The controller source reads two hours of journal; a window that starts later than this can't
# prove two hours without a publication (a controller that only just started, or a full journal).
PROGRESS_COVERAGE_S = PROGRESS_STALL_S - 15 * 60
TICK_TREND_FRACTION = 0.7          # of TICK_LIMIT_S, for the p90 of the last hour's ticks
TICK_TREND_HOLD_S = 3600
# Ticks are minutes apart: a failure line within a minute of a run's end, without an
# invocation id, is that run's.
TICK_FAILURE_SAME_S = 60
# The same drift across two five-minute guard runs: the refresh runbook's first condition (#1462).
FEEDER_DRIFT_HOLD_S = 10 * 60
# The native Qwen cohort controller (#1441): its timer runs every minute.
COHORT_STALL_S = 15 * 60          # no completed run at all
COHORT_REPORT_MAX_AGE_S = 30 * 60  # runs complete but none reports (the step lock stays busy)
COHORT_FAILURE_RUNS = 3           # failed runs in a row since the last success, not one
NO_WORK_HOLD_S = 3600
COLLECTION_HOLD_S = 3 * 60        # three evaluations in a row
# Statuses of runs that scanned candidates and measured backlog, buffers and the window.
COHORT_SCAN_STATUSES = ("no-fresh-verified-pairs", "current-window-still-eligible",
                        "bounded-gpu-inflight-buffer-full", "refilled-finite-window")
MAX_AGE = 180
MAX_GAP = 180
APP_NAMES = ("usnm_ocr", "usnm_qwen")
DELIVERY_TEST_EVENT = "boinc_feed_delivery_test"
# A planned version rotation (#1687): while the window lasts, cpu_shortage and usnm_ocr_no_work
# don't fire, and a controller source reporting no recent tick (its timer stopped, a long tick by
# hand: feed_collect's no_recent_controller_tick) doesn't count towards collection. Any other
# controller source failure, every other check, and a controller tick that fails, still fire.
ROTATION_FILE = "rotation.json"
ROTATION_MAX_MINUTES = 240
ROTATION_DEFERRED = ("cpu_shortage", "usnm_ocr_no_work", "controller_progress")
NO_RECENT_TICK = "no_recent_controller_tick"  # feed_collect.NO_RECENT_TICK
# Own clients configured and fresh, but none can prove its capacity (#1698): feed_collect.collect.
NO_PROVEN_CLIENTS = "no_proven_clients"
# The BOINC database's backups and replica (db-backup.md, db-replica.md), opt-in with a
# "dbbackup" source location in the config. Each fires with a fixed reason code.
DB_CHECKS = ("db_replica", "db_dump", "db_binlog", "db_drill")
REPLICA_STOPPED_HOLD_S = 5 * 60   # the IO thread reconnects within seconds after a tunnel blip
REPLICA_LAG_S = 5 * 60
DUMP_MAX_AGE_S = 26 * 3600        # a nightly dump, with two hours of slack
BINLOG_MAX_AGE_S = 15 * 60        # shipped every 5 minutes
DRILL_MAX_AGE_S = 35 * 86400      # a monthly drill, with a few days of slack
# Which source each check waits on (#1698). An unknown check carries the first of its sources
# that is down as `cause` ("source:server"), and the alert rule raises one alert per cause, not one
# per check: a server source going down turned six checks unknown, and each was its own email.
DEPENDS = (
    ("server", ("qwen_refill_window", "usnm_ocr_no_work", "usnm_qwen_no_work", "cpu_shortage",
                "gpu_shortage", "feeder_allocation")),
    ("controller", ("controller_tick", "plan_discovery") + CONTROLLER_SIGNAL_CHECKS),
    ("backlog", ("usnm_ocr_no_work", "usnm_qwen_no_work")),
    ("clients", ("cpu_shortage", "gpu_shortage")),
    ("dbbackup", DB_CHECKS),
)
# After a healthy evaluation the collection hold runs down at half the rate it builds, so a
# source that fails most minutes still fires, and a single failed read never does.
COLLECTION_DECAY = 0.5


def atomic_text(path, value):
    """Same-directory replacement; a stable separate lock protects the state inode."""
    path = Path(path)
    pending = path.with_name(path.name + ".new")
    try:
        with pending.open("w") as stream:
            stream.write(value)
            stream.flush()
            os.fsync(stream.fileno())
        pending.replace(path)
        if os.name == "posix":
            fd = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
    finally:
        pending.unlink(missing_ok=True)


def atomic_json(path, value):
    atomic_text(path, json.dumps(value, sort_keys=True, allow_nan=False) + "\n")


def qwen_admission(record, facts, now):
    """usnm_qwen_no_work from the native cohort controller: (condition, hold, generation, backlog, reason).

    Qwen work comes from the cohort controller admitting audited units that already exist,
    not from new workunits. Firing: its timer is off, three runs in a row failed, no run
    succeeded for 15 minutes (even while one hangs), no run reported for 30, or, for an hour without one more
    admission, the window was open (time and budget left) with admissible backlog, a host
    below its in-flight buffer and nothing left unsent in the current window. The hour
    restarts with every admission (the generation is the admitted count)."""
    if not isinstance(record, dict) or "error" in record:
        return None, NO_WORK_HOLD_S, None, None, "cohort_unavailable"
    if not record["timer_active"]:
        return True, 0, "timer", None, "cohort_timer_inactive"
    if record["failure_streak"] >= COHORT_FAILURE_RUNS:
        return True, 0, "failed:%d" % record["failing_since"], None, "cohort_run_failed"
    last = record["last_completed_at"]
    # Whether or not a run is in progress: a run that hangs must not hide a stall.
    if last is None or now - last > COHORT_STALL_S:
        return True, 0, "stalled", None, "cohort_stalled"
    report = record["report"]
    if report is None or now - record["report_at"] > COHORT_REPORT_MAX_AGE_S:
        return True, 0, "no_report", None, "cohort_no_report"
    window_open = (report["status"] != "closed-expired-budget-or-release-change"
                   and report["admitted_unique"] < report["limit"]
                   and (facts.get("enabled_until") or 0) > now)
    measured = ("admissible_backlog", "per_host_buffer_limit", "native_gpu_inflight",
                "currently_eligible_unsent_replicas")
    if window_open and report["status"] in COHORT_SCAN_STATUSES and any(report[key] is None for key in measured):
        # A run that scanned must have measured; a missing measurement is not a healthy zero.
        return None, NO_WORK_HOLD_S, None, None, "cohort_report_incomplete"
    backlog = (report["admissible_backlog"] or 0) if window_open else 0
    limit = report["per_host_buffer_limit"]
    inflight = report["native_gpu_inflight"] or {}
    # The controller refills only within the smallest headroom of the hosts it serves.
    capacity = limit is not None and bool(inflight) and all(count < limit for count in inflight.values())
    waiting = (report["currently_eligible_unsent_replicas"] or 0) > 0
    return (backlog > 0 and capacity and not waiting, NO_WORK_HOLD_S, "admitted:%d" % report["admitted_unique"],
            backlog, report["status"])


def rotation_window(raw, now):
    """The planned rotation from ROTATION_FILE's contents, while it lasts, else None. Anything
    malformed, expired or longer than ROTATION_MAX_MINUTES is no window: alerts as usual."""
    try:
        window = json.loads(raw)
    except (TypeError, ValueError):
        return None
    if (not isinstance(window, dict) or set(window) != {"reason", "started_at", "until"}
            or type(window["until"]) not in (int, float) or type(window["started_at"]) not in (int, float)
            or not isinstance(window["reason"], str) or not 0 < len(window["reason"]) <= 200
            or not window["started_at"] <= now < window["until"]
            or window["until"] - window["started_at"] > ROTATION_MAX_MINUTES * 60):
        return None
    return window


def read_rotation(state_dir):
    """ROTATION_FILE's text, or None when it is missing, unreadable or not UTF-8: no window, and
    the monitor carries on."""
    try:
        return (Path(state_dir) / ROTATION_FILE).read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return None


def start_rotation(state_dir, minutes, reason, now):
    if not 1 <= minutes <= ROTATION_MAX_MINUTES:
        raise ValueError(f"--minutes is 1 to {ROTATION_MAX_MINUTES}")
    if not 0 < len(reason) <= 200:
        raise ValueError("--reason is 1 to 200 characters")
    window = {"reason": reason, "started_at": now, "until": now + minutes * 60}
    atomic_json(Path(state_dir) / ROTATION_FILE, window)
    return window


def end_rotation(state_dir):
    path = Path(state_dir) / ROTATION_FILE
    existed = path.exists()
    path.unlink(missing_ok=True)
    return existed


def evaluate(facts, previous, now, monotonic, boot_id, config_id, rotation=None):
    """Durations require uninterrupted, fresh observations; never bridge an unknown gap.
    `rotation` is a current rotation_window: ROTATION_DEFERRED checks wait for its end."""
    if (not isinstance(previous, dict)
            or type(previous.get("time", 0)) not in (int, float)
            or type(previous.get("monotonic", 0)) not in (int, float)
            or type(previous.get("last_failure_at", 0)) not in (int, float)
            or type(previous.get("maintenance_failure_at", 0)) not in (int, float)
            or not isinstance(previous.get("conditions", {}), dict)):
        previous = {}
    if not all(0 <= previous.get(key, 0) <= previous.get("time", now) for key in (
            "last_failure_at", "maintenance_failure_at")):
        previous = {}
    continuous = (previous.get("schema_version") == 1
                  and previous.get("config_id") == config_id
                  and previous.get("boot_id") == boot_id)
    wall = now - previous.get("time", now)
    mono = monotonic - previous.get("monotonic", monotonic)
    continuous = continuous and 0 < wall <= MAX_GAP and 0 < mono <= MAX_GAP and abs(wall - mono) <= 5
    old = previous.get("conditions", {}) if continuous else {}
    conditions, checks = {}, []

    deferred_until = rotation["until"] if rotation else None
    # In a rotation the controller may be missing a tick on purpose; nothing else is excused.
    excused = bool(rotation) and facts.get("sources", {}).get("controller", {}).get("reason") == NO_RECENT_TICK
    down = set()
    for name, source in facts.get("sources", {}).items():
        if isinstance(source, dict) and source.get("state") != "fresh":
            down.add("clients" if name.startswith("client") else name)
    # No own client configured, or none that can prove its capacity (#1698): the clients' cause.
    if set(facts.get("supply_errors", {}).values()) & {"no_own_clients_configured", NO_PROVEN_CLIENTS}:
        down.add("clients")
    if excused:
        down.discard("controller")

    def cause(code):
        for source, codes in DEPENDS:
            # Admissions come from the server source's cohort record, not the backlog source.
            if (source in down and code in codes
                    and not (source == "backlog" and code == "usnm_qwen_no_work" and facts.get("qwen_admission"))):
                return "source:" + source
        return None

    def check(code, condition, hold=0, generation=None, **measurements):
        prior = old.get(code, {})
        if not isinstance(prior, dict):
            prior = {}
        since = prior.get("since", now) if (
            condition is True and prior.get("active") and prior.get("generation") == generation) else now
        # A corrupt or future-dated timer is not an elapsed duration.
        if not isinstance(since, (int, float)) or not 0 <= since <= now:
            since = now
        duration = now - since if condition is True else 0
        deferred = code in ROTATION_DEFERRED and deferred_until is not None and now < deferred_until
        state = ("unknown" if condition is None else
                 "firing" if condition and duration >= hold and not deferred else
                 "pending" if condition else "healthy")
        if deferred and condition:
            measurements["rotation_until"] = deferred_until
        if state == "unknown" and cause(code):
            measurements["cause"] = cause(code)
        if state == "unknown" and excused and code in ("controller_tick", *CONTROLLER_SIGNAL_CHECKS):
            # Paused on purpose in a rotation: still unknown, but no alert row (#1687, #1698).
            measurements["excused"] = True
        conditions[code] = {"active": condition is True, "since": since, "generation": generation}
        checks.append({"code": code, "state": state, "duration_s": int(duration),
                       "threshold_s": hold, **measurements})

    deadline = facts.get("enabled_until")
    check("qwen_refill_window", None if deadline is None else deadline - now <= REFILL_WARN_S,
          remaining_s=None if deadline is None else deadline - now)
    for app in APP_NAMES:
        backlog = facts.get("backlogs", {}).get(app)
        latest = facts.get("last_workunits", {}).get(app)
        if app == "usnm_qwen" and facts.get("qwen_admission"):
            no_work, hold, generation, backlog, reason = qwen_admission(facts.get("cohort"), facts, now)
            check(app + "_no_work", no_work, hold, generation=generation, backlog=backlog,
                  last_workunit_at=latest, reason=reason)
            continue
        # None is unavailable, zero is a successfully queried app with no workunit yet.
        no_work = None if backlog is None or latest is None or latest > now else backlog > 0
        check(app + "_no_work", no_work, NO_WORK_HOLD_S, generation=latest, backlog=backlog,
              last_workunit_at=latest)
    # Own hosts left out because their running tasks can't be counted (#1698), only when there are any.
    excluded = {"excluded_hosts": facts["excluded_hosts"]} if facts.get("excluded_hosts") else {}
    for resource in ("cpu", "gpu"):
        supply = facts.get("supply", {}).get(resource)
        check(resource + "_shortage", None if supply is None else supply["eligible"] < supply["idle"],
              1800, idle_slots=None if supply is None else supply["idle"],
              eligible_results=None if supply is None else supply["eligible"],
              reason=facts.get("supply_errors", {}).get(resource), **excluded)
    tick = facts.get("tick")
    failure_at = tick.get("last_failure_at", 0) if tick else 0
    new_failure = failure_at > previous.get("last_failure_at", 0)
    maintenance_at = previous.get("maintenance_failure_at", 0)
    streak = None if tick is None else tick.get("failed_in_a_row")
    # Failures that alternate with good ticks never make a streak; four in an hour still fire.
    hourly = tick is not None and tick.get("failed_last_hour", 0) >= TICK_FAILURES_PER_HOUR
    if tick is None:
        tick_condition = None
    elif streak is None:
        # A collector from before the streak: any failure fires, as it always did.
        tick_condition = new_failure or tick["ok"] is False or tick["seconds"] > TICK_LIMIT_S
    else:
        # One failed or slow tick is often a blip (a hand-off upload timing out, the step lock
        # busy) that the next tick recovers, so the check fires from the second in a row: the
        # collector counts the controller's finished runs since its last good one. A tick still
        # running past the limit may never finish, and a maintenance unit runs once a day:
        # neither waits for a second.
        new_maintenance = tick["maintenance_failure_at"] > maintenance_at
        maintenance_at = max(maintenance_at, tick["maintenance_failure_at"])
        tick_condition = (streak >= TICK_FAILURES_TO_FIRE or new_maintenance or hourly
                          or (tick["ok"] is None and tick["seconds"] > TICK_HANG_S))
    # The newest failure's class (feed_collect.FAILURE_REASONS), never its text (#1698). While the
    # hourly count fires, the hour's newest class, so a good tick between failures keeps one name.
    repeats = None if tick is None else tick.get("same_error_in_a_row")
    reason = None if tick is None else (tick.get("last_hour_reason") if hourly else tick.get("reason"))
    check("controller_tick", tick_condition,
          seconds=None if tick is None else tick["seconds"], failed_in_a_row=streak,
          failed_last_hour=None if tick is None else tick.get("failed_last_hour"),
          reason=reason, same_error_in_a_row=repeats,
          sticky=repeats is not None and repeats >= STICKY_FAILURES)
    if facts.get("controller_signals"):
        controller_signals(check, tick, now)
    plan_checks(check, facts, now)
    if facts.get("dbbackup_checks"):
        db_checks(check, facts.get("dbbackup"), now)
    guard = facts.get("feeder_allocation")
    if guard is not None:
        # Opt-in (#1462). Maintenance skips the comparison, so it is unknown, like any unproven allocation.
        drift = {"match": False, "drift": True}.get(guard["status"])
        check("feeder_allocation", drift, FEEDER_DRIFT_HOLD_S,
              generation=guard["reason"] + ":" + ",".join(map(str, guard["app_ids"])),
              reason=guard["reason"], app_ids=guard["app_ids"])
    errors = [e for e in facts.get("errors", []) if not (excused and e == "controller")]
    unknown = errors or [c["code"] for c in checks if c["state"] == "unknown"
                         and not (excused and c["code"] in ("controller_tick", *CONTROLLER_SIGNAL_CHECKS))]
    # What a cause row in the alert doesn't already name: the alert sends collection only for these.
    causes = {c["cause"] for c in checks if "cause" in c}
    by_code = {c["code"]: c for c in checks}

    def covered(item):
        if item in by_code:
            return "cause" in by_code[item]
        group = "clients" if item.startswith("client_") or item.endswith(
            ("_no_own_clients_configured", "_" + NO_PROVEN_CLIENTS)) else item
        return "source:" + group in causes
    # Held: one failed source run (a single unknown evaluation) is not worth an email.
    # Its time is kept apart from the condition timers a reset clears, so resets that
    # recur (an evaluator running only every few minutes, configs changing) still fire.
    # It adds monotonic time within one boot only, so a wall-clock jump can't fire it
    # early; across a reboot it adds nothing for that step.
    # A healthy evaluation runs the hold down (COLLECTION_DECAY) rather than clearing it.
    unhealthy = bool(unknown) or not continuous
    held = previous.get("collection_elapsed")
    if held is not None and (type(held) not in (int, float) or not 0 <= held <= 10 ** 9):
        held = None
    step = monotonic - previous.get("monotonic", monotonic) if previous.get("boot_id") == boot_id else 0
    step = step if 0 < step <= 10 ** 9 else 0
    if unhealthy:
        elapsed = held + step if held is not None else 0
        carried = elapsed
    else:
        elapsed = None
        carried = max(0, min(held, COLLECTION_HOLD_S) - COLLECTION_DECAY * step) if held is not None else 0
        carried = carried if carried > 0 and previous.get("boot_id") == boot_id else None
    checks.append({"code": "collection", "state": ("firing" if elapsed >= COLLECTION_HOLD_S else "pending")
                   if unhealthy else "healthy", "duration_s": int(elapsed or 0),
                   "threshold_s": COLLECTION_HOLD_S, "reset": not continuous,
                   "uncovered": (sum(not covered(item) for item in unknown) + (not continuous)) if unhealthy else 0})
    # The plans' checks (#1690) ride beside the others, not in them: the dashboard accepts only the
    # check codes it knows and one per plan can't be listed, so it reads `checks` as before and
    # ignores `plan_checks`; the alert reads both.
    document = {"schema_version": 1, "event": "boinc_feed_health", "observed_at": now,
                "max_age_s": MAX_AGE,
                "checks": [c for c in checks if not c["code"].startswith(PLAN_CHECK_PREFIX)],
                "sources": facts.get("sources", {}),
                "healthy": all(c["state"] == "healthy" for c in checks)}
    if any(c["code"].startswith(PLAN_CHECK_PREFIX) for c in checks):
        document["plan_checks"] = [plan_row(c) for c in checks if c["code"].startswith(PLAN_CHECK_PREFIX)]
    if rotation:
        document["rotation"] = {"reason": rotation["reason"], "until": rotation["until"]}
    state = {"schema_version": 1, "config_id": config_id, "boot_id": boot_id,
             "time": now, "monotonic": monotonic, "conditions": conditions,
             "last_failure_at": max(failure_at, previous.get("last_failure_at", 0)),
             "maintenance_failure_at": maintenance_at,
             "collection_elapsed": carried}
    return document, state


def db_checks(check, parts, now):
    """db_replica, db_dump, db_binlog and db_drill from the dbbackup source. Without the source
    they are unknown and wait on source:dbbackup; a part that failed is unknown with its code."""
    if parts is None:
        for code in DB_CHECKS:
            check(code, None)
        return
    replica = parts["replica"]
    lag = replica.get("lag_s")
    if "error" in replica:
        check("db_replica", None, reason=replica["error"])
    elif lag is not None and lag > REPLICA_LAG_S:
        # Lag already over the limit fires at once, whatever the threads do now.
        check("db_replica", True, generation="lag", reason="replica_lag", seconds=lag)
    elif not replica["io_running"]:
        # One timer for "a thread stopped": IO flapping while SQL stays stopped doesn't restart it.
        check("db_replica", True, REPLICA_STOPPED_HOLD_S, generation="stopped", reason="replica_io_stopped",
              seconds=replica["lag_s"])
    elif not replica["sql_running"]:
        check("db_replica", True, REPLICA_STOPPED_HOLD_S, generation="stopped", reason="replica_sql_stopped",
              seconds=replica["lag_s"])
    elif replica["lag_s"] is None:
        check("db_replica", None, reason="replica_lag_unknown")
    else:
        check("db_replica", False, generation="lag", seconds=lag)
    r2 = parts["r2"]
    for code, field, limit, name in (("db_dump", "newest_dump_at", DUMP_MAX_AGE_S, "dump"),
                                     ("db_binlog", "newest_binlog_at", BINLOG_MAX_AGE_S, "binlog")):
        if "error" in r2:
            check(code, None, reason=r2["error"])
        elif r2[field] is None:
            check(code, True, reason=name + "_missing")
        else:
            age = max(0, now - r2[field])
            check(code, age > limit, reason=name + "_stale" if age > limit else None, seconds=age)
    drill = parts["drill"]
    if "error" in drill:
        # No drill on record is as bad as an old one; an unreadable record proves nothing.
        missing = drill["error"] == "drill_missing"
        check("db_drill", True if missing else None, reason=drill["error"])
    else:
        age = max(0, now - drill["finished_at"])
        reason = "drill_failed" if not drill["pass"] else "drill_stale" if age > DRILL_MAX_AGE_S else None
        check("db_drill", reason is not None, generation=reason, reason=reason, seconds=age)


def plan_row(check):
    """A plan check as it is logged: the evaluator's line is one syslog message, so a healthy
    plan carries only its state, and a failing one the numbers that explain it."""
    row = {key: value for key, value in check.items() if value is not None and key != "threshold_s"}
    if check["state"] == "healthy" and not check.get("failed_in_a_row") and not check.get("failed_last_hour"):
        for key in ("seconds", "failed_in_a_row", "failed_last_hour", "same_error_in_a_row", "sticky"):
            row.pop(key, None)
    return row


def plan_checks(check, facts, now):
    """Every other plan's tick (#1690), when the controller source reports them (it runs with
    --discover-plans): one `plan_tick:<id>` check per plan, listed in `plan_checks`, with the live plan's rules and its
    own failed runs, same-error count and hourly count, so one plan's failure never hides or
    resets another's. The id is a hash of the unit name (feed_collect.plan_ticks), not a secret; a retired plan has no check, so
    its timers and its alert go with it.

    States never fire: a paused native plan (`plan_state` paused, with `paused_age_s`), a draining
    plan, a drained one and one with no tick within its window (feed_collect.plan_window) are healthy, and the failed ticks of any
    of them fire like the live plan's. A plan that can't be read is unknown. `plan_discovery`
    carries how many plans there are and how many are in each state."""
    if "plans_error" in facts:
        check("plan_discovery", None, reason=facts["plans_error"])
        return
    plans = facts.get("plans")
    if plans is None:
        return
    states = {state: sum(plan["state"] == state for plan in plans) for state in PLAN_STATES_SEEN}
    check("plan_discovery", False, plans=len(plans), **states)
    for plan in sorted(plans, key=lambda plan: plan["id"]):
        tick = plan["tick"]
        shown = {"kind": plan["kind"], "plan_state": plan["state"]}
        if plan["paused_at"] is not None:
            shown["paused_age_s"] = max(0, now - plan["paused_at"])
        code = "plan_tick:" + plan["id"]
        if plan["error"] is not None:
            check(code, None, reason=plan["error"], **shown)
        elif tick is None:
            check(code, False, **shown)
        else:
            hourly = tick["failed_last_hour"] >= TICK_FAILURES_PER_HOUR
            fires = (tick["failed_in_a_row"] >= TICK_FAILURES_TO_FIRE or hourly
                     or (tick["ok"] is None and tick["seconds"] > PLAN_HANG_S))
            check(code, fires, seconds=tick["seconds"], failed_in_a_row=tick["failed_in_a_row"],
                  failed_last_hour=tick["failed_last_hour"],
                  reason=tick["last_hour_reason"] if hourly else tick["reason"],
                  same_error_in_a_row=tick["same_error_in_a_row"],
                  sticky=tick["same_error_in_a_row"] >= STICKY_FAILURES, **shown)


def controller_signals(check, tick, now):
    """The controller's own signals (#1698): unknown without a controller source, and
    not_reported from a collector that doesn't send them yet."""
    reported = tick is not None and all(field in tick for field in SIGNAL_FIELDS)
    reason = None if tick is None or reported else "not_reported"
    if not reported:
        for code in CONTROLLER_SIGNAL_CHECKS:
            check(code, None, reason=reason)
        return
    caps = tick["caps"]
    worst = max(caps, key=lambda cap: caps[cap]["count"] / caps[cap]["limit"], default=None)
    percent = None if worst is None else round(100 * caps[worst]["count"] / caps[worst]["limit"], 1)
    # A share under 80% isn't warned about, so no warning in the window is healthy.
    check("controller_cap", worst is not None and caps[worst]["count"] * 100 >= caps[worst]["limit"] * CAP_FIRE_PERCENT,
          generation=worst,
          cap=worst, percent=percent)
    conflicts = tick["ownership_conflicts"]
    check("ownership_conflict", None if conflicts is None else conflicts > 0, ownership_conflicts=conflicts,
          reason="not_reported" if conflicts is None else None)
    progress = tick["progress"]
    published, first, pending = progress["published_at"], progress["first_tick_at"], progress["pending"]
    if pending is None:
        stalled = None
    elif published is not None:
        stalled = pending > 0 and now - published >= PROGRESS_STALL_S
    else:
        stalled = pending > 0 and first is not None and now - first >= PROGRESS_COVERAGE_S
    check("controller_progress", stalled, backlog=pending, published_at=published,
          reason="not_reported" if pending is None else None)
    p90 = tick["tick_p90_s"]
    # No tick in the last hour is controller_tick's to report, not a trend.
    check("tick_trend", p90 is not None and p90 > TICK_TREND_FRACTION * TICK_LIMIT_S, TICK_TREND_HOLD_S,
          seconds=p90)


def metrics(document):
    lines = [f'boinc_feed_observed_at_seconds {document["observed_at"]}']
    for check in (*document["checks"], *document.get("plan_checks", ())):
        label = json.dumps(check["code"])
        for state in ("healthy", "pending", "firing", "unknown"):
            lines.append(f'boinc_feed_check{{check={label},state="{state}"}} {int(check["state"] == state)}')
        lines.append(f'boinc_feed_condition_seconds{{check={label}}} {check["duration_s"]}')
        for field in ("remaining_s", "backlog", "last_workunit_at", "idle_slots", "eligible_results", "seconds",
                      "percent", "paused_age_s"):
            if check.get(field) is not None:
                lines.append(f'boinc_feed_{field}{{check={label}}} {check[field]}')
    return "\n".join(lines) + "\n"


def monitor(config_path, state_dir):
    import fcntl
    import feed_collect

    config_bytes = feed_collect.read_file(config_path)
    config = json.loads(config_bytes)
    state_dir.mkdir(parents=True, exist_ok=True)
    with (state_dir / "monitor.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            previous = json.loads(feed_collect.read_file(state_dir / "duration.json"))
            if not isinstance(previous, dict):
                previous = {}
        except (OSError, ValueError):
            previous = {}
        facts = feed_collect.collect(config)
        now = facts["observed_at"]
        # On Linux this survives process restarts but changes across host restarts.
        if sys.platform == "darwin":
            boot = feed_collect.run(["sysctl", "-n", "kern.boottime"], limit=1024).decode().strip()
        else:
            boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
        raw = read_rotation(state_dir)
        document, state = evaluate(facts, previous, now, time.monotonic(), boot,
                                   hashlib.sha256(config_bytes).hexdigest(), rotation_window(raw, now))
        atomic_json(state_dir / "feed-health.json", document)
        atomic_text(state_dir / "feed-health.prom", metrics(document))
        print(json.dumps(document, sort_keys=True), flush=True)
        # A retry may repeat a failure, but must never acknowledge one before publication.
        atomic_json(state_dir / "duration.json", state)
        return document


def delivery_test(now):
    """The synthetic line the alert's delivery_test branch reads; never a health event."""
    return {"schema_version": 1, "event": DELIVERY_TEST_EVENT, "observed_at": now, "test": True}


def snapshot(now, enabled_until, backlog_open, seconds_since_workunit,
             unsent, idle_slots, last_tick_ok, last_tick_seconds):
    """Return the snapshot dict. Pure: all inputs are passed in."""
    remaining = enabled_until - now
    return {
        "time": now,
        "refill_remaining_s": remaining,
        "refill_expired_or_soon": remaining <= REFILL_WARN_S,
        "backlog_open": backlog_open,
        "seconds_since_workunit": seconds_since_workunit,
        "unsent": unsent,
        "idle_slots": idle_slots,
        "unsent_below_idle": unsent < idle_slots,
        "last_tick_ok": last_tick_ok,
        "last_tick_seconds": last_tick_seconds,
        "tick_failed_or_slow": (not last_tick_ok) or last_tick_seconds > TICK_LIMIT_S,
    }


def main(argv=None):
    mode = argparse.ArgumentParser(add_help=False)
    mode.add_argument("--config", type=Path)
    mode.add_argument("--delivery-test", action="store_true")
    mode.add_argument("--rotation-start", action="store_true")
    mode.add_argument("--rotation-end", action="store_true")
    selected, _ = mode.parse_known_args(argv)
    if selected.rotation_start or selected.rotation_end:
        parser = argparse.ArgumentParser(description="Open or close a planned rotation window (#1687)")
        action = parser.add_mutually_exclusive_group(required=True)
        action.add_argument("--rotation-start", action="store_true",
                            help="defer cpu_shortage and usnm_ocr_no_work, and tolerate a missing "
                                 "controller tick, until --minutes from now")
        action.add_argument("--rotation-end", action="store_true", help="close the window now")
        parser.add_argument("--state-dir", type=Path, required=True)
        parser.add_argument("--minutes", type=int, help=f"1 to {ROTATION_MAX_MINUTES}, with --rotation-start")
        parser.add_argument("--reason", help="what is rotating, with --rotation-start (the #529 claim)")
        args = parser.parse_args(argv)
        if args.rotation_end:
            if args.minutes is not None or args.reason is not None:
                parser.error("--rotation-end takes only --state-dir")
            print(json.dumps({"event": "boinc_feed_rotation", "action": "end",
                              "had_window": end_rotation(args.state_dir)}, sort_keys=True), flush=True)
            return 0
        if args.minutes is None or args.reason is None:
            parser.error("--rotation-start needs --minutes and --reason")
        try:
            window = start_rotation(args.state_dir, args.minutes, args.reason, int(time.time()))
        except ValueError as error:
            parser.error(str(error))
        print(json.dumps({"event": "boinc_feed_rotation", "action": "start", **window}, sort_keys=True), flush=True)
        return 0
    if selected.delivery_test:
        parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
        parser.add_argument("--delivery-test", action="store_true", required=True,
                            help="print one boinc_feed_delivery_test line and exit")
        parser.parse_args(argv)
        print(json.dumps(delivery_test(int(time.time())), sort_keys=True), flush=True)
        return 0
    if selected.config is not None:
        parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
        parser.add_argument("--config", type=Path, required=True)
        parser.add_argument("--state-dir", type=Path, required=True)
        live = parser.parse_args(argv)
        monitor(live.config, live.state_dir)
        return 0
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--enabled-until", type=int, required=True,
                        help="ENABLED_UNTIL refill deadline, Unix seconds")
    parser.add_argument("--backlog-open", action="store_true",
                        help="an app has an open backlog")
    parser.add_argument("--seconds-since-workunit", type=int, required=True,
                        help="seconds since the backlogged app last created a workunit")
    parser.add_argument("--unsent", type=int, required=True)
    parser.add_argument("--idle-slots", type=int, required=True)
    parser.add_argument("--last-tick-ok", action="store_true")
    parser.add_argument("--last-tick-seconds", type=int, required=True)
    parser.add_argument("--now", type=int, default=None,
                        help="Unix seconds; defaults to the current time")
    args = parser.parse_args(argv)
    now = int(time.time()) if args.now is None else args.now
    json.dump(snapshot(now, args.enabled_until, args.backlog_open,
                       args.seconds_since_workunit, args.unsent, args.idle_slots,
                       args.last_tick_ok, args.last_tick_seconds), sys.stdout, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
