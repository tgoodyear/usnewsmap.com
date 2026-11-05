#!/usr/bin/env python3
"""Native Qwen feed gate, run inside the server container under the host bridge lock.

Inventory includes every retained Qwen campaign (single-page and bundled). Submission
uses the existing Rust stage/submit commands and shared pinned models, never a new recipe,
quorum, retry identity or manually generated BOINC result.
"""
import argparse
import hashlib
import json
import logging
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

# The first app version that reads each request version (qwen-auto-cohort.py keeps the same table).
REQUEST_MIN_APP = {1: 0, 2: 115, 3: 117, 4: 117}
# Requests with the loop guard. A server image from before #1437 refuses to submit them, so
# without --submit-binary they go with the stage binary (#1449).
GUARDED_VERSIONS = (3, 4)
YIDDISH = "yid"
# `qwen stage --bundle-size` above 1 names each bundle unit bundle-NNNNN-<first doc>.
BUNDLE_UNIT = re.compile(r"bundle-\d{5}-")


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
$versions = [];
foreach (BoincAppVersion::enum("appid=".intval($app->id)." AND deprecated=0") as $v)
    $versions[] = ["plan_class"=>$v->plan_class, "platformid"=>intval($v->platformid),
                   "version_num"=>intval($v->version_num)];
echo json_encode(["ready"=>count($ready), "dispatched"=>count($running),
    "fresh_pairs"=>intval($fresh), "unresolved_native"=>intval($unresolved),
    "app_versions"=>$versions, "min_version"=>intval($app->min_version)]);
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


def source_fingerprint(page):
    if (not isinstance(page, dict)
            or not re.fullmatch(r"[a-z0-9]{1,16}_\d{4}-\d{2}-\d{2}_ed-\d+_seq-\d+", page.get("doc_id", ""))
            or not re.fullmatch(r"[a-z]+_[a-zA-Z0-9]+", page.get("batch", ""))
            or type(page.get("batch_version")) is not int or not 1 <= page["batch_version"] <= 99
            or type(page.get("bytes")) is not int or page["bytes"] < 1
            or not re.fullmatch(r"[a-f0-9]{64}", page.get("sha256", ""))):
        raise ValueError("invalid bounded source-status membership")
    return {k: page[k] for k in ("doc_id", "batch", "batch_version", "bytes", "sha256")}


def source_bindings(root, job):
    bindings = {}
    for member in job.get("pages") or [job]:
        path = root.parent.parent / "quality" / root.name / "texts" / (
            member["doc_id"] + ".image-provenance.json")
        if not path.exists():
            logging.warning("Native source provenance unavailable: %s", path)
            continue
        binding = json.loads(path.read_text())
        image = member["input"]
        source = source_fingerprint(binding["source"])
        if (binding["schema"] != 1 or source != binding["source"] or source["doc_id"] != member["doc_id"]
                or binding["jpeg"] != {k: image[k] for k in ("bytes", "sha256")}):
            raise ValueError("native image provenance does not bind to job: " + str(path))
        bindings[member["doc_id"]] = binding
    return bindings


def terminal_source(unit):
    receipt = unit / "source-provenance.json"
    if not receipt.exists():
        logging.warning("Terminal source provenance unavailable: %s", unit)
        return None
    record = json.loads(receipt.read_text())
    raw = (unit / "quality.json").read_bytes()
    quality = json.loads(raw)
    binding = record["binding"]
    source = source_fingerprint(binding["source"])
    if (type(record["schema"]) is not int or record["schema"] != 1
            or record["quality_sha256"] != hashlib.sha256(raw).hexdigest()
            or type(binding["schema"]) is not int or binding["schema"] != 1
            or source != binding["source"]
            or source["doc_id"] != unit.name or quality["doc_id"] != unit.name
            or binding["jpeg"] != {k: quality["inputs"]["page"][k] for k in ("bytes", "sha256")}):
        raise ValueError("changed or unbound terminal source provenance: " + str(receipt))
    return source


def source_status(share, project, sample):
    frozen = [source_fingerprint(json.loads(line)) for line in sample.splitlines() if line.strip()]
    pages = [page["doc_id"] for page in frozen]
    if not pages or len(pages) > 10000 or len(set(pages)) != len(pages):
        raise ValueError("invalid bounded source-status membership")
    expected = {p["doc_id"]: p for p in frozen}
    wanted = set(pages)
    states = {doc: {"status": "pending", "workunit_ids": []} for doc in pages}
    audited = {}
    terminals = []
    for campaign in sorted((share / "qwen").glob("*")):
        if not campaign.is_dir():
            continue
        receipt = campaign / "audit.json"
        audit_rows = json.loads(receipt.read_text()) if receipt.exists() else {}
        for path, job in jobs(campaign):
            members = documents(job) & wanted
            if not members:
                continue
            row = audit_rows.get(job["id"])
            if row:
                if (row["job_sha256"] != hashlib.sha256(path.read_bytes()).hexdigest()
                        or not (path.parent / "submitted").is_file()
                        or row["batch"] != 8 or row["min_quorum"] != 2 or row["target_nresults"] != 2
                        or type(row["workunit_id"]) is not int or row["workunit_id"] < 1):
                    raise ValueError("changed or unbound native source audit: " + str(path))
            for doc in members:
                if not row:
                    if (not states[doc]["workunit_ids"]
                            and states[doc]["status"] not in (
                                "source_conflict", "source_provenance_unavailable")):
                        states[doc]["status"] = "pending_audit"
                    continue
                states[doc]["workunit_ids"].append(row["workunit_id"])
                binding = row.get("source_bindings", {}).get(doc)
                member = next(p for p in job.get("pages") or [job] if p["doc_id"] == doc)
                if doc not in row.get("source_bindings", {}) and (campaign / "recovered-provenance").is_dir():
                    from qwen_provenance import recovered_binding
                    binding = recovered_binding(campaign, job, row, member, path.parent / "submitted")
                if binding is None:
                    states[doc]["status"] = "source_provenance_unavailable"
                elif (not isinstance(binding, dict) or binding.get("schema") != 1
                      or binding.get("source") != expected[doc]
                      or binding.get("jpeg") != {k: member["input"][k] for k in ("bytes", "sha256")}):
                    states[doc]["status"] = "source_conflict"
                else:
                    if states[doc]["status"] not in ("source_conflict", "source_provenance_unavailable"):
                        states[doc]["status"] = "published"
                        if "reconstructed_at" in binding:
                            states[doc]["provenance"] = "verified_reconstruction"
                    audited[row["workunit_id"]] = row
        for layout in ("units", "pages"):
            for disposition in ("no_crops", "too_large"):
                for marker in (campaign / layout).glob("*/" + disposition):
                    doc = marker.parent.name
                    if doc in wanted:
                        terminals.append((doc, marker, disposition))
    # Inspect all retained bindings; publication takes precedence only over matching
    # dispositions and unbound legacy markers, never over conflicting evidence.
    for doc, marker, disposition in terminals:
        binding = terminal_source(marker.parent)
        if binding is not None and binding != expected[doc]:
            states[doc]["status"] = "source_conflict"
        elif not states[doc]["workunit_ids"]:
            if binding is None and states[doc]["status"] != "source_conflict":
                states[doc]["status"] = "source_provenance_unavailable"
            elif states[doc]["status"] not in (
                    "source_conflict", "source_provenance_unavailable"):
                states[doc]["status"] = disposition
    ids = sorted(audited)
    totals = {"published_workunits": len(ids), "agreed_workunits": 0, "unsent_replicas": 0,
              "running_replicas": 0, "returned_replicas": 0, "invalid_replicas": 0,
              "error_workunits": 0, "history_unavailable_workunits": 0,
              "source_conflict_pages": sum(state["status"] in (
                  "source_conflict", "source_provenance_unavailable") for state in states.values())}
    if ids:
        php = """
chdir(%s);
require_once("boinc_db.inc");
$rows = BoincDb::get()->enum_general('stdClass',
    "SELECT w.id,w.canonical_resultid,w.error_mask,"
    ."SUM(r.server_state=2) AS unsent,SUM(r.server_state=4) AS running,"
    ."SUM(r.received_time>0) AS returned,SUM(r.validate_state=2) AS invalid "
    ."FROM workunit w LEFT JOIN result r ON r.workunitid=w.id "
    ."WHERE w.id IN (%s) GROUP BY w.id,w.canonical_resultid,w.error_mask");
echo json_encode($rows);
""" % (json.dumps(str(project / "html/inc")), ",".join(map(str, ids)))
        rows = json.loads(subprocess.check_output(["php", "-r", php], text=True))
        observed = {int(row["id"]) for row in rows}
        if not observed <= set(ids):
            raise ValueError("native source report returned unexpected workunits")
        missing = set(ids) - observed
        totals["history_unavailable_workunits"] = len(missing)
        for state in states.values():
            if (missing.intersection(state["workunit_ids"])
                    and state["status"] not in ("source_conflict", "source_provenance_unavailable")):
                state["status"] = "native_history_unavailable"
        for row in rows:
            totals["agreed_workunits"] += int(row["canonical_resultid"]) > 0
            totals["error_workunits"] += int(row["error_mask"]) != 0
            for name in ("unsent", "running", "returned", "invalid"):
                totals[name + "_replicas"] += int(row[name] or 0)
    return {"pages": states, **totals}


def assert_processed(root, sample):
    bundled = None
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
        # A bundled root (--bundle-size above 1) stages each page under pages/ and submits it
        # in a bundle under units/. A page too large for any bundle keeps its own qwen.json.
        page = root / "pages" / doc
        if (page / "no_crops").exists():
            continue
        if (page / "too_large").exists() and any((page / f).exists() for f in ("rejected-qwen.json", "qwen.json")):
            continue
        if bundled is None:
            # Only submitted units' jobs are read, not every staged one.
            bundled = set()
            for marker in sorted((root / "units").glob("*/submitted")):
                path = marker.parent / "qwen.json"
                if path.exists() and (job := json.loads(path.read_text())).get("pages"):
                    bundled.update(documents(job))
        if doc in bundled:
            continue
        raise RuntimeError(f"Chunk still has an unprocessed native source: {doc}; retain READY")


def assert_bundle_layout(root, bundle_size):
    """A bundled campaign has a root of its own. A root staged with bundles can't go back to
    single pages, and bundling a root that already holds single-page units would mix both. Any
    unit directory not named as a bundle was staged one page per unit, whatever its marker and
    whether or not pages/ exists."""
    if bundle_size == 1 and (root / "pages").is_dir():
        raise RuntimeError(f"Root {root.name} was staged with bundles; keep its --bundle-size")
    units = root / "units"
    if bundle_size > 1 and units.is_dir() and any(p.is_dir() and not BUNDLE_UNIT.match(p.name)
                                                  for p in units.iterdir()):
        raise RuntimeError(f"Root {root.name} holds single-page units; a bundled campaign starts in a new root")


def unreadable_versions(state, prompt_version):
    """Dispatchable app versions that can't read the request. With app.min_version 0 the feeder
    loads only the newest per platform and plan class, otherwise everything from min_version up."""
    newest = {}
    for row in state["app_versions"]:
        key = (row["plan_class"], row["platformid"])
        newest[key] = max(newest.get(key, 0), row["version_num"])
    minimum = state["min_version"]
    return sorted((row["plan_class"], row["platformid"], row["version_num"])
                  for row in state["app_versions"]
                  if (row["version_num"] >= minimum if minimum
                      else row["version_num"] == newest[(row["plan_class"], row["platformid"])])
                  and row["version_num"] < REQUEST_MIN_APP[prompt_version])


def assert_request_cohort(job, prompt_version):
    """Yiddish is read at version 4 and only Yiddish is: a guarded root never mixes them."""
    if prompt_version in GUARDED_VERSIONS and (job["language"] == YIDDISH) != (prompt_version == 4):
        raise RuntimeError(f"Language {job['language']} does not belong at request version {prompt_version}: "
                           f"{job['doc_id']}")


def assert_root_request_version(a, root):
    for _, job in jobs(root):
        if job.get("prompt_version") != a.prompt_version:
            raise RuntimeError(f"Root {a.campaign} holds request version {job.get('prompt_version')}, "
                               f"not {a.prompt_version}; a version gets its own root")


def admission(a, state, root):
    assert_root_request_version(a, root)
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
    unreadable = unreadable_versions(state, a.prompt_version)
    if unreadable:
        reason = "dispatchable_app_cannot_read_request"
    elif 2 * (state["fresh_pairs"] + a.max_pages) > a.max_ready:
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
        "audit_total": len(verified), "prompt_version": a.prompt_version,
        "unreadable_app_versions": unreadable,
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
    bindings = {}
    for path, job in jobs(root / "units"):
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        if verified.get(job["id"], {}).get("job_sha256") != digest:
            pending[str(path)] = digest
            expected[job["id"]] = digest
            bindings[job["id"]] = source_bindings(root, job)
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
    for identity, result in new.items():
        result["audited_at"] = time.time()
        if bindings[identity]:
            result["source_bindings"] = bindings[identity]
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


def selected_jobs(root, file, wanted):
    selected = []
    for path in sorted((root / "units").glob("*/" + file)):
        job = json.loads(path.read_text())
        members = documents(job)
        if not members.intersection(wanted):
            continue
        if not members <= set(wanted) and not (path.parent / "submitted").is_file():
            raise RuntimeError("pending unit crosses shipment membership: " + str(path))
        selected.append((path, job))
    selected.sort(key=lambda item: min(wanted.index(doc) for doc in documents(item[1]) if doc in wanted))
    return selected


def publication_rows(project, selected, app):
    """Reconcile by exact scientific identity, not command exit or a marker alone."""
    import qwen_shipment as shipment
    expected = {str(path): shipment.file_hash(path) for path, _ in selected}
    php = """
chdir(%s);
require_once("boinc_db.inc");
$appname = %s;
$app = BoincApp::lookup("name='".$appname."'");
if (!$app) throw new RuntimeException("Application missing");
$out = [];
foreach (json_decode(stream_get_contents(STDIN), true, 512, JSON_THROW_ON_ERROR) as $path => $sha) {
    if (hash_file("sha256", $path) !== $sha) throw new RuntimeException("Job changed during reconciliation");
    $job = json_decode(file_get_contents($path), true, 512, JSON_THROW_ON_ERROR);
    if (!preg_match('/^[a-f0-9]{64}$/D', $job["id"])) throw new RuntimeException("Bad identity");
    $name = $appname."_".$job["id"];
    $rows = BoincWorkunit::enum("name='".$name."'");
    if (count($rows) > 1) throw new RuntimeException("Duplicate scientific identity");
    if (!$rows) continue;
    $wu = $rows[0];
    if (intval($wu->appid) !== intval($app->id) || intval($wu->min_quorum) !== 2 ||
        intval($wu->target_nresults) !== 2 || ($appname === "usnm_qwen" && intval($wu->batch) !== 8) ||
        strpos($wu->xml_doc, $job["id"]."_".($appname === "usnm_qwen" ? "qwen" : "quality")."_json") === false)
        throw new RuntimeException("Unexpected scientific input, routing or quorum");
    $out[$job["id"]] = ["job_sha256"=>$sha, "workunit_id"=>intval($wu->id),
        "batch"=>intval($wu->batch), "min_quorum"=>2, "target_nresults"=>2];
}
echo json_encode((object)$out);
""" % (json.dumps(str(project / "html/inc")), json.dumps(app))
    rows = json.loads(subprocess.check_output(["php", "-r", php], input=json.dumps(expected), text=True))
    known = {job["id"]: shipment.file_hash(path) for path, job in selected}
    if not isinstance(rows, dict) or not set(rows) <= known.keys() or any(
            row["job_sha256"] != known[identity] or type(row["workunit_id"]) is not int
            or row["workunit_id"] < 1 or row["min_quorum"] != 2 or row["target_nresults"] != 2
            or (app == "usnm_qwen" and row["batch"] != 8) for identity, row in rows.items()):
        raise RuntimeError("changed publication reconciliation")
    return rows


def reconcile(project, selected, app):
    import qwen_shipment as shipment
    rows = publication_rows(project, selected, app)
    for path, job in selected:
        unit = path.parent
        name = app + "_" + job["id"]
        for marker in ("submitting", "submitted"):
            if (unit / marker).exists() and (unit / marker).read_text() != name:
                raise RuntimeError("changed submission journal: " + str(unit))
        if job["id"] in rows:
            if not (unit / "submitted").exists() or (unit / "submitting").exists():
                # Positive authoritative evidence repairs an interrupted publication.
                # Absence never licenses deleting a submitting marker.
                marker = unit / "submitting"
                if not marker.exists():
                    marker.write_text(name)
                with marker.open("rb") as stream:
                    import os
                    os.fsync(stream.fileno())
                marker.replace(unit / "submitted")
            else:
                with (unit / "submitted").open("rb") as stream:
                    import os
                    os.fsync(stream.fileno())
            shipment.sync_dir(unit)
        elif any((unit / marker).exists() for marker in ("submitting", "submitted")):
            raise RuntimeError("publication history unavailable/uncertain; retain journals, never replay")
    return rows


def recipe_inputs(a):
    import qwen_shipment as shipment
    binary = Path(shutil.which(a.stage_binary) or a.stage_binary)
    places = a.share / "quality" / a.campaign / "places.txt"
    if not places.exists():
        places = a.share / "quality/v1-wave9/places.txt"
    return {"native_engine": json.loads(a.engine_plan.read_bytes())["engine"],
            "stage_binary": {"bytes": binary.stat().st_size, "sha256": shipment.file_hash(binary)},
            "places": {"bytes": places.stat().st_size, "sha256": shipment.file_hash(places)}}


def quality_source_binding(quality, member, recipe, *, create=False):
    """Only newly staged jobs may acquire an export/reader binding."""
    import qwen_shipment as shipment
    doc = member["source"]["doc_id"]
    receipt = quality / "shipment-quality" / (doc + ".json")
    if not create and not receipt.is_file():
        raise RuntimeError("unbound retained quality reader/text inputs; explicit reconciliation required")
    existing = json.loads(receipt.read_text()) if receipt.is_file() else None
    layout = existing["layout"] if existing else next(
        (name for name in ("pages", "units") if (quality / name / doc / "quality.json").is_file()), None)
    if layout not in ("pages", "units"):
        raise RuntimeError("bound quality source job unavailable")
    path = quality / layout / doc / "quality.json"
    expected = {"schema": 1, "member": member, "recipe": recipe, "layout": layout,
                "quality_sha256": shipment.file_hash(path)}
    if existing is not None and existing != expected:
        raise RuntimeError("changed retained quality reader/text binding; explicit reconciliation required")
    job = json.loads(path.read_text())
    if job["doc_id"] != doc or job.get("pages"):
        raise RuntimeError("invalid bound quality source job")
    if (job["engine"]["image"] != recipe["engine_image"]
            or {key: job["inputs"]["places"][key] for key in ("bytes", "sha256")} != recipe["places"]):
        raise RuntimeError("quality source job recipe changed")
    if create:
        shipment.check_file(path.parent / "texts.json",
                            {key: job["inputs"]["texts"][key] for key in ("bytes", "sha256")})
        shipment.freeze(receipt, expected)
    return job


def archive_provenance(quality, member, input_dir=None):
    import qwen_shipment as shipment
    doc = member["source"]["doc_id"]
    rel = f"texts/{doc}.loc-provenance.json"
    expected = member["files"].get(rel)
    if expected is None:
        raise RuntimeError("missing frozen archive provenance; explicit reconciliation required")
    path = (input_dir if input_dir is not None else quality) / rel
    shipment.check_file(path, expected)
    provenance = json.loads(path.read_bytes())
    if (provenance.get("doc_id") != doc
            or provenance.get("sha256") != member["files"][f"scans/{doc}.xml"]["sha256"]
            or not provenance.get("archive") or not provenance.get("member")
            or int(provenance.get("archive_bytes", 0)) <= 0):
        raise RuntimeError("archive provenance does not bind source ALTO")
    if provenance["sha256"] != member["quality_alto"]["sha256"]:
        raise RuntimeError("archive provenance does not bind prepared ALTO; explicit reconciliation required")
    if input_dir is not None:
        shipment.freeze_file(quality / rel, path, expected)


def transaction_quality(a, value, input_dir, quality):
    import qwen_shipment as shipment
    docs = [member["source"]["doc_id"] for member in value["members"]]
    # Refuse a pre-existing, unsubmitted bundle spanning the reservation before
    # quality stage could repack it. Completed bundles remain immutable.
    staged = selected_jobs(quality, "quality.json", docs)
    staged_docs = set().union(*(documents(job) for _, job in staged))
    for member in value["members"]:
        doc = member["source"]["doc_id"]
        archive_provenance(quality, member, input_dir)
        binding_path = quality / "texts" / (doc + ".image-provenance.json")
        incoming = json.loads((input_dir / "texts" / binding_path.name).read_text())
        existing = [quality / layout / doc / "quality.json" for layout in ("pages", "units")]
        retained = (doc in staged_docs or any(path.exists() for path in existing)
                    or any((path.parent / marker).exists() for path in existing
                           for marker in ("submitting", "submitted")))
        if retained or (quality / "shipment-quality" / (doc + ".json")).exists():
            quality_source_binding(quality, member, value["recipe"])
        if retained and not binding_path.exists():
            raise RuntimeError("legacy/unbound quality source; retain transaction")
        shipment.freeze(binding_path, incoming)
        shipment.freeze(quality / "shipment-sources" / (doc + ".json"), member)
        for path in existing:
            if path.exists():
                job = json.loads(path.read_text())
                for field, expected in (("page", member["files"][f"scans/{doc}.jpg"]), ("alto", member["quality_alto"])):
                    if {k: job["inputs"][field][k] for k in ("bytes", "sha256")} != expected:
                        raise RuntimeError("changed retained quality input")
    recipe = value["recipe"]
    places = quality / "places.txt"
    shipment.freeze_file(places, places if places.exists() else a.share / "quality/v1-wave9/places.txt",
                         recipe["places"])
    langs = sorted({member["sample"]["lang"] for member in value["members"]})
    subprocess.run([a.stage_binary, "quality", "function-words", "--out-dir", str(quality / "function-words"),
                    *[arg for lang in langs for arg in ("--lang", lang)]], check=True)
    subprocess.run([
        a.stage_binary, "quality", "stage", "--sample", str(input_dir / "sample.jsonl"),
        "--root", str(quality), "--group", a.campaign, "--texts-dir", str(input_dir / "texts"),
        "--scans-dir", str(input_dir / "scans"), "--places", str(quality / "places.txt"),
        "--function-words-dir", str(quality / "function-words"), "--readers", recipe["readers"],
        "--engine-image", recipe["engine_image"], "--max-pages", str(len(docs)),
        "--bundle-size", str(recipe["bundle_size"])], check=True)
    for member in value["members"]:
        quality_source_binding(quality, member, recipe, create=True)


def transaction_native_sources(root, value):
    states = {}
    for member in value["members"]:
        doc = member["source"]["doc_id"]
        unit = root / "units" / doc
        if terminal_source(unit) != member["source"]:
            raise RuntimeError("legacy/unbound native source: " + doc)
        if (unit / "qwen.json").exists():
            job = json.loads((unit / "qwen.json").read_text())
            if (job.get("pages") or job["doc_id"] != doc
                    or job["prompt_version"] != value["recipe"]["prompt_version"]
                    or job["language"] != member["sample"]["lang"]
                    or {key: job["input"][key] for key in ("bytes", "sha256")} != member["files"][f"scans/{doc}.jpg"]):
                raise RuntimeError("transaction requires bound single-page native jobs at its request version")
            states[doc] = "pending"
        elif (unit / "no_crops").exists():
            states[doc] = "no_crops"
        elif (unit / "too_large").exists() and (unit / "rejected-qwen.json").exists():
            states[doc] = "too_large"
        else:
            raise RuntimeError("native source has no bound disposition: " + doc)
    return states


def check_transaction_jobs(value, native, quality_jobs, engine):
    expected = {member["source"]["doc_id"]: member for member in value["members"]}
    counts = {doc: 0 for doc in expected}
    for _, job in native:
        if job["engine"] != engine or engine != value["recipe"]["native_engine"]:
            raise RuntimeError("native job engine changed")
        if (job["v"] != 1 or job["group"] != value["recipe"]["campaign"]
                or job["gpu_timeout_secs"] != 14400 or job["cpu_timeout_secs"] != 7200
                or job["max_tokens_per_1000px2"] != 4):
            raise RuntimeError("native job staging parameters changed")
    for path, job in quality_jobs:
        if job["engine"]["image"] != value["recipe"]["engine_image"]:
            raise RuntimeError("quality job engine changed")
        if {key: job["inputs"]["places"][key] for key in ("bytes", "sha256")} != value["recipe"]["places"]:
            raise RuntimeError("quality job places changed")
        for page in job.get("pages") or [job]:
            doc = page["doc_id"]
            if doc not in expected:
                continue
            member = expected[doc]
            archive_provenance(path.parent.parent.parent, member)
            counts[doc] += 1
            if page["language"] != member["sample"]["lang"] or page["group"] != value["recipe"]["campaign"]:
                raise RuntimeError("quality job recipe changed")
            retained = quality_source_binding(path.parent.parent.parent, member, value["recipe"])
            if (page["readers"] != retained["readers"]
                    or any(page["inputs"]["texts"][key] != retained["inputs"]["texts"][key]
                           for key in ("bytes", "sha256"))):
                raise RuntimeError("quality job reader/text inputs changed")
            words = page.get("function_words", [])
            if (not isinstance(words, list)
                    or (words or job["function_words"]) != retained["function_words"]
                    or any(page[key] != retained[key] for key in ("crop_seed", "k_crops"))):
                raise RuntimeError("quality job staging parameters changed")
            for name, expected_input in (("page", member["files"][f"scans/{doc}.jpg"]), ("alto", member["quality_alto"])):
                if {key: page["inputs"][name][key] for key in ("bytes", "sha256")} != expected_input:
                    raise RuntimeError("quality job is not bound to shipment inputs")
    if any(count != 1 for count in counts.values()):
        raise RuntimeError("quality source is missing or duplicated; retain parent")


def verify_acknowledgement(a, tx, value, receipt):
    """Durable positive receipts survive BOINC history purging; unbound markers do not."""
    import qwen_shipment as shipment
    docs = [member["source"]["doc_id"] for member in value["members"]]
    root = a.share / "qwen" / a.campaign
    states = transaction_native_sources(root, value)
    native = selected_jobs(root, "qwen.json", docs)
    quality_jobs = selected_jobs(a.share / "quality" / a.campaign, "quality.json", docs)
    engine = json.loads((tx / "engine.json").read_text())
    if engine != value["recipe"]["native_engine"]:
        raise RuntimeError("changed frozen native engine")
    check_transaction_jobs(value, native, quality_jobs, engine)
    audited = json.loads((root / "audit.json").read_text()) if native else {}
    for kind, selected, app in (("native", native, "usnm_qwen"), ("quality", quality_jobs, "usnm_quality")):
        intent = {job["id"]: shipment.file_hash(path) for path, job in selected}
        if json.loads((tx / (kind + "-intent.json")).read_text()) != intent:
            raise RuntimeError("acknowledged job changed")
        rows = receipt.get(kind + "_workunits", {})
        if set(rows) != set(intent):
            raise RuntimeError("unbound publication acknowledgement")
        for path, job in selected:
            row = rows[job["id"]]
            if (row["job_sha256"] != intent[job["id"]] or row["min_quorum"] != 2 or row["target_nresults"] != 2
                    or type(row["workunit_id"]) is not int or row["workunit_id"] < 1
                    or (path.parent / "submitting").exists()
                    or (path.parent / "submitted").read_text() != app + "_" + job["id"]):
                raise RuntimeError("publication acknowledgement changed")
            if kind == "native":
                audit_row = audited.get(job["id"], {})
                if (row["batch"] != 8 or row.get("source_bindings") != source_bindings(root, job)
                        or any(audit_row.get(key) != item for key, item in row.items())):
                    raise RuntimeError("native acknowledgement lost bound audit")
                states[job["doc_id"]] = "published"
    if (receipt.get("schema") != 1 or receipt.get("transaction_id") != value["id"]
            or receipt.get("parent_id") != value["parent"]["id"] or receipt.get("status") != "acknowledged"
            or not receipt.get("native_acknowledged") or not receipt.get("quality_acknowledged")
            or receipt["members"] != {doc: {"native": states[doc], "quality": "published"} for doc in docs}):
        raise RuntimeError("incomplete transaction acknowledgement")


def run_transaction(a):
    """Called only by the shipper under /run/usnm-boinc-step.lock."""
    import qwen_shipment as shipment
    if not re.fullmatch(r"[a-f0-9]{64}", a.transaction):
        raise ValueError("invalid transaction identity")
    tx = a.share / "qwen-shipments" / a.campaign / a.transaction
    input_dir = tx / "input"
    if a.upload and not re.fullmatch(r"upload-[a-f0-9]{32}", a.upload):
        raise ValueError("invalid shipment upload")
    upload = tx / a.upload if a.upload else None
    if not input_dir.exists():
        if upload is None:
            raise RuntimeError("transaction upload missing; retain parent")
        value = shipment.validate(json.loads((upload / "manifest.json").read_text()))
        if value["id"] != a.transaction:
            raise ValueError("upload transaction identity changed")
        shipment.verify_payload(upload, value)
        shipment.sync_payload(upload)
        upload.rename(input_dir)
        shipment.sync_dir(tx)
    value = shipment.validate(json.loads((input_dir / "manifest.json").read_text()))
    if value["id"] != a.transaction or value["recipe"]["campaign"] != a.campaign:
        raise ValueError("transaction route changed")
    if a.prompt_version != value["recipe"]["prompt_version"]:
        raise ValueError("transaction request version changed")
    shipment.verify_payload(input_dir, value)
    shipment.freeze(tx / "manifest.json", value)
    docs = [member["source"]["doc_id"] for member in value["members"]]
    a.max_pages = len(docs)
    members_path = tx / "members.json"
    shipment.freeze(members_path, docs)
    root = a.share / "qwen" / a.campaign
    quality = a.share / "quality" / a.campaign
    record_path = tx / "receipt.json"
    receipt = json.loads(record_path.read_text()) if record_path.exists() else {
        "schema": 1, "transaction_id": value["id"], "parent_id": value["parent"]["id"],
        "members": {doc: {"native": "pending", "quality": "pending"} for doc in docs},
        "publication_attempted": False, "native_acknowledged": False, "quality_acknowledged": False}
    if (receipt.get("transaction_id") != value["id"] or receipt.get("parent_id") != value["parent"]["id"]
            or set(receipt["members"]) != set(docs)):
        raise ValueError("receipt membership changed")
    assert_root_request_version(a, root)
    if receipt.get("status") == "acknowledged":
        verify_acknowledgement(a, tx, value, receipt)
        print(json.dumps({"transaction_receipt": receipt}, sort_keys=True), flush=True)
        return
    receipt.update(status="in_progress", phase="locked_preflight")
    shipment.save(record_path, receipt)
    try:
        if any(value["recipe"][key] != item for key, item in recipe_inputs(a).items()):
            raise RuntimeError("changed frozen staging recipe")
        state = inventory(a.share, a.project)
        if state["free_bytes"] - len(docs) * a.reserve_mib * 1024**2 < a.min_free_gb * 1024**3:
            receipt.update(shipment.publication_evidence(receipt),
                           phase="locked_storage", reason="reserved_input_free_space", reserve_pages=len(docs))
            raise SystemExit(75)
        elsewhere = set()
        for path, job in jobs(a.share / "qwen"):
            if root not in path.parents:
                elsewhere.update(documents(job))
        for member in value["members"]:
            row = member["sample"]
            if row["lang"] in ("eng", "", "und") or row["doc_id"] in elsewhere:
                raise RuntimeError("English, unclassified or duplicate transaction source")
            assert_request_cohort({"language": row["lang"], "doc_id": row["doc_id"]}, a.prompt_version)
        engine = value["recipe"]["native_engine"]
        shipment.freeze(tx / "engine.json", engine)
        transaction_quality(a, value, input_dir, quality)
        subprocess.run([
            a.stage_binary, "qwen", "stage", "--quality-root", str(quality), "--root", str(root),
            "--engine-image", engine["image"], "--engine-llama", engine["llama"],
            "--engine-weights", engine["weights"], "--max-pages", str(len(docs)),
            "--gpu-timeout-secs", "14400", "--cpu-timeout-secs", "7200", "--bundle-size", "1",
            "--prompt-version", str(a.prompt_version), "--members", str(members_path)], check=True)
        states = transaction_native_sources(root, value)
        native = selected_jobs(root, "qwen.json", docs)
        quality_jobs = selected_jobs(quality, "quality.json", docs)
        check_transaction_jobs(value, native, quality_jobs, engine)
        for kind, selected in (("native", native), ("quality", quality_jobs)):
            shipment.freeze(tx / (kind + "-intent.json"),
                            {job["id"]: shipment.file_hash(path) for path, job in selected})
        audit_path = root / "audit.json"
        audited = json.loads(audit_path.read_text()) if audit_path.exists() else {}
        for path, job in native:
            previous = audited.get(job["id"])
            if previous and (previous.get("job_sha256") != shipment.file_hash(path)
                             or previous.get("source_bindings") != source_bindings(root, job)):
                raise RuntimeError("legacy/unbound or changed native audit; explicit reconciliation required")
        native_rows = reconcile(a.project, native, "usnm_qwen")
        reconcile(a.project, quality_jobs, "usnm_quality")
        # Published replays do not consume new queue capacity; disk reservation
        # remains the exact transferred subset, including terminal dispositions.
        state = inventory(a.share, a.project)
        pending = sum(job["id"] not in native_rows for _, job in native)
        reason = None
        if unreadable_versions(state, a.prompt_version) and pending:
            reason = "dispatchable_app_cannot_read_request"
        elif 2 * (state["fresh_pairs"] + pending) > a.max_ready and pending:
            reason = "fresh_original_watermark"
        elif state["unresolved_native"] + pending > a.max_unresolved and pending:
            reason = "unresolved_native_ceiling"
        elif state["free_bytes"] - len(docs) * a.reserve_mib * 1024**2 < a.min_free_gb * 1024**3:
            reason = "reserved_input_free_space"
        receipt.update(reserve_pages=len(docs), pending_native=pending, reason=reason,
                       phase="locked_recheck")
        if reason:
            receipt.update(shipment.publication_evidence(receipt))
            if native_rows:
                receipt["status"] = "partial"
            raise SystemExit(75)
        receipt.update(phase="native_submit", publication_attempted=True)
        shipment.save(record_path, receipt)
        subprocess.run([a.stage_binary, "qwen", "submit", "--root", str(root), "--project", str(a.project),
                        "--target", "native-8b", "--model-dir", str(a.models), "--max-units", str(len(docs)),
                        "--members", str(members_path)], check=True)
        native_rows = reconcile(a.project, native, "usnm_qwen")
        if set(native_rows) != {job["id"] for _, job in native}:
            raise RuntimeError("native publication incomplete")
        for path, job in native:
            row = native_rows[job["id"]]
            row["source_bindings"] = source_bindings(root, job)
            previous = audited.get(job["id"])
            if previous and any(previous.get(k) != row[k] for k in row):
                raise RuntimeError("legacy/unbound or changed native audit; explicit reconciliation required")
            audited[job["id"]] = {**row, "audited_at": previous.get("audited_at", time.time()) if previous else time.time()}
            states[job["doc_id"]] = "published"
        shipment.save(audit_path, audited)
        for doc, state in states.items():
            receipt["members"][doc]["native"] = state
        receipt["native_workunits"] = native_rows
        receipt.update(native_acknowledged=True, phase="quality_submit")
        shipment.save(record_path, receipt)
        subprocess.run([a.stage_binary, "quality", "submit", "--root", str(quality),
                        "--project", str(a.project), "--max-units", str(len(docs)),
                        "--members", str(members_path)], check=True)
        quality_rows = reconcile(a.project, quality_jobs, "usnm_quality")
        if set(quality_rows) != {job["id"] for _, job in quality_jobs}:
            raise RuntimeError("quality publication incomplete")
        receipt["quality_workunits"] = quality_rows
        for doc in docs:
            receipt["members"][doc]["quality"] = "published"
        receipt.update(quality_acknowledged=True, status="acknowledged", phase="complete")
    except BaseException:
        if receipt["status"] == "in_progress":
            receipt["status"] = ("partial" if receipt["native_acknowledged"]
                                 else "uncertain" if receipt["publication_attempted"] else "failed")
        raise
    finally:
        shipment.save(record_path, receipt)
        print(json.dumps({"transaction_receipt": receipt}, sort_keys=True), flush=True)


def retire_parent(a):
    import qwen_shipment as shipment
    base = a.share / "qwen-shipments" / a.campaign
    records = []
    parent = None
    covered = []
    for path in base.glob("*/manifest.json"):
        value = shipment.validate(json.loads(path.read_text()))
        if value["parent"]["id"] != a.retire_parent:
            continue
        if parent is not None and value["parent"] != parent:
            raise RuntimeError("conflicting parent retirement")
        parent = value["parent"]
        receipt = json.loads((path.parent / "receipt.json").read_text())
        if (receipt.get("transaction_id") != value["id"] or receipt.get("parent_id") != a.retire_parent
                or receipt.get("status") != "acknowledged" or not receipt.get("native_acknowledged")
                or not receipt.get("quality_acknowledged")
                or set(receipt["members"]) != {m["source"]["doc_id"] for m in value["members"]}
                or any(row["native"] not in ("published", "no_crops", "too_large")
                       or row["quality"] != "published" for row in receipt["members"].values())):
            raise RuntimeError("parent still has unacknowledged members")
        verify_acknowledgement(a, path.parent, value, receipt)
        records.append(path.parent)
        covered.extend(member["source"] for member in value["members"])
    if not parent or covered != parent["sources"]:
        # Directory iteration has no semantic order; compare fingerprints and
        # require a disjoint, complete cover, not merely equal page counts.
        if (not parent or len(covered) != len(parent["sources"])
                or sorted(covered, key=lambda p: p["doc_id"]) != sorted(parent["sources"], key=lambda p: p["doc_id"])):
            raise RuntimeError("incomplete or overlapping parent acknowledgement")
    shipment.freeze(base / ("retired-" + a.retire_parent + ".json"), {
        "schema": 1, "parent": parent, "transactions": sorted(path.name for path in records)})
    for path in records:
        if (path / "input").exists():
            shutil.rmtree(path / "input")
            shipment.sync_dir(path)
        for upload in path.glob("upload-*"):
            if upload.is_dir() and re.fullmatch(r"upload-[a-f0-9]{32}", upload.name):
                shutil.rmtree(upload)
        shipment.sync_dir(path)
    print(json.dumps({"retired_parent": a.retire_parent}))


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--share", type=Path, default=Path("/project"))
    ap.add_argument("--project", type=Path, default=Path("/project/usnmocr"))
    ap.add_argument("--inventory", action="store_true")
    ap.add_argument("--recipe-inputs", action="store_true", help="Read-only staging input fingerprints")
    ap.add_argument("--source-status", action="store_true")
    ap.add_argument("--preflight", action="store_true",
                    help="Read-only admission before transferring a READY chunk; exit 75 on capacity deferral")
    ap.add_argument("--transaction", help="Frozen subset identity; run under the existing host bridge mutex")
    ap.add_argument("--upload", help="Unique, completed transfer directory to adopt under the bridge mutex")
    ap.add_argument("--retire-parent", help="Retire input copies only after a complete two-application acknowledgement")
    ap.add_argument("--campaign")
    ap.add_argument("--engine-plan", type=Path)
    ap.add_argument("--models", type=Path)
    ap.add_argument("--sample", type=Path, help="READY chunk; every source needs a terminal native disposition")
    isolated_stage = Path(__file__).with_name("usnm-boinc-stage")
    ap.add_argument("--stage-binary", default=str(isolated_stage) if isolated_stage.is_file() else "usnm-boinc")
    ap.add_argument("--submit-binary",
                    help="submit every request version with this binary (the image's usnm-boinc once it is "
                         "from #1437 on); without it, versions 3 and 4 submit with --stage-binary")
    ap.add_argument("--max-pages", type=int, default=100)
    ap.add_argument("--bundle-size", type=int, default=1,
                    help="Pages per native unit, 1 to 32 (#1435). Above 1 the root keeps bundling for good, "
                         "so a bundled campaign starts in a new root; never with --transaction")
    ap.add_argument("--prompt-version", type=int, choices=(1, 2, 3, 4), default=1,
                    help="Request version to stage; every dispatchable app version must read it")
    ap.add_argument("--max-ready", type=int, default=1000,
                    help="Fresh native original replicas, not host-ineligible retry replicas")
    ap.add_argument("--max-unresolved", type=int, default=2000)
    ap.add_argument("--reserve-mib", type=int, default=64,
                    help="Conservative incremental bytes per new page on every backing filesystem")
    ap.add_argument("--min-free-gb", type=int, default=50)
    a = ap.parse_args()
    if a.recipe_inputs:
        if not a.campaign or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign) or not a.engine_plan:
            ap.error("--recipe-inputs needs a campaign and engine plan")
        print(json.dumps(recipe_inputs(a)))
        return
    if a.source_status:
        if not a.sample:
            ap.error("--source-status needs --sample FILE or -")
        sample = sys.stdin.read(16 * 1024**2 + 1) if str(a.sample) == "-" else a.sample.read_text()
        if len(sample.encode()) > 16 * 1024**2:
            raise ValueError("source-status sample exceeds bound")
        print(json.dumps(source_status(a.share, a.project, sample), sort_keys=True))
        return
    if a.retire_parent:
        if (not a.campaign or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
                or not re.fullmatch(r"[a-f0-9]{64}", a.retire_parent)):
            ap.error("invalid retirement identity")
        retire_parent(a)
        return
    state = inventory(a.share, a.project)
    if a.inventory:
        print(json.dumps(state))
        return
    if (not a.campaign or not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,40}", a.campaign)
            or a.engine_plan is None or a.models is None or not 1 <= a.max_pages <= 500
            or a.max_ready < 2 or not 1 <= a.max_unresolved <= 10000
            or a.reserve_mib < 64 or a.min_free_gb < 1):
        ap.error("need --campaign, --engine-plan, --models and positive bounded limits")
    if not 1 <= a.bundle_size <= 32:
        ap.error("--bundle-size takes 1 to 32 pages a unit")
    root = a.share / "qwen" / a.campaign
    quality = a.share / "quality" / a.campaign
    if a.transaction:
        if a.preflight:
            ap.error("transaction publication is not a read-only preflight")
        if a.bundle_size != 1:
            ap.error("subset transactions stage one page per unit")
        if a.submit_binary:
            ap.error("transactions submit with --stage-binary")
        if a.max_ready > 1000 or a.max_unresolved > 2000 or a.min_free_gb < 50:
            ap.error("subset transactions retain the 1000/2000 watermarks and 50 GiB floor")
        try:
            run_transaction(a)
        except subprocess.CalledProcessError as error:
            raise SystemExit(error.returncode) from error
        return
    assert_bundle_layout(root, a.bundle_size)
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
        assert_request_cohort(job, a.prompt_version)
    engine = json.loads(a.engine_plan.read_text())["engine"]
    stage = [a.stage_binary, "qwen", "stage", "--quality-root", str(quality), "--root", str(root),
             "--engine-image", engine["image"], "--engine-llama", engine["llama"],
             "--engine-weights", engine["weights"], "--max-pages", str(a.max_pages),
             "--gpu-timeout-secs", "14400", "--cpu-timeout-secs", "7200", "--bundle-size", str(a.bundle_size),
             "--prompt-version", str(a.prompt_version)]
    subprocess.run(stage, check=True)
    submit_binary = a.submit_binary or (a.stage_binary if a.prompt_version in GUARDED_VERSIONS else "usnm-boinc")
    subprocess.run([submit_binary, "qwen", "submit", "--root", str(root), "--project", str(a.project),
                    "--target", "native-8b", "--model-dir", str(a.models), "--max-units", str(a.max_pages)],
                   check=True)
    assert_submitted(root)
    print(json.dumps(audit(root, a.project), sort_keys=True))
    if a.sample:
        assert_processed(root, a.sample)


if __name__ == "__main__":
    main()
