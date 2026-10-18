"""Tests for the checksum-locked EffOCR benchmark adapter."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from effocr_trial import collect_samples, save_trial_outputs, stage_model_files, tree_digest


class EffOCRTrialTests(unittest.TestCase):
    def test_tree_digest_is_independent_of_input_order(self):
        first = [
            {"path": "b.onnx", "sha256": "b" * 64},
            {"path": "a.onnx", "sha256": "a" * 64},
        ]
        second = list(reversed(first))
        self.assertEqual(tree_digest(first), tree_digest(second))

    def test_tree_digest_binds_paths_and_file_hashes(self):
        first = [{"path": "a.onnx", "sha256": "a" * 64}]
        changed = [{"path": "b.onnx", "sha256": "a" * 64}]
        self.assertNotEqual(tree_digest(first), tree_digest(changed))

    def test_runtime_staging_contains_only_the_locked_model_files(self):
        paths = [
            "line.onnx",
            "localizer.onnx",
            *(
                f"{kind}_recognizer/{filename}"
                for kind in ("word", "char")
                for filename in ("enc_best.onnx", "ref.index", "ref.txt")
            ),
        ]
        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as temporary:
            root = Path(temporary)
            model_dir = root / "model"
            runtime_dir = root / "runtime"
            for relative in paths + ["word_recognizer/unlocked.onnx"]:
                source = model_dir / relative
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_bytes(b"model")
            runtime_dir.mkdir()

            stage_model_files([{"path": relative} for relative in paths], model_dir, runtime_dir)

            staged = {path.relative_to(runtime_dir).as_posix() for path in runtime_dir.rglob("*") if path.is_symlink()}
            self.assertEqual(staged, {
                "line/line.onnx",
                "localizer/localizer.onnx",
                *(
                f"{kind}_recognizer/{filename}"
                for kind in ("word", "char")
                for filename in ("enc_best.onnx", "ref.index", "ref.txt")
            ),
            })

    def test_failed_inference_writes_partial_outputs_and_receipt_then_exits(self):
        samples = [{"id": "first"}, {"id": "broken"}, {"id": "not-run"}]

        def infer_one(sample):
            if sample["id"] == "broken":
                raise RuntimeError("failed at /Users/private/model/file.onnx")
            return "recognized text"

        outputs, timings, failures, _wall_seconds = collect_samples(samples, infer_one)
        self.assertEqual(outputs, [{"id": "first", "text": "recognized text"}])
        self.assertEqual(len(timings), 1)
        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as temporary:
            out_dir = Path(temporary)
            with self.assertRaises(SystemExit):
                save_trial_outputs(out_dir, outputs, {"timings": timings}, len(samples), failures)

            receipt = json.loads((out_dir / "run.json").read_text(encoding="utf-8"))
            rows = [json.loads(line) for line in (out_dir / "outputs.jsonl").read_text(encoding="utf-8").splitlines()]
            self.assertEqual(rows, outputs)
            self.assertEqual(receipt["samples"], 1)
            self.assertEqual(receipt["missing_outputs"], 2)
            self.assertFalse(receipt["complete"])
            self.assertEqual(receipt["sample_failures"][0]["id"], "broken")
            self.assertEqual(receipt["sample_failures"][0]["error_type"], "RuntimeError")
            self.assertNotIn("/Users/private", receipt["sample_failures"][0]["error_message"])
            self.assertTrue(receipt["output_sha256"])


if __name__ == "__main__":
    unittest.main()
