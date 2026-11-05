"""Regression tests for real-source Qwen replenishment; no GPU, Pillow or network."""
import hashlib
import contextlib
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import io
import inspect
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import stat
import tarfile
import tempfile
import threading
import types
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))


def load(name, path=None):
    spec = importlib.util.spec_from_file_location(name, path or HERE / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


archive = load("archive_alto")
quality = load("quality_feed")
submit = load("qwen_submit")
driver = load("qwen_feed")
source = load("qwen_source")
shipment = load("qwen_shipment")
cohort = load("qwen_auto_cohort", HERE.parents[1] / "qwen-auto-cohort.py")
DOC = "sn1_1900-01-02_ed-1_seq-3"


def frozen_page(doc=DOC):
    return {"doc_id": doc, "batch": "xx_test", "batch_version": 1,
            "bytes": 12, "sha256": "a" * 64}


def terminal_provenance(unit):
    image = {"bytes": 3, "sha256": hashlib.sha256(b"jpg").hexdigest()}
    quality = json.dumps({"doc_id": unit.name, "inputs": {"page": image}}).encode()
    (unit / "quality.json").write_bytes(quality)
    binding = {"schema": 1, "source": frozen_page(unit.name), "jpeg": image}
    (unit / "source-provenance.json").write_text(json.dumps({
        "schema": 1, "binding": binding, "quality_sha256": hashlib.sha256(quality).hexdigest()}))


def empty_boinc_php(project):
    directory = project / "html/inc"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "boinc_db.inc").write_text("""<?php
class BoincApp {
    static function lookup($query) { return (object)["id" => 1]; }
}
class BoincWorkunit {
    static function enum($query) { return []; }
}
""")


@unittest.skipUnless(shutil.which("php"), "PHP CLI required for actual query JSON encoding")
class PublicationRowsPhpTest(unittest.TestCase):
    def test_real_php_empty_result_for_fresh_and_no_selected_workunits(self):
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            empty_boinc_php(project)
            job = {"id": "a" * 64, "doc_id": DOC}
            path = project / "qwen.json"
            path.write_text(json.dumps(job))
            for selected in ([], [(path, job)]):
                with self.subTest(selected=len(selected)):
                    self.assertEqual(submit.publication_rows(project, selected, "usnm_qwen"), {})

    def test_non_object_publication_response_fails_closed(self):
        with mock.patch.object(submit.subprocess, "check_output", return_value="[]"):
            with self.assertRaisesRegex(RuntimeError, "changed publication reconciliation"):
                submit.publication_rows(Path("."), [], "usnm_qwen")


@unittest.skipUnless(shutil.which("flock"), "real util-linux flock required for host/CT gate rehearsal")
class LockedThinPoolTest(unittest.TestCase):
    def test_pool_probe_and_publication_share_the_lock_and_failed_probes_never_publish(self):
        with tempfile.TemporaryDirectory() as folder:
            base = Path(folder)
            for name, body in {
                "pct": """import os, sys
args = sys.argv[4:]
args = [os.environ["BRIDGE_LOCK"] if a == "/run/usnm-boinc-step.lock" else a for a in args]
os.execvp(args[0], args)
""",
                "lvs": """import fcntl, json, os, sys
with open(os.environ["BRIDGE_LOCK"], "a") as lock:
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        pass
    else:
        raise RuntimeError("thin-pool probe outside bridge mutex")
if os.environ["POOL_FREE"] == "fail":
    sys.exit(2)
print(json.dumps({"report": [{"lv": [{"lv_size": os.environ["POOL_FREE"], "data_percent": "0"}]}]}))
""",
            }.items():
                path = base / name
                path.write_text("#!" + sys.executable + "\n" + body)
                path.chmod(0o755)
            server = """import fcntl, os
from pathlib import Path
with open(os.environ["BRIDGE_LOCK"], "a") as lock:
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        pass
    else:
        raise RuntimeError("publication outside bridge mutex")
Path(os.environ["PUBLISHED"]).write_text("published")
print("server entered")
"""
            script = (inspect.getsource(shipment.locked_transaction)
                      + "\nimport sys, json\nraise SystemExit(locked_transaction(*json.loads(sys.argv[1])))")
            published = base / "published"
            environment = dict(os.environ, PATH=str(base) + os.pathsep + os.environ["PATH"],
                               BRIDGE_LOCK=str(base / "bridge.lock"), PUBLISHED=str(published))
            minimum = 50 * 1024**3 + 54 * 64 * 1024**2
            for free, status in ((str(minimum - 1), 75), ("fail", 1), (str(minimum), 0)):
                with self.subTest(free=free):
                    result = subprocess.run(
                        [sys.executable, "-c", script,
                         json.dumps([230, 1, "a" * 64, 54, 50, [sys.executable, "-c", server]])],
                        env=dict(environment, POOL_FREE=free), capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, status, result.stderr)
                    self.assertEqual(published.exists(), status == 0)
                    if status == 75:
                        self.assertEqual(json.loads(result.stdout)["shipment_gate"]["reserve_pages"], 54)
                    with (base / "bridge.lock").open("a") as lock:
                        driver.fcntl.flock(lock, driver.fcntl.LOCK_EX | driver.fcntl.LOCK_NB)


@unittest.skipUnless(os.environ.get("USNM_QUALITY_BUNDLE_FIXTURE"),
                     "invoked by the actual Rust default-bundle staging regression")
class RustQualityBundleContractTest(unittest.TestCase):
    def test_actual_default_bundle_and_retained_acknowledgement_share_worker_defaults(self):
        base = Path(os.environ["USNM_QUALITY_BUNDLE_FIXTURE"])
        qroot = base / "quality/v1-wave5"
        native_root = base / "qwen/v1-wave5"
        quality_path, = (qroot / "units").glob("*/quality.json")
        raw_bundle = quality_path.read_bytes()
        bundle = json.loads(raw_bundle)
        self.assertEqual(len(bundle["pages"]), 2)
        self.assertTrue(bundle["function_words"])
        self.assertTrue(all("function_words" not in page for page in bundle["pages"]))
        native = list(submit.jobs(native_root))
        recipe = {
            "campaign": "v1-wave5", "prompt_version": 3, "bundle_size": 200,
            "engine_image": bundle["engine"]["image"], "readers": "loc,as,ours-tesseract",
            "native_engine": native[0][1]["engine"],
        }

        def fingerprint(path):
            return {"bytes": path.stat().st_size, "sha256": shipment.file_hash(path)}

        recipe["stage_binary"] = fingerprint(Path(os.environ["USNM_QUALITY_STAGE_BINARY"]))
        recipe["places"] = fingerprint(base / "places.txt")
        samples = {row["doc_id"]: row for row in map(json.loads, (base / "sample.jsonl").read_text().splitlines())}
        members = []
        for page in bundle["pages"]:
            doc = page["doc_id"]
            original = json.loads((qroot / "texts" / (doc + ".image-provenance.json")).read_text())
            provenance = qroot / "texts" / (doc + ".loc-provenance.json")
            shipment.save(provenance, {
                "doc_id": doc, "sha256": shipment.file_hash(base / "scans" / (doc + ".xml")),
                "archive": "dlc_abc_ver01.tar.bz2", "member": doc + "/ocr.xml", "archive_bytes": 1000})
            files = {f"scans/{doc}.{ext}": fingerprint(base / "scans" / (doc + "." + ext))
                     for ext in ("jpg", "xml")}
            files[f"texts/{doc}.json"] = fingerprint(base / "texts" / (doc + ".json"))
            for suffix in ("image-provenance.json", "loc-provenance.json"):
                files[f"texts/{doc}.{suffix}"] = fingerprint(qroot / "texts" / (doc + "." + suffix))
            member = {"source": original["source"], "sample": samples[doc], "files": files,
                      "quality_alto": shipment.quality_alto((base / "scans" / (doc + ".xml")).read_bytes())}
            members.append(member)
        parent = {"schema": 1, "campaign": "v1-wave5", "chunk": "c00001",
                  "checksums_sha256": "a" * 64, "members": members}
        value = shipment.manifest(parent, members, recipe)
        for member in members:
            submit.quality_source_binding(qroot, member, recipe, create=True)
        quality_jobs = [(quality_path, bundle)]
        submit.check_transaction_jobs(value, native, quality_jobs, recipe["native_engine"])

        tx = base / "qwen-shipments/v1-wave5" / value["id"]
        shipment.freeze(tx / "engine.json", recipe["native_engine"])
        receipt = {"schema": 1, "transaction_id": value["id"], "parent_id": value["parent"]["id"],
                   "status": "acknowledged", "native_acknowledged": True, "quality_acknowledged": True,
                   "members": {m["source"]["doc_id"]: {"native": "published", "quality": "published"} for m in members}}
        for kind, selected, app in (("native", native, "usnm_qwen"), ("quality", quality_jobs, "usnm_quality")):
            shipment.freeze(tx / (kind + "-intent.json"), {job["id"]: shipment.file_hash(path) for path, job in selected})
            rows = {}
            for number, (path, job) in enumerate(selected, 1):
                (path.parent / "submitted").write_text(app + "_" + job["id"])
                rows[job["id"]] = {"job_sha256": shipment.file_hash(path), "workunit_id": number,
                                   "batch": 8 if kind == "native" else 0, "min_quorum": 2, "target_nresults": 2}
                if kind == "native":
                    rows[job["id"]]["source_bindings"] = submit.source_bindings(native_root, job)
            receipt[kind + "_workunits"] = rows
        shipment.save(native_root / "audit.json", receipt["native_workunits"])
        args = types.SimpleNamespace(share=base, campaign="v1-wave5")
        submit.verify_acknowledgement(args, tx, value, receipt)

        # serde defaults absent/empty page words; the worker inherits the bundle list.
        for words in ([], bundle["function_words"]):
            changed = json.loads(raw_bundle)
            changed["pages"][0]["function_words"] = words
            submit.check_transaction_jobs(value, native, [(quality_path, changed)], recipe["native_engine"])
        for field in ("function_words", "crop_seed", "k_crops", "bundle_words", "null_words"):
            changed = json.loads(raw_bundle)
            if field == "bundle_words":
                changed["function_words"] = ["different"]
            elif field in ("function_words", "null_words"):
                changed["pages"][0]["function_words"] = ["different"] if field == "function_words" else None
            else:
                changed["pages"][0][field] += 1
            quality_path.write_text(json.dumps(changed))
            with self.subTest(field=field), self.assertRaisesRegex(RuntimeError, "quality job staging parameters"):
                submit.verify_acknowledgement(args, tx, value, receipt)
        quality_path.write_bytes(raw_bundle)
        submit.verify_acknowledgement(args, tx, value, receipt)


class SubsetShipmentTest(unittest.TestCase):
    """Offline process-boundary rehearsal; never a live BOINC/SSH proof."""

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.base = Path(self.folder.name)
        self.outbox = self.base / "outbox"
        self.chunk = self.outbox / "test/c00001"
        self.chunk.parent.mkdir(parents=True)
        pages = [{**frozen_page(f"sn1_1900-01-02_ed-1_seq-{i}"), "date": "1900-01-02", "lang": "srp"}
                 for i in range(1, 97)]
        cache = self.base / "archive-cache"
        cache.mkdir()
        for page in pages:
            doc = page["doc_id"]
            (cache / (doc + ".provenance.json")).write_text(json.dumps({
                "doc_id": doc, "sha256": hashlib.sha256(b"alto").hexdigest(),
                "archive": "xx_test_ver01.tar.bz2", "member": doc + "/ocr.xml",
                "archive_bytes": 1000}, indent=2) + "\n")
        quality.write_chunk(str(self.chunk.parent), self.chunk.name, pages,
                            {p["doc_id"]: (b"jpg", b"alto", {}, None) for p in pages}, str(cache))
        self.share = self.base / "server"
        (self.share / "quality/v1-wave9").mkdir(parents=True)
        (self.share / "quality/v1-wave9/places.txt").write_text("London\n")
        self.plan = self.base / "plan.json"
        self.plan.write_text(json.dumps({"engine": dict(cohort.ENGINE, models=cohort.MODELS)}))
        self.binary = self.base / "usnm-boinc-stage"
        self.binary.write_text("offline stage fixture")
        self.state = {"fresh_pairs": 446, "unresolved_native": 1946, "free_bytes": 100 * 1024**3,
                      "app_versions": [{"plan_class": "test_8b", "platformid": 1, "version_num": 117}],
                      "min_version": 117}
        self.args = types.SimpleNamespace(
            campaign="test", outbox=self.outbox, server_host="offline", server_ct=230,
            qwen_root=None, prompt_version=3, qwen_yiddish_root=None, yiddish_prompt_version=4,
            engine_image="sha256:" + "a" * 64, readers="loc-hist,as", bundle_size=200,
            max_chunks=1, min_free_gb=50, qwen_max_ready=1000, max_unresolved=2000,
            qwen_engine_plan="/project/plan.json", qwen_models="/project/models",
            step_window_minutes=0, what_if=False)
        self.db = {"usnm_qwen": {}, "usnm_quality": {}}
        self.commands = []
        self.transfers = []
        self.native_crash = False
        self.quality_failure = False
        self.lose_response = False
        self.outcomes = {}
        self.server_race = False
        self.locked_pool_free = 100 * 1024**3
        self.real_publication_rows = submit.publication_rows
        self.real_subprocess_run = subprocess.run
        for owner, name, replacement in (
                (driver, "remote", self.remote), (driver, "transfer_subset", self.transfer),
                (submit, "publication_rows", self.rows),
                (submit.subprocess, "run", self.run_command),
                (submit, "inventory", lambda *_: {**self.state, **(
                    {"fresh_pairs": 500, "unresolved_native": 2000} if self.server_race else {})})):
            patch = mock.patch.object(owner, name, side_effect=replacement)
            patch.start()
            self.addCleanup(patch.stop)
        patch = mock.patch.object(driver.shutil, "disk_usage", return_value=types.SimpleNamespace(free=100 * 1024**3))
        patch.start()
        self.addCleanup(patch.stop)

    def transfer(self, args, payload, identity):
        value = shipment.validate(json.loads((payload / "manifest.json").read_text()))
        shipment.verify_payload(payload, value)
        self.transfers.append(value)
        token = "upload-" + "a" * 32
        target = self.share / "qwen-shipments/test" / identity / token
        target.parent.mkdir(parents=True, exist_ok=True)
        driver.shutil.copytree(payload, target)
        return token

    def server_args(self, identity=None):
        return types.SimpleNamespace(
            campaign="test", share=self.share, project=self.share / "usnmocr", transaction=identity,
            upload="upload-" + "a" * 32, prompt_version=3, max_pages=100,
            stage_binary=str(self.binary), engine_plan=self.plan, models=self.base / "models",
            max_ready=1000, max_unresolved=2000, reserve_mib=64, min_free_gb=50)

    def remote(self, host, args, data=None):
        if "--recipe-inputs" in args:
            return json.dumps(submit.recipe_inputs(self.server_args())).encode()
        if "--inventory" in args:
            return json.dumps(self.state).encode()
        if "lvs" in args:
            return json.dumps({"report": [{"lv": [{"lv_size": str(100 * 1024**3), "data_percent": "0"}]}]}).encode()
        if args[:2] == ["python3", "-c"]:
            ct, wait, identity, pages, floor, command = json.loads(args[-1])
            self.assertEqual(ct, self.args.server_ct)
            self.assertIn("/run/usnm-boinc-step.lock", args[2])
            if self.locked_pool_free - pages * 64 * 1024**2 < floor * 1024**3:
                gate = {"shipment_gate": {"transaction_id": identity, "reserve_pages": pages,
                                          "phase": "locked_thin_pool", "free_bytes": self.locked_pool_free}}
                raise subprocess.CalledProcessError(75, args, output=json.dumps(gate).encode())
            args = command
        else:
            self.assertIn("/run/usnm-boinc-step.lock", args)
        if "--retire-parent" in args:
            a = self.server_args()
            a.retire_parent = args[args.index("--retire-parent") + 1]
            with contextlib.redirect_stdout(io.StringIO()) as output:
                submit.retire_parent(a)
            return output.getvalue().encode()
        identity = args[args.index("--transaction") + 1]
        with contextlib.redirect_stdout(io.StringIO()) as output:
            try:
                submit.run_transaction(self.server_args(identity))
            except (subprocess.CalledProcessError, SystemExit) as error:
                code = error.returncode if isinstance(error, subprocess.CalledProcessError) else error.code
                raise subprocess.CalledProcessError(code, args, output=output.getvalue().encode()) from error
        if self.lose_response:
            self.lose_response = False
            raise subprocess.TimeoutExpired(args, 7200)
        return output.getvalue().encode()

    def rows(self, project, selected, app):
        return {job["id"]: self.db[app][job["id"]] for _, job in selected if job["id"] in self.db[app]}

    def run_command(self, command, **kwargs):
        self.commands.append(command)
        app, operation = command[1:3]
        if operation == "function-words":
            return
        root = Path(command[command.index("--root") + 1])
        if operation == "stage" and app == "quality":
            sample = Path(command[command.index("--sample") + 1])
            for row in map(json.loads, sample.read_text().splitlines()):
                doc = row["doc_id"]
                if any((root / layout / doc / "quality.json").exists() for layout in ("pages", "units")):
                    continue
                prepared = shipment.encoded({
                    "export": json.loads((sample.parent / f"texts/{doc}.json").read_text()),
                    "readers": command[command.index("--readers") + 1]})
                job = {"doc_id": doc, "id": shipment.digest(row), "language": row["lang"], "group": "test",
                       "engine": {"image": self.args.engine_image},
                       "function_words": ["the"], "crop_seed": 522, "k_crops": 12,
                       "readers": [{"name": "loc", "source": "loc-alto"}], "inputs": {
                    "page": {"bytes": 3, "sha256": hashlib.sha256(b"jpg").hexdigest()},
                    "alto": shipment.quality_alto((sample.parent / f"scans/{doc}.xml").read_bytes()),
                    "places": {"bytes": (root / "places.txt").stat().st_size,
                               "sha256": shipment.file_hash(root / "places.txt")},
                    "texts": {"bytes": len(prepared), "sha256": hashlib.sha256(prepared).hexdigest()}}}
                job["id"] = shipment.digest(job)
                for layout in ("pages", "units"):
                    shipment.save(root / layout / doc / "quality.json", job)
                    (root / layout / doc / "texts.json").write_bytes(prepared)
                    (root / layout / doc / "page.jpg").write_bytes(b"jpg")
                    shipment.save(root / layout / doc / "sample.json", row)
            return
        members = json.loads(Path(command[command.index("--members") + 1]).read_text())
        if operation == "stage":
            qroot = Path(command[command.index("--quality-root") + 1])
            for doc in members:
                unit = root / "units" / doc
                if (unit / "qwen.json").exists() or any((unit / marker).exists() for marker in ("no_crops", "too_large")):
                    continue
                raw = (qroot / "pages" / doc / "quality.json").read_bytes()
                job = json.loads(raw)
                shipment.mkdir(unit)
                (unit / "quality.json").write_bytes(raw)
                binding = json.loads((qroot / "texts" / (doc + ".image-provenance.json")).read_text())
                shipment.save(unit / "source-provenance.json", {
                    "schema": 1, "quality_sha256": hashlib.sha256(raw).hexdigest(), "binding": binding})
                native = {"doc_id": doc, "id": shipment.digest({"qwen": job}), "input": job["inputs"]["page"],
                          "v": 1, "prompt_version": 3, "language": "srp", "group": "test",
                          "gpu_timeout_secs": 14400, "cpu_timeout_secs": 7200,
                          "max_tokens_per_1000px2": 4, "crops": [{"id": "c01", "loc_text": "text"}],
                          "engine": json.loads(self.plan.read_text())["engine"]}
                for name in ("page.jpg", "sample.json"):
                    shutil.copyfile(qroot / "pages" / doc / name, unit / name)
                outcome = self.outcomes.get(doc)
                if outcome:
                    (unit / outcome).write_text("bound terminal evidence")
                    if outcome == "too_large":
                        shipment.save(unit / "rejected-qwen.json", native)
                else:
                    shipment.save(unit / "qwen.json", native)
            return
        self.assertEqual(operation, "submit")
        if app == "quality" and self.quality_failure:
            raise subprocess.CalledProcessError(75, command)
        for path, job in submit.selected_jobs(root, app + ".json", members):
            if (path.parent / "submitted").exists():
                continue
            name = "usnm_" + app + "_" + job["id"]
            (path.parent / "submitting").write_text(name)
            self.db["usnm_" + app][job["id"]] = {
                "job_sha256": shipment.file_hash(path), "workunit_id": len(self.db["usnm_" + app]) + 1,
                "batch": 8 if app == "qwen" else 0, "min_quorum": 2, "target_nresults": 2}
            if app == "qwen":
                download = self.server_args().project / "download"
                shipment.mkdir(download)
                shutil.copyfile(path, download / (job["id"] + "_qwen_json"))
                shutil.copyfile(path.parent / "page.jpg", download / (job["id"] + "_page_jpg"))
            if app == "qwen" and self.native_crash:
                self.native_crash = False
                raise subprocess.CalledProcessError(1, command)
            (path.parent / "submitting").replace(path.parent / "submitted")

    def ship(self):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            code = driver.ship_subsets(self.args)
        return code, output.getvalue()

    def receipts(self):
        return [json.loads(path.read_text()) for path in
                (self.outbox / ".qwen-transactions/test/c00001").glob("*/receipt.json")]

    def test_exact_54_of_96_transfer_mixed_outcomes_and_parent_retirement(self):
        self.outcomes = {DOC: "no_crops", "sn1_1900-01-02_ed-1_seq-4": "too_large"}
        unrelated = self.share / "qwen/test/units/sn9_1900-01-02_ed-1_seq-1"
        shipment.save(unrelated / "qwen.json", {"doc_id": unrelated.name, "id": "d" * 64, "prompt_version": 3})
        code, output = self.ship()
        self.assertEqual(code, 0)
        self.assertEqual(len(self.transfers), 1)
        self.assertEqual(len(self.transfers[0]["members"]), 54)
        self.assertEqual([m["source"]["doc_id"] for m in self.transfers[0]["members"]],
                         [f"sn1_1900-01-02_ed-1_seq-{i}" for i in range(1, 55)])
        self.assertEqual(len(self.db["usnm_qwen"]), 52)
        self.assertFalse((unrelated / "submitted").exists())
        self.assertTrue((self.chunk / "READY").exists())
        receipt = self.receipts()[0]
        self.assertEqual(receipt["reserve_pages"], 54)
        self.assertEqual(receipt["members"][DOC]["native"], "no_crops")
        self.assertIn('"native_acknowledged": true', output)
        retire = self.server_args()
        retire.retire_parent = self.transfers[0]["parent"]["id"]
        with self.assertRaisesRegex(RuntimeError, "incomplete or overlapping"):
            submit.retire_parent(retire)
        self.assertTrue(list((self.share / "qwen-shipments/test").glob("*/input")))
        self.state.update(fresh_pairs=0, unresolved_native=0)
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual([len(v["members"]) for v in self.transfers], [54, 42])
        self.assertFalse(self.chunk.exists())
        self.assertEqual(len(self.db["usnm_qwen"]), 94)
        self.assertEqual(len(self.db["usnm_quality"]), 96)
        self.assertFalse(list((self.share / "qwen-shipments/test").glob("*/input")))

    def test_native_publication_crash_reconciles_then_quality_exit75_is_partial(self):
        self.native_crash = True
        self.assertEqual(self.ship()[0], 1)
        self.assertEqual(len(self.db["usnm_qwen"]), 1)
        self.assertTrue(self.chunk.exists())
        self.quality_failure = True
        code, output = self.ship()
        self.assertEqual(code, 75)
        self.assertIn('"status": "partial"', output)
        self.assertIn('"native_acknowledged": true', output)
        self.assertEqual(len(self.db["usnm_qwen"]), 54)
        self.assertEqual(len(self.transfers), 1)
        before = dict(self.db["usnm_qwen"])
        self.quality_failure = False
        # A published replay needs no new native headroom.
        self.server_race = True
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(self.db["usnm_qwen"], before)
        self.assertTrue(self.chunk.exists())
        self.assertFalse(list((self.share / "qwen/test/units").glob("*/submitting")))

    def test_locked_capacity_race_defers_without_publication_and_keeps_subset(self):
        self.server_race = True
        code, output = self.ship()
        self.assertEqual(code, 75)
        self.assertIn('"status": "deferred"', output)
        self.assertEqual(len(self.transfers[0]["members"]), 54)
        self.assertEqual(len(self.db["usnm_qwen"]), 0)
        self.assertEqual(len(list((self.share / "qwen/test/units").glob("*/qwen.json"))), 54)
        self.server_race = False
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(len(self.transfers), 1)

    def test_locked_thin_pool_race_preserves_reservation_and_uncertain_publication(self):
        self.locked_pool_free = 50 * 1024**3 + 54 * 64 * 1024**2 - 1
        code, output = self.ship()
        event = json.loads(output)["shipment_invocation"]
        self.assertEqual((code, event["phase"], event["status"]), (75, "locked_thin_pool", "deferred"))
        self.assertFalse(event["publication_attempted"])
        self.assertFalse(self.commands)
        self.assertEqual(len(self.transfers[0]["members"]), 54)
        self.locked_pool_free = 100 * 1024**3
        self.lose_response = True
        with self.assertRaises(subprocess.TimeoutExpired):
            self.ship()
        self.locked_pool_free = 50 * 1024**3
        code, output = self.ship()
        event = json.loads(output)["shipment_invocation"]
        self.assertEqual((code, event["status"]), (75, "uncertain"))
        self.assertTrue(event["publication_attempted"])
        self.assertEqual(len(self.transfers), 1)
        self.locked_pool_free = 100 * 1024**3
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(len(self.db["usnm_qwen"]), 54)

    def test_engine_binary_and_places_pins_are_frozen_before_any_locked_staging(self):
        with mock.patch.object(submit, "inventory", return_value={**self.state, "free_bytes": 50 * 1024**3}):
            self.assertEqual(self.ship()[0], 75)
        self.assertFalse(self.commands)
        value = self.transfers[0]
        tx = self.share / "qwen-shipments/test" / value["id"]
        self.assertFalse((tx / "engine.json").exists())
        original = self.plan.read_bytes()
        for field in ("image", "llama", "weights", "models"):
            engine = json.loads(original)
            engine["engine"][field] = ([{**engine["engine"]["models"][0], "min_gpu_mb": 1}]
                                        if field == "models" else "sha256:" + "f" * 64)
            self.plan.write_text(json.dumps(engine))
            with self.subTest(field=field), contextlib.redirect_stdout(io.StringIO()), \
                    self.assertRaisesRegex(RuntimeError, "changed frozen staging recipe"):
                submit.run_transaction(self.server_args(value["id"]))
            self.assertFalse(self.commands)
        self.plan.write_bytes(original)
        for path in (self.binary, self.share / "quality/v1-wave9/places.txt"):
            raw = path.read_bytes()
            path.write_bytes(raw + b"changed")
            with self.subTest(path=path.name), contextlib.redirect_stdout(io.StringIO()), \
                    self.assertRaisesRegex(RuntimeError, "changed frozen staging recipe"):
                submit.run_transaction(self.server_args(value["id"]))
            path.write_bytes(raw)
        self.assertEqual(self.ship()[0], 0)
        # A later subset of this same parent must not acquire newly installed pins.
        self.plan.write_text(json.dumps({"engine": {**json.loads(original)["engine"], "llama": "sha256:" + "e" * 64}}))
        with self.assertRaisesRegex(ValueError, "changed frozen shipment evidence"):
            self.ship()
        self.assertEqual(len(self.transfers), 1)
        self.assertEqual(len(self.db["usnm_qwen"]), 54)

    def test_places_copy_crash_never_promotes_partial_bytes_or_changes_retry_recipe(self):
        places = self.share / "quality/test/places.txt"
        source = self.share / "quality/v1-wave9/places.txt"
        original = shutil.copyfile

        def partial_copy(src, dst, *args, **kwargs):
            if Path(dst) == places.with_name("places.txt.pending"):
                Path(dst).write_bytes(b"Lon")
                raise OSError("interrupted places copy")
            return original(src, dst, *args, **kwargs)

        with mock.patch.object(shutil, "copyfile", side_effect=partial_copy), \
                self.assertRaisesRegex(OSError, "interrupted places copy"):
            self.ship()
        self.assertFalse(places.exists())
        self.assertFalse(self.commands)
        source.write_text("changed vocabulary\n")
        with self.assertRaisesRegex(ValueError, "changed frozen shipment evidence"):
            self.ship()
        source.write_text("London\n")
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(places.read_bytes(), source.read_bytes())
        self.assertFalse(places.with_name("places.txt.pending").exists())
        self.assertEqual(len(self.transfers), 1)

    def marker_sync_crash(self, app):
        original = shipment.sync_dir
        failed = []

        def crash(path):
            if (not failed and path.parent == self.share / app / "test/units"
                    and (path / "submitted").exists()):
                failed.append(path)
                raise OSError("interrupted marker directory sync")
            original(path)

        with mock.patch("qwen_shipment.sync_dir", side_effect=crash), \
                self.assertRaisesRegex(OSError, "interrupted marker directory sync"):
            self.ship()
        tx = self.share / "qwen-shipments/test" / self.transfers[0]["id"]
        receipt = json.loads((tx / "receipt.json").read_text())
        self.assertFalse(receipt["quality_acknowledged" if app == "quality" else "native_acknowledged"])
        before = dict(self.db["usnm_" + app])
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(self.db["usnm_" + app], before)
        self.assertTrue(self.receipts()[0]["quality_acknowledged"])

    def test_native_existing_submitted_marker_sync_failure_cannot_acknowledge(self):
        self.marker_sync_crash("qwen")

    def test_quality_existing_submitted_marker_sync_failure_cannot_acknowledge(self):
        self.marker_sync_crash("quality")

    def test_positive_reconciliation_durably_finishes_duplicate_matching_markers(self):
        self.native_crash = True
        self.assertEqual(self.ship()[0], 1)
        marker = next((self.share / "qwen/test/units").glob("*/submitting"))
        marker.with_name("submitted").write_bytes(marker.read_bytes())
        self.assertEqual(self.ship()[0], 0)
        self.assertFalse(marker.exists())
        self.assertEqual(len(self.db["usnm_qwen"]), 54)

    def test_acknowledgement_uses_one_campaign_audit_snapshot(self):
        self.assertEqual(self.ship()[0], 0)
        audit = self.share / "qwen/test/audit.json"
        original = Path.read_text
        reads = []

        def observe(path, *args, **kwargs):
            if path == audit:
                reads.append(path)
            return original(path, *args, **kwargs)

        value = self.transfers[0]
        tx = self.share / "qwen-shipments/test" / value["id"]
        with mock.patch.object(Path, "read_text", observe):
            submit.verify_acknowledgement(self.server_args(), tx, value, self.receipts()[0])
        self.assertEqual(reads, [audit])

    def test_retained_jobs_cannot_bypass_the_complete_frozen_recipe(self):
        self.assertEqual(self.ship()[0], 0)
        value = self.transfers[0]
        docs = [member["source"]["doc_id"] for member in value["members"]]
        native = submit.selected_jobs(self.share / "qwen/test", "qwen.json", docs)
        quality_jobs = submit.selected_jobs(self.share / "quality/test", "quality.json", docs)
        engine = value["recipe"]["native_engine"]
        for field in ("group", "gpu_timeout_secs", "cpu_timeout_secs", "max_tokens_per_1000px2", "engine"):
            path, job = native[0]
            changed = json.loads(json.dumps(job))
            if field == "engine":
                changed[field]["models"][0]["min_gpu_mb"] += 1
            else:
                changed[field] = "changed" if field == "group" else 1
            with self.subTest(field=field), self.assertRaisesRegex(RuntimeError, "native job"):
                submit.check_transaction_jobs(value, [(path, changed)], quality_jobs, engine)
        for field in ("places", "function_words", "crop_seed", "k_crops"):
            path, job = quality_jobs[0]
            changed = json.loads(json.dumps(job))
            if field == "places":
                changed["inputs"]["places"]["sha256"] = "e" * 64
            else:
                changed[field] = [] if field == "function_words" else 1
            with self.subTest(field=field), self.assertRaisesRegex(RuntimeError, "quality job"):
                submit.check_transaction_jobs(value, native, [(path, changed)], engine)

    def test_lost_success_response_replays_exact_identity_without_retransfer(self):
        self.lose_response = True
        with self.assertRaises(subprocess.TimeoutExpired):
            self.ship()
        before = dict(self.db["usnm_qwen"])
        self.assertTrue(self.chunk.exists())
        # A durable complete acknowledgement remains valid after DB history is purged.
        self.db = {"usnm_qwen": {}, "usnm_quality": {}}
        self.assertEqual(self.ship()[0], 0)
        self.assertTrue(before)
        self.assertEqual(self.db["usnm_qwen"], {})
        self.assertEqual(len(self.transfers), 1)

    def test_deferrals_preserve_newer_uncertain_invocation_until_server_reconciliation(self):
        self.server_race = True
        self.assertEqual(self.ship()[0], 75)
        self.assertFalse(self.receipts()[0]["publication_attempted"])
        self.server_race = False
        self.lose_response = True
        with self.assertRaises(subprocess.TimeoutExpired):
            self.ship()
        tx = self.outbox / ".qwen-transactions/test/c00001" / self.transfers[0]["id"]
        before = dict(self.db["usnm_qwen"])
        self.args.step_window_minutes = 5
        outside = types.SimpleNamespace(tm_min=30, tm_sec=0)

        def no_publication(host, args, data=None):
            self.assertFalse(args[:2] == ["python3", "-c"])
            return self.remote(host, args, data)

        with mock.patch.object(driver.time, "localtime", return_value=outside), \
                mock.patch.object(driver, "remote", side_effect=no_publication):
            for phase in ("step_window", "step_window", "preflight"):
                if phase == "preflight":
                    self.state["free_bytes"] = 50 * 1024**3
                code, output = self.ship()
                event = json.loads(output)
                event = event.get("shipment_invocation", event)
                self.assertEqual((code, event["phase"], event["status"]), (75, phase, "uncertain"))
                self.assertTrue(event["publication_attempted"])
                self.assertTrue(json.loads((tx / "invocation.json").read_text())["publication_attempted"])
        self.assertFalse(self.receipts()[0]["publication_attempted"])
        self.assertEqual(self.db["usnm_qwen"], before)
        self.assertEqual(len(self.transfers), 1)
        self.state["free_bytes"] = 100 * 1024**3
        self.args.step_window_minutes = 0
        self.assertEqual(self.ship()[0], 0)
        self.assertTrue(self.receipts()[0]["native_acknowledged"])
        self.assertEqual(self.db["usnm_qwen"], before)

    def test_step_window_and_storage_preserve_positive_evidence_in_either_journal(self):
        self.quality_failure = True
        self.assertEqual(self.ship()[0], 75)
        tx = self.outbox / ".qwen-transactions/test/c00001" / self.transfers[0]["id"]
        partial = self.receipts()[0]
        zero = {**partial, "status": "deferred", "publication_attempted": False, "native_acknowledged": False}
        self.args.step_window_minutes = 5
        with mock.patch.object(driver.time, "localtime", return_value=types.SimpleNamespace(tm_min=30)):
            for positive in ("receipt.json", "invocation.json"):
                for phase in ("step_window", "preflight"):
                    shipment.save(tx / "receipt.json", partial if positive == "receipt.json" else zero)
                    shipment.save(tx / "invocation.json", partial if positive == "invocation.json" else zero)
                    self.state["free_bytes"] = (50 if phase == "preflight" else 100) * 1024**3
                    code, output = self.ship()
                    event = json.loads(output)
                    event = event.get("shipment_invocation", event)
                    self.assertEqual((code, event["phase"], event["status"]), (75, phase, "partial"))
                    self.assertTrue(event["native_acknowledged"])
                    self.assertTrue(event["publication_attempted"])

    def test_server_storage_receipt_cannot_clear_an_unreconciled_local_attempt(self):
        self.server_race = True
        self.assertEqual(self.ship()[0], 75)
        self.server_race = False

        def lost_response(host, args, data=None):
            if args[:2] == ["python3", "-c"]:
                raise subprocess.TimeoutExpired(args, 7200)
            return self.remote(host, args, data)

        with mock.patch.object(driver, "remote", side_effect=lost_response), \
                self.assertRaises(subprocess.TimeoutExpired):
            self.ship()
        with mock.patch.object(submit, "inventory", return_value={**self.state, "free_bytes": 50 * 1024**3}):
            code, output = self.ship()
        event = json.loads(output)["shipment_invocation"]
        self.assertEqual((code, event["phase"], event["status"]), (75, "locked_storage", "uncertain"))
        self.assertTrue(event["publication_attempted"])
        self.assertFalse(self.receipts()[0]["publication_attempted"])
        self.assertEqual(self.ship()[0], 0)
        self.assertTrue(self.receipts()[0]["native_acknowledged"])

    def test_first_out_of_window_deferral_does_not_invent_publication(self):
        self.args.step_window_minutes = 5
        with mock.patch.object(driver.time, "localtime", return_value=types.SimpleNamespace(tm_min=30)):
            code, output = self.ship()
        event = json.loads(output)["shipment_invocation"]
        self.assertEqual((code, event["status"], event["publication_attempted"]), (75, "deferred", False))
        self.assertFalse(self.commands)
        self.assertFalse(self.db["usnm_qwen"])

    def test_mutex_and_locked_storage_deferrals_do_not_claim_zero_publication(self):
        with mock.patch.object(driver.fcntl, "flock", side_effect=BlockingIOError):
            code, output = self.ship()
        self.assertEqual((code, json.loads(output)["status"]), (75, "uncertain"))
        self.assertIsNone(json.loads(output)["publication_attempted"])
        self.native_crash = True
        self.assertEqual(self.ship()[0], 1)
        with mock.patch.object(submit, "inventory", return_value={**self.state, "free_bytes": 50 * 1024**3}):
            code, output = self.ship()
        event = json.loads(output)["shipment_invocation"]
        self.assertEqual((code, event["phase"], event["status"]), (75, "locked_storage", "uncertain"))
        self.assertTrue(event["publication_attempted"])
        self.assertEqual(len(self.db["usnm_qwen"]), 1)

    def test_subset_rejects_mixed_request_versions_across_unselected_root_before_staging(self):
        for layout, version, submitted in (("units", 2, False), ("units", 4, True), ("bundles", 4, False)):
            with self.subTest(layout=layout, version=version, submitted=submitted):
                unit = self.share / "qwen/test" / layout / "unrelated"
                shipment.save(unit / "qwen.json",
                              {"id": "e" * 64, "doc_id": "unrelated", "prompt_version": version})
                if submitted:
                    (unit / "submitted").write_text("usnm_qwen_" + "e" * 64)
                with self.assertRaisesRegex(RuntimeError, f"holds request version {version}, not 3"):
                    self.ship()
                self.assertFalse(self.commands)
                self.assertFalse(self.db["usnm_qwen"])
                self.assertTrue((self.chunk / "READY").is_file())
                shutil.rmtree(unit)
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(len(self.transfers), 1)

    def test_retirement_rejects_changed_acknowledged_jobs_and_retains_all_sources(self):
        self.assertEqual(self.ship()[0], 0)
        path = next((self.share / "qwen/test/units").glob("*/qwen.json"))
        job = json.loads(path.read_text())
        job["prompt_version"] = 4
        path.write_text(json.dumps(job))
        value = self.transfers[0]
        receipt = self.receipts()[0]
        with self.assertRaisesRegex(RuntimeError, "request version"):
            submit.verify_acknowledgement(self.server_args(),
                                          self.share / "qwen-shipments/test" / value["id"], value, receipt)
        self.assertTrue((self.chunk / "READY").exists())

    def test_changed_frozen_membership_and_unbound_legacy_receipts_are_rejected(self):
        self.assertEqual(self.ship()[0], 0)
        frozen = next((self.outbox / ".qwen-transactions/test/c00001").glob("*/manifest.json"))
        value = json.loads(frozen.read_text())
        value["members"][0]["source"]["batch_version"] = 2
        frozen.write_text(json.dumps(value))
        with self.assertRaisesRegex(ValueError, "changed frozen"):
            self.ship()
        value = self.transfers[0]
        self.assertFalse(driver.acknowledged(value, {"status": "acknowledged",
                                                     "native_acknowledged": True,
                                                     "quality_acknowledged": True}))

    def test_uncertain_missing_database_row_is_never_cleared_or_replayed(self):
        self.native_crash = True
        self.assertEqual(self.ship()[0], 1)
        self.db["usnm_qwen"].clear()
        with self.assertRaisesRegex(RuntimeError, "history unavailable/uncertain"):
            self.ship()
        self.assertTrue(list((self.share / "qwen/test/units").glob("*/submitting")))
        self.assertTrue((self.chunk / "READY").exists())

    def test_reservation_and_transfer_membership_reject_boundaries(self):
        self.assertEqual(shipment.capacity(self.state, 100 * 1024**3), 54)
        self.assertEqual(shipment.capacity(self.state, 50 * 1024**3 + 53 * 64 * 1024**2), 53)
        self.assertEqual(shipment.capacity(self.state, 50 * 1024**3 - 1), 0)
        parent = shipment.chunk_snapshot(self.chunk, "test")
        recipe = {"campaign": "test", "prompt_version": 3, "engine_image": self.args.engine_image,
                  "readers": self.args.readers, "bundle_size": 200, **submit.recipe_inputs(self.server_args())}
        value = shipment.manifest(parent, parent["members"][:2], recipe)
        payload = self.base / "payload"
        shipment.materialize(self.chunk, payload, value)
        (payload / "unrelated.json").write_text("{}")
        with self.assertRaisesRegex(ValueError, "outside frozen subset"):
            shipment.verify_payload(payload, value)
        with self.assertRaisesRegex(ValueError, "order/identity"):
            shipment.manifest(parent, list(reversed(parent["members"][:2])), recipe)
        (self.chunk / f"texts/{DOC}.image-provenance.json").unlink()
        with self.assertRaises(FileNotFoundError):
            shipment.chunk_snapshot(self.chunk, "test")

    def test_normalized_alto_cannot_publish_with_incompatible_downstream_archive_binding(self):
        raw = b'<alto xmlns="http://schema.ccs-gmbh.com/ALTO"></alto>'
        path = self.chunk / f"scans/{DOC}.xml"
        path.write_bytes(raw)
        sums = self.chunk / "SHA256SUMS"
        provenance = self.chunk / f"texts/{DOC}.loc-provenance.json"
        shipment.save(provenance, {**json.loads(provenance.read_text()), "sha256": hashlib.sha256(raw).hexdigest()})
        sums.write_text("".join(shipment.file_hash(self.chunk / line.split("  ", 1)[1]) + "  "
                               + line.split("  ", 1)[1] + "\n" for line in sums.read_text().splitlines()))
        with self.assertRaisesRegex(RuntimeError, "archive provenance does not bind prepared ALTO"):
            self.ship()
        member = next(m for m in self.transfers[0]["members"] if m["source"]["doc_id"] == DOC)
        self.assertNotEqual(member["quality_alto"], member["files"][f"scans/{DOC}.xml"])
        self.assertEqual(path.read_bytes(), raw)
        self.assertFalse(self.commands)
        self.assertFalse(self.db["usnm_qwen"])
        self.assertTrue((self.chunk / "READY").is_file())

    def test_partial_local_materialization_recovers_without_changing_parent(self):
        parent = shipment.chunk_snapshot(self.chunk, "test")
        value = shipment.manifest(parent, parent["members"][:1], {
            "campaign": "test", "prompt_version": 3, "engine_image": self.args.engine_image,
            "readers": self.args.readers, "bundle_size": 200, **submit.recipe_inputs(self.server_args())})
        payload = self.base / "partial"
        original_copy = shipment.shutil.copyfile

        def partial_copy(src, dst):
            Path(dst).write_bytes(b"partial")
            raise OSError("crash")

        with mock.patch.object(shipment.shutil, "copyfile", side_effect=partial_copy), self.assertRaises(OSError):
            shipment.materialize(self.chunk, payload, value)
        with mock.patch.object(shipment.shutil, "copyfile", side_effect=original_copy):
            shipment.materialize(self.chunk, payload, value)
        shipment.verify_payload(payload, value)
        self.assertEqual(shipment.chunk_snapshot(self.chunk, "test"), parent)

    def crash_parent_cleanup(self, remove_ready=False):
        self.state.update(fresh_pairs=0, unresolved_native=0)
        original = shutil.rmtree

        def partial_cleanup(path, *args, **kwargs):
            if Path(path).name == "retired-parent":
                next((path / "scans").glob("*.jpg")).unlink()
                (path / ".qwen-shipment-owner.json").unlink()
                if remove_ready:
                    (path / "READY").unlink()
                raise OSError("interrupted parent cleanup")
            return original(path, *args, **kwargs)

        with mock.patch.object(driver.shutil, "rmtree", side_effect=partial_cleanup):
            with self.assertRaisesRegex(OSError, "interrupted parent cleanup"):
                self.ship()
        return self.outbox / ".qwen-transactions/test/c00001"

    def test_parent_cleanup_resumes_before_snapshot_with_ready_retained(self):
        journal = self.crash_parent_cleanup()
        self.assertFalse(self.chunk.exists())
        self.assertTrue((journal / "retired-parent/READY").exists())
        before = dict(self.db["usnm_qwen"])
        with mock.patch.object(driver, "remote", side_effect=AssertionError("must not republish")):
            self.assertEqual(self.ship()[0], 0)
        self.assertFalse(self.chunk.exists())
        self.assertTrue((journal / "retired.json").exists())
        self.assertTrue((journal / "cleaned.json").exists())
        self.assertFalse((journal / "retired-parent").exists())
        self.assertEqual(self.db["usnm_qwen"], before)

    def test_parent_cleanup_resumes_even_if_ready_was_already_removed(self):
        journal = self.crash_parent_cleanup(remove_ready=True)
        self.assertFalse((journal / "retired-parent/READY").exists())
        with mock.patch.object(driver, "remote", side_effect=AssertionError("must not republish")):
            self.assertEqual(self.ship()[0], 0)
        self.assertFalse(self.chunk.exists())

    def replacement_parent(self):
        # A re-created directory with identical source bytes is still a new incarnation.
        original = self.base / "replacement-source"
        if not original.exists():
            shutil.copytree(self.chunk, original, ignore=shutil.ignore_patterns(".qwen-shipment-owner.json"))
        if self.chunk.exists():
            shutil.rmtree(self.chunk)
        shutil.copytree(original, self.chunk)
        return shipment.chunk_snapshot(self.chunk, "test")

    def test_retirement_never_deletes_replacement_after_completed_cleanup(self):
        self.replacement_parent()
        self.state.update(fresh_pairs=0, unresolved_native=0)
        self.assertEqual(self.ship()[0], 0)
        expected = self.replacement_parent()
        with self.assertRaisesRegex(ValueError, "retired chunk name reused"):
            self.ship()
        self.assertEqual(shipment.chunk_snapshot(self.chunk, "test"), expected)

    def test_retirement_recovers_partial_tombstone_without_deleting_replacement(self):
        self.replacement_parent()
        journal = self.crash_parent_cleanup(remove_ready=True)
        expected = self.replacement_parent()
        with self.assertRaisesRegex(ValueError, "retired chunk name reused"):
            self.ship()
        self.assertEqual(shipment.chunk_snapshot(self.chunk, "test"), expected)
        self.assertFalse((journal / "retired-parent").exists())
        self.assertTrue((journal / "cleaned.json").exists())

    def test_retirement_crash_before_move_cannot_claim_replacement_incarnation(self):
        self.replacement_parent()
        self.state.update(fresh_pairs=0, unresolved_native=0)
        original = Path.rename

        def fail_move(path, destination):
            if path == self.chunk:
                raise OSError("crash before rename")
            return original(path, destination)

        with mock.patch.object(Path, "rename", fail_move), self.assertRaisesRegex(OSError, "before rename"):
            self.ship()
        expected = self.replacement_parent()
        with self.assertRaisesRegex(ValueError, "parent directory incarnation changed"):
            self.ship()
        self.assertEqual(shipment.chunk_snapshot(self.chunk, "test"), expected)

    def test_unbound_legacy_retirement_never_claims_or_deletes_a_parent(self):
        journal = self.crash_parent_cleanup()
        (journal / "owner.json").unlink()
        with self.assertRaisesRegex(ValueError, "unbound parent incarnation"):
            self.ship()
        self.assertTrue((journal / "retired-parent").exists())

    def test_retirement_recovers_rename_and_completed_delete_crash_boundaries(self):
        self.state.update(fresh_pairs=0, unresolved_native=0)
        journal = self.outbox / ".qwen-transactions/test/c00001"
        original_sync = shipment.sync_dir

        def fail_after_move(path):
            if path == self.chunk.parent and (journal / "retired-parent").exists():
                raise OSError("crash after rename")
            original_sync(path)

        # Both modules import the canonical shipment helper internally.
        with mock.patch("qwen_shipment.sync_dir", side_effect=fail_after_move), \
                self.assertRaisesRegex(OSError, "after rename"):
            self.ship()
        self.assertFalse(self.chunk.exists())
        original_freeze = shipment.freeze

        def fail_after_delete(path, value):
            if path == journal / "cleaned.json":
                raise OSError("crash after deletion")
            original_freeze(path, value)

        with mock.patch("qwen_shipment.freeze", side_effect=fail_after_delete), \
                self.assertRaisesRegex(OSError, "after deletion"):
            self.ship()
        self.assertFalse((journal / "retired-parent").exists())
        with mock.patch.object(driver, "remote", side_effect=AssertionError("must not republish")):
            self.assertEqual(self.ship()[0], 0)
        self.assertTrue((journal / "cleaned.json").exists())

    def test_parent_ownership_recovers_crash_between_marker_and_journal(self):
        original = shipment.freeze
        journal = self.outbox / ".qwen-transactions/test/c00001"

        def fail_owner(path, value):
            if path == journal / "owner.json":
                raise OSError("crash before owner journal")
            original(path, value)

        with mock.patch("qwen_shipment.freeze", side_effect=fail_owner), \
                self.assertRaisesRegex(OSError, "before owner journal"):
            self.ship()
        owner = json.loads((self.chunk / ".qwen-shipment-owner.json").read_text())
        self.assertFalse(self.transfers)
        self.assertEqual(self.ship()[0], 0)
        self.assertEqual(json.loads((journal / "owner.json").read_text()), owner)

    def test_retirement_record_cannot_bypass_incomplete_acknowledgements(self):
        self.assertEqual(self.ship()[0], 0)
        journal = self.outbox / ".qwen-transactions/test/c00001"
        shipment.save(journal / "retired.json", {"retired_parent": self.transfers[0]["parent"]["id"]})
        with self.assertRaisesRegex(ValueError, "incomplete parent retirement acknowledgement"):
            self.ship()
        self.assertTrue((self.chunk / "READY").exists())
        self.assertEqual(len(self.transfers), 1)

    @unittest.skipUnless(shutil.which("php"), "PHP CLI required for all-terminal native reconciliation")
    def test_all_terminal_subset_uses_real_php_empty_native_results(self):
        self.outcomes = {f"sn1_1900-01-02_ed-1_seq-{i}": "no_crops" for i in range(1, 55)}
        empty_boinc_php(self.server_args().project)

        def rows(project, selected, app):
            if app == "usnm_qwen":
                self.assertEqual(selected, [])
                with mock.patch.object(submit.subprocess, "run", self.real_subprocess_run):
                    return self.real_publication_rows(project, selected, app)
            return self.rows(project, selected, app)

        with mock.patch.object(submit, "publication_rows", side_effect=rows):
            self.assertEqual(self.ship()[0], 0)
        self.assertEqual(self.db["usnm_qwen"], {})
        self.assertEqual(len(self.db["usnm_quality"]), 54)
        self.assertTrue(self.receipts()[0]["native_acknowledged"])
        self.assertTrue((self.chunk / "READY").exists())

    def test_exact_archive_provenance_survives_retirement_and_actual_cohort_binding(self):
        expected = {path.name: path.read_bytes() for path in (self.chunk / "texts").glob("*.loc-provenance.json")}
        self.assertEqual(self.ship()[0], 0)
        self.state.update(fresh_pairs=0, unresolved_native=0)
        self.assertEqual(self.ship()[0], 0)
        self.assertFalse(self.chunk.exists())
        self.assertFalse(list((self.share / "qwen-shipments/test").glob("*/input")))
        project = self.server_args().project
        audit = json.loads((self.share / "qwen/test/audit.json").read_text())
        for path, job in submit.jobs(self.share / "qwen/test"):
            identity, doc = job["id"], job["doc_id"]
            names = (identity + "_qwen_json", identity + "_page_jpg")
            row = {"unit": {"id": audit[identity]["workunit_id"], "name": "usnm_qwen_" + identity,
                           "xml_doc": "".join("<file_info><name>" + name + "</name><url>"
                                             "https://example.test/usnmocr_download/" + name
                                             + "</url></file_info>" for name in names)}}
            bound = cohort.bind_inputs(project, row, identity, audit[identity], "test", 3)
            raw = expected[doc + ".loc-provenance.json"]
            self.assertEqual(bound["provenance_sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual((self.share / "quality/test/texts" / (doc + ".loc-provenance.json")).read_bytes(), raw)
        self.assertEqual(len(audit), 96)

    def test_missing_or_changed_archive_provenance_cannot_publish(self):
        self.server_race = True
        self.assertEqual(self.ship()[0], 75)
        path = self.share / "quality/test/texts" / (DOC + ".loc-provenance.json")
        original = path.read_bytes()
        path.write_bytes(original + b"\n")
        self.server_race = False
        with self.assertRaisesRegex(ValueError, "changed shipment input"):
            self.ship()
        self.assertEqual(path.read_bytes(), original + b"\n")
        self.assertFalse(self.db["usnm_qwen"])
        path.write_bytes(original)
        value = json.loads(json.dumps(self.transfers[0]))
        value["members"][0]["files"].pop(f"texts/{value['members'][0]['source']['doc_id']}.loc-provenance.json")
        tx = self.share / "qwen-shipments/test" / value["id"]
        with self.assertRaisesRegex(RuntimeError, "missing frozen archive provenance"):
            submit.transaction_quality(self.server_args(), value, tx / "input", self.share / "quality/test")

    def test_archive_provenance_copy_recovers_crash_before_publication(self):
        original = shutil.copyfile
        interrupted = []

        def partial_copy(src, dst, *args, **kwargs):
            if (Path(dst).parent == self.share / "quality/test/texts"
                    and Path(dst).name.endswith(".loc-provenance.json.pending")):
                Path(dst).write_bytes(b"partial")
                interrupted.append(Path(dst))
                raise OSError("interrupted provenance retention")
            return original(src, dst, *args, **kwargs)

        with mock.patch.object(shutil, "copyfile", side_effect=partial_copy), \
                self.assertRaisesRegex(OSError, "interrupted provenance retention"):
            self.ship()
        self.assertFalse(self.db["usnm_qwen"])
        self.assertFalse(self.commands)
        self.assertEqual(len(interrupted), 1)
        self.assertEqual(self.ship()[0], 0)
        self.assertFalse(interrupted[0].exists())
        self.assertEqual(len(self.transfers), 1)

    def test_preexisting_quality_job_without_reader_text_binding_is_not_adopted(self):
        parent = shipment.chunk_snapshot(self.chunk, "test")
        member = parent["members"][0]
        doc = member["source"]["doc_id"]
        qroot = self.share / "quality/test"
        # A legacy job can match source/JPEG/ALTO while reading older exported
        # text with another reader recipe; image provenance alone is insufficient.
        binding = json.loads((self.chunk / f"texts/{doc}.image-provenance.json").read_text())
        shipment.save(qroot / "texts" / (doc + ".image-provenance.json"), binding)
        old_text = b'{"readers":[{"name":"as","source":"american-stories","text":"old export"}]}'
        shipment.save(qroot / "units" / doc / "quality.json",
                      {"id": "b" * 64, "doc_id": doc, "language": "srp", "group": "test",
                       "engine": {"image": self.args.engine_image},
                       "readers": [{"name": "as", "source": "american-stories"}],
                       "inputs": {"page": member["files"][f"scans/{doc}.jpg"],
                                  "alto": member["quality_alto"],
                                  "texts": {"bytes": len(old_text), "sha256": hashlib.sha256(old_text).hexdigest()}}})
        (qroot / "units" / doc / "texts.json").write_bytes(old_text)
        shutil.copytree(qroot / "units" / doc, qroot / "pages" / doc)
        with self.assertRaisesRegex(RuntimeError, "unbound retained quality reader/text"):
            self.ship()
        self.assertFalse((qroot / "shipment-quality" / (doc + ".json")).exists())
        self.assertEqual(self.db["usnm_qwen"], {})
        self.assertTrue((self.chunk / "READY").exists())

    def test_retained_quality_binding_rejects_changed_requested_readers(self):
        self.server_race = True
        self.assertEqual(self.ship()[0], 75)
        value = self.transfers[0]
        value = json.loads(json.dumps(value))
        value["recipe"]["readers"] = "loc-hist,ours-tesseract"
        tx = self.share / "qwen-shipments/test" / self.transfers[0]["id"]
        with self.assertRaisesRegex(RuntimeError, "changed retained quality reader/text binding"):
            submit.transaction_quality(self.server_args(), value, tx / "input", self.share / "quality/test")
        self.assertEqual(self.db["usnm_qwen"], {})
        self.assertTrue((self.chunk / "READY").exists())

    def test_retained_quality_binding_rejects_changed_exported_text(self):
        self.server_race = True
        self.assertEqual(self.ship()[0], 75)
        value = json.loads(json.dumps(self.transfers[0]))
        member = value["members"][0]
        doc = member["source"]["doc_id"]
        tx = self.share / "qwen-shipments/test" / value["id"]
        path = tx / "input/texts" / (doc + ".json")
        path.write_text('{"readers":[{"name":"as","text":"changed exported text"}]}')
        member["files"][f"texts/{doc}.json"] = {"bytes": path.stat().st_size, "sha256": shipment.file_hash(path)}
        with self.assertRaisesRegex(RuntimeError, "changed retained quality reader/text binding"):
            submit.transaction_quality(self.server_args(), value, tx / "input", self.share / "quality/test")
        self.assertEqual(self.db["usnm_quality"], {})
        self.assertTrue((self.chunk / "READY").exists())

    def test_quality_bundle_reader_text_must_match_bound_source_job(self):
        self.assertEqual(self.ship()[0], 0)
        value = self.transfers[0]
        quality_jobs = submit.selected_jobs(self.share / "quality/test", "quality.json",
                                            [m["source"]["doc_id"] for m in value["members"]])
        path, job = quality_jobs[0]
        for field in ("readers", "texts"):
            changed = json.loads(json.dumps(job))
            if field == "readers":
                changed["readers"] = [{"name": "as", "source": "american-stories"}]
            else:
                changed["inputs"]["texts"]["sha256"] = "e" * 64
            bundle = {**job, "pages": [changed]}
            with self.subTest(field=field), self.assertRaisesRegex(RuntimeError, "reader/text inputs changed"):
                submit.check_transaction_jobs(value, [], [(path, bundle)], {})


class PrepareDiagnosticsTest(unittest.TestCase):
    def test_child_stdout_is_preserved_and_exceptions_propagate(self):
        for error in (subprocess.CalledProcessError(1, ["prepare"], output=b'{"doc_id":"p","error":"bad"}\n'),
                      subprocess.TimeoutExpired(["prepare"], 30, output=b"partial prepare output")):
            with self.subTest(error=type(error).__name__), \
                    mock.patch.object(driver, "command", side_effect=error), \
                    contextlib.redirect_stderr(io.StringIO()) as output:
                with self.assertRaises(type(error)) as raised:
                    driver.prepare(["prepare"])
                self.assertIs(raised.exception, error)
                self.assertIn(error.output.decode(), output.getvalue())

    def test_success_forwards_diagnostics_and_counts_only_ready_pages(self):
        output = (b'{"msg":"page failed; will retry","doc_id":"p"}\n'
                  b'{"msg":"chunk ready","pages":2}\nignored line\n'
                  b'{"msg":"chunk ready","pages":1}\n')
        with mock.patch.object(driver, "command", return_value=output), \
                contextlib.redirect_stdout(io.StringIO()) as logged:
            self.assertEqual(driver.prepare(["prepare"]), 3)
        self.assertIn("page failed; will retry", logged.getvalue())
        with mock.patch.object(driver, "command", return_value=b'{"msg":"chunk ready","pages":true}\n'), \
                contextlib.redirect_stdout(io.StringIO()), \
                self.assertRaisesRegex(ValueError, "invalid prepared chunk page count"):
            driver.prepare(["prepare"])


class SourceRequestTest(unittest.TestCase):
    def test_concurrent_progress_saves_publish_complete_unique_atomic_files(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "request.progress"
            barrier = threading.Barrier(2)
            fsync = source.os.fsync

            def synchronize(descriptor):
                if stat.S_ISREG(source.os.fstat(descriptor).st_mode):
                    barrier.wait(timeout=5)
                fsync(descriptor)

            values = [{"value": "a" * 10000}, {"value": "b" * 10000}]
            with mock.patch.object(source.os, "fsync", side_effect=synchronize), \
                    ThreadPoolExecutor(2) as executor:
                list(executor.map(lambda value: source.save(path, value), values))
            self.assertIn(json.loads(path.read_text()), values)
            self.assertEqual(list(path.parent.glob("*.tmp")), [])

    def test_selectors_are_exact_and_bounded(self):
        row = {"doc_id": DOC, "batch": "xx_test", "batch_version": 2}
        for value in (DOC, "sn1", "xx_test_ver02"):
            self.assertTrue(source.matches(row, source.specification(value, 100, "test")))
        for value in ("sn10", "xx_test_ver01"):
            self.assertFalse(source.matches(row, source.specification(value, 100, "test")))
        for value, cap in (("../anything", 1), ("xx_test", 1), ("sn1", 0), ("sn1", 10001),
                           ("sn1", True)):
            with self.assertRaises(ValueError):
                source.specification(value, cap, "test")

    def test_frozen_priority_replays_and_completed_requests_do_not_starve_normal_feed(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            page = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1,
                    "bytes": 12, "sha256": "a" * 64}
            other = {**page, "doc_id": "sn2_1900-01-02_ed-1_seq-3"}
            spec = source.specification("sn1", 100, "test")
            identity = source.digest(spec)
            record = {"request_id": identity, "spec": spec, "pages": [page],
                      "pages_sha256": source.digest([page])}
            path = root / "requests" / (identity + ".json")
            source.save(path, record)
            index = b"".join((json.dumps(p) + "\n").encode() for p in (other, page))
            selected, request_id = source.priority_index(index, root, "test", set())
            self.assertEqual(json.loads(selected), page)
            self.assertEqual(request_id, identity)
            self.assertEqual(source.priority_index(index, root, "test", {DOC}), (index, None))
            changed = json.dumps({**page, "sha256": "b" * 64}).encode()
            with self.assertRaisesRegex(ValueError, "changed"):
                source.priority_index(changed, root, "test", set())
            record["pages"][0]["bytes"] += 1
            source.save(path, record)
            with self.assertRaisesRegex(ValueError, "integrity"):
                source.read(path)

    def test_quarantined_frozen_member_never_rebinds_and_does_not_stop_other_pages(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            old = frozen_page()
            good = frozen_page("sn1_1900-01-02_ed-1_seq-4")
            spec = source.specification("sn1", 10, "test")
            record = {"request_id": source.digest(spec), "spec": spec, "pages": [old, good],
                      "pages_sha256": source.digest([old, good])}
            source.save(root / "requests" / (record["request_id"] + ".json"), record)
            changed = {**old, "sha256": "b" * 64}
            index = b"".join((json.dumps(p) + "\n").encode() for p in (changed, good))
            quarantine_id = quality.scan_quarantine.source_id(old)
            self.assertEqual(source.source_id(old), quarantine_id)
            with self.assertRaisesRegex(ValueError, "requested source changed"):
                source.priority_index(index, root, "test", set())
            selected, request_id = source.priority_index(index, root, "test", set(),
                                                         quarantined_ids={quarantine_id})
            self.assertEqual(request_id, record["request_id"])
            self.assertEqual(json.loads(selected), good)
            selected, request_id = source.priority_index(index, root, "test", {good["doc_id"]},
                                                         quarantined_ids={quarantine_id})
            self.assertEqual((selected, request_id), (index, None))
            self.assertEqual(source.read(root / "requests" / (record["request_id"] + ".json")), record)

    def test_registered_source_replays_without_rerunning_selection(self):
        with tempfile.TemporaryDirectory() as folder:
            state = Path(folder)
            args = types.SimpleNamespace(state=state, prep_ct=215, scan_server="http://scan")
            spec = source.specification(DOC, 1, "test")
            page = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1}
            with mock.patch.object(source.urllib.request, "urlopen",
                                   return_value=contextlib.closing(io.BytesIO(
                                       (json.dumps(page) + "\n").encode()))), \
                    mock.patch("qwen_feed.command", side_effect=[b"", b"", b"", b"",
                                                               json.dumps(page).encode()]) as command:
                first = source.register(args, spec)
            self.assertEqual(command.call_count, 5)
            with mock.patch("qwen_feed.command", side_effect=AssertionError("must replay")), \
                    mock.patch.object(source.urllib.request, "urlopen",
                                      side_effect=AssertionError("must replay")):
                self.assertEqual(source.register(args, spec), first)

    def test_ineligible_oldest_request_falls_through_to_later_request_or_ordinary_pages(self):
        for later_request in (False, True):
            with self.subTest(later_request=later_request), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                (root / "requests").mkdir()
                old = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1,
                       "bytes": 12, "sha256": "a" * 64, "damage_rate": 0.9}
                fresh = {**old, "doc_id": "sn2_1900-01-02_ed-1_seq-3"}
                for page in ([old, fresh] if later_request else [old]):
                    spec = source.specification(page["doc_id"], 1, "test")
                    source.save(root / "requests" / (source.digest(spec) + ".json"),
                                {"request_id": source.digest(spec), "spec": spec, "pages": [page],
                                 "pages_sha256": source.digest([page])})
                args = types.SimpleNamespace(
                    state=root, outbox=root, campaign="test", prep_ct=215, server_ct=230,
                    server_host="server", archive_host="archive", scan_server="http://scan",
                    archives="/archives", alto_export="/cache", min_free_gb=50, max_ready=1000,
                    max_unresolved=2000, max_pending=3, chunk_pages=100, reserve_mib=64,
                    yiddish_budget=None, german_budget=None, german_pages=None)
                index = b"".join((json.dumps(p) + "\n").encode() for p in (old, fresh))
                attempts = []
                selected = b""

                def command(argv, data=None):
                    nonlocal selected
                    if "tee" in argv and argv[-1].endswith("/index.jsonl"):
                        attempts.append([json.loads(line) for line in data.splitlines()])
                    if "select" in argv:
                        selected = b"".join((json.dumps(p) + "\n").encode()
                                            for p in attempts[-1] if p["doc_id"] != DOC)
                    if "cat" in argv:
                        return selected
                    return b"[]" if "-c" in argv else b""

                remote = [
                    {"fresh_pairs": 0, "unresolved_native": 0, "documents": [], "free_bytes": 10**12,
                     "ready": 0},
                    {"report": [{"lv": [{"lv_size": 10**12, "data_percent": 0}]}]},
                    {"available_archives": ["xx_test_ver01.tar.bz2"]},
                    {"cached": [], "missing": [], "pending": [fresh["doc_id"]]},
                ]
                with mock.patch.object(driver, "remote", side_effect=[
                        json.dumps(value).encode() for value in remote]), \
                        mock.patch.object(driver, "command", side_effect=command), \
                        mock.patch.object(driver.shutil, "disk_usage",
                                          return_value=types.SimpleNamespace(free=10**12)), \
                        mock.patch.object(driver.urllib.request, "urlopen",
                                          return_value=contextlib.closing(io.BytesIO(index))), \
                        contextlib.redirect_stdout(io.StringIO()) as output:
                    driver.tick(args)
                self.assertEqual(attempts[0], [old])
                self.assertEqual(attempts[1], [fresh] if later_request else [old, fresh])
                self.assertIn('"source_request_no_eligible_pages"', output.getvalue())
                for path in (root / "requests").glob("*.json"):
                    source.read(path)

    def test_native_source_status_binds_exact_audit_and_never_counts_source_rejections_as_jobs(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            campaign = root / "qwen/test"
            unit = campaign / "units" / DOC
            unit.mkdir(parents=True)
            image = {"bytes": 3, "sha256": "b" * 64}
            binding = {"schema": 1, "source": frozen_page(), "jpeg": image}
            job = {"id": "a" * 64, "doc_id": DOC, "input": image}
            raw = json.dumps(job).encode()
            (unit / "qwen.json").write_bytes(raw)
            (unit / "submitted").touch()
            entry = {"workunit_id": 42, "job_sha256": hashlib.sha256(raw).hexdigest(),
                     "batch": 8, "min_quorum": 2, "target_nresults": 2,
                     "source_bindings": {DOC: binding}}
            (campaign / "audit.json").write_text(json.dumps({job["id"]: entry}))
            rejected = "sn2_1900-01-02_ed-1_seq-3"
            no_crops = campaign / "units" / rejected
            no_crops.mkdir()
            (no_crops / "no_crops").touch()
            terminal_provenance(no_crops)
            sample = "\n".join(json.dumps(frozen_page(d)) for d in (DOC, rejected))
            response = json.dumps([{"id": 42, "canonical_resultid": 0, "error_mask": 0,
                                    "unsent": 2, "running": 0, "returned": 0, "invalid": 0}])
            with mock.patch.object(submit.subprocess, "check_output", return_value=response):
                status = submit.source_status(root, root, sample)
            self.assertEqual(status["published_workunits"], 1)
            self.assertEqual(status["unsent_replicas"], 2)
            self.assertEqual(status["agreed_workunits"], 0)
            self.assertEqual(status["pages"][rejected]["status"], "no_crops")
            (unit / "too_large").touch()
            other = "sn3_1900-01-02_ed-1_seq-3"
            other_unit = campaign / "units" / other
            other_unit.mkdir()
            other_job = {"id": "b" * 64, "doc_id": other, "input": image}
            other_raw = json.dumps(other_job).encode()
            (other_unit / "qwen.json").write_bytes(other_raw)
            (other_unit / "submitted").touch()
            (campaign / "audit.json").write_text(json.dumps({
                job["id"]: entry,
                other_job["id"]: {**entry, "workunit_id": 43,
                                  "job_sha256": hashlib.sha256(other_raw).hexdigest(),
                                  "source_bindings": {other: {**binding, "source": frozen_page(other)}}},
            }))
            with mock.patch.object(submit.subprocess, "check_output", return_value=response):
                status = submit.source_status(root, root, sample + "\n" + json.dumps(frozen_page(other)))
            self.assertEqual(status["published_workunits"], 2)
            self.assertEqual(status["unsent_replicas"], 2)
            self.assertEqual(status["history_unavailable_workunits"], 1)
            self.assertEqual(status["pages"][DOC]["status"], "published")
            self.assertEqual(status["pages"][other]["status"], "native_history_unavailable")
            for change in ({"sha256": "c" * 64}, {"bytes": 13}, {"batch": "xx_other"}, {"batch_version": 2}):
                with mock.patch.object(submit.subprocess, "check_output") as query:
                    conflict = submit.source_status(root, root, json.dumps({**frozen_page(), **change}))
                self.assertEqual(conflict["pages"][DOC]["status"], "source_conflict")
                self.assertEqual(conflict["published_workunits"], 0)
                self.assertEqual(conflict["agreed_workunits"], 0)
                self.assertEqual(conflict["source_conflict_pages"], 1)
                query.assert_not_called()
            (campaign / "audit.json").write_text(json.dumps({job["id"]: {
                k: v for k, v in entry.items() if k != "source_bindings"}}))
            with mock.patch.object(submit.subprocess, "check_output") as query:
                legacy = submit.source_status(root, root, sample)
            self.assertEqual(legacy["pages"][DOC]["status"], "source_provenance_unavailable")
            self.assertEqual(legacy["published_workunits"], 0)
            query.assert_not_called()
            (unit / "qwen.json").write_bytes(raw + b" ")
            with self.assertRaisesRegex(ValueError, "changed or unbound"):
                submit.source_status(root, root, sample)

    def test_source_rejections_are_reported_in_single_page_and_bundle_layouts(self):
        for layout in ("units", "pages"):
            for disposition in ("no_crops", "too_large"):
                with self.subTest(layout=layout, disposition=disposition), \
                        tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    page = root / "qwen/test" / layout / DOC
                    page.mkdir(parents=True)
                    (page / disposition).touch()
                    terminal_provenance(page)
                    if disposition == "too_large":
                        (page / "qwen.json").write_text(json.dumps({"id": "a" * 64, "doc_id": DOC}))
                    with mock.patch.object(submit.subprocess, "check_output") as query:
                        result = submit.source_status(root, root, json.dumps(frozen_page()))
                    self.assertEqual(result["pages"][DOC]["status"], disposition)
                    self.assertEqual(result["published_workunits"], 0)
                    query.assert_not_called()
                    if disposition == "too_large":
                        (page / "qwen.json").unlink()
                        self.assertEqual(submit.source_status(root, root, json.dumps(frozen_page()))[
                            "pages"][DOC]["status"], "too_large")

    def test_terminal_dispositions_require_exact_retained_source_and_quality_evidence(self):
        for layout in ("units", "pages"):
            for disposition in ("no_crops", "too_large"):
                with self.subTest(layout=layout, disposition=disposition), \
                        tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    unit = root / "qwen/test" / layout / DOC
                    unit.mkdir(parents=True)
                    (unit / disposition).touch()
                    sample = json.dumps(frozen_page())
                    with self.assertLogs(level="WARNING"):
                        legacy = submit.source_status(root, root, sample)
                    self.assertEqual(legacy["pages"][DOC]["status"], "source_provenance_unavailable")
                    self.assertEqual(legacy["source_conflict_pages"], 1)
                    self.assertFalse((unit / "source-provenance.json").exists())
                    terminal_provenance(unit)
                    for change in ({"sha256": "c" * 64}, {"bytes": 13},
                                   {"batch": "xx_other"}, {"batch_version": 2}):
                        result = submit.source_status(
                            root, root, json.dumps({**frozen_page(), **change}))
                        self.assertEqual(result["pages"][DOC]["status"], "source_conflict")
                        self.assertEqual(result["source_conflict_pages"], 1)
                        self.assertEqual(result["published_workunits"], 0)
                    (unit / "quality.json").write_text((unit / "quality.json").read_text() + " ")
                    with self.assertRaisesRegex(ValueError, "changed or unbound terminal"):
                        submit.source_status(root, root, sample)

    def test_later_terminal_marker_cannot_hide_a_conflicting_source(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for campaign in ("a", "z"):
                unit = root / "qwen" / campaign / "units" / DOC
                unit.mkdir(parents=True)
                (unit / "too_large").touch()
                terminal_provenance(unit)
            (unit / "qwen.json").write_text(json.dumps({"id": "a" * 64, "doc_id": DOC}))
            receipt = root / "qwen/a/units" / DOC / "source-provenance.json"
            record = json.loads(receipt.read_text())
            record["binding"]["source"]["batch_version"] = 2
            receipt.write_text(json.dumps(record))
            result = submit.source_status(root, root, json.dumps(frozen_page()))
            self.assertEqual(result["pages"][DOC]["status"], "source_conflict")
            self.assertEqual(result["source_conflict_pages"], 1)

    def test_partial_terminal_evidence_is_not_a_completed_disposition(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            unit = root / "qwen/test/units" / DOC
            unit.mkdir(parents=True)
            terminal_provenance(unit)
            result = submit.source_status(root, root, json.dumps(frozen_page()))
            self.assertEqual(result["pages"][DOC]["status"], "pending")
            (unit / "no_crops").touch()
            result = submit.source_status(root, root, json.dumps(frozen_page()))
            self.assertEqual(result["pages"][DOC]["status"], "no_crops")
            receipt = unit / "source-provenance.json"
            record = json.loads(receipt.read_text())
            record["binding"]["jpeg"]["bytes"] += 1
            receipt.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, "changed or unbound terminal"):
                submit.source_status(root, root, json.dumps(frozen_page()))

    def test_published_work_does_not_hide_terminal_conflicts_in_either_campaign_order(self):
        for published, rejected in (("a", "z"), ("z", "a")):
            for kind in ("matching", "conflict", "legacy"):
                with self.subTest(published=published, kind=kind), \
                        tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    campaign = root / "qwen" / published
                    unit = campaign / "units" / DOC
                    unit.mkdir(parents=True)
                    image = {"bytes": 3, "sha256": hashlib.sha256(b"jpg").hexdigest()}
                    binding = {"schema": 1, "source": frozen_page(), "jpeg": image}
                    job = {"id": "a" * 64, "doc_id": DOC, "input": image}
                    raw = json.dumps(job).encode()
                    (unit / "qwen.json").write_bytes(raw)
                    (unit / "submitted").touch()
                    (campaign / "audit.json").write_text(json.dumps({job["id"]: {
                        "workunit_id": 42, "job_sha256": hashlib.sha256(raw).hexdigest(),
                        "batch": 8, "min_quorum": 2, "target_nresults": 2,
                        "source_bindings": {DOC: binding}}}))
                    terminal = root / "qwen" / rejected / "units" / DOC
                    terminal.mkdir(parents=True)
                    (terminal / "no_crops").touch()
                    if kind != "legacy":
                        terminal_provenance(terminal)
                        if kind == "conflict":
                            path = terminal / "source-provenance.json"
                            record = json.loads(path.read_text())
                            record["binding"]["source"]["batch_version"] = 2
                            path.write_text(json.dumps(record))
                    response = json.dumps([{"id": 42, "canonical_resultid": 0, "error_mask": 0,
                                            "unsent": 2, "running": 0, "returned": 0, "invalid": 0}])
                    with mock.patch.object(submit.subprocess, "check_output", return_value=response):
                        result = submit.source_status(root, root, json.dumps(frozen_page()))
                    self.assertEqual(result["published_workunits"], 1)
                    self.assertEqual(result["source_conflict_pages"], int(kind == "conflict"))
                    self.assertEqual(result["pages"][DOC]["status"],
                                     "source_conflict" if kind == "conflict" else "published")

    def test_native_audit_retains_original_to_jpeg_binding_after_handoff_cleanup(self):
        with tempfile.TemporaryDirectory() as folder:
            share = Path(folder)
            root = share / "qwen/test"
            unit = root / "units" / DOC
            unit.mkdir(parents=True)
            image = {"bytes": 3, "sha256": hashlib.sha256(b"jpg").hexdigest()}
            job = {"id": "a" * 64, "doc_id": DOC, "input": image}
            raw = json.dumps(job).encode()
            (unit / "qwen.json").write_bytes(raw)
            (unit / "submitted").touch()
            source = share / "quality/test/texts" / (DOC + ".image-provenance.json")
            source.parent.mkdir(parents=True)
            binding = {"schema": 1, "source": frozen_page(), "jpeg": image}
            source.write_text(json.dumps({**binding, "jpeg": {**image, "sha256": "c" * 64}}))
            with self.assertRaisesRegex(ValueError, "does not bind"):
                submit.audit(root, share)
            source.write_text(json.dumps(binding))
            receipt = {job["id"]: {"job_sha256": hashlib.sha256(raw).hexdigest(),
                                  "workunit_id": 42, "batch": 8, "min_quorum": 2, "target_nresults": 2}}
            with mock.patch.object(submit.subprocess, "check_output", return_value=json.dumps(receipt)):
                submit.audit(root, share)
            source.unlink()
            with mock.patch.object(submit.subprocess, "check_output", return_value=json.dumps([
                    {"id": 42, "canonical_resultid": 43, "error_mask": 0, "unsent": 0,
                     "running": 0, "returned": 2, "invalid": 0}])):
                progress = submit.source_status(share, share, json.dumps(frozen_page()))
            self.assertEqual(progress["published_workunits"], 1)
            self.assertEqual(progress["agreed_workunits"], 1)
            self.assertEqual(progress["source_conflict_pages"], 0)
            self.assertEqual(progress["pages"][DOC]["status"], "published")

    def test_source_progress_exists_before_the_first_native_campaign_is_staged(self):
        with tempfile.TemporaryDirectory() as folder, \
                mock.patch.object(submit.subprocess, "check_output") as query:
            result = submit.source_status(Path(folder), Path(folder), json.dumps(frozen_page()))
        self.assertEqual(result["pages"][DOC]["status"], "pending")
        self.assertEqual(result["published_workunits"], 0)
        query.assert_not_called()

    def test_progress_distinguishes_publication_dispatch_consensus_and_error(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            args = types.SimpleNamespace(state=root, prep_ct=215, server_ct=230,
                                         campaign="test", server_host="server", archive_host="archive")
            spec = source.specification(DOC, 1, "test")
            page = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1}
            record = {"request_id": source.digest(spec), "spec": spec,
                      "pages": [page], "pages_sha256": source.digest([page])}
            base = {"pages": {DOC: {"status": "published"}}, "published_workunits": 1,
                    "agreed_workunits": 0, "unsent_replicas": 2, "running_replicas": 0,
                    "returned_replicas": 0, "invalid_replicas": 0, "error_workunits": 0,
                    "history_unavailable_workunits": 0, "source_conflict_pages": 0}
            for delta, expected in (
                    ({}, "awaiting_dispatch_or_admission"),
                    ({"pages": {DOC: {"status": "published", "provenance": "verified_reconstruction"}}},
                     "awaiting_dispatch_or_admission"),
                    ({"pages": {DOC: {"status": "source_conflict", "provenance": "verified_reconstruction"}},
                      "source_conflict_pages": 1}, "needs_science_review"),
                    ({"running_replicas": 1}, "awaiting_host_results"),
                    ({"agreed_workunits": 1}, "native_consensus_complete"),
                    ({"error_workunits": 1}, "needs_science_review"),
                    ({"history_unavailable_workunits": 1}, "needs_science_review"),
                    ({"source_conflict_pages": 1}, "needs_science_review"),
                    *[({"pages": {DOC: {"status": disposition}}, "published_workunits": 0},
                       "needs_science_review")
                      for disposition in ("no_crops", "too_large", "preparation_rejected")]):
                with mock.patch("qwen_feed.remote", side_effect=[
                        json.dumps({**base, **delta}).encode(),
                        b'{"available_archives":["xx_test_ver01.tar.bz2"]}']), \
                        mock.patch("qwen_feed.command",
                                   return_value=b'{"done.txt":{},"skipped.txt":{}}'):
                    result = source.inspect(args, record)
                self.assertEqual(result["state"], expected)
                self.assertEqual(result["selected_pages"], 1)
                self.assertEqual(result["reconstructed_source_pages"], int(
                    delta.get("pages", {}).get(DOC, {}) == {
                        "status": "published", "provenance": "verified_reconstruction"}))

    def test_quarantine_progress_binds_exact_fingerprint_even_after_another_copy_is_prepared(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            args = types.SimpleNamespace(state=root, prep_ct=215, server_ct=230, campaign="test",
                                         server_host="server", archive_host="archive")
            spec, page = source.specification(DOC, 1, "test"), frozen_page()
            record = {"request_id": source.digest(spec), "spec": spec, "pages": [page],
                      "pages_sha256": source.digest([page])}
            native = {"pages": {DOC: {"status": "pending"}}, "published_workunits": 0,
                      "agreed_workunits": 0, "unsent_replicas": 0, "running_replicas": 0,
                      "returned_replicas": 0, "invalid_replicas": 0, "error_workunits": 0,
                      "history_unavailable_workunits": 0, "source_conflict_pages": 0}
            for sha, done, expected in ((page["sha256"], {DOC: "another copy"}, "preparation_quarantined"),
                                        ("b" * 64, {}, "awaiting_preparation")):
                prepared = {"done.txt": done, "skipped.txt": {},
                            "quarantine": [{"source": {**page, "sha256": sha}, "refetch_state": "manual_review"}]}
                with mock.patch("qwen_feed.remote", side_effect=[
                        json.dumps(native).encode(), b'{"available_archives":["xx_test_ver01.tar.bz2"]}']), \
                        mock.patch("qwen_feed.command", return_value=json.dumps(prepared).encode()):
                    result = source.inspect(args, record)
                self.assertEqual(result["source_dispositions"], {expected: 1})
                self.assertEqual(result["state"], "needs_science_review" if done else "preparing_and_publishing")
                self.assertEqual(result["quarantined_refetch_states"], {"manual_review": 1} if done else {})


class ArchiveTest(unittest.TestCase):
    def test_existing_forced_stdin_command_returns_additive_inventory_on_empty_input(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "xx_present_ver01.tar.bz2").write_bytes(b"not opened")
            output = io.StringIO()
            with mock.patch.object(sys, "argv", [
                    "archive_alto.py", "--pages", "-", "--archives", str(root),
                    "--out", str(root / "cache")]), \
                    mock.patch.object(sys, "stdin", io.StringIO("")), \
                    mock.patch.object(archive.tarfile, "open", side_effect=AssertionError("not metadata-only")), \
                    contextlib.redirect_stdout(output):
                archive.main()
            self.assertEqual(json.loads(output.getvalue()), {
                "cached": [], "missing": [], "pending": [],
                "available_archives": ["xx_present_ver01.tar.bz2"]})

    def test_inventory_lists_only_retained_supported_archive_files_without_opening_them(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for name in ("xx_present_ver01.tar.bz2", "xx_invalid_ver00.tar.bz2", "unrelated.tar.bz2"):
                (root / name).write_bytes(b"not an archive; inventory never reads contents")
            (root / "xx_directory_ver01.tar.bz2").mkdir()
            with mock.patch.object(archive.tarfile, "open", side_effect=AssertionError("not metadata-only")):
                self.assertEqual(archive.available_archives(root), ["xx_present_ver01.tar.bz2"])

    def test_budget_yields_between_archives_without_deferring_unread_pages(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            body = b"<alto>original</alto>"
            pages = []
            for n, batch in enumerate(("xx_first", "xx_second", "xx_third"), 1):
                doc = f"sn{n}_1900-01-02_ed-1_seq-3"
                pages.append({"doc_id": doc, "batch": batch, "batch_version": 1})
                with tarfile.open(root / f"{batch}_ver01.tar.bz2", "w:bz2") as packed:
                    info = tarfile.TarInfo(f"sn{n}/1900/01/02/ed-1/seq-3/ocr.xml")
                    info.size = len(body)
                    packed.addfile(info, io.BytesIO(body))
            out = root / "cache"
            for n in range(1, 4):
                with mock.patch.object(archive.time, "monotonic", side_effect=[0, 2, 2]):
                    report = archive.extract(pages, root, out, max_seconds=1)
                self.assertEqual(report["cached"], [p["doc_id"] for p in pages[:n]])
                self.assertEqual(report["pending"], [p["doc_id"] for p in pages[n:]])
                self.assertEqual(report["missing"], [])

    def test_supported_batch_prefix_and_both_date_layouts(self):
        for prefix in ("", "./", "xx_test_ver01/", "./xx_test_ver01/"):
            for date in ("1900/01/02", "1900-01-02"):
                with self.subTest(prefix=prefix, date=date):
                    self.assertEqual(archive.member_doc(prefix + "sn1/" + date + "/ed-1/seq-3/ocr.xml"), DOC)
        for path in ("../sn1/1900-01-02/ed-1/seq-3/ocr.xml",
                     "/sn1/1900-01-02/ed-1/seq-3/ocr.xml",
                     "sn1/1900-02-31/ed-1/seq-3/ocr.xml"):
            self.assertIsNone(archive.member_doc(path))

    def test_exact_member_cache_receipt_corruption_and_missing_page(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "xx_test_ver01.tar.bz2"
            body = b"<alto>original LoC coordinates</alto>"
            name = "./sn1/1900/01/02/ed-1/seq-3/ocr.xml"
            with tarfile.open(path, "w:bz2") as packed:
                for member, raw in [(name, body), ("../escape/ocr.xml", b"unrelated")]:
                    info = tarfile.TarInfo(member)
                    info.size = len(raw)
                    packed.addfile(info, io.BytesIO(raw))
            pages = [{"doc_id": DOC, "batch": "xx_test", "batch_version": 1},
                     {"doc_id": "sn2_1900-01-02_ed-1_seq-3", "batch": "xx_test", "batch_version": 1}]
            out = root / "cache"
            report = archive.extract(pages, root, out)
            self.assertEqual(report["cached"], [DOC])
            self.assertEqual(report["missing"], [pages[1]["doc_id"]])
            receipt = json.loads((out / (DOC + ".provenance.json")).read_text())
            self.assertEqual(receipt["member"], name)
            self.assertEqual(receipt["sha256"], hashlib.sha256(body).hexdigest())
            self.assertEqual(quality.cached_alto(str(out), DOC), body)
            (out / (DOC + ".xml")).write_bytes(b"corrupt")
            with self.assertRaisesRegex(ValueError, "differs"):
                quality.cached_alto(str(out), DOC)
            archive.extract(pages[:1], root, out)
            self.assertEqual(quality.cached_alto(str(out), DOC), body)
            with mock.patch.object(archive.tarfile, "open", side_effect=AssertionError("cache reread")):
                self.assertEqual(archive.extract(pages[:1], root, out)["cached"], [DOC])
            with self.assertRaises(ValueError):
                archive.extract([{**pages[0], "batch": "../escape"}], root, out)


class SourceTest(unittest.TestCase):
    def test_source_index_skips_absent_archives_without_claiming_member_verification(self):
        rows = [
            {"doc_id": "absent", "batch": "xx_absent", "batch_version": 1},
            {"doc_id": "wrong-version", "batch": "xx_present", "batch_version": 2},
            {"doc_id": "candidate-not-member-proven", "batch": "xx_present", "batch_version": 1},
        ]
        lines = [(json.dumps(row) + "\n").encode() for row in rows]
        selected, total, retained = driver.available_source_index(
            lines, {"available_archives": ["xx_present_ver01.tar.bz2"]})
        self.assertEqual((total, retained), (3, 1))
        self.assertEqual(json.loads(selected), rows[-1])
        self.assertEqual(driver.available_source_index(lines, {"available_archives": []}), (b"", 3, 0))
        for inventory in ({}, {"available_archives": ["../xx_present_ver01.tar.bz2"]},
                          {"available_archives": ["xx_present_ver01.tar.bz2"] * 2}):
            with self.assertRaisesRegex(ValueError, "valid fresh"):
                driver.available_source_index(lines, inventory)

    def test_bounded_staging_cannot_acknowledge_unattempted_chunk_sources(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            sample = root / "sample.jsonl"
            sample.write_text(json.dumps({"doc_id": DOC}) + "\n")
            unit = root / "units" / DOC
            unit.mkdir(parents=True)
            for marker in (None, "too_large", "submitted"):
                if marker:
                    (unit / marker).touch()
                with self.assertRaisesRegex(RuntimeError, "retain READY"):
                    submit.assert_processed(root, sample)
                if marker:
                    (unit / marker).unlink()
            for files in (("too_large", "rejected-qwen.json"), ("submitted", "qwen.json"), ("no_crops",)):
                for name in files:
                    (unit / name).touch()
                submit.assert_processed(root, sample)
                for name in files:
                    (unit / name).unlink()

    def test_isolated_staging_artifact_never_replaces_submit_binary(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            plan = root / "plan.json"
            plan.write_text(json.dumps({"engine": {"image": "image", "llama": "llama", "weights": "weights"}}))
            argv = ["qwen_submit.py", "--share", str(root), "--project", str(root),
                    "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(root)]
            for isolated in (False, True):
                artifact = root / "usnm-boinc-stage"
                if isolated:
                    artifact.touch()
                with mock.patch.object(submit, "__file__", str(root / "qwen_submit.py")), \
                        mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(submit, "inventory", return_value={
                            "ready": 0, "fresh_pairs": 0, "unresolved_native": 0,
                            "free_bytes": 100 * 1024**3, "app_versions": [], "min_version": 0}), \
                        mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "assert_submitted"), \
                        mock.patch.object(submit, "audit", return_value={}):
                    submit.main()
                self.assertEqual(run.call_args_list[0].args[0][:3],
                                 [str(artifact) if isolated else "usnm-boinc", "qwen", "stage"])
                self.assertEqual(run.call_args_list[1].args[0][:3], ["usnm-boinc", "qwen", "submit"])

    def test_inventory_remembers_rejected_source_without_counting_it_as_work(self):
        with tempfile.TemporaryDirectory() as folder:
            share = Path(folder)
            for kind in ("units", "pages"):
                unit = share / "qwen" / "campaign" / kind / DOC
                unit.mkdir(parents=True)
                (unit / "too_large").write_text("crop c01: 8193 UTF-8 bytes over 8192\n")
                (unit / "rejected-qwen.json").write_text(json.dumps({"doc_id": DOC}))
            with mock.patch.object(submit.subprocess, "check_output",
                                   return_value='{"ready": 0, "dispatched": 0}'), \
                    mock.patch.object(submit.shutil, "disk_usage",
                                      return_value=types.SimpleNamespace(free=100)):
                self.assertEqual(submit.inventory(share, share)["documents"], [DOC])
                self.assertEqual(list(submit.jobs(share / "qwen")), [])
                submit.assert_submitted(share / "qwen" / "campaign")

    def test_inventory_uses_lowest_space_across_project_and_bulk_mounts(self):
        with tempfile.TemporaryDirectory() as folder:
            share = Path(folder)
            project = share / "usnmocr"
            for lowest in range(3):
                sizes = [100, 100, 100]
                sizes[lowest] = 39
                with self.subTest(lowest=lowest), \
                        mock.patch.object(submit.subprocess, "check_output",
                                          return_value='{"ready": 0, "dispatched": 0}'), \
                        mock.patch.object(submit.shutil, "disk_usage",
                                          side_effect=[types.SimpleNamespace(free=n) for n in sizes]) as usage:
                    self.assertEqual(submit.inventory(share, project)["free_bytes"], 39)
                    self.assertEqual([call.args[0] for call in usage.call_args_list],
                                     [share, share / "quality", project / "download"])

    def test_original_alto_chunk_finishes_without_shadowing_reader_map(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            camp = root / "camp"
            camp.mkdir()
            alto = b"<alto>original</alto>"
            (root / (DOC + ".xml")).write_bytes(alto)
            (root / (DOC + ".provenance.json")).write_text(json.dumps({
                "doc_id": DOC, "sha256": hashlib.sha256(alto).hexdigest()}))
            p = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1, "lccn": "sn1",
                 "date": "1900-01-02", "lang": "rus", "scan": "keep/test.jpg", "damage_rate": 0.9,
                 "bytes": 6, "sha256": hashlib.sha256(b"source").hexdigest()}
            tx = mock.Mock()
            tx.loc.return_value = ({DOC: "original text"}, True)
            tx.american_stories.return_value = {}
            tx.ours.return_value = {DOC: ("tesseract", "source", "ours")}
            args = types.SimpleNamespace(workers=1, scan_server="http://scan", alto_cache=folder)
            with mock.patch.object(quality, "prepare_page", return_value=(b"jpg", b"empty", {}, None)):
                self.assertEqual(quality.process_chunk(args, tx, [p], str(camp), folder, 1,
                                                       str(root / "done.txt"), str(root / "skipped.txt")),
                                 (1, 0, 2))
            self.assertEqual((camp / "c00001/scans" / (DOC + ".xml")).read_bytes(), alto)
            self.assertIn(".loc-provenance.json", (camp / "c00001/SHA256SUMS").read_text())

    def test_audited_history_survives_database_purge_but_changed_jobs_do_not(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            unit = root / "units" / DOC
            unit.mkdir(parents=True)
            path = unit / "qwen.json"
            job_id = "a" * 64
            path.write_text(json.dumps({"id": job_id, "doc_id": DOC}))
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            response = json.dumps({job_id: {"job_sha256": digest, "workunit_id": 42}})
            with mock.patch.object(submit.subprocess, "check_output", return_value=response):
                self.assertEqual(submit.audit(root, root)["verified_new"], 1)
            with mock.patch.object(submit.subprocess, "check_output", side_effect=RuntimeError("workunit purged")):
                self.assertEqual(submit.audit(root, root), {"verified_new": 0, "verified_total": 1})
                path.write_text(path.read_text() + "\n")
                with self.assertRaisesRegex(RuntimeError, "purged"):
                    submit.audit(root, root)

    def test_nonenglish_damage_order_and_real_scan_availability(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            index = root / "index.jsonl"
            docs = [f"sn{n}_1900-01-02_ed-1_seq-3" for n in range(1, 6)]
            index.write_text("".join(json.dumps({"doc_id": d, "batch": "xx_test", "bytes": 1,
                                                "sha256": "a", "format": "jp2"}) + "\n" for d in docs))
            curated = root / "curated/audit/damage/v2"
            curated.mkdir(parents=True)
            (curated / "xx_test_ver01.parquet").touch()
            values = [("eng", 0.99), ("srp", 0.8), ("rus", 0.6), (None, 0.9), ("pol", 0.1)]
            rows = [{"doc_id": d, "lang": lang, "damage_rate": rate}
                    for d, (lang, rate) in zip(docs, values)]
            pq = mock.Mock()
            pq.read_table.return_value.to_pylist.return_value = rows
            pa = types.ModuleType("pyarrow")
            pa.parquet = pq
            a = types.SimpleNamespace(index=str(index), staged="", curated=str(root / "curated"),
                                      non_english=True, order="damage", count=1,
                                      out=str(root / "out.jsonl"), scan_server="http://scan")
            # Worst foreign page's image is absent; the next available one is Russian,
            # not the more-damaged English or unknown-language page.
            with mock.patch.dict("sys.modules", {"pyarrow": pa, "pyarrow.parquet": pq}), \
                    mock.patch.object(quality, "scan_set", side_effect=lambda _, p: None if p["doc_id"] == docs[1] else "keep"):
                quality.cmd_select(a)
            chosen = json.loads(Path(a.out).read_text())
            self.assertEqual((chosen["doc_id"], chosen["lang"]), (docs[2], "rus"))

    def test_uncertain_or_missing_submit_receipts_cannot_be_cleaned_up(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            unit = root / "units" / DOC
            unit.mkdir(parents=True)
            (unit / "qwen.json").write_text(json.dumps({"doc_id": DOC}))
            for marker in (None, "submitting"):
                if marker:
                    (unit / marker).touch()
                with self.assertRaisesRegex(RuntimeError, "incomplete"):
                    submit.assert_submitted(root)
            (unit / "submitting").rename(unit / "submitted")
            submit.assert_submitted(root)

    def test_storage_and_queue_backpressure_do_not_prepare_or_fetch_sources(self):
        for pool_pct, ready, producer_gb in [(90, 0, 100), (50, 1000, 100), (0, 0, 39)]:
            with self.subTest(pool_pct=pool_pct, ready=ready), tempfile.TemporaryDirectory() as folder:
                a = types.SimpleNamespace(state=Path(folder), campaign="qwen-test",
                                          server_host="boinc.example", server_ct=230, prep_ct=215,
                                          outbox=Path(folder), min_free_gb=50, max_ready=1000, max_pending=3,
                                          max_unresolved=2000, reserve_mib=64)
                inv = {"ready": ready, "fresh_pairs": ready // 2, "unresolved_native": ready // 2,
                       "documents": [], "free_bytes": 100 * 1024**3}
                pool = {"report": [{"lv": [{"lv_size": str(100 * 1024**3), "data_percent": str(pool_pct)}]}]}
                with mock.patch.object(driver, "remote", side_effect=[json.dumps(inv), json.dumps(pool)]) as remote, \
                        mock.patch.object(driver.shutil, "disk_usage",
                                          return_value=types.SimpleNamespace(free=producer_gb * 1024**3)), \
                        mock.patch.object(driver, "command", side_effect=AssertionError("preparation started")):
                    driver.tick(a)
                self.assertEqual(remote.call_count, 2)

    def test_ready_sources_return_without_waiting_for_new_scan_manifest(self):
        with tempfile.TemporaryDirectory() as folder:
            a = types.SimpleNamespace(state=Path(folder), campaign="qwen-test",
                                      server_host="boinc.example", server_ct=230, prep_ct=215,
                                      archive_host="archive.example", archives="/archives",
                                      alto_export="/cache", scan_server="http://scan.example",
                                      outbox=Path(folder), min_free_gb=50, max_ready=1000,
                                      max_pending=3, chunk_pages=100, max_unresolved=2000, reserve_mib=64,
                                      yiddish_budget=None, german_budget=None, german_pages=None)
            page = {"doc_id": DOC, "damage_rate": 0.9}
            inv = {"ready": 4000, "fresh_pairs": 0, "unresolved_native": 100,
                   "documents": [], "free_bytes": 100 * 1024**3}
            pool = {"report": [{"lv": [{"lv_size": str(200 * 1024**3), "data_percent": "50"}]}]}
            receipt = {"cached": [DOC], "missing": [], "pending": []}
            archive_inventory = {"available_archives": ["xx_test_ver01.tar.bz2"]}

            def run(args, data=None):
                if any("ocr_manifest.py" in arg for arg in args):
                    raise TimeoutError("scan-manifest generation must not block ready work")
                if args[-1].endswith("/selected.jsonl") and "cat" in args:
                    return json.dumps(page).encode()
                if "-c" in args:
                    return b"[]"
                return b""

            with mock.patch.object(driver, "remote", side_effect=[
                    json.dumps(inv), json.dumps(pool), json.dumps(archive_inventory), json.dumps(receipt)]), \
                    mock.patch.object(driver.shutil, "disk_usage",
                                      return_value=types.SimpleNamespace(free=100 * 1024**3)), \
                    mock.patch.object(driver, "command", side_effect=run) as command, \
                    mock.patch.object(driver.urllib.request, "urlopen") as source, \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                source.return_value.__enter__.return_value.__iter__.return_value = iter([
                    b'{"doc_id":"candidate","batch":"xx_test","batch_version":1}\n'])
                driver.tick(a)
            prepared = [call for call in command.call_args_list
                        if "run" in call.args[0] and any(
                            arg.endswith("/quality_feed.py") for arg in call.args[0])]
            self.assertEqual(len(prepared), 1)
            result = next(json.loads(line) for line in output.getvalue().splitlines()
                          if json.loads(line).get("status") == "prepared")
            self.assertEqual((result["selected_for_preparation"], result["pages"]), (1, 0))

    def test_native_gate_uses_fresh_original_pairs_but_retains_capacity_and_unresolved_bounds(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            plan = root / "plan.json"
            plan.write_text(json.dumps({"engine": {"image": "image", "llama": "llama", "weights": "weights"}}))
            argv = ["qwen_submit.py", "--share", str(root), "--project", str(root),
                    "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(root)]
            base = {"ready": 4000, "fresh_pairs": 0, "unresolved_native": 100,
                    "free_bytes": 100 * 1024**3, "app_versions": [], "min_version": 0}
            for change, error in (({}, None), ({"fresh_pairs": 401}, "fresh_original_watermark"),
                                  ({"unresolved_native": 1901}, "unresolved_native_ceiling"),
                                  ({"free_bytes": 51 * 1024**3}, "reserved_input_free_space")):
                with self.subTest(change=change), mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(submit, "inventory", return_value={**base, **change}), \
                        mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "assert_submitted"), \
                        mock.patch.object(submit, "audit", return_value={}):
                    if error:
                        output = io.StringIO()
                        with contextlib.redirect_stdout(output), self.assertRaises(SystemExit) as caught:
                            submit.main()
                        self.assertEqual(caught.exception.code, 75)
                        decision = json.loads(output.getvalue())
                        self.assertEqual((decision["status"], decision["reason"]),
                                         ("deferred", error))
                        self.assertEqual(decision["phase"], "locked_recheck")
                        run.assert_not_called()
                    else:
                        submit.main()
                        self.assertEqual(run.call_count, 2)

    GUARDED = [{"plan_class": "nvidia_vulkan_8b", "platformid": 1, "version_num": 114},
               {"plan_class": "nvidia_vulkan_8b", "platformid": 1, "version_num": 117},
               {"plan_class": "amd_vulkan_8b", "platformid": 5, "version_num": 117}]

    def test_a_dispatchable_app_that_cannot_read_the_request_defers_before_any_transfer(self):
        behind = [*self.GUARDED, {"plan_class": "apple_gpu_8b", "platformid": 2, "version_num": 116}]
        for versions, minimum, version, unreadable in (
                (self.GUARDED, 0, 3, []),  # an older row the feeder doesn't load
                (self.GUARDED, 114, 3, [["nvidia_vulkan_8b", 1, 114]]),  # min_version loads it
                (self.GUARDED, 115, 2, []),
                (behind, 0, 3, [["apple_gpu_8b", 2, 116]]),
                (behind, 0, 4, [["apple_gpu_8b", 2, 116]]),
                (behind, 0, 1, [])):
            with self.subTest(minimum=minimum, version=version, n=len(versions)):
                with tempfile.TemporaryDirectory() as folder:
                    root = Path(folder)
                    plan = root / "plan.json"
                    plan.write_text("{}")
                    state = {"ready": 0, "fresh_pairs": 0, "unresolved_native": 0, "free_bytes": 10**12,
                             "app_versions": versions, "min_version": minimum}
                    argv = ["qwen_submit.py", "--share", str(root), "--project", str(root), "--preflight",
                            "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(root),
                            "--prompt-version", str(version)]
                    output = io.StringIO()
                    with mock.patch.object(sys, "argv", argv), \
                            mock.patch.object(submit, "inventory", return_value=state), \
                            contextlib.redirect_stdout(output):
                        if unreadable:
                            with self.assertRaises(SystemExit) as caught:
                                submit.main()
                            self.assertEqual(caught.exception.code, 75)
                        else:
                            submit.main()
                    decision = json.loads(output.getvalue())
                    self.assertEqual(decision["unreadable_app_versions"], unreadable)
                    self.assertEqual(decision["status"], "deferred" if unreadable else "admitted")

    def test_a_root_keeps_one_request_version(self):
        with tempfile.TemporaryDirectory() as folder:
            share = Path(folder)
            unit = share / "qwen/qwen-test/units" / DOC
            unit.mkdir(parents=True)
            (unit / "qwen.json").write_text(json.dumps({"id": "a" * 64, "doc_id": DOC, "prompt_version": 1}))
            plan = share / "plan.json"
            plan.write_text("{}")
            state = {"ready": 0, "fresh_pairs": 0, "unresolved_native": 0, "free_bytes": 10**12,
                     "app_versions": [], "min_version": 0}
            argv = ["qwen_submit.py", "--share", str(share), "--project", str(share), "--preflight",
                    "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(share),
                    "--prompt-version", "3"]
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(submit, "inventory", return_value=state), \
                    self.assertRaisesRegex(RuntimeError, "holds request version 1"):
                submit.main()

    def test_guarded_roots_keep_yiddish_at_version_4_and_only_yiddish(self):
        for language, version, allowed in (("yid", 4, True), ("srp", 3, True), ("yid", 3, False),
                                           ("srp", 4, False), ("yid", 1, True), ("yid", 2, True)):
            with self.subTest(language=language, version=version):
                job = {"language": language, "doc_id": DOC}
                if allowed:
                    submit.assert_request_cohort(job, version)
                else:
                    with self.assertRaisesRegex(RuntimeError, "does not belong"):
                        submit.assert_request_cohort(job, version)

    def test_guarded_requests_submit_with_the_stage_binary_unless_a_submit_binary_is_set(self):
        # Until the server image is from #1437 on, versions 3 and 4 submit with the binary that
        # staged them; --submit-binary then names one binary for every version (#1449).
        for extra, version, expected in (([], "3", "/guard/usnm-boinc"), ([], "1", "usnm-boinc"),
                                         (["--submit-binary", "usnm-boinc"], "3", "usnm-boinc"),
                                         (["--submit-binary", "/image/usnm-boinc"], "1", "/image/usnm-boinc")):
            with self.subTest(extra=extra, version=version), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                plan = root / "plan.json"
                plan.write_text(json.dumps({"engine": {"image": "image", "llama": "llama", "weights": "weights"}}))
                argv = ["qwen_submit.py", "--share", str(root), "--project", str(root), "--campaign", "qwen-test",
                        "--engine-plan", str(plan), "--models", str(root), "--stage-binary", "/guard/usnm-boinc",
                        "--prompt-version", version, *extra]
                state = {"ready": 0, "fresh_pairs": 0, "unresolved_native": 0, "free_bytes": 10**12,
                         "app_versions": [], "min_version": 0}
                with mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(submit, "inventory", return_value=state), \
                        mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "assert_submitted"), \
                        mock.patch.object(submit, "audit", return_value={}):
                    submit.main()
                stage, publish = (call.args[0] for call in run.call_args_list)
                self.assertEqual(stage[:3], ["/guard/usnm-boinc", "qwen", "stage"])
                self.assertEqual(stage[stage.index("--prompt-version") + 1], version)
                self.assertEqual(publish[:3], [expected, "qwen", "submit"])

    def test_transactions_refuse_a_submit_binary_they_would_not_use(self):
        argv = ["qwen_submit.py", "--campaign", "qwen-test", "--engine-plan", "plan.json", "--models", "models",
                "--transaction", "a" * 64, "--submit-binary", "usnm-boinc"]
        state = {"ready": 0, "fresh_pairs": 0, "unresolved_native": 0, "free_bytes": 10**12,
                 "app_versions": [], "min_version": 0}
        with mock.patch.object(sys, "argv", argv), mock.patch.object(submit, "inventory", return_value=state), \
                mock.patch.object(submit, "run_transaction") as transaction, \
                mock.patch("sys.stderr"), self.assertRaises(SystemExit):
            submit.main()
        transaction.assert_not_called()

    def budget_tick(self, root, budget, yiddish, others, german_budget=None, german_pages=None, retryable=()):
        """One feed tick with a stub corpus; returns the filters each select used and the
        pages it sent to `run`. A filter's refused languages are one string when one is refused."""
        args = types.SimpleNamespace(
            state=root, outbox=root, campaign="test", prep_ct=215, server_ct=230, server_host="server",
            archive_host="archive", scan_server="http://scan", archives="/archives", alto_export="/cache",
            min_free_gb=50, max_ready=1000, max_unresolved=2000, max_pending=3, chunk_pages=100,
            reserve_mib=64, yiddish_budget=budget, german_budget=german_budget, german_pages=german_pages)
        filters, prepared = [], []
        selected = b""
        index = b""

        def command(argv, data=None):
            nonlocal selected, index
            if "tee" in argv and argv[-1].endswith("/index.jsonl"):
                index = data
            if "select" in argv:
                wanted = argv[argv.index("--language") + 1] if "--language" in argv else None
                refused = [argv[i + 1] for i, arg in enumerate(argv) if arg == "--exclude-language"]
                filters.append((wanted, refused[0] if len(refused) == 1 else tuple(refused) or None,
                                int(argv[argv.index("--count") + 1])))
                indexed = {json.loads(line)["doc_id"] for line in index.splitlines()}
                pool = [p for p in yiddish + others
                        if (wanted is None or p["lang"] == wanted) and p["lang"] not in refused
                        and p["doc_id"] in indexed]
                selected = "".join(json.dumps(p) + "\n" for p in pool[:filters[-1][2]]).encode()
            if "cat" in argv:
                return selected
            if "tee" in argv and argv[-1].endswith("/pages.jsonl"):
                prepared.extend(json.loads(line) for line in data.splitlines())
            if "-c" in argv and "retryable_ids" in argv[-1]:
                return json.dumps(list(retryable)).encode()
            return b"[]" if "-c" in argv else b""

        def remote(host, argv, data=None):
            if "--inventory" in argv:
                return json.dumps({"fresh_pairs": 0, "unresolved_native": 0, "documents": [],
                                   "free_bytes": 10**12, "ready": 0}).encode()
            if "lvs" in argv:
                return json.dumps({"report": [{"lv": [{"lv_size": 10**12, "data_percent": 0}]}]}).encode()
            if data == b"":
                return json.dumps({"available_archives": ["xx_test_ver01.tar.bz2"]}).encode()
            return json.dumps({"cached": [json.loads(line)["doc_id"] for line in data.splitlines()],
                               "missing": [], "pending": []}).encode()

        rows = [{"doc_id": doc, "batch": "xx_test", "batch_version": 1, "bytes": 12, "sha256": "a" * 64}
                for doc in [DOC] + [p["doc_id"] for p in yiddish + others if p["doc_id"] != DOC]]
        listing = b"".join(json.dumps(row).encode() + b"\n" for row in rows)
        with mock.patch.object(driver, "remote", side_effect=remote), \
                mock.patch.object(driver, "command", side_effect=command), \
                mock.patch.object(driver.shutil, "disk_usage", return_value=types.SimpleNamespace(free=10**12)), \
                mock.patch.object(driver.urllib.request, "urlopen",
                                  return_value=contextlib.closing(io.BytesIO(listing))), \
                contextlib.redirect_stdout(io.StringIO()):
            driver.tick(args)
        return filters, [p["doc_id"] for p in prepared]

    def test_yiddish_budget_makes_small_chunks_of_its_own_and_never_more(self):
        yiddish = [{"doc_id": f"sn1_1900-01-02_ed-1_seq-{n}", "lang": "yid", "damage_rate": .9}
                   for n in range(1, 40)]
        others = [{"doc_id": f"sn2_1900-01-02_ed-1_seq-{n}", "lang": "srp", "damage_rate": .8}
                  for n in range(1, 40)]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            filters, pages = self.budget_tick(root, 30, yiddish, others)
            self.assertEqual(filters, [("yid", None, 30)])
            self.assertEqual(pages, [p["doc_id"] for p in yiddish[:30]])
            # The budget survives a restart: the next ticks take everything else, without Yiddish.
            filters, pages = self.budget_tick(root, 30, yiddish[30:], others)
            self.assertEqual(filters, [(None, "yid", 100)])
            self.assertEqual(pages, [p["doc_id"] for p in others])

    def test_failed_budget_reservation_can_retry_without_spending_another_page(self):
        page = {**frozen_page(), "lang": "yid", "damage_rate": .9}
        other = {"doc_id": "sn2_1900-01-02_ed-1_seq-1", "lang": "srp", "damage_rate": .8}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "yiddish-budget.json").write_text(json.dumps([DOC]))
            retryable = {quality.scan_quarantine.source_id(page)}
            filters, chosen = self.budget_tick(root, 1, [page], [other], retryable=retryable)
            self.assertEqual(filters, [("yid", None, 1)])
            self.assertEqual(chosen, [DOC])
            self.assertEqual(json.loads((root / "yiddish-budget.json").read_text()), [DOC])
            filters, chosen = self.budget_tick(root, 1, [page], [other], retryable=())
            self.assertEqual(filters, [(None, "yid", 100)])
            self.assertEqual(chosen, [other["doc_id"]])
            self.assertEqual(json.loads((root / "yiddish-budget.json").read_text()), [DOC])

    def test_the_budget_goes_ahead_of_a_source_request(self):
        yiddish = [{"doc_id": "sn1_1900-01-02_ed-1_seq-1", "lang": "yid", "damage_rate": .9}]
        other = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1, "bytes": 12, "sha256": "a" * 64,
                 "lang": "srp", "damage_rate": .8}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            spec = source.specification(DOC, 1, "test")
            source.save(root / "requests" / (source.digest(spec) + ".json"),
                        {"request_id": source.digest(spec), "spec": spec, "pages": [other],
                         "pages_sha256": source.digest([other])})
            filters, pages = self.budget_tick(root, 30, yiddish, [other])
            self.assertEqual(filters, [("yid", None, 30)])
            self.assertEqual(pages, [yiddish[0]["doc_id"]])
            # With the budget spent the request carries on, without Yiddish.
            filters, pages = self.budget_tick(root, 1, [], [other])
            self.assertEqual(filters, [(None, "yid", 100)])
            self.assertEqual(pages, [DOC])

    def test_a_source_request_never_spends_past_the_budget(self):
        page = {"doc_id": DOC, "batch": "xx_test", "batch_version": 1,
                "bytes": 12, "sha256": "a" * 64, "lang": "yid", "damage_rate": .8}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "requests").mkdir()
            (root / "yiddish-budget.json").write_text(json.dumps(["spent"]))
            spec = source.specification(page["doc_id"], 1, "test")
            source.save(root / "requests" / (source.digest(spec) + ".json"),
                        {"request_id": source.digest(spec), "spec": spec, "pages": [page],
                         "pages_sha256": source.digest([page])})
            filters, chosen = self.budget_tick(root, 1, [page], [])
            self.assertEqual(filters, [(None, "yid", 100), (None, "yid", 100)])
            self.assertEqual(chosen, [])
            self.assertEqual(json.loads((root / "yiddish-budget.json").read_text()), ["spent"])

    def test_no_yiddish_left_falls_through_to_the_normal_order(self):
        others = [{"doc_id": f"sn2_1900-01-02_ed-1_seq-{n}", "lang": "srp", "damage_rate": .8}
                  for n in range(1, 4)]
        with tempfile.TemporaryDirectory() as folder:
            filters, pages = self.budget_tick(Path(folder), 30, [], others)
            self.assertEqual(filters, [("yid", None, 30), (None, "yid", 100)])
            self.assertEqual(pages, [p["doc_id"] for p in others])

    def test_german_budget_comes_after_a_spent_yiddish_budget_and_never_more(self):
        german = [{"doc_id": f"sn3_1900-01-02_ed-1_seq-{n}", "lang": "deu", "damage_rate": .9}
                  for n in range(1, 9)]
        others = [{"doc_id": f"sn2_1900-01-02_ed-1_seq-{n}", "lang": "srp", "damage_rate": .8}
                  for n in range(1, 4)]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "yiddish-budget.json").write_text(json.dumps(["spent"]))
            filters, pages = self.budget_tick(root, 1, [], german + others, german_budget=5)
            self.assertEqual(filters, [("deu", None, 5)])
            self.assertEqual(pages, [p["doc_id"] for p in german[:5]])
            self.assertEqual(json.loads((root / "german-budget.json").read_text()),
                             sorted(p["doc_id"] for p in german[:5]))
            self.assertEqual(json.loads((root / "yiddish-budget.json").read_text()), ["spent"])
            # Spent: the normal order carries on without Yiddish or German.
            filters, pages = self.budget_tick(root, 1, [], german[5:] + others, german_budget=5)
            self.assertEqual(filters, [(None, ("yid", "deu"), 100)])
            self.assertEqual(pages, [p["doc_id"] for p in others])

    def test_german_pages_limit_the_german_budget_to_a_list(self):
        german = [{"doc_id": f"sn3_1900-01-02_ed-1_seq-{n}", "lang": "deu", "damage_rate": .9}
                  for n in range(1, 9)]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            listed = root / "german-pages.txt"
            listed.write_text("".join(p["doc_id"] + "\n" for p in german[4:6]))
            filters, pages = self.budget_tick(root, None, [], german, german_budget=50, german_pages=listed)
            self.assertEqual(filters, [("deu", None, 50)])
            self.assertEqual(pages, [p["doc_id"] for p in german[4:6]])

    def test_without_a_budget_yiddish_keeps_its_place_in_the_normal_order(self):
        pages = [{"doc_id": f"sn{n}_1900-01-02_ed-1_seq-1", "lang": lang, "damage_rate": .8}
                 for n, lang in enumerate(("yid", "srp"), 1)]
        with tempfile.TemporaryDirectory() as folder:
            filters, chosen = self.budget_tick(Path(folder), None, pages[:1], pages[1:])
            self.assertEqual(filters, [(None, None, 100)])
            self.assertEqual(chosen, [p["doc_id"] for p in pages])

    def test_select_filters_by_language(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            index = root / "index.jsonl"
            docs = [f"sn{n}_1900-01-02_ed-1_seq-3" for n in range(1, 4)]
            index.write_text("".join(json.dumps({"doc_id": d, "batch": "xx_test", "bytes": 1,
                                                "sha256": "a", "format": "jp2"}) + "\n" for d in docs))
            curated = root / "curated/audit/damage/v2"
            curated.mkdir(parents=True)
            (curated / "xx_test_ver01.parquet").touch()
            rows = [{"doc_id": d, "lang": lang, "damage_rate": .5}
                    for d, lang in zip(docs, ("yid", "srp", "eng"))]
            pq = mock.Mock()
            pq.read_table.return_value.to_pylist.return_value = rows
            pa = types.ModuleType("pyarrow")
            pa.parquet = pq
            for filters, expected in (({"language": "yid"}, [docs[0]]), ({"exclude_language": ["yid"]}, [docs[1]]),
                                      ({"exclude_language": ["yid", "srp"]}, []),
                                      ({}, docs[:2])):
                a = types.SimpleNamespace(index=str(index), staged="", curated=str(root / "curated"),
                                          non_english=True, order="damage", count=0,
                                          out=str(root / "out.jsonl"), scan_server=None, **filters)
                with self.subTest(filters=filters), \
                        mock.patch.dict("sys.modules", {"pyarrow": pa, "pyarrow.parquet": pq}):
                    quality.cmd_select(a)
                    chosen = [json.loads(line)["doc_id"] for line in Path(a.out).read_text().splitlines()]
                    self.assertEqual(chosen, expected)

    def test_preflight_and_locked_recheck_preserve_partial_jobs_and_hash_bound_audit(self):
        with tempfile.TemporaryDirectory() as folder:
            share = Path(folder)
            root = share / "qwen/qwen-test"
            unit = root / "units" / DOC
            unit.mkdir(parents=True)
            raw = json.dumps({"id": "a" * 64, "doc_id": DOC, "prompt_version": 1}).encode()
            (unit / "qwen.json").write_bytes(raw)
            submitted = root / "units" / "sn2_1900-01-02_ed-1_seq-3"
            submitted.mkdir()
            job = {"id": "b" * 64, "doc_id": submitted.name, "prompt_version": 1}
            old_raw = json.dumps(job).encode()
            (submitted / "qwen.json").write_bytes(old_raw)
            (submitted / "submitted").write_text("already published\n")
            old_audit = json.dumps({job["id"]: {"job_sha256": hashlib.sha256(old_raw).hexdigest(),
                                                "workunit_id": 42}}).encode()
            (root / "audit.json").write_bytes(old_audit)
            plan = share / "plan.json"
            plan.write_text(json.dumps({"engine": {"image": "image", "llama": "llama",
                                                  "weights": "weights"}}))
            argv = ["qwen_submit.py", "--share", str(share), "--project", str(share),
                    "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(share),
                    "--max-pages", "1", "--max-ready", "200"]
            state = {"ready": 5000, "fresh_pairs": 99, "unresolved_native": 100,
                     "free_bytes": 100 * 1024**3, "app_versions": [], "min_version": 0}
            output = io.StringIO()
            with mock.patch.object(submit.subprocess, "run") as run, \
                    mock.patch.object(submit, "inventory", return_value=state), \
                    mock.patch.object(sys, "argv", [*argv, "--preflight"]), \
                    contextlib.redirect_stdout(output):
                submit.main()
            run.assert_not_called()
            decision = json.loads(output.getvalue())
            self.assertEqual(decision["status"], "admitted")
            self.assertEqual(decision["pending_staged_units"], 1)
            self.assertEqual(decision["audit_sha256"], hashlib.sha256(old_audit).hexdigest())

            for preflight in (False, True):
                output = io.StringIO()
                with mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "inventory", return_value={**state, "fresh_pairs": 100}), \
                        mock.patch.object(sys, "argv", [*argv, *(["--preflight"] if preflight else [])]), \
                        contextlib.redirect_stdout(output), self.assertRaises(SystemExit) as caught:
                    submit.main()
                self.assertEqual(caught.exception.code, 75)
                self.assertEqual(json.loads(output.getvalue())["reason"], "fresh_original_watermark")
                run.assert_not_called()
                self.assertEqual((root / "audit.json").read_bytes(), old_audit)
                self.assertEqual((unit / "qwen.json").read_bytes(), raw)
                self.assertFalse((unit / "submitted").exists())

            (unit / "submitting").write_text("uncertain\n")
            with mock.patch.object(submit, "inventory", return_value=state), \
                    mock.patch.object(sys, "argv", [*argv, "--preflight"]), \
                    self.assertRaisesRegex(RuntimeError, "Uncertain Qwen"):
                submit.main()
            (unit / "submitting").unlink()
            (submitted / "qwen.json").write_bytes(old_raw + b"\n")
            with mock.patch.object(submit, "inventory", return_value=state), \
                    mock.patch.object(sys, "argv", [*argv, "--preflight"]), \
                    self.assertRaisesRegex(RuntimeError, "Previously audited"):
                submit.main()
            (submitted / "qwen.json").write_text(json.dumps({**job, "id": "c" * 64}))
            for preflight in (False, True):
                with mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "inventory", return_value={**state, "fresh_pairs": 100}), \
                        mock.patch.object(sys, "argv", [*argv, *(["--preflight"] if preflight else [])]), \
                        self.assertRaisesRegex(RuntimeError, "missing or identity changed"):
                    submit.main()
                run.assert_not_called()
                self.assertEqual((root / "audit.json").read_bytes(), old_audit)
            (submitted / "qwen.json").write_bytes(old_raw)
            sample = share / "sample.jsonl"
            sample.write_text(json.dumps({"doc_id": DOC}) + "\n")
            published = []

            def execute(args, **kwargs):
                if args[1:3] == ["qwen", "submit"] and not (unit / "submitted").exists():
                    published.append("a" * 64)
                    (unit / "submitted").write_text("new workunit\n")

            response = json.dumps({"a" * 64: {
                "job_sha256": hashlib.sha256(raw).hexdigest(), "workunit_id": 43}})
            for _ in range(2):
                with mock.patch.object(submit, "inventory", return_value=state), \
                        mock.patch.object(sys, "argv", [*argv, "--sample", str(sample)]), \
                        mock.patch.object(submit.subprocess, "run", side_effect=execute), \
                        mock.patch.object(submit.subprocess, "check_output", return_value=response):
                    submit.main()
            self.assertEqual(published, ["a" * 64])
            self.assertEqual(json.loads((root / "audit.json").read_bytes())[job["id"]],
                             json.loads(old_audit)[job["id"]])


if __name__ == "__main__":
    unittest.main()
