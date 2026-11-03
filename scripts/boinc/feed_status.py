#!/usr/bin/env python3
"""Dry-run feed-health snapshot for the BOINC OCR/Qwen feed (#1441).

Reports the raw facts the feed alerts need, and the conditions that can be
decided from one snapshot. It sends nothing: the output is one JSON line on
stdout. The duration rules from #1441 (unsent below idle slots for 30 minutes,
no workunits for an open backlog for an hour) need the previous snapshots, so
the alerting layer must apply them over a window of these lines.
"""
import argparse
import json
import sys
import time

REFILL_WARN_S = 6 * 3600
TICK_LIMIT_S = 15 * 60


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
