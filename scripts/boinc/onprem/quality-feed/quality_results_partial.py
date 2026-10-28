#!/usr/bin/env python3
"""Certificate-upsert one verified QUALITY unit without replacing a complete campaign snapshot."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import time
import uuid

FILES = ("pages.parquet", "crops.parquet", "summary.md")
UNIT = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,255}")
SNAPSHOT = re.compile(r"[a-f0-9]{32}")
LOCK_ACQUIRED = "USNM_QUALITY_PARTIAL_STEP_LOCK_ACQUIRED"
FRONTIER_LIMIT = 10_000
REMOTE_WORK_SECONDS = 160  # Leave 20 seconds before the outer TERM for durable failure progress.


def replay_collection_diagnostics(stderr):
    stderr = stderr or b""
    if stderr:
        print(stderr.decode("utf-8", errors="replace"), file=sys.stderr, end="", flush=True)
    return LOCK_ACQUIRED.encode() in stderr.splitlines()


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    staged = path.with_name(path.name + "." + uuid.uuid4().hex)
    with staged.open("x") as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(staged, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def select_unit(units, after, through=""):
    names = sorted(units)
    if not names:
        raise ValueError("no submitted QUALITY units")
    through = through or names[-1]
    selected = next((name for name in names if after < name <= through), None)
    if selected is None:
        selected, through = names[0], names[-1]
    return selected, through


def select_frontier(inventory, state):
    """Three changed/unloaded visits per fair visit, with finite lexical fences."""
    names = sorted(inventory)
    if not names or len(names) > FRONTIER_LIMIT or any(not UNIT.fullmatch(n) for n in names):
        raise ValueError("invalid or oversized completion inventory")
    for item in inventory.values():
        if (item.get("status") not in {"pending", "failed", "canonical-present"}
                or (item.get("status") != "canonical-present" and item.get("fingerprint") is not None)
                or ((item.get("status") == "canonical-present")
                    != bool(re.fullmatch(r"[a-f0-9]{64}", item.get("fingerprint") or "")))):
            raise ValueError("invalid canonical inventory entry")
    loaded = state.get("loaded", {})
    if (not isinstance(loaded, dict) or len(loaded) > FRONTIER_LIMIT
            or state.get("turns", 0) not in range(4)
            or any(not UNIT.fullmatch(n) or not re.fullmatch(r"[a-f0-9]{64}", v)
                   for n, v in loaded.items())
            or any(state.get(key) and not UNIT.fullmatch(state[key])
                   for key in ("priority_after", "priority_through", "fair_after", "fair_through"))):
        raise ValueError("invalid completion frontier progress")
    dirty = [n for n in names if inventory[n]["fingerprint"]
             and inventory[n]["fingerprint"] != loaded.get(n)]
    lane = "priority" if dirty and state.get("turns", 0) % 4 != 3 else "fair"
    selected, through = select_unit(
        dirty if lane == "priority" else names,
        state.get(lane + "_after", ""), state.get(lane + "_through", ""),
    )
    updated = {**state, "schema": 1, "loaded": {n: v for n, v in loaded.items() if n in inventory},
               "turns": (state.get("turns", 0) + 1) % 4,
               lane + "_after": selected, lane + "_through": through}
    return selected, names[-1], lane, updated


def completion_inventory(args, project, base, selected=None, deadline=None):
    qwen_root = project / "qwen" / args.qwen_campaign if args.qwen_campaign else base / "no-qwen"
    if args.qwen_campaign:
        # An unavailable configured reading source is not an empty reading campaign.
        with os.scandir(qwen_root / "units"):
            pass
        with os.scandir(project / "usnmocr/results/8"):
            pass
    command = [
        args.collector, "quality", "collect", "--campaign", args.campaign,
        "--root", str(project / "quality" / args.campaign),
        "--spool", str(project / "usnmocr/results/0"),
        "--qwen-root", str(qwen_root),
        "--qwen-spool", str(project / "usnmocr/results/8") if args.qwen_campaign else str(qwen_root),
        "--out-dir", str(base / "unused-inventory-out"),
        "--cache", str(base / "frontier-parts"), "--inventory-only",
    ]
    if selected:
        command += ["--inventory-unit", selected]
    budget = min(120, deadline - time.monotonic()) if deadline is not None else 120
    if budget <= 0:
        raise subprocess.TimeoutExpired(command, 0)
    result = subprocess.run(command, capture_output=True, text=True, timeout=budget)
    if result.returncode:
        print(result.stderr, file=sys.stderr, end="", flush=True)
    result.check_returncode()
    return json.loads(result.stdout)


def run_loader(command, timeout=120):
    with subprocess.Popen(command, start_new_session=True, stdout=sys.stderr) as process:
        try:
            code = process.wait(timeout=timeout)
            if code:
                raise subprocess.CalledProcessError(code, command)
        except BaseException:
            for sig in (signal.SIGTERM, signal.SIGKILL):
                try:
                    os.killpg(process.pid, sig)
                except ProcessLookupError:
                    pass
                if sig == signal.SIGTERM:
                    try:
                        process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        pass
            process.wait()
            raise


def scoped_collect(args, project=Path("/project")):
    pass_started = time.monotonic()
    root = project / "quality" / args.campaign
    frontier = getattr(args, "completion_frontier", False)
    deadline = pass_started + REMOTE_WORK_SECONDS if frontier else None
    names = []
    for index, path in enumerate((root / "units").iterdir()):
        if frontier and index >= FRONTIER_LIMIT:
            raise ValueError("completion inventory exceeds 10000 unit entries")
        if (UNIT.fullmatch(path.name) and (path / "submitted").is_file()
                and (path / "quality.json").is_file()):
            names.append(path.name)
    base = project / "ops/quality-results/scoped" / args.campaign
    frontier_receipt = None
    if frontier:
        frontier_path = base / "frontier.json"
        state = json.loads(frontier_path.read_bytes()) if frontier_path.exists() else {"schema": 1}
        if state.get("schema") != 1 or len(state.get("loaded", {})) > FRONTIER_LIMIT:
            raise ValueError("invalid completion frontier state")
        inventory = completion_inventory(args, project, base, deadline=deadline)
        if set(inventory) != set(names):
            raise ValueError("submitted inventory changed or unavailable")
        state["loaded"] = {n: v for n, v in state.get("loaded", {}).items() if n in inventory}
        ack = getattr(args, "frontier_ack", None)
        if ack:
            ack = json.loads(ack)
            if (not UNIT.fullmatch(ack.get("unit", ""))
                    or not re.fullmatch(r"[a-f0-9]{64}", ack.get("fingerprint", ""))):
                raise ValueError("invalid verified frontier acknowledgement")
            if ack["unit"] in inventory:
                state["loaded"][ack["unit"]] = ack["fingerprint"]
        selected, through, lane, frontier_state = select_frontier(inventory, state)
        frontier_receipt = {"schema": 1, "lane": lane, **inventory[selected],
                            "inventory_seconds": time.monotonic() - pass_started}
    else:
        selected, through = select_unit(names, args.after, args.through)
    source = root / "units" / selected
    job = json.loads((source / "quality.json").read_bytes())
    scope = base / hashlib.sha256(selected.encode()).hexdigest()
    scope_units = scope / "quality/units"
    scope_units.mkdir(parents=True, exist_ok=True, mode=0o700)
    link = scope_units / "selected"
    if link.is_symlink():
        if link.readlink() != source:
            raise ValueError("scope source changed")
    elif link.exists():
        raise ValueError("scope source is not an isolated symlink")
    else:
        link.symlink_to(source, target_is_directory=True)
    out = scope / "snapshots" / args.snapshot / "out"
    if out.parent.exists():
        raise ValueError("snapshot already exists")
    out.mkdir(parents=True, mode=0o700)
    qwen_root = (str(project / "qwen" / args.qwen_campaign) if args.qwen_campaign
                 else str(scope / "no-qwen"))
    command = [
        args.collector, "quality", "collect", "--campaign", args.campaign,
        "--root", str(scope / "quality"), "--spool", str(project / "usnmocr/results/0"),
        "--qwen-root", qwen_root,
        "--qwen-spool", str(project / "usnmocr/results/8") if args.qwen_campaign else qwen_root,
        "--out-dir", str(out), "--cache", str(scope / "parts"), "--max-dirty-units", "1",
    ]
    started = time.time()
    try:
        budget = min(120, deadline - time.monotonic()) if deadline is not None else 120
        if budget <= 0:
            raise subprocess.TimeoutExpired(command, 0)
        result = subprocess.run(command, capture_output=True, text=True, timeout=budget)
        (out.parent / "collector.log").write_text(result.stdout + result.stderr)
        result.check_returncode()
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        if frontier:
            # A poison canonical result must not pin either lane. This is not a load ack.
            atomic_json(frontier_path, frontier_state)
        raise
    if frontier:
        # Reuse the canonical dependency key, not mtimes or a guessed completion filename.
        current = completion_inventory(args, project, base, selected, deadline=deadline)
        if current.get(selected) != inventory[selected]:
            raise ValueError("selected completion dependencies changed during collection")
    docs = [p["doc_id"] for p in job.get("pages", [])] or [job["doc_id"]]
    receipt = {
        "schema": 1, "kind": "verified-quality-unit-partial",
        "campaign": args.campaign, "snapshot": args.snapshot,
        "campaign_complete": False, "coverage": "selected-unit-only-at-collection",
        "submitted_units_observed": len(names), "unvisited_units": len(names) - 1,
        "selected_unit": selected, "selected_job": job["id"], "selected_doc_ids": docs,
        "cycle_through": through,
        "qwen_campaign": args.qwen_campaign, "collector_sha256": digest(Path(args.collector)),
        "started_at": started, "finished_at": time.time(),
        "files": {name: digest(out / name) for name in FILES},
        "snapshot_dir": str(out.parent),
        "prior_science_policy": "upsert-present-rows-only; never delete absent or unvisited rows",
    }
    if frontier:
        receipt["frontier"] = frontier_receipt
        frontier_receipt["remote_seconds"] = time.monotonic() - pass_started
    atomic_json(out.parent / "coverage.json", receipt)
    if frontier:
        # Scheduling progress is not load acknowledgement. Failed loads stay priority-eligible.
        atomic_json(frontier_path, frontier_state)
    return receipt


def validate_receipt(receipt, campaign, snapshot):
    if (receipt.get("schema") != 1 or receipt.get("kind") != "verified-quality-unit-partial"
            or receipt.get("campaign") != campaign or receipt.get("snapshot") != snapshot
            or receipt.get("campaign_complete") is not False
            or receipt.get("coverage") != "selected-unit-only-at-collection"
            or not UNIT.fullmatch(receipt.get("selected_unit", ""))
            or not UNIT.fullmatch(receipt.get("cycle_through", ""))
            or receipt["selected_unit"] > receipt["cycle_through"]
            or not re.fullmatch(r"[a-f0-9]{64}", receipt.get("selected_job", ""))
            or not re.fullmatch(r"[a-f0-9]{64}", receipt.get("collector_sha256", ""))
            or not isinstance(receipt.get("selected_doc_ids"), list)
            or not receipt["selected_doc_ids"]
            or not all(isinstance(d, str) for d in receipt["selected_doc_ids"])
            or receipt.get("submitted_units_observed", 0) < 1
            or receipt.get("unvisited_units") != receipt["submitted_units_observed"] - 1
            or set(receipt.get("files", {})) != set(FILES)
            or not all(re.fullmatch(r"[a-f0-9]{64}", d) for d in receipt["files"].values())):
        raise ValueError("invalid or nonpartial coverage receipt")
    base = f"/project/ops/quality-results/scoped/{campaign}/"
    key = hashlib.sha256(receipt["selected_unit"].encode()).hexdigest()
    if receipt.get("snapshot_dir") != f"{base}{key}/snapshots/{snapshot}":
        raise ValueError("invalid snapshot location")
    if "frontier" in receipt:
        frontier = receipt["frontier"]
        if (frontier.get("schema") != 1 or frontier.get("lane") not in {"priority", "fair"}
                or frontier.get("status") not in {"canonical-present", "pending", "failed"}
                or (frontier["status"] != "canonical-present" and frontier.get("fingerprint") is not None)
                or ((frontier["status"] == "canonical-present")
                    != bool(re.fullmatch(r"[a-f0-9]{64}", frontier.get("fingerprint") or "")))):
            raise ValueError("invalid completion frontier receipt")


def verified_load(args):
    import pyarrow.parquet as pq

    root = args.snapshot_dir
    receipt = json.loads((root / "coverage.json").read_bytes())
    validate_receipt(receipt, args.campaign, args.snapshot)
    for name in FILES:
        path = root / "out" / name
        if path.is_symlink() or not path.is_file() or digest(path) != receipt["files"][name]:
            raise ValueError("partial snapshot checksum mismatch")
    rows = pq.read_table(root / "out/pages.parquet").to_pylist()
    allowed = set(receipt["selected_doc_ids"])
    docs = {r["doc_id"] for r in rows}
    if docs and (docs != allowed or any(r["job_id"] != receipt["selected_job"] for r in rows)):
        raise ValueError("partial rows do not belong to selected unit")
    if rows:
        run_loader(
            ["/opt/usnm-quality-feed/venv/bin/python", "/opt/usnm-quality-feed/load.py",
             "results", "--campaign", args.campaign, "--engine", args.engine_image,
             "--file", str(root / "out/pages.parquet"),
             "--dsn", Path("/etc/usnm/quality-db/dsn").read_text().strip()],
        )
    return {"snapshot": args.snapshot, "campaign": args.campaign, "campaign_complete": False,
            "pages": len(docs), "reader_rows": len(rows), "selected_unit": receipt["selected_unit"]}


def host_pass(args):
    state_path = args.state_dir / args.campaign / "cursor.json"
    state = json.loads(state_path.read_bytes()) if state_path.exists() else {}
    after = state.get("after", "")
    through = state.get("through", "")
    if (after and not UNIT.fullmatch(after)) or (through and not UNIT.fullmatch(through)):
        raise ValueError("invalid persisted cursor")
    snapshot = uuid.uuid4().hex
    common = ["--campaign", args.campaign, "--snapshot", snapshot]
    remote = [
        "pct", "exec", str(args.server_ct), "--", "flock", "-n", "-E", "75",
        "/run/usnm-boinc-step.lock", "sh", "-c",
        'printf "%s\\n" ' + LOCK_ACQUIRED + ' >&2; exec "$@"', "sh",
        "podman", "exec", "-i", "usnm-boinc-server",
        "timeout", "--signal=TERM", "--kill-after=10s", "180s", "python3", "-", "--remote",
        *common, "--collector", args.collector,
    ]
    if after:
        remote += ["--after", after]
    if through:
        remote += ["--through", through]
    if args.qwen_campaign:
        remote += ["--qwen-campaign", args.qwen_campaign]
    frontier = getattr(args, "completion_frontier", False)
    if frontier:
        remote += ["--completion-frontier"]
        if state.get("frontier_ack"):
            remote += ["--frontier-ack", json.dumps(state["frontier_ack"], separators=(",", ":"))]
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15", f"root@{args.server_host}"]
    try:
        response = subprocess.run(
            [*ssh, shlex.join(remote)], input=Path(__file__).read_bytes(),
            capture_output=True, timeout=210,
        )
    except subprocess.TimeoutExpired as error:
        acquired = replay_collection_diagnostics(error.stderr)
        print(json.dumps({"phase": "collection_transport_timeout", "step_lock_acquired": acquired}),
              file=sys.stderr, flush=True)
        raise
    acquired = replay_collection_diagnostics(response.stderr)
    if response.returncode:
        phase = ("step_lock_busy" if response.returncode == 75 and not acquired
                 else "collection_failed" if acquired else "remote_admission_failed")
        print(json.dumps({"phase": phase, "exit": response.returncode}),
              file=sys.stderr, flush=True)
    response.check_returncode()
    if not acquired:
        raise RuntimeError("Missing step-lock acquisition receipt; refusing snapshot transfer")
    receipt = json.loads(response.stdout)
    validate_receipt(receipt, args.campaign, snapshot)
    if frontier != ("frontier" in receipt):
        raise ValueError("completion frontier mode does not match receipt")
    host_receipt = args.state_dir / args.campaign / "receipts" / (snapshot + ".json")
    atomic_json(host_receipt, {"coverage": receipt, "verified": False, "loaded": False})
    dest = f"/var/lib/usnm-quality-feed/results/{args.campaign}/partial/{snapshot}"
    subprocess.run(["pct", "exec", str(args.ctid), "--", "install", "-d", "-m", "700", dest],
                   check=True, timeout=30)
    remote_source = "/srv/usnm-boinc/project/" + receipt["snapshot_dir"].removeprefix("/project/")
    archive = subprocess.run(
        [*ssh, shlex.join(["pct", "exec", str(args.server_ct), "--",
                          "timeout", "--signal=TERM", "--kill-after=5s", "45s",
                          "tar", "-C", remote_source,
                          "-cf", "-", "coverage.json", *("out/" + name for name in FILES)])],
        capture_output=True, check=True, timeout=60,
    ).stdout
    subprocess.run(["pct", "exec", str(args.ctid), "--",
                    "timeout", "--signal=TERM", "--kill-after=5s", "45s",
                    "tar", "-x", "--no-same-owner", "-C", dest],
                   input=archive, check=True, timeout=60)
    load = ["pct", "exec", str(args.ctid), "--",
            "timeout", "--signal=TERM", "--kill-after=10s", "150s",
            "/opt/usnm-quality-feed/venv/bin/python", "-", "--load", *common,
            "--snapshot-dir", dest, "--engine-image", args.engine_image]
    result = subprocess.run(load, input=Path(__file__).read_bytes(),
                            capture_output=True, check=True, timeout=180)
    loaded = json.loads(result.stdout)
    if (loaded.get("snapshot") != snapshot or loaded.get("campaign") != args.campaign
            or loaded.get("campaign_complete") is not False
            or loaded.get("selected_unit") != receipt["selected_unit"]):
        raise ValueError("loader receipt does not match partial snapshot")
    atomic_json(host_receipt, {"coverage": receipt, "verified": True,
                               "loaded": loaded["pages"] > 0, "load": loaded})
    cursor = {"after": receipt["selected_unit"], "through": receipt["cycle_through"],
              "last_snapshot": snapshot, "coverage": "cursor-only; no campaign freshness assertion"}
    if frontier and loaded["pages"] > 0 and receipt["frontier"]["fingerprint"]:
        cursor["frontier_ack"] = {"unit": receipt["selected_unit"],
                                  "fingerprint": receipt["frontier"]["fingerprint"]}
    atomic_json(state_path, cursor)
    return loaded


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--remote", action="store_true")
    mode.add_argument("--load", action="store_true")
    parser.add_argument("--campaign", required=True)
    parser.add_argument("--engine-image")
    parser.add_argument("--qwen-campaign")
    parser.add_argument("--collector", default="/project/ops/quality-results/usnm-boinc")
    parser.add_argument("--snapshot")
    parser.add_argument("--snapshot-dir", type=Path)
    parser.add_argument("--after", default="")
    parser.add_argument("--through", default="")
    parser.add_argument("--completion-frontier", action="store_true",
                        help="opt in to canonical change priority with one fair visit every four passes")
    parser.add_argument("--frontier-ack", help=argparse.SUPPRESS)
    parser.add_argument("--server-host", default="pve.tjg.fyi")
    parser.add_argument("--server-ct", type=int, default=230)
    parser.add_argument("--ctid", type=int, default=215)
    parser.add_argument("--state-dir", type=Path, default=Path("/var/lib/usnm-quality-feed/partial-results"))
    parser.add_argument("--lock", type=Path, default=Path("/run/usnm-quality-results.lock"))
    args = parser.parse_args(argv)
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.campaign)
            or (args.qwen_campaign and not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", args.qwen_campaign))
            or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", args.server_host)
            or min(args.ctid, args.server_ct) < 1
            or (args.after and not UNIT.fullmatch(args.after))
            or (args.through and not UNIT.fullmatch(args.through))
            or not re.fullmatch(r"/project/[A-Za-z0-9_./-]+", args.collector)
            or ".." in Path(args.collector).parts
            or (args.snapshot and not SNAPSHOT.fullmatch(args.snapshot))
            or ((args.remote or args.load) and not args.snapshot)
            or (not args.remote and not re.fullmatch(r"sha256:[a-f0-9]{64}", args.engine_image or ""))
            or (args.load and args.snapshot_dir is None)):
        parser.error("invalid partial collection arguments")
    os.umask(0o077)
    if args.remote:
        report = scoped_collect(args)
    elif args.load:
        def terminate(signum, _frame):
            raise SystemExit(128 + signum)

        signal.signal(signal.SIGTERM, terminate)
        report = verified_load(args)
    else:
        with args.lock.open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            report = host_pass(args)
    print(json.dumps(report, sort_keys=True), flush=True)


if __name__ == "__main__":
    main()
