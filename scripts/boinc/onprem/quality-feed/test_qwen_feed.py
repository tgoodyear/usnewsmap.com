"""Regression tests for real-source Qwen replenishment; no GPU, Pillow or network."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile
import types
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name, HERE / (name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


archive = load("archive_alto")
quality = load("quality_feed")
submit = load("qwen_submit")
driver = load("qwen_feed")
DOC = "sn1_1900-01-02_ed-1_seq-3"


class ArchiveTest(unittest.TestCase):
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
                            "free_bytes": 100 * 1024**3}), \
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
                 "date": "1900-01-02", "lang": "rus", "scan": "keep/test.jpg", "damage_rate": 0.9}
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
        for pool_pct, ready in [(90, 0), (50, 1000)]:
            with self.subTest(pool_pct=pool_pct, ready=ready), tempfile.TemporaryDirectory() as folder:
                a = types.SimpleNamespace(state=Path(folder), campaign="qwen-test",
                                          server_host="boinc.example", server_ct=230, prep_ct=215,
                                          outbox=Path(folder), min_free_gb=50, max_ready=1000, max_pending=3,
                                          max_unresolved=2000, reserve_mib=64)
                inv = {"ready": ready, "fresh_pairs": ready // 2, "unresolved_native": ready // 2,
                       "documents": [], "free_bytes": 100 * 1024**3}
                pool = {"report": [{"lv": [{"lv_size": str(100 * 1024**3), "data_percent": str(pool_pct)}]}]}
                with mock.patch.object(driver, "remote", side_effect=[json.dumps(inv), json.dumps(pool)]) as remote, \
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
                                      max_pending=3, chunk_pages=100, max_unresolved=2000, reserve_mib=64)
            page = {"doc_id": DOC, "damage_rate": 0.9}
            inv = {"ready": 4000, "fresh_pairs": 0, "unresolved_native": 100,
                   "documents": [], "free_bytes": 100 * 1024**3}
            pool = {"report": [{"lv": [{"lv_size": str(200 * 1024**3), "data_percent": "50"}]}]}
            receipt = {"cached": [DOC], "missing": [], "pending": []}

            def run(args, data=None):
                if any("ocr_manifest.py" in arg for arg in args):
                    raise TimeoutError("scan-manifest generation must not block ready work")
                if args[-1].endswith("/selected.jsonl") and "cat" in args:
                    return json.dumps(page).encode()
                if "-c" in args:
                    return b"[]"
                return b""

            with mock.patch.object(driver, "remote", side_effect=[
                    json.dumps(inv), json.dumps(pool), json.dumps(receipt)]), \
                    mock.patch.object(driver, "command", side_effect=run) as command, \
                    mock.patch.object(driver.urllib.request, "urlopen") as source:
                source.return_value.__enter__.return_value.read.return_value = b"index"
                driver.tick(a)
            prepared = [call for call in command.call_args_list
                        if "run" in call.args[0] and any(
                            arg.endswith("/quality_feed.py") for arg in call.args[0])]
            self.assertEqual(len(prepared), 1)

    def test_native_gate_uses_fresh_original_pairs_but_retains_capacity_and_unresolved_bounds(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            plan = root / "plan.json"
            plan.write_text(json.dumps({"engine": {"image": "image", "llama": "llama", "weights": "weights"}}))
            argv = ["qwen_submit.py", "--share", str(root), "--project", str(root),
                    "--campaign", "qwen-test", "--engine-plan", str(plan), "--models", str(root)]
            base = {"ready": 4000, "fresh_pairs": 0, "unresolved_native": 100,
                    "free_bytes": 100 * 1024**3}
            for change, error in (({}, None), ({"fresh_pairs": 401}, "fresh-original"),
                                  ({"unresolved_native": 1901}, "unresolved-native"),
                                  ({"free_bytes": 51 * 1024**3}, "reserved-input")):
                with self.subTest(change=change), mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(submit, "inventory", return_value={**base, **change}), \
                        mock.patch.object(submit.subprocess, "run") as run, \
                        mock.patch.object(submit, "assert_submitted"), \
                        mock.patch.object(submit, "audit", return_value={}):
                    if error:
                        with self.assertRaisesRegex(RuntimeError, error):
                            submit.main()
                        run.assert_not_called()
                    else:
                        submit.main()
                        self.assertEqual(run.call_count, 2)


if __name__ == "__main__":
    unittest.main()
