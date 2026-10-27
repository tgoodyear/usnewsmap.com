"""Synthetic preparation, submission recovery and untrusted-adapter checks."""
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch

import campaign as c
import check as check
from test_worker import TrainingContract
from worker import sha


class CampaignTest(unittest.TestCase):
    def sources(self, cache):
        for source in c.SOURCES:
            directory = cache / source
            (directory / "LICENSES").mkdir(parents=True)
            (directory / "LICENSES/SOURCE.json").write_text('{"attribution":"Synthetic fixture"}')
            image = f"SYNTHETIC PIXELS {source}".encode()
            (directory / "one.png").write_bytes(image)
            row = {"crop_id": source + "-one", "image": "one.png", "lang": "eng",
                   "group": source, "for_scoring": True, "transcription": f"SYNTHETIC {source}\u0085\u2028\u2029",
                   "transcriber": "synthetic", "reviewer": "synthetic-reviewer"}
            meta = {"crop_id": row["crop_id"], "source_dataset": source, "page_id": source,
                    "source_url": "https://example.org/synthetic", "source_license": "CC-BY-4.0",
                    "crop_sha256": sha(image), "source_attribution": "Synthetic fixture"}
            (directory / "crops.jsonl").write_text(json.dumps(row) + "\n")
            (directory / "manifest.jsonl").write_text(json.dumps(meta) + "\n")

    def test_private_disjoint_preparation_replay_submission_and_uncertainty(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.sources(root / "cache")
            out = root / "campaigns/pilot"
            result = c.prepare(root / "cache", out, "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64)
            replay = c.prepare(root / "cache", out, "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "a" * 64)
            self.assertFalse(result["replayed"])
            self.assertTrue(replay["replayed"])
            self.assertEqual(len(result["jobs"]), 2)
            other = c.prepare(root / "cache", root / "campaigns/other",
                              "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "b" * 64)
            self.assertTrue(set(result["jobs"]).isdisjoint(other["jobs"]))
            self.assertEqual(c.collect(out, root / "seen")["returned_trials"], 0)
            (root / "seen/.candidate.unpublished").mkdir(parents=True)
            self.assertEqual(len(c.collect(out, root / "seen")["pending_trials"]), 2)
            import zipfile
            with zipfile.ZipFile(out / "training.zip") as archive:
                manifest = json.loads(archive.read("training.json"))
                self.assertTrue(manifest["examples"][0]["text"].endswith("\u0085\u2028\u2029"))
                self.assertIn(b"Synthetic fixture", archive.read("NOTICE.txt"))
            split = json.loads((out / "split.json").read_text())
            self.assertEqual(split["counts"], {"train": 1, "validation": 1, "test": 1})
            project = root / "project"
            (project / "download").mkdir(parents=True)
            calls = []
            def stage(args, **kwargs):
                return str(project / "download" / args[1])
            def create(args, **kwargs):
                calls.append(args)
            with patch.object(c.subprocess, "check_output", side_effect=stage), \
                    patch.object(c.subprocess, "run", side_effect=create):
                self.assertEqual(c.submit(out, project), {"submitted": 2})
                self.assertEqual(c.submit(out, project), {"submitted": 0})
            self.assertEqual(len(calls), 2)
            for args in calls:
                self.assertIn("usnm_qwen_train", args)
                self.assertEqual(args[args.index("--min_quorum") + 1], "1")
                self.assertEqual(args[args.index("--rsc_disk_bound") + 1], "42949672960")
            unit = out / "units" / result["jobs"][0]
            (unit / "submitted").rename(unit / "submitting")
            with self.assertRaisesRegex(ValueError, "uncertain submission"):
                c.submit(out, project)
            (unit / "train.json").write_text("{}")
            with self.assertRaises(ValueError):
                c.submit(out, project)

    def adapter(self, rank):
        header, offset = {}, 0
        for name, shape in check.tensor_shapes(rank).items():
            end = offset + shape[0] * shape[1] * 4
            header[name] = {"dtype": "F32", "shape": shape, "data_offsets": [offset, end]}
            offset = end
        raw = json.dumps(header).encode()
        return struct.pack("<Q", len(raw)) + raw + struct.pack("<f", 0.001) * (offset // 4)

    def test_structural_check_quarantines_never_promotes(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            draft = root / "draft"
            draft.mkdir()
            job, _ = TrainingContract().job(draft)
            identity = sha((draft / "train.json").read_bytes())
            unit = root / "campaigns/pilot/units" / identity
            unit.parent.mkdir(parents=True)
            draft.rename(unit)
            (unit / "submitted").touch()
            adapter = self.adapter(4)
            report = {**job, "job_sha256": identity, "losses": [2.0, 1.9],
                      "adapter_sha256": sha(adapter), "adapter_bytes": len(adapter),
                      "elapsed_seconds": 10.0, "peak_gpu_bytes": 6_000_000_000,
                      "gpu": "synthetic", "torch": "2.8.0",
                      "assigned_gpu": 1,
                      "candidate_status": "untrusted-requires-held-out-evaluation"}
            raw = json.dumps(report).encode()
            check.validate(adapter, raw, unit)
            (root / "adapter").write_bytes(adapter)
            (root / "report").write_bytes(raw)
            args = (root / "adapter", root / "report", root / "campaigns", root / "seen")
            self.assertEqual(check.quarantine(*args)["status"], "quarantined")
            self.assertEqual(check.quarantine(*args)["status"], "quarantined")
            self.assertEqual(len(list((root / "seen").iterdir())), 1)
            for key, value in (("base_revision", "main"), ("losses", [float("nan")] * 2),
                               ("candidate_status", "promoted"), ("job_sha256", "0" * 64)):
                with self.subTest(key=key), self.assertRaises(ValueError):
                    check.validate(adapter, json.dumps({**report, key: value}).encode(), unit)
            with self.assertRaises(ValueError):
                check.check_tensors(adapter[:-4] + struct.pack("<f", float("inf")), 4)
            with self.assertRaises(ValueError):
                check.check_tensors(adapter, 8)
            with self.assertRaises(ValueError):
                check.check_tensors(adapter + b"trailing", 4)


if __name__ == "__main__":
    unittest.main()
