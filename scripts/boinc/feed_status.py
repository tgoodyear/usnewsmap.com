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
MAX_AGE = 180
MAX_GAP = 180
APP_NAMES = ("usnm_ocr", "usnm_qwen")
DELIVERY_TEST_EVENT = "boinc_feed_delivery_test"


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


def evaluate(facts, previous, now, monotonic, boot_id, config_id):
    """Durations require uninterrupted, fresh observations; never bridge an unknown gap."""
    if (not isinstance(previous, dict)
            or type(previous.get("time", 0)) not in (int, float)
            or type(previous.get("monotonic", 0)) not in (int, float)
            or type(previous.get("last_failure_at", 0)) not in (int, float)
            or not isinstance(previous.get("conditions", {}), dict)):
        previous = {}
    if not 0 <= previous.get("last_failure_at", 0) <= previous.get("time", now):
        previous = {}
    continuous = (previous.get("schema_version") == 1
                  and previous.get("config_id") == config_id
                  and previous.get("boot_id") == boot_id)
    wall = now - previous.get("time", now)
    mono = monotonic - previous.get("monotonic", monotonic)
    continuous = continuous and 0 < wall <= MAX_GAP and 0 < mono <= MAX_GAP and abs(wall - mono) <= 5
    old = previous.get("conditions", {}) if continuous else {}
    conditions, checks = {}, []

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
        state = ("unknown" if condition is None else
                 "firing" if condition and duration >= hold else
                 "pending" if condition else "healthy")
        conditions[code] = {"active": condition is True, "since": since, "generation": generation}
        checks.append({"code": code, "state": state, "duration_s": int(duration),
                       "threshold_s": hold, **measurements})

    deadline = facts.get("enabled_until")
    check("qwen_refill_window", None if deadline is None else deadline - now <= REFILL_WARN_S,
          remaining_s=None if deadline is None else deadline - now)
    for app in APP_NAMES:
        backlog = facts.get("backlogs", {}).get(app)
        latest = facts.get("last_workunits", {}).get(app)
        # None is unavailable, zero is a successfully queried app with no workunit yet.
        no_work = None if backlog is None or latest is None or latest > now else backlog > 0
        check(app + "_no_work", no_work, 3600, generation=latest, backlog=backlog,
              last_workunit_at=latest)
    for resource in ("cpu", "gpu"):
        supply = facts.get("supply", {}).get(resource)
        check(resource + "_shortage", None if supply is None else supply["eligible"] < supply["idle"],
              1800, idle_slots=None if supply is None else supply["idle"],
              eligible_results=None if supply is None else supply["eligible"],
              reason=facts.get("supply_errors", {}).get(resource))
    tick = facts.get("tick")
    failure_at = tick.get("last_failure_at", 0) if tick else 0
    new_failure = failure_at > previous.get("last_failure_at", 0)
    check("controller_tick", None if tick is None else (
        new_failure or tick["ok"] is False or tick["seconds"] > TICK_LIMIT_S),
        seconds=None if tick is None else tick["seconds"])
    unknown = facts.get("errors", []) or [c["code"] for c in checks if c["state"] == "unknown"]
    check("collection", bool(unknown) or not continuous, reset=not continuous)
    document = {"schema_version": 1, "event": "boinc_feed_health", "observed_at": now,
                "max_age_s": MAX_AGE, "checks": checks,
                "sources": facts.get("sources", {}),
                "healthy": all(c["state"] == "healthy" for c in checks)}
    state = {"schema_version": 1, "config_id": config_id, "boot_id": boot_id,
             "time": now, "monotonic": monotonic, "conditions": conditions,
             "last_failure_at": max(failure_at, previous.get("last_failure_at", 0))}
    return document, state


def metrics(document):
    lines = [f'boinc_feed_observed_at_seconds {document["observed_at"]}']
    for check in document["checks"]:
        label = json.dumps(check["code"])
        for state in ("healthy", "pending", "firing", "unknown"):
            lines.append(f'boinc_feed_check{{check={label},state="{state}"}} {int(check["state"] == state)}')
        lines.append(f'boinc_feed_condition_seconds{{check={label}}} {check["duration_s"]}')
        for field in ("remaining_s", "backlog", "last_workunit_at", "idle_slots", "eligible_results", "seconds"):
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
        document, state = evaluate(facts, previous, now, time.monotonic(), boot,
                                   hashlib.sha256(config_bytes).hexdigest())
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
    selected, _ = mode.parse_known_args(argv)
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
