#!/usr/bin/env python3
"""One short, guarded automatic NVIDIA/Intel native-Qwen cohort refill."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid
import xml.etree.ElementTree as ET

CLASSES = ("intel_vulkan_8b", "nvidia_vulkan_8b")
MANAGED = (*CLASSES, "nvidia_cuda_2b", "nvidia_cuda_8b", "docker_nvidia_cuda_8b")
ENGINE = {
    "image": "sha256:a25da41a10f266e5c2d02206bf9f0e40b54d476609f85d70a72e4e11792d0efe",
    "llama": "sha256:d962a8228c42c84583150e809d654f9ec995cdf9f4e02e37365293642210a309",
    "weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
}
MODEL = {"file": "Qwen3VL-8B-Instruct-Q4_K_M.gguf", "min_gpu_mb": 7000,
         "mmproj": "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf", "name": "qwen3-vl-8b-q4km"}
MODELS = [MODEL, {"file": "Qwen3-VL-2B-Instruct-Q8_0.gguf", "min_gpu_mb": 0,
                  "mmproj": "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf", "name": "qwen3-vl-2b-q8"}]


def require(condition, message):
    if not condition:
        raise ValueError(message)


class OutOfCohort(ValueError):
    """A sound producer job in a language this controller was not reviewed to admit."""


COHORT_LANGUAGES = ("yid", "srp")


def sha(payload):
    return hashlib.sha256(payload).hexdigest()


def policy_window(payload):
    root = ET.fromstring(payload, parser=ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    nodes = root.findall("plan_class")
    classes = {node.findtext("name"): node for node in nodes}
    require(root.tag == "plan_classes" and len(classes) == len(nodes), "Invalid/duplicate plan classes")
    ranges = []
    for name in CLASSES:
        node = classes[name]
        fields = [node.findall(tag) for tag in ("min_wu_id", "max_wu_id")]
        require(all(len(value) == 1 for value in fields), "General admission is not owned by this controller")
        low, high = (int(value[0].text) for value in fields)
        require((low, high) == (2, 1) or 2 <= low <= high and high - low < 10, "Not a finite pilot")
        ranges.append([low, high])
    require(ranges[0] == ranges[1], "Different NVIDIA/Intel windows: another operator owns admission")
    for name in ("nvidia_cuda_2b", "nvidia_cuda_8b", "docker_nvidia_cuda_8b"):
        node = classes[name]
        require(node.findtext("min_wu_id") == "2" and node.findtext("max_wu_id") == "1",
                "Another managed GPU pilot is active")
    markers = [node for node in root.iter() if node.tag is ET.Comment
               and "usnm-amd-first-reservation-v1" in (node.text or "")]
    reserved = None
    if markers:
        require(len(markers) == 1 and markers[0] in list(root), "Invalid AMD reservation")
        match = re.fullmatch(r"usnm-amd-first-reservation-v1:[a-f0-9]{32}:([1-9][0-9]*):[1-9][0-9]*",
                             (markers[0].text or "").strip())
        require(match is not None, "Malformed AMD reservation")
        reserved = int(match[1])
    return ranges[0], reserved


def class_scope(payload, without_bounds=False):
    root = ET.fromstring(payload)
    classes = {node.findtext("name"): node for node in root.findall("plan_class")}
    fingerprints = {}
    for name in MANAGED:
        node = classes[name]
        node.tail = None
        if without_bounds:
            for child in list(node):
                if child.tag in ("min_wu_id", "max_wu_id"):
                    node.remove(child)
        canonical = ET.canonicalize(ET.tostring(node, encoding="unicode"), strip_text=True)
        fingerprints[name] = sha(canonical.encode())
    return fingerprints


def target_scope(payload, target):
    root = ET.fromstring(payload)
    for name in CLASSES:
        node = root.find(f"plan_class[name='{name}']")
        node.find("min_wu_id").text, node.find("max_wu_id").text = map(str, target)
    return class_scope(ET.tostring(root))


def fresh(row):
    wu, results = row["unit"], row["results"]
    expected = {"appid": 6, "batch": 8, "min_quorum": 2, "target_nresults": 2,
                "rsc_disk_bound": 19327352832, "canonical_resultid": 0, "error_mask": 0,
                "transitioner_flags": 0, "need_validate": 0, "app_version_num": 0, "hr_class": 0}
    if not all(int(wu[key]) == value for key, value in expected.items()) or len(results) != 2:
        return False
    return int(row["assignments"]) == 0 and all(
        all(int(result[key]) == value for key, value in {
            "server_state": 2, "sent_time": 0, "hostid": 0, "outcome": 0, "validate_state": 0,
            "app_version_id": 0, "received_time": 0,
        }.items()) for result in results)


def available_in_window(rows, window, hosts):
    return sum(
        1 for row in rows if window[0] <= int(row["unit"]["id"]) <= window[1]
        and int(row["unit"]["canonical_resultid"]) == 0 and int(row["unit"]["error_mask"]) == 0
        for result in row["results"] if int(result["server_state"]) == 2
        and any(host not in {int(r["hostid"]) for r in row["results"]} for host in hosts)
    )


def assignable_units(rows, window, host):
    # One result per host per workunit: a host can receive at most one replica of each unit.
    return sum(
        1 for row in rows if window[0] <= int(row["unit"]["id"]) <= window[1]
        and int(row["unit"]["canonical_resultid"]) == 0 and int(row["unit"]["error_mask"]) == 0
        and any(int(result["server_state"]) == 2 for result in row["results"])
        and host not in {int(result["hostid"]) for result in row["results"]}
    )


def choose(rows, floor, reserved, remaining, admissible=lambda row: True):
    if remaining <= 0:
        return []
    candidates = sorted((row for row in rows if int(row["unit"]["id"]) >= floor
                         and int(row["unit"]["id"]) != reserved and fresh(row)),
                        key=lambda row: int(row["unit"]["id"]))
    result = []
    for row in candidates:
        current = int(row["unit"]["id"])
        if result and current != int(result[-1]["unit"]["id"]) + 1:
            break
        # The window is a workunit id range: an inadmissible unit ends the run rather than
        # being opened inside it, and one before the run is passed over.
        if not admissible(row):
            if result:
                break
            continue
        result.append(row)
        if len(result) >= min(10, remaining):
            break
    return result


def bind_inputs(project, row, job_id, audit, prompt_version=1):
    wu = row["unit"]
    require(re.fullmatch("[a-f0-9]{64}", job_id) is not None, "Bad native audit identity")
    require(wu["name"] == "usnm_qwen_" + job_id and int(audit["workunit_id"]) == int(wu["id"])
            and all(int(audit[key]) == value for key, value in
                    {"batch": 8, "min_quorum": 2, "target_nresults": 2}.items()), "Audit/WU identity mismatch")
    download = (project / "download").resolve()
    files = {}
    for node in ET.fromstring("<root>" + wu["xml_doc"] + "</root>").findall("file_info"):
        name, url = node.findtext("name", ""), node.findtext("url", "")
        kind = "job" if name.endswith("_qwen_json") else "page" if name.endswith("_page_jpg") else None
        if kind:
            require(kind not in files and "/usnmocr_download/" in url, "Ambiguous input binding")
            path = download / url.split("/usnmocr_download/", 1)[1]
            require(path.resolve().is_relative_to(download), "Input escaped download tree")
            files[kind] = path.read_bytes()
    require(set(files) == {"job", "page"}, "Missing job/page")
    job = json.loads(files["job"])
    doc = job["doc_id"]
    require(isinstance(doc, str) and re.fullmatch("[a-zA-Z0-9][a-zA-Z0-9_.-]*", doc), "Unsafe doc identity")
    require(sha(files["job"]) == audit["job_sha256"] and job["id"] == job_id,
            "Actual job differs from producer audit")
    require(job["input"]["sha256"] == sha(files["page"]) and job["input"]["bytes"] == len(files["page"]),
            "Actual page identity changed")
    require(all(job["engine"][key] == value for key, value in ENGINE.items())
            and job["engine"]["models"] == MODELS, "Unreviewed engine/models")
    require(type(job["v"]) is int and job["v"] == 1 and type(job["prompt_version"]) is int
            and job["prompt_version"] == prompt_version and job["max_tokens_per_1000px2"] == 4
            and job["group"] == "qwen-nonenglish-v1", "Unreviewed prompt/budget/cohort")
    crops = job["crops"]
    require(1 <= len(crops) <= 12 and len({crop["id"] for crop in crops}) == len(crops)
            and all(isinstance(crop["id"], str) and crop["id"] and
                    len(crop["loc_text"].encode("utf-8")) <= 8192 for crop in crops), "Bad crop contract")
    base = project.parent
    unit = base / "qwen/qwen-nonenglish-v1/units" / doc
    quality_path = base / "quality/qwen-nonenglish-v1/pages" / doc / "quality.json"
    provenance_path = base / "quality/qwen-nonenglish-v1/texts" / (doc + ".loc-provenance.json")
    require((unit / "qwen.json").read_bytes() == files["job"]
            and (unit / "page.jpg").read_bytes() == files["page"]
            and (unit / "submitted").read_text().strip() == wu["name"], "Producer input/submission mismatch")
    quality_raw, provenance_raw, sample_raw = (quality_path.read_bytes(), provenance_path.read_bytes(),
                                             (unit / "sample.json").read_bytes())
    quality, provenance, sample = map(json.loads, (quality_raw, provenance_raw, sample_raw))
    require(quality["doc_id"] == provenance["doc_id"] == sample["doc_id"] == doc
            and quality["language"] == sample["lang"] == job["language"],
            "Source-quality/provenance identity mismatch")
    require(quality["inputs"]["page"]["sha256"] == sha(files["page"])
            and quality["inputs"]["page"]["bytes"] == len(files["page"])
            and quality["inputs"]["alto"]["sha256"] == provenance["sha256"]
            and re.fullmatch("[a-f0-9]{64}", provenance["sha256"])
            and provenance["archive"] and provenance["member"] and int(provenance["archive_bytes"]) > 0,
            "Source-quality/archive-provenance binding mismatch")
    # Checked last: an out-of-cohort job is only passed over once every integrity check holds.
    if job["language"] not in COHORT_LANGUAGES:
        raise OutOfCohort("Unreviewed prompt/budget/cohort: language " + repr(job["language"]))
    return {"workunit": int(wu["id"]), "job_id": job_id, "doc_id": doc, "crops": len(crops),
            "job_sha256": sha(files["job"]), "page_sha256": sha(files["page"]),
            "quality_sha256": sha(quality_raw), "provenance_sha256": sha(provenance_raw),
            "sample_sha256": sha(sample_raw), "source_quality": sample, "provenance": provenance,
            "accuracy_established": False}


def read_database(project, ids, hosts=(9, 10)):
    require(ids and len(ids) <= 1010, "Bounded snapshot requires 1..1010 workunits")
    code = """
require_once 'boinc_db.inc'; $ids=json_decode($argv[1],true);$clause=implode(',',array_map('intval',$ids));
$hosts=implode(',',array_map('intval',json_decode($argv[2],true)));
$all=BoincResult::enum_fields('id,workunitid,hostid,server_state,sent_time,received_time,outcome,validate_state,app_version_id',
 "workunitid IN ($clause)"); $group=[];foreach($all as $r)$group[$r->workunitid][]=$r;
$out=['rows'=>[],'app'=>BoincApp::lookup_id(6),'versions'=>BoincAppVersion::enum('appid=6 AND deprecated=0'),
 'gpu_inflight'=>BoincResult::enum_fields('id,hostid,workunitid,app_version_id',
 "appid=6 AND server_state=4 AND hostid IN ($hosts) AND workunitid IN "
 ."(SELECT id FROM workunit WHERE appid=6 AND batch=8 AND rsc_disk_bound=19327352832)")];
$assigned=[];foreach(BoincDb::get()->enum_fields('assignment','StdClass','workunitid',"workunitid IN ($clause)",'') as $a)
 $assigned[$a->workunitid]=($assigned[$a->workunitid]??0)+1;
foreach(BoincWorkunit::enum_fields('id,name,appid,batch,min_quorum,target_nresults,rsc_disk_bound,canonical_resultid,error_mask,'
 .'transitioner_flags,need_validate,app_version_num,hr_class,xml_doc', "id IN ($clause)") as $w) {
 $out['rows'][]=['unit'=>$w,'results'=>$group[$w->id]??[],'assignments'=>$assigned[$w->id]??0];
} echo json_encode($out);
"""
    proc = subprocess.run(["php", "-r", code, json.dumps(ids), json.dumps(hosts)], cwd=project / "html/inc",
                          text=True, capture_output=True, check=True, timeout=15)
    return json.loads(proc.stdout)


def persist(path, value, exclusive=False):
    staged = path.with_name("." + path.name + "-" + uuid.uuid4().hex)
    created = False
    try:
        with staged.open("x") as stream:
            created = True
            json.dump(value, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        if exclusive:
            os.link(staged, path)
            staged.unlink()
        else:
            os.replace(staged, path)
        fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        if created:
            staged.unlink(missing_ok=True)


def run(args):
    project = args.project.resolve(strict=True)
    require(sha(args.helper.read_bytes()) == args.helper_sha256, "Reviewed helper changed")
    config_changed = sha((project / "config.xml").read_bytes()) != args.config_sha256
    policy_path = project / "plan_class_spec.xml"
    require(not policy_path.is_symlink(), "Symlinked policy")
    policy = policy_path.read_bytes()
    window, reserved = policy_window(policy)
    root = project / "ops-issue-559/qwen-auto-cohort"
    require(not root.is_symlink() and not root.parent.is_symlink(), "Symlinked state directory")
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    for folder in (root, root.parent, project):
        fd = os.open(folder, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    with (root / "controller.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state_path = root / "state.json"
        require(not state_path.is_symlink(), "Symlinked state")
        if state_path.exists():
            state = json.loads(state_path.read_bytes())
            require(state["controller"] == args.controller, "Another controller owns state")
            require(state["resource_gates"] == class_scope(policy, without_bounds=True),
                    "Reviewed GPU resource gates changed: refuse automatic adoption")
            pending = state.get("pending")
            if pending:
                if window == pending["target"] and class_scope(policy) == pending["after_scope"]:
                    state["window"] = window
                    state["window_since"] = int(time.time())
                    state["admitted"] += pending["selected"]
                    del state["pending"]
                    if args.apply:
                        persist(state_path, state)
                    if (int(time.time()) < args.enabled_until and len(state["admitted"]) < args.max_admissions
                            and not config_changed):
                        return {"status": "reconciled-exact-published-intent", "window": window,
                                "admitted_unique": len(state["admitted"]), "apply": args.apply}
                else:
                    require(window == state["window"] and class_scope(policy) == pending["before_scope"],
                            "Uncertain publication differs from its owned intent: refuse overwrite")
                    del state["pending"]
            require(state["window"] == window, "Another operator changed the owned cohort: refuse overwrite")
            if "window_since" not in state:
                # State from before this field: start the hold clock now, never backdate it.
                state["window_since"] = int(time.time())
                if args.apply:
                    persist(state_path, state)
        else:
            require(sha(policy) == args.initial_policy_sha256, "Initial operator policy changed")
            state = {"controller": args.controller, "window": window, "admitted": [],
                     "resource_gates": class_scope(policy, without_bounds=True), "window_since": int(time.time())}
            if args.apply:
                # Persist the validated baseline before any early return so later ticks use the
                # scope-aware ownership checks rather than the whole-policy SHA.
                persist(state_path, state)
        output = {"at": int(time.time()), "window": window, "reserved_amd": reserved,
                  "admitted_unique": len(state["admitted"]), "limit": args.max_admissions,
                  "apply": args.apply, "accuracy_established": False}
        expired = int(time.time()) >= args.enabled_until
        remaining = args.max_admissions - len(state["admitted"])
        by_wu = {}
        release = expired or remaining <= 0 or config_changed
        if not release:
            subprocess.run(
                ["php", "-r", "require $argv[1]; exit(native_qwen_one_result_per_host($argv[2])?0:1);",
                 str(args.helper.resolve()), str(project / "config.xml")],
                check=True, timeout=10, capture_output=True,
            )
            audit_path = project.parent / "qwen/qwen-nonenglish-v1/audit.json"
            audit_raw = audit_path.read_bytes()
            require(len(audit_raw) <= 16 * 1024 * 1024, "Producer audit exceeds bounded parser size")
            audit = json.loads(audit_raw)
            for key, value in audit.items():
                workunit = int(value["workunit_id"])
                require(workunit not in by_wu, "Duplicate native producer workunit binding")
                by_wu[workunit] = (key, value)
            candidates = sorted(workunit for workunit in by_wu
                                if workunit >= args.minimum_workunit and workunit not in state["admitted"]
                                and workunit != reserved)
            later = [workunit for workunit in candidates if workunit > state.get("scan_after", 0)]
            ids = (later or candidates)[:1000]
            current_ids = list(range(window[0], window[1] + 1)) if window != [2, 1] else []
            wanted = sorted(set(ids + current_ids))
            if not wanted:
                return output | {"status": "no-audited-fresh-inputs", "fresh_pairs_in_scan": 0}
            database = read_database(project, wanted, (args.intel_host, args.nvidia_host))
            rows = database["rows"]
            require(database["app"]["name"] == "usnm_qwen"
                    and int(database["app"]["homogeneous_redundancy"]) == 0, "Qwen app/HR changed")
            allowed = {}
            for plan, expected in (("intel_vulkan_8b", args.intel_app_version),
                                   ("nvidia_vulkan_8b", args.nvidia_app_version)):
                # shared_8b opens these classes on every platform: any non-Linux release closes admission.
                # The feeder loads only the newest version per platform/plan class unless app.min_version
                # is set, in which case every version >= min_version is dispatchable.
                versions = [row for row in database["versions"] if row["plan_class"] == plan]
                minimum = int(database["app"].get("min_version") or 0)
                newest = max((int(row["version_num"]) for row in versions), default=None)
                dispatchable = sorted((int(row["platformid"]), int(row["version_num"]), int(row["id"]))
                                      for row in versions if (int(row["version_num"]) >= minimum if minimum
                                                              else int(row["version_num"]) == newest))
                allowed[plan] = all(int(row["platformid"]) == 1 for row in versions) \
                    and dispatchable == [(1, args.app_version_num, expected)]
            release = int(time.time()) >= args.enabled_until or not all(allowed.values())
        if release:
            command = ["--stop"]
            selected, proof = [], []
            status = "closed-expired-budget-or-release-change"
            target = [2, 1]
        else:
            inflight = {host: sum(int(row["hostid"]) == host for row in database["gpu_inflight"])
                        for host in (args.intel_host, args.nvidia_host)}
            # Old-window replicas stay dispatchable until the new policy is published, so each host's
            # headroom also reserves every one it could still receive.
            capacity = max(0, min(args.max_inflight_per_host - inflight[host]
                                  - assignable_units(rows, window, host) for host in inflight))
            available = available_in_window(rows, window, (args.intel_host, args.nvidia_host))
            scanned = set(ids)
            fresh_count = sum(fresh(row) for row in rows if int(row["unit"]["id"]) in scanned)
            if not fresh_count and ids:
                state["scan_after"] = ids[-1]
                if args.apply:
                    persist(state_path, state)
            output |= {"currently_eligible_unsent_replicas": available, "fresh_pairs_in_scan": fresh_count,
                       "audited_candidates_total": len(candidates), "scanned_candidates": len(ids),
                       "producer_audit_sha256": sha(audit_raw), "native_gpu_inflight": inflight,
                       "per_host_buffer_limit": args.max_inflight_per_host}
            # A host holding a replica of every remaining window unit can never take the rest: if it
            # has no native work left while the peer holds the window, it starves behind the peer.
            starved = [host for host in (args.intel_host, args.nvidia_host) if not inflight[host]
                       and not assignable_units(rows, window, host)]
            held = int(time.time()) - int(state["window_since"])
            if available and not (starved and held >= args.rotate_after_seconds):
                return output | {"status": "current-window-still-eligible", "starved_hosts": starved,
                                 "window_held_seconds": held}
            if available:
                output |= {"starved_hosts": starved, "window_held_seconds": held}
            if not capacity:
                return output | {"status": "bounded-gpu-inflight-buffer-full"}
            skipped = []

            def admissible(row):
                # Only a language outside the reviewed cohort is passed over; every other
                # binding failure still stops the tick.
                try:
                    bind_inputs(project, row, *by_wu[int(row["unit"]["id"])], args.prompt_version)
                except OutOfCohort:
                    skipped.append(int(row["unit"]["id"]))
                    return False
                return True

            selected = choose([row for row in rows if int(row["unit"]["id"]) in scanned],
                              args.minimum_workunit, reserved, min(remaining, capacity), admissible)
            if skipped:
                output |= {"out_of_cohort_skipped": skipped}
            if not selected:
                if skipped and ids:
                    # Every fresh unit in this page is out of cohort and never becomes admissible:
                    # move past it so later in-cohort work is reached.
                    state["scan_after"] = ids[-1]
                    if args.apply:
                        persist(state_path, state)
                return output | {"status": "no-fresh-verified-pairs"}
            if available:
                # The left-behind replicas stay ordinary unsent work for every other eligible class;
                # nothing is cancelled, reassigned or edited.
                output |= {"rotated_past_window": window, "left_unsent_replicas": available}
            proof = [bind_inputs(project, row, *by_wu[int(row["unit"]["id"])], args.prompt_version)
                     for row in selected]
            target = [int(selected[0]["unit"]["id"]), int(selected[-1]["unit"]["id"])]
            command = ["--workunit", str(target[0]), "--workunit-end", str(target[1]), "--fresh-pair",
                       "--admit-until", str(args.enabled_until)]
            status = "refilled-finite-window"
        if target == window:
            return output | {"status": status, "changed": False}
        command = ["php", str(args.helper), "--project", str(project), "--plan-class", "shared_8b",
                   "--expected-policy-sha256", sha(policy), *command]
        preview = json.loads(subprocess.check_output(command, timeout=15))
        receipt = output | {"status": status, "target": target, "inputs": proof, "preview": preview}
        if args.apply:
            # Intent survives interruption. A mismatching state/window on the next tick stops
            # rather than replaying an uncertain publication or clobbering another owner.
            receipt_path = root / ("intent-" + uuid.uuid4().hex + ".json")
            persist(receipt_path, receipt, exclusive=True)
            state["pending"] = {
                "target": target, "before_sha256": sha(policy), "after_sha256": preview["after_sha256"],
                "before_scope": class_scope(policy), "after_scope": target_scope(policy, target),
                "selected": [int(row["unit"]["id"]) for row in selected], "intent": str(receipt_path),
            }
            persist(state_path, state)
            if selected:
                require([bind_inputs(project, row, *by_wu[int(row["unit"]["id"])], args.prompt_version)
                         for row in selected] == proof,
                        "Input/source provenance changed before apply")
                require(int(time.time()) < args.enabled_until, "Admission deadline passed before publication")
            applied = json.loads(subprocess.check_output(command + ["--apply"], timeout=15))
            require(applied["before_sha256"] == sha(policy), "Atomic policy precondition failed")
            # The helper's rename is durable only once the directory entry is; never record a
            # window in state that power loss could still revert.
            fd = os.open(project, os.O_RDONLY)
            try:
                os.fsync(fd)
            finally:
                os.close(fd)
            require(sha(policy_path.read_bytes()) == applied["after_sha256"], "Published policy readback mismatch")
            state["window"] = target
            state["window_since"] = int(time.time())
            state["admitted"] += [int(row["unit"]["id"]) for row in selected]
            state["last_intent"] = str(receipt_path)
            del state["pending"]
            persist(state_path, state)
            receipt["applied"] = applied
        return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--helper", type=Path, required=True)
    parser.add_argument("--helper-sha256", required=True)
    parser.add_argument("--config-sha256", required=True)
    parser.add_argument("--initial-policy-sha256", required=True)
    parser.add_argument("--controller", required=True)
    parser.add_argument("--minimum-workunit", type=int, required=True)
    parser.add_argument("--max-admissions", type=int, default=500)
    parser.add_argument("--max-inflight-per-host", type=int, default=10)
    # Several native task times: a host that is merely slow keeps its window.
    parser.add_argument("--rotate-after-seconds", type=int, default=1800)
    parser.add_argument("--enabled-until", type=int, required=True)
    parser.add_argument("--intel-host", type=int, default=9)
    parser.add_argument("--nvidia-host", type=int, default=10)
    # The one reviewed Linux release per opened class; any other dispatchable version closes admission.
    parser.add_argument("--app-version-num", type=int, default=114)
    parser.add_argument("--nvidia-app-version", type=int, default=66)
    parser.add_argument("--intel-app-version", type=int, default=67)
    # Request version 2 (--image-min-tokens 1024) is read only by app versions 1.15 and later.
    parser.add_argument("--prompt-version", type=int, choices=(1, 2), default=1)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)
    require(args.app_version_num >= 114 and args.nvidia_app_version > 0 and args.intel_app_version > 0
            and args.nvidia_app_version != args.intel_app_version, "Expected reviewed app versions")
    require(args.prompt_version == 1 or args.app_version_num >= 115,
            "Request version 2 needs app versions that read it")
    require(re.fullmatch("[a-f0-9]{32}", args.controller) is not None, "Expected controller identity")
    require(all(re.fullmatch("[a-f0-9]{64}", value) for value in
                (args.helper_sha256, args.config_sha256, args.initial_policy_sha256)), "Expected SHA256 guards")
    require(1 <= args.max_admissions <= 10000 and args.minimum_workunit > 1, "Invalid finite budget/floor")
    require(1 <= args.max_inflight_per_host <= 10, "Keep at most ten native GPU tasks buffered per pilot host")
    require(args.rotate_after_seconds >= 600, "Rotate a held window only after several native task times")
    require(args.intel_host > 0 and args.nvidia_host > 0 and args.intel_host != args.nvidia_host,
            "Independent distinct GPU hosts required")
    require(args.enabled_until <= int(time.time()) + 86400, "Enable at most 24 hours per reviewed ramp")
    print(json.dumps(run(args), indent=2))


if __name__ == "__main__":
    os.umask(0o077)
    main()
