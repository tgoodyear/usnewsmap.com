#!/usr/bin/env python3
"""The Xiaomi-OCR-0 page-mode English pilot's timer step (#1998; docs/boinc/apps/quality.md).

  xiaomi_pilot.py --state FILE --pilot-dir DIR --root DIR --quality-root DIR --project DIR
      --spool DIR --crop-states DIR --held DIR --engine-image sha256:.. --engine-llama ..
      --engine-weights .. [--seen DIR] [--acceptance 0.9] [--log FILE] [--dsn-file FILE]

One run does, in order:

  1. select   the 10,000 pages, once: pilot_select.py into --pilot-dir (needs --dsn-file; with
              the directory already drawn this does nothing, so the pages never change under
              a running pilot).
  2. collect  `usnm-boinc qwen page-collect`: assembles and HOLDS the pages whose pair was
              judged and writes the ocr_outputs and vlm_page_cost rows under --held.
  3. stop     checks the three stop conditions below; a stopped pilot only collects, and its
              reason is in the state file.
  4. submit   when fewer than --max-in-flight pages are staged and unsettled, stages the next
              members chunk (`qwen stage --page-mode`) and submits it, two results a unit
              (`qwen submit --target native-xiaomi-page`, the create_work quorum of 2).
  5. log      one JSON line on stdout and in --log.

Stop conditions (the first that holds is written to the state as `stopped`):

  all-settled    every page of the draw is staged and none is waiting for a result.
  invalid-share  once --min-returned units have come back, more than --max-invalid-share of
                 them are invalid (the canonical result unfinished or no agreeing pair, BOINC
                 gave the unit up, or a page of it was refused at assembly).
  no-return      no page has come back for --stall-hours (default 48) while some are out.

Nothing here writes ocr-local, ocr-out or the search index: the pages are held, and the
collector refuses a held root under any of them. Loading the rows into usnm_quality is
scripts/onprem/quality/load.py xiaomi. Remove the `stopped` key from the state to resume.
"""
import argparse
import datetime
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
CAMPAIGN = "x08-eng-pilot"
TOTAL_PAGES = 10000
MIN_RETURNED = 200
MAX_INVALID_SHARE = 0.20
STALL_HOURS = 48.0
TIMEOUT = 6 * 3600


def utc(now=None):
    return (now or datetime.datetime.now(datetime.timezone.utc)).astimezone(datetime.timezone.utc)


def parse_time(text):
    return datetime.datetime.fromisoformat(text)


def load_state(path):
    try:
        with open(path, encoding="utf-8") as f:
            state = json.load(f)
    except FileNotFoundError:
        return {}
    except ValueError as e:
        sys.exit(f"{path}: not JSON: {e}")
    if not isinstance(state, dict):
        sys.exit(f"{path}: not a JSON object")
    return state


def save_state(path, state):
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=2, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


def chunks(pilot_dir):
    return sorted(n for n in os.listdir(pilot_dir) if n.startswith("members-") and n.endswith(".json"))


def decide(summary, state, total, now, min_returned=MIN_RETURNED,
           max_invalid_share=MAX_INVALID_SHARE, stall_hours=STALL_HOURS, staged_all=False):
    """The stop reason, or None. `summary` is page-collect's."""
    pages = summary.get("pages", 0)
    pending = summary.get("pages_pending", 0)
    returned = pages - pending
    # The safety stop counts units, as #1998 defines it: a unit packs up to four pages, so
    # pages would trip the 200 threshold early and weigh a failure by its bundle's size.
    units_returned = summary.get("units", 0) - summary.get("pending", 0)
    units_invalid = summary.get("units_invalid", 0)
    if returned >= total or (staged_all and pending == 0):
        return "all-settled"
    if units_returned >= min_returned and units_invalid > max_invalid_share * units_returned:
        return "invalid-share"
    since = parse_time(state.get("last_return_at") or state.get("started_at"))
    if pending > 0 and returned <= state.get("last_returned", 0) \
            and (now - since).total_seconds() > stall_hours * 3600:
        return "no-return"
    return None


def run_cmd(cmd):
    """The command's stdout; its failure stops the run, so the timer shows it failed."""
    done = subprocess.run(cmd, capture_output=True, text=True, timeout=TIMEOUT)
    if done.returncode != 0:
        sys.exit(f"{' '.join(cmd[:4])}: exit {done.returncode}: {done.stderr.strip()[-2000:]}")
    return done.stdout


def collect(a, run=run_cmd):
    cmd = [a.binary, "qwen", "page-collect", "--root", a.root, "--quality-root", a.quality_root,
           "--spool", a.spool, "--crop-states", a.crop_states, "--held", a.held]
    if a.seen:
        cmd += ["--seen", a.seen]
    if a.acceptance is not None:
        cmd += ["--acceptance", str(a.acceptance)]
    out = run(cmd).strip().splitlines()
    return json.loads(out[-1]) if out else {}


def submit_chunk(a, name, run=run_cmd):
    members = os.path.join(a.pilot_dir, name)
    run([a.binary, "qwen", "stage", "--page-mode", "--campaign", a.campaign,
         "--strata", os.path.join(a.pilot_dir, "strata.json"), "--members", members,
         "--quality-root", a.quality_root, "--root", a.root, "--engine-image", a.engine_image,
         "--engine-llama", a.engine_llama, "--engine-weights", a.engine_weights,
         "--max-pages", str(a.chunk_pages), "--flush-short"])
    run([a.binary, "qwen", "submit", "--target", "native-xiaomi-page", "--members", members,
         "--root", a.root, "--project", a.project, "--max-units", str(a.chunk_pages)])


def step(a, now=None, run=run_cmd):
    """One run. Returns the log record; the state is saved before it returns."""
    now = utc(now)
    state = load_state(a.state)
    state.setdefault("started_at", now.isoformat())
    state.setdefault("next_chunk", 0)
    state.setdefault("last_returned", 0)
    # pilot_select.py writes pilot.csv first and campaign.json last: only campaign.json says
    # the members files and strata.json are all there, so a draw cut short is made again.
    if not os.path.exists(os.path.join(a.pilot_dir, "campaign.json")):
        if not a.dsn_file:
            sys.exit("no draw in --pilot-dir and no --dsn-file to make one")
        run([sys.executable, os.path.join(HERE, "pilot_select.py"), "--out-dir", a.pilot_dir,
             "--dsn-file", a.dsn_file, "--n", str(a.total), "--seed", a.seed,
             "--chunk", str(a.chunk_pages)])
    names = chunks(a.pilot_dir)
    summary = collect(a, run)
    returned = summary.get("pages", 0) - summary.get("pages_pending", 0)
    if returned > state["last_returned"]:
        state["last_return_at"] = now.isoformat()
    in_flight = summary.get("pages_pending", 0)
    staged_all = state["next_chunk"] >= len(names)
    if "stopped" not in state:
        reason = decide(summary, state, a.total, now, a.min_returned, a.max_invalid_share,
                        a.stall_hours, staged_all)
        if reason:
            state["stopped"] = {"reason": reason, "at": now.isoformat(), "returned": returned}
    action = "none"
    if "stopped" in state:
        action = "stopped"
    elif not staged_all and (
        state.get("active_chunk") == names[state["next_chunk"]]
        or in_flight + a.chunk_pages <= a.max_in_flight
    ):
        name = names[state["next_chunk"]]
        # The chunk is marked before it is staged. If staging worked and submitting did not,
        # the next run finishes this chunk: its pages are already counted in flight, so it
        # does not reserve the allowance a second time.
        state["active_chunk"] = name
        save_state(a.state, state)
        submit_chunk(a, name, run)
        state["next_chunk"] += 1
        state.pop("active_chunk", None)
        action = f"submitted {name}"
    state["last_returned"] = returned
    save_state(a.state, state)
    return {"at": now.isoformat(), "action": action, "chunk": state["next_chunk"], "chunks": len(names),
            "returned": returned, "in_flight": in_flight, "held": summary.get("held", 0),
            "accepted": summary.get("accepted", 0),
            "invalid_units": summary.get("units_invalid", 0),
            "stopped": state.get("stopped", {}).get("reason")}


def parser():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--state", required=True)
    ap.add_argument("--log")
    ap.add_argument("--binary", default="usnm-boinc")
    ap.add_argument("--pilot-dir", required=True)
    ap.add_argument("--dsn-file", help="the quality_reader DSN, to draw the pages when --pilot-dir has none")
    ap.add_argument("--seed", default="x08-eng-pilot-1")
    ap.add_argument("--campaign", default=CAMPAIGN)
    ap.add_argument("--total", type=int, default=TOTAL_PAGES)
    ap.add_argument("--root", required=True, help="the page-mode staging root")
    ap.add_argument("--quality-root", required=True)
    ap.add_argument("--project", required=True)
    ap.add_argument("--spool", required=True)
    ap.add_argument("--crop-states", required=True)
    ap.add_argument("--seen")
    ap.add_argument("--held", required=True)
    ap.add_argument("--acceptance", type=float)
    ap.add_argument("--engine-image", required=True)
    ap.add_argument("--engine-llama", required=True)
    ap.add_argument("--engine-weights", required=True)
    ap.add_argument("--chunk-pages", type=int, default=500, help="pages a run stages and submits at most")
    ap.add_argument("--max-in-flight", type=int, default=1500, help="staged pages waiting for a result, at most")
    ap.add_argument("--min-returned", type=int, default=MIN_RETURNED)
    ap.add_argument("--max-invalid-share", type=float, default=MAX_INVALID_SHARE)
    ap.add_argument("--stall-hours", type=float, default=STALL_HOURS)
    return ap


def main(argv=None):
    ap = parser()
    a = ap.parse_args(argv)
    if not 1 <= a.chunk_pages <= 500 or a.max_in_flight < a.chunk_pages or a.total < 1:
        ap.error("--chunk-pages 1 to 500, --max-in-flight at least that, --total at least 1")
    if not 0 < a.max_invalid_share < 1 or a.stall_hours <= 0:
        ap.error("--max-invalid-share between 0 and 1, --stall-hours positive")
    record = step(a)
    line = json.dumps(record, sort_keys=True)
    print(line)
    if a.log:
        with open(a.log, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    return record


if __name__ == "__main__":
    main()
