"""Regression tests for real-source Qwen replenishment; no GPU, Pillow or network."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
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
                                          outbox=Path(folder), min_free_gb=50, max_ready=1000, max_pending=3)
                inv = {"ready": ready, "documents": [], "free_bytes": 100 * 1024**3}
                pool = {"report": [{"lv": [{"lv_size": str(100 * 1024**3), "data_percent": str(pool_pct)}]}]}
                with mock.patch.object(driver, "remote", side_effect=[json.dumps(inv), json.dumps(pool)]) as remote, \
                        mock.patch.object(driver, "command", side_effect=AssertionError("preparation started")):
                    driver.tick(a)
                self.assertEqual(remote.call_count, 2)


if __name__ == "__main__":
    unittest.main()
