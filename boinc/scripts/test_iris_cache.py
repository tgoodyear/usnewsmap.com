"""usnm_iris persistent compile cache (#736): cache.py alone, then through the worker with fakes."""
from __future__ import annotations

import dataclasses
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_iris_bundle as tb  # noqa: E402
import test_iris_worker as t  # noqa: E402
from test_iris_worker import FakePipeline, c, w  # noqa: E402
import cache  # noqa: E402

PARTS = {"model": "m", "openvino": "2026.4.1"}
GiB = 2**30


class Disk:
    def __init__(self, free):
        self.free = free


class CacheTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="iris-cache-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.free = patch.object(cache.shutil, "disk_usage", return_value=Disk(100 * GiB))
        self.disk = self.free.start()
        self.addCleanup(self.free.stop)
        self.log = patch.object(cache, "log")
        self.log.start()
        self.addCleanup(self.log.stop)

    def acquire(self, parts=PARTS):
        diag = {}
        lease = cache.acquire(self.root, parts, diag)
        if lease is not None:
            self.addCleanup(cache.release, lease)
        return lease, diag

    def warm(self, files=(("model.blob", b"blob"), ("k1.cl_cache", b"k1"))):
        lease, _ = self.acquire()
        for name, data in files:
            (lease.directory / name).write_bytes(data)
        diag = {}
        cache.commit(lease, diag)
        cache.release(lease)
        self.assertEqual(diag["compile_cache_commit"], "ok")
        return lease

    def test_key_covers_every_part_and_is_stable(self):
        self.assertEqual(cache.key_for(PARTS), cache.key_for(dict(reversed(list(PARTS.items())))))
        self.assertNotEqual(cache.key_for(PARTS), cache.key_for({**PARTS, "openvino": "2026.4.2"}))
        self.assertRegex(cache.key_for(PARTS), "^[0-9a-f]{32}$")

    def test_missing_or_unwritable_root_runs_without_cache(self):
        diag = {}
        self.assertIsNone(cache.acquire(self.root / "absent", PARTS, diag))
        self.assertEqual(diag["compile_cache"], "unavailable")
        (self.root / "file").write_text("x")
        self.assertIsNone(cache.acquire(self.root / "file", PARTS, diag))

    def test_cold_then_warm(self):
        lease, diag = self.acquire()
        self.assertEqual((diag["compile_cache"], diag["compile_cache_files"]), ("cold", 0))
        cache.release(lease)
        self.warm()
        lease, diag = self.acquire()
        self.assertEqual((diag["compile_cache"], diag["compile_cache_files"], diag["compile_cache_bytes"]), ("warm", 2, 6))
        manifest = json.loads(lease.manifest.read_text())
        self.assertEqual(sorted(manifest["files"]), ["k1.cl_cache", "model.blob"])

    def test_second_user_runs_without_cache(self):
        first, _ = self.acquire()
        self.assertIsNotNone(first)
        lease, diag = self.acquire()
        self.assertIsNone(lease)
        self.assertEqual(diag["compile_cache"], "busy")

    def test_unregistered_and_changed_files_are_dropped(self):
        lease = self.warm()
        (lease.directory / "partial.cl_cache").write_bytes(b"killed mid-write")
        (lease.directory / "k1.cl_cache").write_bytes(b"changed")
        (lease.directory / "sub").mkdir()
        os.symlink("/etc/passwd", lease.directory / "link")
        lease, diag = self.acquire()
        self.assertEqual(sorted(os.listdir(lease.directory)), ["model.blob"])
        self.assertEqual((diag["compile_cache"], diag["compile_cache_files"], diag["compile_cache_dropped"]), ("warm", 1, 4))

    def test_corrupt_manifest_means_cold(self):
        lease = self.warm()
        lease.manifest.write_text("{not json")
        lease, diag = self.acquire()
        self.assertEqual(diag["compile_cache"], "cold")
        self.assertEqual(os.listdir(lease.directory), [])
        self.assertFalse(lease.manifest.exists())

    def test_other_keys_and_strays_are_pruned(self):
        old = self.warm()
        (self.root / "stray").write_text("x")
        lease, diag = self.acquire({**PARTS, "openvino": "new"})
        self.assertEqual(diag["compile_cache"], "cold")
        self.assertEqual(sorted(os.listdir(self.root)), sorted([cache.LOCK, lease.key]))
        self.assertFalse(old.directory.exists())

    def test_over_size_cache_is_wiped(self):
        self.warm()
        with patch.object(cache, "MAX_CACHE_BYTES", 5):
            lease, diag = self.acquire()
        self.assertEqual((diag["compile_cache"], os.listdir(lease.directory)), ("cold", []))

    def test_commit_over_limit_discards(self):
        lease, _ = self.acquire()
        (lease.directory / "model.blob").write_bytes(b"x" * 10)
        diag = {}
        with patch.object(cache, "MAX_CACHE_BYTES", 5):
            cache.commit(lease, diag)
        self.assertEqual(diag["compile_cache_commit"], "over_limit")
        self.assertFalse(lease.directory.exists())

    def test_free_space_floor(self):
        self.disk.return_value = Disk(cache.MIN_FREE_COLD_BYTES - 1)
        lease, diag = self.acquire()
        self.assertIsNone(lease)
        self.assertEqual(diag["compile_cache"], "no_space")
        self.disk.return_value = Disk(100 * GiB)
        self.warm()
        self.disk.return_value = Disk(cache.MIN_FREE_WARM_BYTES)
        lease, diag = self.acquire()
        self.assertEqual(diag["compile_cache"], "warm")
        cache.release(lease)
        self.disk.return_value = Disk(cache.MIN_FREE_WARM_BYTES - 1)
        lease, diag = self.acquire()
        self.assertEqual(diag["compile_cache"], "no_space")
        self.assertEqual(len(os.listdir(self.root / diag["compile_cache_key"])), 2)

    def test_discard(self):
        lease = self.warm()
        cache.discard(lease)
        self.assertFalse(lease.directory.exists() or lease.manifest.exists())


class CachingPipeline(FakePipeline):
    """Writes a blob at load and a kernel per generate into CACHE_DIR, like OpenVINO does."""
    seen: list = []

    def __init__(self, model, device, **props):
        self.dir = props.get("CACHE_DIR")
        CachingPipeline.seen.append(sorted(os.listdir(self.dir)) if self.dir else None)
        super().__init__(model, device, **props)
        if self.dir and not os.path.exists(os.path.join(self.dir, "model.blob")):
            Path(self.dir, "model.blob").write_bytes(b"blob")

    def generate(self, prompt, **kw):
        if self.dir:
            n = len([k for k in FakePipeline.calls if k[0] == "generate"])
            kernel = Path(self.dir, f"k{n}.cl_cache")
            if not kernel.exists():
                kernel.write_bytes(b"kernel")
        return super().generate(prompt, **kw)


class WorkerCacheTest(unittest.TestCase):
    n = 3
    run_worker = t.WorkerTest.run_worker
    run_bundle = tb.BundleWorkerTest.run_bundle

    def setUp(self):
        tb.BundleWorkerTest.setUp(self)
        self.paths = dataclasses.replace(self.paths, cache=self.paths.work.parent / "cache")
        self.paths.cache.mkdir()
        CachingPipeline.seen = []
        sys.modules["openvino_genai"].VLMPipeline = CachingPipeline
        disk = patch.object(cache.shutil, "disk_usage", return_value=Disk(100 * GiB))
        disk.start()
        self.addCleanup(disk.stop)
        quiet = patch.object(cache, "log")
        quiet.start()
        self.addCleanup(quiet.stop)

    def key_dir(self, result):
        return self.paths.cache / result["results"][0]["diagnostics"]["compile_cache_key"]

    def test_bundle_cold_then_warm_through_cache(self):
        first = self.run_bundle()
        self.assertEqual(first["status"], "ok")
        d0, last = first["results"][0]["diagnostics"], first["results"][-1]["diagnostics"]
        self.assertEqual(d0["compile_cache"], "cold")
        self.assertEqual((last["compile_cache_commit"], last["compile_cache_new_files"]), ("ok", 4))
        load = FakePipeline.calls[0]
        self.assertEqual(load[3], {"INFERENCE_PRECISION_HINT": "f16", "CACHE_DIR": str(self.key_dir(first))})
        FakePipeline.calls = []
        second = self.run_bundle()
        self.assertEqual(second["results"][0]["diagnostics"]["compile_cache"], "warm")
        self.assertEqual(CachingPipeline.seen[-1], ["k0.cl_cache", "k1.cl_cache", "k2.cl_cache", "model.blob"])
        self.assertEqual(second["results"][-1]["diagnostics"]["compile_cache_new_files"], 0)

    def test_generate_exception_discards_cache(self):
        self.run_bundle()

        def gen(self_, prompt, **kw):
            Path(self_.dir, "half.cl_cache").write_bytes(b"x")
            raise RuntimeError("gpu hang")
        with patch.object(CachingPipeline, "generate", gen):
            result = self.run_bundle()
        self.assertEqual(result["results"][-1]["diagnostics"]["compile_cache_commit"], "discarded")
        self.assertFalse(self.key_dir(result).exists())
        self.assertEqual(os.listdir(self.paths.cache), [cache.LOCK])

    def test_failed_warm_load_falls_back_to_cold_without_cache(self):
        self.run_bundle()
        real = CachingPipeline.__init__

        def init(self_, model, device, **props):
            if "CACHE_DIR" in props:
                raise RuntimeError("corrupt blob")
            real(self_, model, device, **props)
        FakePipeline.calls = []
        with patch.object(CachingPipeline, "__init__", init):
            result = self.run_bundle()
        self.assertEqual(result["status"], "ok")
        diag = result["results"][0]["diagnostics"]
        self.assertEqual(diag["compile_cache"], "load_failed")
        self.assertIn("corrupt blob", diag["compile_cache_load_error"])
        self.assertEqual(FakePipeline.calls[0][3], {"INFERENCE_PRECISION_HINT": "f16"})
        self.assertFalse(self.key_dir(result).exists())

    def test_load_failure_without_cache_still_fails_the_unit(self):
        FakePipeline.load_error = RuntimeError("boom")
        result = self.run_bundle()
        self.assertEqual({r["error"]["code"] for r in result["results"]}, {"pipeline_load"})

    def test_busy_cache_runs_cold_without_it(self):
        holder = cache.acquire(self.paths.cache, PARTS, {})
        self.addCleanup(cache.release, holder)
        result = self.run_bundle()
        self.assertEqual(result["status"], "ok")
        self.assertEqual(result["results"][0]["diagnostics"]["compile_cache"], "busy")
        self.assertEqual(FakePipeline.calls[0][3], {"INFERENCE_PRECISION_HINT": "f16"})

    def test_v1_unit_uses_and_commits_cache(self):
        page = t.jpeg(825, 429, pad=64)
        self.job = t.job_for(page)
        (self.paths.work / "iris.json").write_bytes(c.encode_job(self.job))
        (self.paths.work / "page.jpg").write_bytes(page)
        (self.paths.work / "pages.pack").unlink()
        status, result = self.run_worker()
        self.assertEqual(status, 0, result["error"])
        diag = result["diagnostics"]
        self.assertEqual((diag["compile_cache"], diag["compile_cache_commit"]), ("cold", "ok"))
        status, result = self.run_worker()
        self.assertEqual(result["diagnostics"]["compile_cache"], "warm")


if __name__ == "__main__":
    unittest.main()
