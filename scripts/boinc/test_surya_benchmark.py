import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("surya_benchmark", Path(__file__).with_name("surya-benchmark.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class SuryaBenchmarkTests(unittest.TestCase):
    def test_checked_rejects_mismatch_and_escape(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "input").write_bytes(b"image")
            self.assertEqual(module.checked(root, "input", module.digest(b"image")), b"image")
            with self.assertRaises(ValueError):
                module.checked(root, "input", "0" * 64)
            child = root / "child"
            child.mkdir()
            with self.assertRaises(ValueError):
                module.checked(child, "../input", module.digest(b"image"))

    def test_export_requires_validator_and_keeps_missing_and_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "campaign"
            unit = root / "units" / "sample-1"
            unit.mkdir(parents=True)
            row = {"sample_id": "sample-1", "group": "english", "image_sha256": module.digest(b"image"),
                   "reference_sha256": module.digest(b"reference")}
            (root / "target-host.json").write_bytes(module.encode([10, True]))
            raw = module.encode([row])
            (root / "benchmark-inputs.json").write_bytes(raw)
            for name in ("benchmark-inputs.sha256", "benchmark-ready"):
                (root / name).write_text(module.digest(raw))
            (unit / "page.jpg").write_bytes(b"image")
            (unit / "reference.txt").write_bytes(b"reference")
            job = {"id": "job", "doc_id": "sample-1", "engine": {"image": "pinned"},
                   "input": {"sha256": row["image_sha256"]}, "cpu_modes": ["tiled"], "gpu_modes": list(module.MODES)}
            (unit / "surya.json").write_bytes(module.encode(job))
            seen = Path(directory) / "seen"
            result_dir = seen / "usnm_surya_job_host10_4321_1792340312_0_3"
            result_dir.mkdir(parents=True)
            result = {"job_id": "job", "engine": job["engine"], "runs": [
                {"mode": "tiled", "device": "cuda", "outcome": "ok", "engine_ms": 123},
                {"mode": "whole", "device": "cuda", "outcome": "timeout"}]}
            (result_dir / "result.json").write_bytes(module.encode(result))
            (result_dir / "text-tiled.txt").write_bytes(b"text\r\nwith\rnewline")
            other = seen / "usnm_surya_job_host11_4321_1792340312_0_3"
            other.mkdir()
            (other / "result.json").write_bytes(module.encode(result))
            with patch.object(module.subprocess, "run") as validator:
                out = Path(directory) / "out"
                receipt = module.collect(root, seen, out, "usnm-boinc", "cuda")
                validator.assert_called_once()
                self.assertEqual(receipt["returned"], 1)
                exported = json.loads((out / "cuda-tiled-english.jsonl").read_text())
                self.assertEqual(exported, {"id": "sample-1", "text": "text\r\nwith\rnewline"})
                self.assertEqual((out / "cuda-whole-english.jsonl").read_bytes(), b"")
                saved = json.loads((out / "receipt.json").read_text())
                self.assertEqual(saved["records"][1]["outcome"], "timeout")
            with patch.object(module.subprocess, "run", side_effect=RuntimeError("validator rejected")):
                with self.assertRaises(RuntimeError):
                    module.collect(root, seen, Path(directory) / "rejected", "usnm-boinc", "cuda")
            self.assertFalse((Path(directory) / "rejected").exists())
            (result_dir / "result.json").unlink()
            empty = Path(directory) / "empty"
            receipt = module.collect(root, seen, empty, "usnm-boinc", "cuda")
            self.assertEqual(receipt["missing"], ["sample-1"])
            for mode in module.MODES:
                self.assertEqual((empty / f"cuda-{mode}-english.jsonl").read_bytes(), b"")
            (root / "benchmark-ready").unlink()
            with self.assertRaises(FileNotFoundError):
                module.collect(root, seen, Path(directory) / "partial", "usnm-boinc", "cuda")


if __name__ == "__main__":
    unittest.main()
