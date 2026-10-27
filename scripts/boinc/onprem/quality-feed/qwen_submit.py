#!/usr/bin/env python3
"""Native Qwen feed gate, run inside the server container under the host bridge lock.

Inventory includes every retained Qwen campaign (single-page and bundled). Submission
uses the existing Rust stage/submit commands and shared pinned models, never a new recipe,
quorum, retry identity or manually generated BOINC result.
"""
import argparse
import json
from pathlib import Path
import re
import shutil
import subprocess


def jobs(root):
    for path in sorted(root.glob("**/qwen.json")):
        yield path, json.loads(path.read_text())


def documents(job):
    return {p["doc_id"] for p in job.get("pages", [])} or {job["doc_id"]}


def inventory(share, project):
    seen = set()
    for _, job in jobs(share / "qwen"):
        seen.update(documents(job))
    php = """
chdir(%s);
require_once("boinc_db.inc");
$app = BoincApp::lookup("name='usnm_qwen'");
if (!$app) throw new RuntimeException("Qwen app missing");
$ready = BoincResult::enum("appid=".intval($app->id)." AND server_state=2");
$running = BoincResult::enum("appid=".intval($app->id)." AND server_state=4");
echo json_encode(["ready"=>count($ready), "dispatched"=>count($running)]);
""" % json.dumps(str(project / "html/inc"))
    counts = json.loads(subprocess.check_output(["php", "-r", php], text=True))
    return {**counts, "documents": sorted(seen), "free_bytes": shutil.disk_usage(share).free}


def assert_submitted(root):
    uncertain = sorted(root.glob("units/*/submitting"))
    pending = [p for p, _ in jobs(root / "units") if not (p.parent / "submitted").exists()]
    if uncertain or pending:
        raise RuntimeError(f"Qwen submission incomplete: {len(uncertain)} uncertain, {len(pending)} without receipts")


def audit(root, project):
    php = """
chdir(%s);
require_once("boinc_db.inc");
$ids = [];
foreach (glob(%s) as $path) {
    $job = json_decode(file_get_contents($path), true, 512, JSON_THROW_ON_ERROR);
    if (!preg_match('/^[a-f0-9]{64}$/D', $job["id"])) throw new RuntimeException("Bad identity");
    $rows = BoincWorkunit::enum("name='usnm_qwen_".$job["id"]."'");
    if (count($rows) !== 1) throw new RuntimeException("Missing or duplicate scientific identity");
    $wu = $rows[0];
    if (intval($wu->batch) !== 8 || intval($wu->min_quorum) !== 2 || intval($wu->target_nresults) !== 2)
        throw new RuntimeException("Unexpected native routing or quorum");
    $ids[] = intval($wu->id);
}
echo json_encode(["verified_workunits"=>count($ids), "first"=>min($ids ?: [0]), "last"=>max($ids ?: [0])]);
""" % (json.dumps(str(project / "html/inc")), json.dumps(str(root / "units/*/qwen.json")))
    return json.loads(subprocess.check_output(["php", "-r", php], text=True))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--share", type=Path, default=Path("/project"))
    ap.add_argument("--project", type=Path, default=Path("/project/usnmocr"))
    ap.add_argument("--inventory", action="store_true")
    ap.add_argument("--campaign")
    ap.add_argument("--engine-plan", type=Path)
    ap.add_argument("--models", type=Path)
    ap.add_argument("--max-pages", type=int, default=100)
    ap.add_argument("--max-ready", type=int, default=1000)
    ap.add_argument("--min-free-gb", type=int, default=50)
    a = ap.parse_args()
    state = inventory(a.share, a.project)
    if a.inventory:
        print(json.dumps(state))
        return
    if (not a.campaign or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
            or a.engine_plan is None or a.models is None or not 1 <= a.max_pages <= 500
            or a.max_ready < 2 or a.min_free_gb < 1):
        ap.error("need --campaign, --engine-plan, --models and positive bounded limits")
    root = a.share / "qwen" / a.campaign
    quality = a.share / "quality" / a.campaign
    if list(root.glob("units/*/submitting")):
        raise RuntimeError("Uncertain Qwen submission: reconcile existing markers against BOINC, never replay")
    # A paused ship must stay pending, not return success to quality-ship's cleanup.
    if state["ready"] + 2 * a.max_pages > a.max_ready:
        raise RuntimeError(f"Qwen watermark: {state['ready']} ready, reserve {2 * a.max_pages}, limit {a.max_ready}")
    if state["free_bytes"] < a.min_free_gb * 1024**3:
        raise RuntimeError("Qwen free-space floor reached")
    elsewhere = set()
    for path, job in jobs(a.share / "qwen"):
        if root not in path.parents:
            elsewhere.update(documents(job))
    for path in [*quality.glob("pages/*/quality.json"), *quality.glob("units/*/quality.json")]:
        job = json.loads(path.read_text())
        if job["language"] in ("eng", "", "und") or job["doc_id"] in elsewhere:
            raise RuntimeError(f"Refusing English, unclassified or duplicate page: {path}")
    engine = json.loads(a.engine_plan.read_text())["engine"]
    stage = ["usnm-boinc", "qwen", "stage", "--quality-root", str(quality), "--root", str(root),
             "--engine-image", engine["image"], "--engine-llama", engine["llama"],
             "--engine-weights", engine["weights"], "--max-pages", str(a.max_pages),
             "--gpu-timeout-secs", "14400", "--cpu-timeout-secs", "7200", "--bundle-size", "1"]
    subprocess.run(stage, check=True)
    subprocess.run(["usnm-boinc", "qwen", "submit", "--root", str(root), "--project", str(a.project),
                    "--target", "native-8b", "--model-dir", str(a.models), "--max-units", str(a.max_pages)],
                   check=True)
    assert_submitted(root)
    print(json.dumps(audit(root, a.project), sort_keys=True))


if __name__ == "__main__":
    main()
