#!/usr/bin/env python3
"""Native Qwen feed gate, run inside the server container under the host bridge lock.

Inventory includes every retained Qwen campaign (single-page and bundled). Submission
uses the existing Rust stage/submit commands and shared pinned models, never a new recipe,
quorum, retry identity or manually generated BOINC result.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import time


def jobs(root):
    for path in sorted(root.glob("**/qwen.json")):
        yield path, json.loads(path.read_text())


def documents(job):
    return {p["doc_id"] for p in job.get("pages", [])} or {job["doc_id"]}


def inventory(share, project):
    seen = set()
    for _, job in jobs(share / "qwen"):
        seen.update(documents(job))
    for marker in (share / "qwen").glob("*/*/*/too_large"):
        evidence = marker.parent / "rejected-qwen.json"
        if evidence.exists():
            seen.update(documents(json.loads(evidence.read_text())))
    php = """
chdir(%s);
require_once("boinc_db.inc");
$app = BoincApp::lookup("name='usnm_qwen'");
if (!$app) throw new RuntimeException("Qwen app missing");
$ready = BoincResult::enum("appid=".intval($app->id)." AND server_state=2");
$running = BoincResult::enum("appid=".intval($app->id)." AND server_state=4");
$native = "appid=".intval($app->id)." AND batch=8 AND canonical_resultid=0 AND error_mask=0";
$db = BoincDb::get();
$unresolved = $db->count("workunit", $native);
$fresh = $db->count("workunit", $native." AND min_quorum=2 AND target_nresults=2"
    ." AND (SELECT COUNT(*) FROM result r WHERE r.workunitid=workunit.id)=2"
    ." AND (SELECT COUNT(*) FROM result r WHERE r.workunitid=workunit.id"
    ." AND r.server_state=2 AND r.hostid=0 AND r.outcome=0 AND r.validate_state=0"
    ." AND r.sent_time=0 AND r.received_time=0)=2"
    ." AND NOT EXISTS (SELECT 1 FROM assignment a WHERE a.workunitid=workunit.id)");
echo json_encode(["ready"=>count($ready), "dispatched"=>count($running),
    "fresh_pairs"=>intval($fresh), "unresolved_native"=>intval($unresolved)]);
""" % json.dumps(str(project / "html/inc"))
    counts = json.loads(subprocess.check_output(["php", "-r", php], text=True))
    free = min(shutil.disk_usage(path).free
               for path in (share, share / "quality", project / "download"))
    return {**counts, "documents": sorted(seen), "free_bytes": free}


def assert_submitted(root):
    uncertain = sorted(root.glob("units/*/submitting"))
    pending = [p for p, _ in jobs(root / "units") if not (p.parent / "submitted").exists()]
    if uncertain or pending:
        raise RuntimeError(f"Qwen submission incomplete: {len(uncertain)} uncertain, {len(pending)} without receipts")


def assert_processed(root, sample):
    for line in sample.read_text().splitlines():
        doc = json.loads(line)["doc_id"]
        if not re.fullmatch(r"[a-z0-9]+_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+", doc):
            raise RuntimeError("Invalid source document identity")
        unit = root / "units" / doc
        if (unit / "qwen.json").exists() and (unit / "submitted").exists():
            continue
        if (unit / "no_crops").exists():
            continue
        if (unit / "too_large").exists() and (unit / "rejected-qwen.json").exists():
            continue
        raise RuntimeError(f"Chunk still has an unprocessed native source: {doc}; retain READY")


def admission(a, state, root):
    if list(root.glob("units/*/submitting")):
        raise RuntimeError("Uncertain Qwen submission: reconcile existing markers against BOINC, never replay")
    receipt = root / "audit.json"
    raw = receipt.read_bytes() if receipt.exists() else None
    verified = json.loads(raw) if raw is not None else {}
    pending = 0
    seen = set()
    for path, job in jobs(root / "units"):
        if job["id"] in seen:
            raise RuntimeError(f"Duplicate staged Qwen identity: {job['id']}; reconcile, never replay")
        seen.add(job["id"])
        previous = verified.get(job["id"])
        if previous is not None and previous["job_sha256"] != hashlib.sha256(path.read_bytes()).hexdigest():
            raise RuntimeError(f"Previously audited Qwen job changed: {path}; reconcile, never replay")
        pending += not (path.parent / "submitted").exists()
    if set(verified) - seen:
        raise RuntimeError("Previously audited Qwen job missing or identity changed; reconcile, never replay")
    reason = None
    if 2 * (state["fresh_pairs"] + a.max_pages) > a.max_ready:
        reason = "fresh_original_watermark"
    elif state["unresolved_native"] + a.max_pages > a.max_unresolved:
        reason = "unresolved_native_ceiling"
    elif state["free_bytes"] - a.max_pages * a.reserve_mib * 1024**2 < a.min_free_gb * 1024**3:
        reason = "reserved_input_free_space"
    decision = {
        "status": "deferred" if reason else "admitted",
        "phase": "preflight" if a.preflight else "locked_recheck",
        "campaign": a.campaign, "reason": reason, "reserve_pages": a.max_pages,
        "fresh_pairs": state["fresh_pairs"], "max_ready_replicas": a.max_ready,
        "unresolved_native": state["unresolved_native"], "max_unresolved": a.max_unresolved,
        "free_bytes": state["free_bytes"], "reserve_mib_per_page": a.reserve_mib,
        "min_free_gb": a.min_free_gb, "pending_staged_units": pending,
        "audit_total": len(verified),
        "audit_sha256": hashlib.sha256(raw).hexdigest() if raw is not None else None,
        "ready_retained": True, "publication_attempted": False,
    }
    if a.preflight:
        decision["transferred_pages"] = 0
    if reason or a.preflight:
        print(json.dumps(decision, sort_keys=True), flush=True)
    if reason:
        raise SystemExit(75)


def audit(root, project):
    receipt = root / "audit.json"
    verified = json.loads(receipt.read_text()) if receipt.exists() else {}
    pending = {}
    expected = {}
    for path, job in jobs(root / "units"):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if verified.get(job["id"], {}).get("job_sha256") != digest:
            pending[str(path)] = digest
            expected[job["id"]] = digest
    if not pending:
        return {"verified_new": 0, "verified_total": len(verified)}
    php = """
chdir(%s);
require_once("boinc_db.inc");
$verified = [];
foreach (json_decode(stream_get_contents(STDIN), true, 512, JSON_THROW_ON_ERROR) as $path => $sha) {
    if (hash_file("sha256", $path) !== $sha) throw new RuntimeException("Job changed during audit");
    $job = json_decode(file_get_contents($path), true, 512, JSON_THROW_ON_ERROR);
    if (!preg_match('/^[a-f0-9]{64}$/D', $job["id"])) throw new RuntimeException("Bad identity");
    $rows = BoincWorkunit::enum("name='usnm_qwen_".$job["id"]."'");
    if (count($rows) !== 1) throw new RuntimeException("Missing or duplicate scientific identity");
    $wu = $rows[0];
    if (intval($wu->batch) !== 8 || intval($wu->min_quorum) !== 2 || intval($wu->target_nresults) !== 2)
        throw new RuntimeException("Unexpected native routing or quorum");
    $verified[$job["id"]] = ["job_sha256"=>$sha, "workunit_id"=>intval($wu->id), "batch"=>8,
                             "min_quorum"=>2, "target_nresults"=>2];
}
echo json_encode($verified);
""" % json.dumps(str(project / "html/inc"))
    new = json.loads(subprocess.check_output(["php", "-r", php], input=json.dumps(pending), text=True))
    if set(new) != set(expected) or any(row["job_sha256"] != expected[job] for job, row in new.items()):
        raise RuntimeError("Incomplete or changed scientific audit receipt")
    for result in new.values():
        result["audited_at"] = time.time()
    verified.update(new)
    tmp = receipt.with_suffix(".tmp")
    with tmp.open("w") as f:
        json.dump(verified, f, sort_keys=True)
        f.flush()
        import os
        os.fsync(f.fileno())
    tmp.replace(receipt)
    return {"verified_new": len(new), "verified_total": len(verified),
            "workunit_ids": [v["workunit_id"] for v in new.values()]}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--share", type=Path, default=Path("/project"))
    ap.add_argument("--project", type=Path, default=Path("/project/usnmocr"))
    ap.add_argument("--inventory", action="store_true")
    ap.add_argument("--preflight", action="store_true",
                    help="Read-only admission before transferring a READY chunk; exit 75 on capacity deferral")
    ap.add_argument("--campaign")
    ap.add_argument("--engine-plan", type=Path)
    ap.add_argument("--models", type=Path)
    ap.add_argument("--sample", type=Path, help="READY chunk; every source needs a terminal native disposition")
    isolated_stage = Path(__file__).with_name("usnm-boinc-stage")
    ap.add_argument("--stage-binary", default=str(isolated_stage) if isolated_stage.is_file() else "usnm-boinc")
    ap.add_argument("--max-pages", type=int, default=100)
    ap.add_argument("--max-ready", type=int, default=1000,
                    help="Fresh native original replicas, not host-ineligible retry replicas")
    ap.add_argument("--max-unresolved", type=int, default=2000)
    ap.add_argument("--reserve-mib", type=int, default=64,
                    help="Conservative incremental bytes per new page on every backing filesystem")
    ap.add_argument("--min-free-gb", type=int, default=50)
    a = ap.parse_args()
    state = inventory(a.share, a.project)
    if a.inventory:
        print(json.dumps(state))
        return
    if (not a.campaign or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
            or a.engine_plan is None or a.models is None or not 1 <= a.max_pages <= 500
            or a.max_ready < 2 or not 1 <= a.max_unresolved <= 10000
            or a.reserve_mib < 64 or a.min_free_gb < 1):
        ap.error("need --campaign, --engine-plan, --models and positive bounded limits")
    root = a.share / "qwen" / a.campaign
    quality = a.share / "quality" / a.campaign
    admission(a, state, root)
    if a.preflight:
        return
    elsewhere = set()
    for path, job in jobs(a.share / "qwen"):
        if root not in path.parents:
            elsewhere.update(documents(job))
    for path in [*quality.glob("pages/*/quality.json"), *quality.glob("units/*/quality.json")]:
        job = json.loads(path.read_text())
        if job["language"] in ("eng", "", "und") or job["doc_id"] in elsewhere:
            raise RuntimeError(f"Refusing English, unclassified or duplicate page: {path}")
    engine = json.loads(a.engine_plan.read_text())["engine"]
    stage = [a.stage_binary, "qwen", "stage", "--quality-root", str(quality), "--root", str(root),
             "--engine-image", engine["image"], "--engine-llama", engine["llama"],
             "--engine-weights", engine["weights"], "--max-pages", str(a.max_pages),
             "--gpu-timeout-secs", "14400", "--cpu-timeout-secs", "7200", "--bundle-size", "1"]
    subprocess.run(stage, check=True)
    subprocess.run(["usnm-boinc", "qwen", "submit", "--root", str(root), "--project", str(a.project),
                    "--target", "native-8b", "--model-dir", str(a.models), "--max-units", str(a.max_pages)],
                   check=True)
    assert_submitted(root)
    print(json.dumps(audit(root, a.project), sort_keys=True))
    if a.sample:
        assert_processed(root, a.sample)


if __name__ == "__main__":
    main()
