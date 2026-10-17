"""usnm_iris v2 bundle contract and worker (#736), with the same fakes as test_iris_worker.py."""
from __future__ import annotations

import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_iris_worker as t  # noqa: E402
from test_iris_worker import FakePipeline, c, w  # noqa: E402
import bundle as b  # noqa: E402


def crops(n):
    return [dict(crop_id=f"crop-{i:04d}", doc_id="sn84024828/1887-06-18/ed-1/seq-1",
                 page=t.jpeg(825, 429, pad=64 + i), width=825, height=429) for i in range(n)]


class BundleContractTest(unittest.TestCase):
    def test_roundtrip_and_derived_jobs(self):
        job, pack = b.make_bundle(crops=crops(3), worker_image=t.IMAGE)
        self.assertEqual(b.parse_bundle_job_bytes(b.encode_bundle_job(job)), job)
        b.check_pack(job, pack)
        for i, (off, ln) in enumerate(b.crop_slices(job)):
            single = b.crop_job(job, i)
            self.assertEqual(c.validate_job(single), single)
            self.assertEqual(single["page_sha256"], c.hashlib.sha256(pack[off:off + ln]).hexdigest())
        self.assertEqual(b.timeout_seconds(job), 900 + 90 * 3)

    def test_bounds_and_duplicates(self):
        with self.assertRaises(c.ContractError):
            b.make_bundle(crops=crops(b.MAX_CROPS + 1), worker_image=t.IMAGE)
        one = crops(1)
        with self.assertRaises(c.ContractError):
            b.make_bundle(crops=one + one, worker_image=t.IMAGE)


class BundleWorkerTest(unittest.TestCase):
    n = 3
    run_worker = t.WorkerTest.run_worker

    def setUp(self):
        t.WorkerTest.setUp(self)
        self.bjob, pack = b.make_bundle(crops=crops(self.n), worker_image=t.IMAGE)
        (self.paths.work / "iris.json").write_bytes(b.encode_bundle_job(self.bjob))
        (self.paths.work / b.PACK_FILE).write_bytes(pack)
        (self.paths.work / "page.jpg").unlink()
        self.pack = pack

    def run_bundle(self, code_ok=(0,)):
        with patch("sys.stderr"):
            w.main(["--infer"], self.paths)
        return b.parse_bundle_result_bytes((self.paths.scratch / c.RESULT_FILE).read_bytes(), self.bjob)

    def test_one_load_many_generates_each_bound(self):
        result = self.run_bundle()
        self.assertEqual(result["status"], "ok")
        self.assertEqual([x["crop_id"] for x in result["results"]], [f"crop-{i:04d}" for i in range(self.n)])
        self.assertEqual([k[0] for k in FakePipeline.calls], ["load"] + ["generate"] * self.n)
        for r in result["results"]:
            self.assertEqual(r["status"], "ok")

    def test_load_failure_marks_every_crop(self):
        FakePipeline.load_error = RuntimeError("boom")
        result = self.run_bundle()
        self.assertEqual(result["status"], "error")
        self.assertEqual(len(result["results"]), self.n)
        self.assertEqual({r["error"]["code"] for r in result["results"]}, {"pipeline_load"})

    def test_token_limit_fails_only_that_crop(self):
        outputs = iter([(12, "STOP"), (512, "LENGTH"), (12, "STOP")])
        real = FakePipeline.generate

        def gen(self_, prompt, **kw):
            FakePipeline.tokens, FakePipeline.finish = next(outputs)
            return real(self_, prompt, **kw)
        with patch.object(FakePipeline, "generate", gen):
            result = self.run_bundle()
        self.assertEqual(result["status"], "partial")
        self.assertEqual([r["status"] for r in result["results"]], ["ok", "error", "ok"])
        self.assertEqual(result["results"][1]["error"]["code"], "token_limit_reached")

    def test_generate_exception_aborts_the_rest(self):
        def gen(self_, prompt, **kw):
            if len([k for k in FakePipeline.calls if k[0] == "generate"]) == 1:
                FakePipeline.calls.append(("generate", prompt, kw))
                raise RuntimeError("gpu hang")
            FakePipeline.calls.append(("generate", prompt, kw))
            return t.FakeOutput("text here", 5)
        with patch.object(FakePipeline, "generate", gen):
            result = self.run_bundle()
        self.assertEqual([r["status"] for r in result["results"]], ["ok", "error", "error"])
        self.assertEqual(result["results"][2]["error"]["code"], "bundle_aborted")
        self.assertEqual(result["status"], "partial")

    def test_bad_pack_is_a_bundle_error(self):
        (self.paths.work / b.PACK_FILE).write_bytes(self.pack[:-1] + b"\x00")
        result = self.run_bundle()
        self.assertEqual(result["status"], "error")
        self.assertEqual(result["results"][0]["error"]["code"], "pack_invalid")

    def test_result_must_match_job(self):
        result = self.run_bundle()
        other, _ = b.make_bundle(crops=crops(self.n)[::-1], worker_image=t.IMAGE)
        with self.assertRaises(c.ContractError):
            b.validate_bundle_result(result, other)
        result["results"].pop()
        with self.assertRaises(c.ContractError):
            b.validate_bundle_result(result, self.bjob)

    def test_supervisor_salvages_after_crash(self):
        rows = self.run_bundle()["results"][:2]
        crash = [sys.executable, "-c", (
            "import json,pathlib,sys;p=pathlib.Path(sys.argv[1]);p.mkdir(parents=True,exist_ok=True);"
            "(p/'iris-partial.json').write_text(sys.argv[2]);sys.exit(9)"), str(self.paths.scratch), json.dumps(rows)]
        result = w.supervise(self.paths, crash, 30)
        self.assertEqual(result["status"], "partial")
        self.assertEqual([r["status"] for r in result["results"]], ["ok", "ok", "error"])
        self.assertEqual(result["results"][2]["error"]["code"], "worker_crashed")
        self.assertEqual(b.validate_bundle_result(result, self.bjob), result)

    def test_partial_exits_zero(self):
        rows = self.run_bundle()["results"][:1]
        with patch.object(w, "supervise", return_value=w.complete_bundle(self.bjob, rows, w.new_result(), "x", "y", "z", 1, 2)), \
                patch("sys.stderr"):
            self.assertEqual(w.main([], self.paths), 0)

    def test_v1_unit_still_works(self):
        (self.paths.work / "iris.json").write_bytes(c.encode_job(self.job))
        (self.paths.work / "page.jpg").write_bytes(self.page)
        (self.paths.work / b.PACK_FILE).unlink()
        status, result = self.run_worker()
        self.assertEqual(status, 0)


if __name__ == "__main__":
    unittest.main()
