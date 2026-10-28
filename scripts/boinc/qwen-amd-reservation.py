#!/usr/bin/env python3
"""Lease one untouched ordinary Qwen tail pair to AMD before its normal peer."""
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

MARKER = "usnm-amd-first-reservation-v1"
AMD = "amd_vulkan_8b"
APPLE = "apple_gpu_8b"
COMPETITORS = (
    "nvidia_cuda_2b", "nvidia_cuda_8b", "intel_vulkan_8b",
    "nvidia_vulkan_8b", "docker_nvidia_cuda_8b",
)
ENGINE = {
    "image": "sha256:a25da41a10f266e5c2d02206bf9f0e40b54d476609f85d70a72e4e11792d0efe",
    "llama": "sha256:d962a8228c42c84583150e809d654f9ec995cdf9f4e02e37365293642210a309",
    "weights": "sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd",
}
MODEL = {
    "file": "Qwen3VL-8B-Instruct-Q4_K_M.gguf", "min_gpu_mb": 7000,
    "mmproj": "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf", "name": "qwen3-vl-8b-q4km",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(payload):
    return hashlib.sha256(payload).hexdigest()


def parse_policy(payload):
    require(len(payload) <= 1024 * 1024 and b"<!DOCTYPE" not in payload, "Invalid policy size/doctype")
    root = ET.fromstring(payload, parser=ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    require(root.tag == "plan_classes", "Expected plan_classes")
    nodes = root.findall("plan_class")
    classes = {node.findtext("name"): node for node in nodes}
    require(None not in classes and len(classes) == len(nodes), "Duplicate or missing class")
    return root, classes


def reservation_marker(root):
    matches = [node for node in root.iter() if node.tag is ET.Comment
               and MARKER in (node.text or "")]
    require(len(matches) <= 1, "Duplicate reservation marker")
    if not matches:
        return None
    text = (matches[0].text or "").strip()
    match = re.fullmatch(MARKER + r":([a-f0-9]{32}):([1-9][0-9]*):([1-9][0-9]*)", text)
    require(match is not None and matches[0] in list(root), "Malformed reservation marker")
    return matches[0], match[1], int(match[2]), int(match[3])


def bounds(node):
    values = [node.findall(tag) for tag in ("min_wu_id", "max_wu_id")]
    require(all(len(value) == 1 for value in values), "Missing or duplicate native bounds")
    require(all(re.fullmatch(r"[1-9][0-9]*", value[0].text or "") for value in values),
            "Non-numeric native bounds")
    return tuple(int(value[0].text) for value in values)


def serialize(root):
    return ET.tostring(root, encoding="utf-8", xml_declaration=True) + b"\n"


def reserve_policy(payload, lease, workunit, expires):
    root, classes = parse_policy(payload)
    require(reservation_marker(root) is None, "A reservation already owns the policy")
    require(set((AMD, APPLE, *COMPETITORS)).issubset(classes), "Missing GPU class")
    require(bounds(classes[AMD]) == (2, 1), "AMD must initially be closed")
    for name in COMPETITORS:
        low, high = bounds(classes[name])
        require((low, high) == (2, 1) or 2 <= low <= high < workunit and high - low < 10,
                f"{name} can compete with the reserved tail or is not bounded")
    apple = classes[APPLE]
    require(not apple.findall("min_wu_id") and not apple.findall("max_wu_id")
            and not apple.findall("project_prefs_tag"), "Apple must initially be unbounded")
    ET.SubElement(apple, "max_wu_id").text = str(workunit - 1)
    for tag in ("min_wu_id", "max_wu_id"):
        classes[AMD].find(tag).text = str(workunit)
    root.append(ET.Comment(f"{MARKER}:{lease}:{workunit}:{expires}"))
    return serialize(root)


def release_policy(payload, lease, workunit, original):
    root, classes = parse_policy(payload)
    marker = reservation_marker(root)
    if marker is None or marker[1] != lease:
        return payload
    require(marker[2] == workunit, "Reservation identity changed")
    parse_policy(original)
    apple = classes[APPLE]
    fields = apple.findall("max_wu_id")
    require(len(fields) == 1 and fields[0].text == str(workunit - 1)
            and not apple.findall("min_wu_id"), "Owned Apple ceiling changed")
    apple.remove(fields[0])
    # Remove only our ceiling, not another operator's unrelated Apple configuration.
    if bounds(classes[AMD]) == (workunit, workunit):
        classes[AMD].find("min_wu_id").text = "2"
        classes[AMD].find("max_wu_id").text = "1"
    root.remove(marker[0])
    return serialize(root)


def guarded_policy(payload, intent):
    root, classes = parse_policy(payload)
    marker = reservation_marker(root)
    if marker is None or marker[1:] != (intent["lease"], intent["workunit"], intent["expires"]):
        return False
    workunit = intent["workunit"]
    if bounds(classes[AMD]) != (workunit, workunit):
        return False
    apple = classes[APPLE]
    fields = apple.findall("max_wu_id")
    if len(fields) != 1 or fields[0].text != str(workunit - 1) or apple.findall("min_wu_id"):
        return False
    for name in COMPETITORS:
        low, high = bounds(classes[name])
        if (low, high) != (2, 1) and not (2 <= low <= high < workunit and high - low < 10):
            return False
    return True


def strict_pair(snapshot, workunit, app_version):
    wu, results = snapshot["unit"], snapshot["results"]
    expected = {
        "id": workunit, "appid": 6, "batch": 8, "min_quorum": 2, "target_nresults": 2,
        "rsc_disk_bound": 19327352832, "canonical_resultid": 0, "error_mask": 0,
        "transitioner_flags": 0, "need_validate": 0, "app_version_num": 0,
    }
    require(wu is not None and all(int(wu[key]) == value for key, value in expected.items()),
            "Not an untouched ordinary native 8B quorum-2 workunit")
    require(snapshot["app"]["name"] == "usnm_qwen" and int(snapshot["newer"]) == 0,
            "Only the newest Qwen tail workunit can be reserved")
    require(int(snapshot["assignments"]) == 0 and len(results) == 2, "Not two generic replicas")
    require(len({int(row["id"]) for row in results}) == 2, "Duplicate replica identity")
    for row in results:
        require(all(int(row[key]) == value for key, value in {
            "server_state": 2, "sent_time": 0, "hostid": 0, "outcome": 0, "validate_state": 0,
            "app_version_id": 0,
        }.items()), "A replica is already assigned, failed, or validated")
    versions = snapshot["versions"]
    require(all(row["plan_class"] in (AMD, APPLE, *COMPETITORS) for row in versions),
            "An unguarded Qwen app class can compete")
    target = [row for row in versions if int(row["id"]) == app_version]
    require(len(target) == 1 and target[0]["plan_class"] == AMD
            and int(target[0]["version_num"]) == 114 and int(target[0]["platformid"]) == 5,
            "Expected the published Windows AMD 1.14 app")
    require(not any(row["plan_class"] == AMD and int(row["platformid"]) == 5
                    and int(row["version_num"]) > 114 for row in versions), "AMD release superseded")


def validate_inputs(project, snapshot, expected):
    wu = snapshot["unit"]
    require(int(expected["workunit_id"]) == int(wu["id"]), "Input receipt is for another workunit")
    require(all(re.fullmatch("[a-f0-9]{64}", expected.get(key, "")) for key in
                ("job_sha256", "page_sha256", "quality_sha256", "provenance_sha256"))
            and expected["provenance"]["doc_id"] == expected["doc_id"],
            "Independent source-quality/archive-provenance receipt required")
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
    require(set(files) == {"job", "page"}, "Missing job/page input")
    job = json.loads(files["job"])
    require(digest(files["job"]) == expected["job_sha256"]
            and digest(files["page"]) == expected["page_sha256"], "Input changed since independent receipt")
    require(job["doc_id"] == expected["doc_id"] and job["input"]["sha256"] == digest(files["page"])
            and job["input"]["bytes"] == len(files["page"]), "Page/doc identity mismatch")
    require(all(job["engine"][key] == value for key, value in ENGINE.items())
            and job["engine"]["models"] == [MODEL], "Engine/model contract changed")
    require(job["v"] == job["prompt_version"] == 1 and job["max_tokens_per_1000px2"] == 4
            and job["group"] == "qwen-nonenglish-v1" and job["language"] == "yid",
            "Prompt/budget/cohort changed")
    crops = job["crops"]
    require(1 <= len(crops) <= 12 and len({crop["id"] for crop in crops}) == len(crops)
            and all(len(crop["loc_text"].encode("utf-8")) <= 8192 for crop in crops),
            "Crop count/identity/UTF-8 contract violation")
    return {"job_sha256": digest(files["job"]), "page_sha256": digest(files["page"]),
            "doc_id": job["doc_id"], "crop_count": len(crops)}


def snapshot(project, workunit):
    code = """
require_once 'boinc_db.inc';
$id=intval($argv[1]); $w=BoincWorkunit::lookup_id($id);
echo json_encode(['unit'=>$w,'app'=>$w?BoincApp::lookup_id($w->appid):null,
 'results'=>BoincResult::enum("workunitid=$id"),
 'assignments'=>BoincDb::get()->count('assignment',"workunitid=$id"),
 'newer'=>BoincDb::get()->count('workunit',"appid=6 AND id>$id"),
 'versions'=>BoincAppVersion::enum('appid=6 AND deprecated=0')]);
"""
    proc = subprocess.run(["php", "-r", code, str(workunit)], cwd=project / "html/inc",
                          text=True, capture_output=True, check=True, timeout=15)
    return json.loads(proc.stdout)


def fsync_directory(path):
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def durable_file(path, payload, mode=0o600):
    require(not path.is_symlink(), "Refusing symlinked destination")
    created = False
    try:
        with path.open("xb") as stream:
            created = True
            os.fchmod(stream.fileno(), mode)
            stream.write(payload)
            stream.flush()
            os.fsync(stream.fileno())
        fsync_directory(path.parent)
    except BaseException:
        if created:
            path.unlink(missing_ok=True)
        raise


def replace_policy(path, original, updated):
    require(not path.is_symlink(), "Refusing symlinked policy")
    require(path.read_bytes() == original, "Concurrent policy change")
    stage = path.with_name(".amd-reservation-" + uuid.uuid4().hex)
    created = False
    try:
        durable_file(stage, updated, path.stat().st_mode & 0o777)
        created = True
        require(path.read_bytes() == original, "Concurrent policy change")
        os.replace(stage, path)
        fsync_directory(path.parent)
    finally:
        if created:
            stage.unlink(missing_ok=True)


def scheduler_processes(project):
    """Track PID plus start time: a reused PID is not an old-policy scheduler."""
    proc = Path("/proc")
    require(proc.is_dir(), "Scheduler-drain proof requires Linux procfs")
    require((project / "cgi-bin/cgi").exists(), "Missing project CGI")
    found = {}
    for folder in proc.glob("[0-9]*"):
        try:
            if (folder / "comm").read_text().strip() != "cgi":
                continue
            # Conservatively track every CGI in this single-project container. Reading
            # exe/cwd links would need ptrace capability that production intentionally lacks.
            stat = (folder / "stat").read_text()
            found[folder.name] = stat[stat.rfind(")") + 2:].split()[19]
        except (FileNotFoundError, ProcessLookupError):
            continue
    return found


def old_scheduler_alive(project, guard):
    current = scheduler_processes(project)
    return any(current.get(pid) == start for pid, start in guard.items())


def tick_reason(current, intent, now):
    assigned = [row for row in current["results"] if int(row["hostid"]) != 0
                or int(row["sent_time"]) != 0 or int(row["server_state"]) != 2]
    if any(int(row["hostid"]) == intent["host"] and int(row["app_version_id"]) == intent["app_version"]
           for row in assigned):
        return "actual_amd_assignment_not_scientific_acceptance"
    if assigned:
        return "other_scheduler_won_race_no_assignment_stolen"
    if now >= intent["expires"]:
        return "lease_expired"
    return None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("reserve", "tick", "release"))
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--lease", required=True, help="new 32-hex ID; provision its independent rollback first")
    parser.add_argument("--workunit", type=int)
    parser.add_argument("--host", type=int, default=5)
    parser.add_argument("--app-version", type=int, default=68)
    parser.add_argument("--lease-seconds", type=int, default=1200)
    parser.add_argument("--expected-policy-sha256")
    parser.add_argument("--evidence", type=Path, help="independent immutable per-workunit download/provenance receipt")
    parser.add_argument("--cohort-helper", type=Path, help="reviewed native-qwen-cohort.php for the config guard")
    parser.add_argument("--cohort-helper-sha256")
    parser.add_argument("--rollback-receipt", type=Path, help="operator receipt for this lease's prearmed rollback unit")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args(argv)
    require(re.fullmatch("[a-f0-9]{32}", args.lease), "Expected a new 32-hex lease identity")
    project = args.project.resolve(strict=True)
    policy = project / "plan_class_spec.xml"
    lock = project / ".native-qwen-cohort.lock"
    require(not policy.is_symlink() and not lock.is_symlink(), "Refusing symlinked policy/lock")
    journal = project / "ops-issue-559" / "amd-reservation"
    require(not any(p.is_symlink() for p in (journal, journal.parent)), "Refusing symlinked journal")
    if args.apply:
        journal.mkdir(parents=True, exist_ok=True, mode=0o700)
        for folder in (journal, journal.parent, project):
            fsync_directory(folder)
    with lock.open("a") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        original = policy.read_bytes()
        intent_path = journal / (args.lease + ".json")
        guard_path = journal / (args.lease + ".scheduler-guard.json")
        now = int(time.time())
        if args.action == "reserve":
            require(args.workunit is not None and args.workunit > 1 and args.host > 0
                    and args.app_version > 0 and 60 <= args.lease_seconds <= 1800,
                    "One positive workunit/host/app version; lease must be 60..1800 seconds")
            require(args.evidence is not None and args.rollback_receipt is not None,
                    "Independent input evidence and prearmed rollback receipt required")
            require(args.cohort_helper is not None
                    and digest(args.cohort_helper.read_bytes()) == args.cohort_helper_sha256,
                    "Reviewed cohort helper hash required")
            subprocess.run(
                ["php", "-r", "require $argv[1]; exit(native_qwen_one_result_per_host($argv[2])?0:1);",
                 str(args.cohort_helper.resolve()), str(project / "config.xml")],
                check=True, timeout=15, capture_output=True,
            )
            require(digest(original) == args.expected_policy_sha256, "Policy changed since reviewed preview")
            require(not intent_path.exists(), "Lease identity already used")
            rollback = json.loads(args.rollback_receipt.read_bytes())
            require(rollback["lease"] == args.lease and rollback["active"] is True
                    and now < int(rollback["deadline"]) <= now + args.lease_seconds,
                    "Independent rollback is not prearmed for this lease/deadline")
            expires = int(rollback["deadline"])
            current = snapshot(project, args.workunit)
            strict_pair(current, args.workunit, args.app_version)
            evidence_bytes = args.evidence.read_bytes()
            inputs = validate_inputs(project, current, json.loads(evidence_bytes))
            updated = reserve_policy(original, args.lease, args.workunit, expires)
            intent = {
                "lease": args.lease, "workunit": args.workunit, "host": args.host,
                "app_version": args.app_version, "expires": expires, "created": now,
                "original_xml": original.decode(), "before_sha256": digest(original),
                "after_sha256": digest(updated), "initial_snapshot": current, "inputs": inputs,
                "evidence_sha256": digest(evidence_bytes), "rollback_receipt": rollback,
                "accuracy_established": False,
            }
            if args.apply:
                before_processes = scheduler_processes(project)
                durable_file(intent_path, (json.dumps(intent, indent=2) + "\n").encode())
                # Recheck after journaling, immediately before publication; an in-flight CGI may
                # still win afterwards. tick detects that and restores instead of stealing work.
                strict_pair(snapshot(project, args.workunit), args.workunit, args.app_version)
                require(int(time.time()) < expires, "Lease expired before publication")
                replace_policy(policy, original, updated)
                after_processes = scheduler_processes(project)
                guard = {"lease": args.lease, "old_processes": before_processes | after_processes,
                         "checked_at": int(time.time())}
                durable_file(guard_path, (json.dumps(guard, indent=2) + "\n").encode())
            output = {key: value for key, value in intent.items() if key != "original_xml"}
            output["apply"] = args.apply
        else:
            require(not intent_path.is_symlink(), "Refusing symlinked intent")
            intent = json.loads(intent_path.read_bytes())
            require(intent["lease"] == args.lease, "Intent identity mismatch")
            current = snapshot(project, intent["workunit"]) if args.action == "tick" else None
            reason = tick_reason(current, intent, now) if current else "operator_release"
            guarded = False
            if current and not reason and not guarded_policy(original, intent):
                reason = "reservation_policy_changed_no_exclusive_admission_claim"
            if current and not reason and guard_path.exists():
                require(not guard_path.is_symlink(), "Refusing symlinked scheduler guard")
                guard = json.loads(guard_path.read_bytes())
                require(guard["lease"] == args.lease, "Scheduler guard identity mismatch")
                if not old_scheduler_alive(project, guard["old_processes"]):
                    # The policy predates every surviving CGI; re-read DB *after* observing
                    # the old processes gone, not before their last possible commit.
                    current = snapshot(project, intent["workunit"])
                    reason = tick_reason(current, intent, now)
                    if not reason:
                        strict_pair(current, intent["workunit"], intent["app_version"])
                        guarded = True
            updated = release_policy(original, args.lease, intent["workunit"],
                                     intent["original_xml"].encode()) if reason else original
            output = {"lease": args.lease, "workunit": intent["workunit"], "reason": reason,
                      "before_sha256": digest(original), "after_sha256": digest(updated),
                      "changed": original != updated, "apply": args.apply, "checked_at": now,
                      "snapshot": current, "old_schedulers_drained_and_pair_untouched": guarded}
            if args.apply and original != updated:
                durable_file(journal / (args.lease + ".release-" + uuid.uuid4().hex + ".json"),
                             (json.dumps(output, indent=2) + "\n").encode())
                replace_policy(policy, original, updated)
        print(json.dumps(output, indent=2))


if __name__ == "__main__":
    main()
