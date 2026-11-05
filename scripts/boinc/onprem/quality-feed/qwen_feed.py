#!/usr/bin/env python3
"""One bounded replenishment tick on the quality shipper host; schedule with systemd.

Uses the existing quality CT/venv/outbox and shipper. Original ALTO is extracted on
the archive host into its existing read-only locstats export, not downloaded from LoC.
The normal scan fetcher alone handles new corpus manifests through its global pacer.
"""
import argparse
import fcntl
import inspect
import json
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time
import urllib.request
import uuid

YIDDISH = "yid"
GERMAN = "deu"


def command(args, data=None):
    return subprocess.check_output(args, input=data, timeout=7200)


def prepare(args):
    """Keep the child's diagnostics in the unit journal, even when it fails."""
    try:
        output = command(args)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
        if error.output:
            text = error.output.decode("utf-8", errors="replace") if isinstance(error.output, bytes) else error.output
            print(text, file=sys.stderr, end="" if text.endswith("\n") else "\n", flush=True)
        raise
    text = output.decode("utf-8", errors="replace") if isinstance(output, bytes) else output
    if text:
        print(text, end="" if text.endswith("\n") else "\n", flush=True)
    written = 0
    for line in text.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if isinstance(event, dict) and event.get("msg") == "chunk ready":
            pages = event.get("pages")
            if type(pages) is not int or pages < 0:
                raise ValueError("invalid prepared chunk page count")
            written += pages
    return written


def remote(host, args, data=None):
    return command(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                    "root@" + host, shlex.join(args)], data)


def available_source_index(lines, inventory):
    names = inventory.get("available_archives")
    if (not isinstance(names, list) or len(names) > 100000
            or any(not isinstance(n, str) or not re.fullmatch(
                r"[a-z]+_[a-zA-Z0-9]+_ver(?:0[1-9]|[1-9][0-9])\.tar\.bz2", n) for n in names)
            or len(set(names)) != len(names)):
        raise ValueError("archive helper must provide a valid fresh retained-archive inventory")
    retained = set(names)
    selected = []
    total = 0
    for line in lines:
        row = json.loads(line)
        total += 1
        name = f"{row['batch']}_ver{int(row.get('batch_version') or 1):02d}.tar.bz2"
        if name in retained:
            selected.append(line)
    return b"".join(selected), total, len(selected)


def budget_spent(path):
    """Pages already prepared under a language budget, across restarts."""
    return json.loads(path.read_text()) if path.exists() else []


def scan_fingerprint(row):
    """A scan as the decode quarantine names it (scan_quarantine.fingerprint)."""
    return (row["doc_id"], row["batch"], int(row.get("batch_version") or 1), row["bytes"], row["sha256"])


# An index row's doc_id, read without parsing the row. Index rows are flat, and a quote inside
# a JSON string is escaped, so this matches only the doc_id key.
DOC_ID_FIELD = re.compile(rb'"doc_id"\s*:\s*"([^"\\]*)"')


def truncated_reservations(index, reserved, truncated, finished):
    """The reserved pages whose scans are cut short at source (#1457), given the fingerprints
    the quarantine records as source_truncated. select never takes such a page again, so it
    frees its place in its budget. A page counts only when its current scan, the last full
    one in the index as select reads it, is cut short: a page done or skipped, or with a newer
    scan to prepare, stays spent."""
    candidates = {fingerprint[0] for fingerprint in truncated} & (reserved - finished)
    if not candidates:
        return set()
    wanted = {doc.encode() for doc in candidates}
    current = {}
    for line in index.splitlines():
        if (found := DOC_ID_FIELD.search(line)) and found[1] not in wanted:
            continue
        row = json.loads(line)
        if row["doc_id"] in candidates and row.get("resolution", "full") == "full":
            current[row["doc_id"]] = scan_fingerprint(row)
    return {doc for doc, scan in current.items() if scan in truncated}


def budgets(a):
    """(language, name, budget) for each language the feed prepares only under a budget."""
    return [(language, name, budget) for language, name, budget in (
        (YIDDISH, "yiddish", a.yiddish_budget), (GERMAN, "german", a.german_budget))
        if budget is not None]


def listed_pages(index, path):
    """The index rows for the pages a --german-pages file names, one doc_id a line."""
    wanted = {line.strip() for line in path.read_text().splitlines() if line.strip()}
    return b"".join(line + b"\n" for line in index.splitlines() if json.loads(line)["doc_id"] in wanted)


def shipment_receipt(output, identity):
    found = None
    for line in (output or b"").decode("utf-8", errors="replace").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and "transaction_receipt" in row:
            found = row["transaction_receipt"]
            if found.get("transaction_id") != identity:
                raise ValueError("unbound remote transaction receipt")
    return found


def acknowledged(value, receipt):
    docs = {member["source"]["doc_id"] for member in value["members"]}
    return bool(receipt and receipt.get("schema") == 1
                and receipt.get("transaction_id") == value["id"]
                and receipt.get("parent_id") == value["parent"]["id"]
                and receipt.get("status") == "acknowledged"
                and receipt.get("native_acknowledged") and receipt.get("quality_acknowledged")
                and set(receipt.get("members", {})) == docs
                and all(row.get("native") in ("published", "no_crops", "too_large")
                        and row.get("quality") == "published" for row in receipt["members"].values()))


def transfer_subset(a, payload, identity):
    upload = "upload-" + uuid.uuid4().hex
    target = "/srv/usnm-boinc/project/qwen-shipments/" + a.target + "/" + identity + "/" + upload
    remote(a.server_host, ["pct", "exec", str(a.server_ct), "--", "mkdir", "-p", target])
    args = ["pct", "exec", str(a.server_ct), "--", "tar", "-x", "--no-same-owner", "-C", target]
    with subprocess.Popen(["tar", "-C", str(payload), "-cf", "-", "."], stdout=subprocess.PIPE) as archive:
        try:
            subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                            "root@" + a.server_host, shlex.join(args)], stdin=archive.stdout,
                           check=True, timeout=7200)
        finally:
            archive.stdout.close()
        if archive.wait() != 0:
            raise RuntimeError("subset archive transfer incomplete")
    return upload


def ship_subsets(a):
    """The existing Temporal ship activity opts in; this creates no new owner or timer."""
    outbox = a.outbox
    with (outbox / (".qwen-ship-" + a.campaign + ".lock")).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print(json.dumps({"status": "uncertain", "phase": "ship_mutex",
                              "publication_attempted": None, "transferred_pages": 0}))
            return 75
        return _ship_subsets(a)


def parent_owner(journal, parent_id):
    path = journal / "owner.json"
    if not path.is_file():
        raise ValueError("unbound parent incarnation; explicit reconciliation required")
    owner = json.loads(path.read_text())
    if (set(owner) != {"schema", "parent_id", "token"} or owner["schema"] != 1
            or owner["parent_id"] != parent_id or not re.fullmatch(r"[a-f0-9]{32}", owner["token"])):
        raise ValueError("changed parent incarnation evidence")
    return owner


def bind_parent_owner(chunk, journal, parent_id):
    import qwen_shipment as shipment
    marker = chunk / ".qwen-shipment-owner.json"
    if not (journal / "owner.json").exists():
        if not marker.exists():
            shipment.save(marker, {"schema": 1, "parent_id": parent_id, "token": uuid.uuid4().hex})
        shipment.freeze(journal / "owner.json", json.loads(marker.read_text()))
    owner = parent_owner(journal, parent_id)
    if chunk.is_symlink() or not marker.is_file() or json.loads(marker.read_text()) != owner:
        raise ValueError("parent directory incarnation changed; retain replacement")


def retained_publication(tx, value):
    import qwen_shipment as shipment
    records = []
    for name in ("receipt.json", "invocation.json"):
        path = tx / name
        if path.exists():
            record = json.loads(path.read_text())
            if (record.get("schema") != 1 or record.get("transaction_id") != value["id"]
                    or record.get("parent_id") != value["parent"]["id"]):
                raise ValueError("unbound retained publication evidence")
            records.append(record)
    return shipment.publication_evidence(*records)


def cleanup_retired_parent(a, chunk, journal):
    """Validate durable acknowledgements, not possibly half-deleted source files."""
    import qwen_shipment as shipment
    if not (journal / "retired.json").exists():
        return False
    parent = json.loads((journal / "parent.json").read_text())
    recipe = json.loads((journal / "recipe.json").read_text())
    retired = json.loads((journal / "retired.json").read_text())
    if (parent["campaign"] != a.campaign or parent["chunk"] != chunk.name
            or retired != {"retired_parent": shipment.digest(parent)}):
        raise ValueError("unbound parent retirement evidence")
    expected = {member["source"]["doc_id"]: member for member in parent["members"]}
    covered = set()
    for path in journal.glob("*/manifest.json"):
        value = shipment.validate(json.loads(path.read_text()))
        receipt = json.loads((path.parent / "receipt.json").read_text())
        if (path.parent.name != value["id"]
                or value != shipment.manifest(parent, value["members"], recipe)
                or not acknowledged(value, receipt)):
            raise ValueError("unbound transaction retirement evidence")
        for member in value["members"]:
            doc = member["source"]["doc_id"]
            if expected.get(doc) != member or doc in covered:
                raise ValueError("conflicting parent retirement membership")
            covered.add(doc)
    if not covered or covered != set(expected) or len(expected) != len(parent["members"]):
        raise ValueError("incomplete parent retirement acknowledgement")
    owner = parent_owner(journal, shipment.digest(parent))
    tombstone = journal / "retired-parent"
    moved = journal / "retirement-move.json"
    cleaned = journal / "cleaned.json"
    if cleaned.exists():
        if json.loads(cleaned.read_text()) != owner or tombstone.exists():
            raise ValueError("changed parent cleanup evidence")
        if chunk.exists():
            raise ValueError("retired chunk name reused; retain replacement")
        return True
    if tombstone.exists():
        if tombstone.is_symlink() or not moved.is_file() or json.loads(moved.read_text()) != owner:
            raise ValueError("unbound retirement tombstone")
    elif chunk.exists():
        marker = chunk / ".qwen-shipment-owner.json"
        if chunk.is_symlink() or not marker.is_file() or json.loads(marker.read_text()) != owner:
            raise ValueError("parent directory incarnation changed; retain replacement")
        if not a.what_if:
            shipment.freeze(moved, owner)
            chunk.rename(tombstone)
    elif not moved.is_file() or json.loads(moved.read_text()) != owner:
        raise ValueError("retired parent missing without owned move evidence")
    if not a.what_if:
        # Persist both sides of the rename before deleting even the ownership marker.
        shipment.sync_dir(chunk.parent)
        shipment.sync_dir(journal)
        for path in journal.glob("*/input"):
            shutil.rmtree(path)
            shipment.sync_dir(path.parent)
        if tombstone.exists():
            shutil.rmtree(tombstone)
            shipment.sync_dir(journal)
        shipment.freeze(cleaned, owner)
    if chunk.exists() and (not a.what_if or tombstone.exists()):
        raise ValueError("retired chunk name reused; retain replacement")
    return True


def _ship_subsets(a):
    import qwen_shipment as shipment
    helper = "/project/ops/qwen-feed/qwen_submit.py"
    ct = ["pct", "exec", str(a.server_ct), "--"]
    server = ct + ["podman", "exec", "usnm-boinc-server", "python3", helper]
    completed = 0
    campaign = a.outbox / a.campaign
    journals = a.outbox / ".qwen-transactions" / a.campaign
    chunks = set(campaign.glob("c[0-9][0-9][0-9][0-9][0-9]"))
    chunks.update(campaign / path.parent.name for path in
                  journals.glob("c[0-9][0-9][0-9][0-9][0-9]/retired.json")
                  if not (path.parent / "cleaned.json").exists())
    for chunk in sorted(chunks):
        if completed >= a.max_chunks:
            break
        journal = a.outbox / ".qwen-transactions" / a.campaign / chunk.name
        if cleanup_retired_parent(a, chunk, journal):
            completed += 1
            continue
        if not (chunk / "READY").is_file():
            continue
        parent = shipment.chunk_snapshot(chunk, a.campaign)
        languages = {member["sample"]["lang"] for member in parent["members"]}
        a.target, version = a.qwen_root or a.campaign, a.prompt_version
        if version >= 3 and "yid" in languages:
            if languages != {"yid"} or not a.qwen_yiddish_root:
                raise ValueError("mixed/unrouted Yiddish parent; retain READY")
            a.target, version = a.qwen_yiddish_root, a.yiddish_prompt_version
        recipe = {"campaign": a.target, "prompt_version": version, "engine_image": a.engine_image,
                  "readers": a.readers, "bundle_size": a.bundle_size}
        recipe.update(json.loads(remote(a.server_host, server + [
            "--recipe-inputs", "--campaign", a.target, "--engine-plan", a.qwen_engine_plan])))
        if not a.what_if:
            shipment.freeze(journal / "parent.json", parent)
            shipment.freeze(journal / "recipe.json", recipe)
            bind_parent_owner(chunk, journal, shipment.digest(parent))
        values = [shipment.validate(json.loads(path.read_text())) for path in journal.glob("*/manifest.json")]
        done = set()
        active = []
        for value in values:
            if value["parent"]["id"] != shipment.digest(parent) or value["recipe"] != recipe:
                raise ValueError("changed frozen parent or transaction recipe")
            receipt_path = journal / value["id"] / "receipt.json"
            receipt = json.loads(receipt_path.read_text()) if receipt_path.exists() else None
            members = {member["source"]["doc_id"] for member in value["members"]}
            if acknowledged(value, receipt):
                if done & members:
                    raise ValueError("overlapping transaction acknowledgements")
                done.update(members)
            else:
                active.append(value)
        if len(active) > 1:
            raise ValueError("multiple active subsets; reconcile before shipping")
        if active and done.intersection(member["source"]["doc_id"] for member in active[0]["members"]):
            raise ValueError("active subset overlaps acknowledged membership")
        left = [member for member in parent["members"] if member["source"]["doc_id"] not in done]
        if left:
            state = json.loads(remote(a.server_host, server + ["--inventory"]))
            pool = json.loads(remote(a.server_host, ["lvs", "--reportformat", "json", "--units", "b",
                                                    "--nosuffix", "-o", "lv_size,data_percent",
                                                    "pve/data"]))["report"][0]["lv"][0]
            free = min(shutil.disk_usage(a.outbox).free,
                       int(float(pool["lv_size"]) * (1 - float(pool["data_percent"]) / 100)))
            limit = shipment.capacity(state, free, max_ready=a.qwen_max_ready,
                                      max_unresolved=a.max_unresolved, min_free_gb=a.min_free_gb)
            value = active[0] if active else shipment.manifest(parent, left[:limit], recipe) if limit else None
            count = len(value["members"]) if value else 0
            if value is None or min(state["free_bytes"], free) - count * 64 * 1024**2 < a.min_free_gb * 1024**3:
                evidence = (retained_publication(journal / active[0]["id"], active[0]) if active
                            else shipment.publication_evidence())
                print(json.dumps({**evidence,
                                  "phase": "preflight", "transferred_pages": 0,
                                  "reserve_pages": count, "parent_id": shipment.digest(parent),
                                  "ready_retained": True}))
                return 75
            if a.what_if:
                print(json.dumps({"status": "would_ship_subset", "reserve_pages": count,
                                  "transaction_id": value["id"], "parent_id": value["parent"]["id"]}))
                return 0
            tx = journal / value["id"]
            shipment.freeze(tx / "manifest.json", value)
            state_path = tx / "transfer.json"
            transferred = state_path.exists()
            if not transferred:
                shipment.materialize(chunk, tx / "input", value)
                upload = transfer_subset(a, tx / "input", value["id"])
                shipment.save(state_path, {"transaction_id": value["id"], "transferred_pages": count,
                                          "upload": upload})
            transfer = json.loads(state_path.read_text())
            if (transfer.get("transaction_id") != value["id"] or transfer.get("transferred_pages") != count
                    or not re.fullmatch(r"upload-[a-f0-9]{32}", transfer.get("upload", ""))):
                raise ValueError("changed transfer journal")
            previous_path = tx / "receipt.json"
            evidence = retained_publication(tx, value)
            invocation_path = tx / "invocation.json"
            invocation = {**evidence, "schema": 1, "transaction_id": value["id"],
                          "parent_id": value["parent"]["id"], "phase": "locked_publication",
                          "ready_retained": True,
                          "transferred_pages": 0 if transferred else count, "reserve_pages": count}
            lock_wait = 900
            if a.step_window_minutes:
                now = time.localtime()
                if now.tm_min >= a.step_window_minutes:
                    invocation["phase"] = "step_window"
                    shipment.save(invocation_path, invocation)
                    print(json.dumps({"shipment_invocation": invocation}))
                    return 75
                lock_wait = a.step_window_minutes * 60 - now.tm_min * 60 - now.tm_sec
            invocation.update(status="partial" if evidence["status"] == "partial" else "uncertain",
                              publication_attempted=True)
            shipment.save(invocation_path, invocation)
            submit = [*server[len(ct):], "--campaign", a.target, "--transaction", value["id"],
                      "--upload", transfer["upload"],
                      "--engine-plan", a.qwen_engine_plan, "--models", a.qwen_models,
                      "--prompt-version", str(version), "--max-ready", str(a.qwen_max_ready),
                      "--max-unresolved", str(a.max_unresolved), "--min-free-gb", str(a.min_free_gb)]
            script = (inspect.getsource(shipment.locked_transaction)
                      + "\nimport json, sys\nraise SystemExit(locked_transaction(*json.loads(sys.argv[1])))\n")
            args = ["python3", "-c", script,
                    json.dumps([a.server_ct, lock_wait, value["id"], count, a.min_free_gb, submit])]
            code, output = 0, b""
            try:
                output = remote(a.server_host, args)
            except subprocess.CalledProcessError as error:
                code, output = error.returncode, error.output
            except BaseException:
                print(json.dumps({"shipment_invocation": invocation}))
                raise
            receipt = shipment_receipt(output, value["id"])
            for line in (output or b"").splitlines():
                try:
                    gate = json.loads(line).get("shipment_gate")
                except (ValueError, AttributeError):
                    continue
                if gate:
                    if gate.get("transaction_id") != value["id"] or gate.get("reserve_pages") != count:
                        raise ValueError("unbound locked storage deferral")
                    invocation.update(evidence, phase=gate["phase"])
            if receipt is not None:
                if (receipt.get("parent_id") != value["parent"]["id"]
                        or set(receipt.get("members", {})) != {m["source"]["doc_id"] for m in value["members"]}):
                    raise ValueError("remote receipt membership changed")
                shipment.save(previous_path, receipt)
                combined = shipment.publication_evidence(evidence, receipt)
                invocation.update(combined, phase=receipt["phase"])
                if receipt["status"] == "acknowledged" or not combined["publication_attempted"]:
                    invocation["status"] = receipt["status"]
            invocation["exit_code"] = code
            shipment.save(invocation_path, invocation)
            print(json.dumps({"shipment_invocation": invocation}), flush=True)
            if code:
                return code
            if not acknowledged(value, receipt):
                raise RuntimeError("server returned success without exact two-application acknowledgement")
            done.update(member["source"]["doc_id"] for member in value["members"])
        if len(done) == len(parent["members"]):
            if a.what_if:
                return 0
            output = json.loads(remote(a.server_host, ct + [
                "flock", "-w", "900", "-E", "75", "/run/usnm-boinc-step.lock", *server[len(ct):],
                "--campaign", a.target, "--retire-parent", shipment.digest(parent)]))
            if output != {"retired_parent": shipment.digest(parent)}:
                raise RuntimeError("unbound parent retirement response")
            shipment.freeze(journal / "retired.json", output)
            cleanup_retired_parent(a, chunk, journal)
        # A partial parent stays oldest: a later tick finishes it before newer work.
        completed += 1
        if len(done) != len(parent["members"]):
            break
    return 0


def ship_main():
    import qwen_shipment as shipment
    ap = argparse.ArgumentParser(description="Opt-in bounded Qwen subset shipment for the existing Temporal owner")
    ap.add_argument("--ship-subsets", action="store_true")
    ap.add_argument("--qwen-subsets", action="store_true")
    ap.add_argument("--campaign", required=True)
    ap.add_argument("--outbox", type=Path, default=Path("/var/lib/usnm-quality-feed/outbox"))
    ap.add_argument("--server-host", default="pve.tjg.fyi")
    ap.add_argument("--server-ct", type=int, default=230)
    ap.add_argument("--engine-image", required=True)
    ap.add_argument("--qwen-engine-plan", required=True)
    ap.add_argument("--qwen-models", required=True)
    ap.add_argument("--qwen-root")
    ap.add_argument("--qwen-yiddish-root")
    ap.add_argument("--prompt-version", type=int, choices=(1, 2, 3, 4), default=1)
    ap.add_argument("--yiddish-prompt-version", type=int, choices=(1, 2, 3, 4), default=4)
    ap.add_argument("--readers", default="loc-hist,as,ours-ndlocr,ours-paddle,ours-surya,ours-tesseract")
    ap.add_argument("--bundle-size", type=int, default=200)
    ap.add_argument("--qwen-max-ready", type=int, default=1000)
    ap.add_argument("--max-unresolved", type=int, default=2000)
    ap.add_argument("--max-chunks", type=int, default=1)
    ap.add_argument("--min-free-gb", type=int, default=50)
    ap.add_argument("--max-unsent", type=int, default=300)  # Legacy CLI compatibility; never gates native.
    ap.add_argument("--step-window-minutes", type=int, default=0)
    ap.add_argument("--what-if", action="store_true")
    a = ap.parse_args()
    if (any(not re.fullmatch(shipment.NAME, name) for name in
            (a.campaign, a.qwen_root or a.campaign, a.qwen_yiddish_root or a.campaign))
            or not re.fullmatch(r"[a-z0-9][a-z0-9.-]*", a.server_host)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", a.engine_image)
            or any(not re.fullmatch(r"/project/[a-zA-Z0-9_./-]+", path)
                   for path in (a.qwen_engine_plan, a.qwen_models))
            or not re.fullmatch(r"[a-z0-9_,-]+", a.readers)
            or not 1 <= a.bundle_size <= 500 or a.max_chunks < 1 or a.server_ct < 1
            or not 2 <= a.qwen_max_ready <= 1000 or not 1 <= a.max_unresolved <= 2000
            or a.min_free_gb < 50 or not 0 <= a.step_window_minutes < 60):
        ap.error("invalid subset shipment route or bounded limits")
    return ship_subsets(a)


def tick(a):
    state = a.state
    state.mkdir(parents=True, exist_ok=True)
    with (state / "lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        _tick(a)


def _tick(a):
    phase_started = time.monotonic()

    def phase(name, **counts):
        nonlocal phase_started
        now = time.monotonic()
        print(json.dumps({"phase": name, "seconds": now - phase_started, **counts}), flush=True)
        phase_started = now

    ct = ["pct", "exec", str(a.prep_ct), "--"]
    python = ct + ["/opt/usnm-quality-feed/venv/bin/python"]
    helper = "/opt/usnm-quality-feed/"
    inv = json.loads(remote(a.server_host, ["pct", "exec", str(a.server_ct), "--", "podman", "exec",
                                          "usnm-boinc-server", "python3",
                                          "/project/ops/qwen-feed/qwen_submit.py", "--inventory"]))
    pool = json.loads(remote(a.server_host, ["lvs", "--reportformat", "json", "--units", "b", "--nosuffix",
                                            "-o", "lv_size,data_percent", "pve/data"]))["report"][0]["lv"][0]
    pool_free = float(pool["lv_size"]) * (1 - float(pool["data_percent"]) / 100)
    producer_free = shutil.disk_usage(a.outbox).free
    free = min(pool_free, producer_free)
    phase("inventory", fresh_pairs=inv["fresh_pairs"], unresolved_native=inv["unresolved_native"])
    if min(free, inv["free_bytes"]) < a.min_free_gb * 1024**3:
        print(json.dumps({"status": "storage_backpressure", "pool_free_bytes": int(pool_free),
                          "producer_free_bytes": producer_free}))
        return
    camp_dir = a.outbox / a.campaign
    pending = list(camp_dir.glob("c*/READY"))
    if (2 * inv["fresh_pairs"] >= a.max_ready
            or inv["unresolved_native"] >= a.max_unresolved or len(pending) >= a.max_pending):
        print(json.dumps({"status": "queue_backpressure", "ready": inv["ready"],
                          "fresh_pairs": inv["fresh_pairs"], "unresolved_native": inv["unresolved_native"],
                          "pending_chunks": len(pending)}), flush=True)
        return
    # Each new source page creates two independent native GPU replicas.
    count = min(a.chunk_pages, a.max_ready // 2 - inv["fresh_pairs"],
                a.max_unresolved - inv["unresolved_native"],
                max(0, int((min(free, inv["free_bytes"]) - a.min_free_gb * 1024**3)
                           // (a.reserve_mib * 1024**2))))
    if count < 1:
        return
    work = "/var/lib/usnm-quality-feed/work/" + a.campaign
    quarantine_state = "/var/lib/usnm-quality-feed/" + a.campaign
    command(ct + ["mkdir", "-p", work])
    done = command(python + ["-c", (
        "from pathlib import Path; import json; "
        f"p=Path('/var/lib/usnm-quality-feed/{a.campaign}'); "
        "print(json.dumps([line.split('\\t')[0] for n in ('done.txt','skipped.txt') "
        "if (p/n).exists() for line in (p/n).read_text().splitlines() if line]))"
    )])
    deferred_path = a.state / "deferred.json"
    deferred = json.loads(deferred_path.read_text()) if deferred_path.exists() else {}
    seen = set(inv["documents"]) | set(json.loads(done)) | {
        d for d, retry_at in deferred.items() if retry_at > time.time()}
    command(ct + ["tee", work + "/staged.txt"], ("\n".join(sorted(seen)) + "\n").encode())
    archive_inventory = json.loads(remote(
        a.archive_host, ["python3", "/opt/usnm-quality-feed/archive_alto.py",
                         "--pages", "-", "--archives", a.archives, "--out", a.alto_export], b""))
    with urllib.request.urlopen(a.scan_server.rstrip("/") + "/index/full.jsonl", timeout=300) as r:
        index, total, retained = available_source_index(r, archive_inventory)
    from qwen_source import priority_index, source_id
    excluded_requests = set()

    def select(limit, *filters):
        command(python + [helper + "quality_feed.py", "select", "--index", work + "/index.jsonl",
                         "--staged", work + "/staged.txt", "--curated", "/srv/curated", "--count", str(limit),
                         "--out", work + "/selected.jsonl", "--non-english", "--order", "damage",
                         "--damage-dsn-file", "/etc/usnm/quality-db/dsn",
                         "--scan-server", a.scan_server,
                         "--quarantine-state", quarantine_state, *filters])
        selected = command(ct + ["cat", work + "/selected.jsonl"])
        return selected, [json.loads(line) for line in selected.splitlines() if line]

    # A budgeted language comes as a chunk of its own, ahead of any source request and only up
    # to its budget: Yiddish because it reads at another request version, German while it is
    # being calibrated. Every other page keeps the normal order without them.
    budgeted = budgets(a)
    # The budgeted pass is the only way in for these languages, whatever a source request names.
    filters = tuple(arg for language, _, _ in budgeted for arg in ("--exclude-language", language))
    pages = []
    retryable = None
    # A reserved page whose scan is cut short at source is refunded (#1457): the quarantine
    # holds it for good, so it would otherwise take its place in the budget forever.
    reservations = {name: budget_spent(a.state / f"{name}-budget.json") for _, name, _ in budgeted}
    refunds = set()
    if any(reservations.values()):
        truncated = {scan_fingerprint(source) for source in json.loads(command(python + ["-c", (
            "from pathlib import Path; import json, sys; "
            "sys.path.insert(0, sys.argv[1]); from scan_quarantine import quarantined; "
            "print(json.dumps([r['source'] for r in quarantined(Path(sys.argv[2])) "
            "if r['refetch_state'] == 'source_truncated']))"
        ), helper, quarantine_state]))}
        # done holds both done.txt and skipped.txt.
        refunds = truncated_reservations(index, {doc for spent in reservations.values() for doc in spent},
                                         truncated, set(json.loads(done)))
    for language, name, budget in budgeted:
        budget_path = a.state / f"{name}-budget.json"
        spent = reservations[name]
        if refunds & set(spent):
            spent = [doc for doc in spent if doc not in refunds]
            tmp = budget_path.with_suffix(".tmp")
            tmp.write_text(json.dumps(spent))
            tmp.replace(budget_path)
            phase(name + "_budget_refund", refunded=len(reservations[name]) - len(spent), spent=len(spent),
                  budget=budget)
        own = index
        if language == GERMAN and a.german_pages is not None:
            own = listed_pages(index, a.german_pages)
        if spent and len(spent) <= budget:
            if retryable is None:
                retryable = set(json.loads(command(python + ["-c", (
                    "from pathlib import Path; import json, sys; "
                    "sys.path.insert(0, '/opt/usnm-quality-feed'); from scan_quarantine import retryable_ids; "
                    f"print(json.dumps(sorted(retryable_ids(Path('/var/lib/usnm-quality-feed/{a.campaign}')))))"
                )])))
            reserved = set(spent)
            retry_index = b"".join(line + b"\n" for line in own.splitlines()
                                   if (row := json.loads(line))["doc_id"] in reserved
                                   and source_id(row) in retryable)
            if retry_index:
                command(ct + ["tee", work + "/index.jsonl"], retry_index)
                raw, pages = select(min(count, len(reserved)), "--language", language)
                phase(name + "_retry", reserved=len(reserved), selected=len(pages))
                if pages:
                    break
        left = min(count, budget - len(spent))
        if left <= 0:
            continue
        command(ct + ["tee", work + "/index.jsonl"], own)
        raw, pages = select(left, "--language", language)
        phase(name + "_budget", spent=len(spent), budget=budget, selected=len(pages))
        if pages:
            break
    quarantined = set()
    if not pages:
        quarantined = set(json.loads(command(python + ["-c", (
            "from pathlib import Path; import json, sys; "
            "sys.path.insert(0, '/opt/usnm-quality-feed'); from scan_quarantine import quarantined_ids; "
            f"print(json.dumps(sorted(quarantined_ids(Path('/var/lib/usnm-quality-feed/{a.campaign}')))))"
        )])))
    while not pages:
        selected_index, request_id = priority_index(index, a.state, a.campaign, seen, excluded_requests,
                                                     quarantined)
        command(ct + ["tee", work + "/index.jsonl"], selected_index)
        phase("retained_archive_filter", indexed_pages=total, retained_archive_pages=retained,
              source_request=request_id)
        raw, pages = select(count, *filters)
        if pages or request_id is None:
            break
        phase("source_request_no_eligible_pages", source_request=request_id)
        excluded_requests.add(request_id)
    phase("source_selection", selected=len(pages))
    if not pages:
        print(json.dumps({"status": "waiting_for_crop_sources", "deferred": len(deferred)}))
    else:
        receipt = json.loads(remote(a.archive_host, ["nice", "-n", "19", "ionice", "-c", "3",
                                                    "python3", "/opt/usnm-quality-feed/archive_alto.py",
                                                    "--pages", "-", "--archives", a.archives,
                                                    "--out", a.alto_export], raw))
        phase("original_alto", cached=len(receipt["cached"]), missing=len(receipt["missing"]),
              pending=len(receipt["pending"]))
        for doc in receipt["missing"]:
            deferred[doc] = time.time() + 86400
        tmp = deferred_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(deferred, sort_keys=True))
        tmp.replace(deferred_path)
        cached = set(receipt["cached"])
        usable = [p for p in pages if p["doc_id"] in cached]
        command(ct + ["tee", work + "/pages.jsonl"],
                "".join(json.dumps(p) + "\n" for p in usable).encode())
        for language, name, _ in budgeted:
            # Written before the chunk exists, so a crash can't overrun the budget.
            budget_path = a.state / f"{name}-budget.json"
            spent = budget_spent(budget_path)
            tmp = budget_path.with_suffix(".tmp")
            tmp.write_text(json.dumps(sorted(set(spent) | {p["doc_id"] for p in usable
                                                           if p["lang"] == language})))
            tmp.replace(budget_path)
        written = 0
        if usable:
            written = prepare(python + [helper + "quality_feed.py", "run", "--campaign", a.campaign,
                             "--pages", work + "/pages.jsonl", "--curated", "/srv/curated",
                             "--scan-server", a.scan_server, "--outbox", str(a.outbox),
                             "--state", "/var/lib/usnm-quality-feed", "--chunk-pages", str(a.chunk_pages),
                             "--max-pending", str(a.max_pending), "--workers", "2", "--once",
                             "--alto-cache", "/srv/locstats/qwen-alto", "--quarantine-decode"])
        phase("prepare_ready", pages=written)
        print(json.dumps({"status": "prepared", "pages": written, "selected_for_preparation": len(usable),
                          "missing_original_alto": receipt["missing"],
                          "pending_original_alto": receipt["pending"],
                          "ready_before": inv["ready"], "highest_damage": pages[0]["damage_rate"]}))


def main():
    if "--ship-subsets" in sys.argv:
        raise SystemExit(ship_main())
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--campaign", required=True)
    ap.add_argument("--state", type=Path, required=True)
    ap.add_argument("--outbox", type=Path, default=Path("/var/lib/usnm-quality-feed/outbox"))
    ap.add_argument("--prep-ct", type=int, default=215)
    ap.add_argument("--server-host", required=True)
    ap.add_argument("--server-ct", type=int, default=230)
    ap.add_argument("--archive-host", required=True)
    ap.add_argument("--archives", default="/tank/locbulk/ocr")
    ap.add_argument("--alto-export", default="/tank/locstats/qwen-alto")
    ap.add_argument("--scan-server", required=True)
    ap.add_argument("--min-free-gb", type=int, default=50)
    ap.add_argument("--max-ready", type=int, default=1000)
    ap.add_argument("--max-unresolved", type=int, default=2000)
    ap.add_argument("--reserve-mib", type=int, default=64)
    ap.add_argument("--chunk-pages", type=int, default=100)
    ap.add_argument("--max-pending", type=int, default=3)
    ap.add_argument("--yiddish-budget", type=int,
                    help="Total Yiddish pages to prepare, as chunks of their own; the normal order leaves "
                         "Yiddish out. Without this flag Yiddish takes its place in the normal order")
    ap.add_argument("--german-budget", type=int,
                    help="Total German pages to prepare, as chunks of their own after any Yiddish "
                         "budget; the normal order leaves German out")
    ap.add_argument("--german-pages", type=Path,
                    help="With --german-budget: a file of doc_ids, one a line; the German budget "
                         "takes only these pages, worst damage first")
    a = ap.parse_args()
    if (not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
            or not 1 <= a.chunk_pages <= 490 or a.min_free_gb < 1 or a.max_ready < 2 or a.max_pending < 1
            or not 1 <= a.max_unresolved <= 10000 or a.reserve_mib < 64
            or (a.yiddish_budget is not None and a.yiddish_budget < 0)
            or (a.german_budget is not None and a.german_budget < 0)
            or (a.german_pages is not None and (a.german_budget is None or not a.german_pages.is_file()))):
        ap.error("invalid campaign or non-positive/out-of-range limits")
    tick(a)


if __name__ == "__main__":
    main()
