"""The receipt of a native build's challenge run (#1418, native_challenge_receipt.py)."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("receipt", Path(__file__).with_name("native_challenge_receipt.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

ROOT = Path(__file__).resolve().parents[2]
PREPARED = json.loads((ROOT / "boinc/project/challenges/eng-d8cd7279.prepared.json").read_text())
IMAGE = "sha256:" + "a" * 64
BASE = "sha256:" + "b" * 64
KWARGS = dict(platform="arm64-apple-darwin", run=7, commit="c" * 40, artifact_sha256="d" * 64,
              machine="macOS 15", max_cer=0.08734328574534386, result_sha256="e" * 64)
IDENTITY = {"elapsed_secs": 100, "engine_base": BASE, "engine_image": IMAGE}


def result(edit_rate=0.07):
    crops = [{"crop_id": crop, "source": source, "ref_chars": counts["ref_chars"],
              "char_edits": round(counts["ref_chars"] * edit_rate), "ref_words": counts["ref_words"],
              "word_edits": 0, "elapsed_ms": 1, "profile": "eng"}
             for crop, (source, counts) in PREPARED["crops"].items()]
    return {"manifest_sha256": PREPARED["manifest_sha256"], "limit": None, "skipped": [], "crops": crops}


class Receipt(unittest.TestCase):
    def test_a_passing_run_makes_a_valid_registry_entry(self):
        out = module.receipt(result(), IDENTITY, PREPARED, **KWARGS)
        self.assertTrue(out["pass"])
        registry = json.loads((ROOT / "boinc/project/native-build-trust.json").read_text())
        merged = module.merge(registry, out["build"])
        self.assertIn(IMAGE, merged["builds"])
        self.assertEqual(list(merged["builds"]), sorted(merged["builds"]))
        run = merged["builds"][IMAGE]["challenges"][0]
        self.assertEqual(run["crops"], 30)
        self.assertEqual(run["ref_chars"], sum(c["ref_chars"] for _, c in PREPARED["crops"].values()))

    def test_a_run_over_the_threshold_fails(self):
        self.assertFalse(module.receipt(result(0.09), IDENTITY, PREPARED, **KWARGS)["pass"])

    def test_it_refuses_a_partial_or_altered_run(self):
        for change in (
            lambda r: r["crops"].pop(),
            lambda r: r.update(skipped=[{"crop_id": "x", "reason": "engine-error"}]),
            lambda r: r.update(limit=3),
            lambda r: r.update(manifest_sha256="0" * 64),
            lambda r: r["crops"][0].update(ref_chars=1),
            lambda r: r["crops"].append(copy.deepcopy(r["crops"][0])),
        ):
            bad = result()
            change(bad)
            with self.assertRaises(ValueError):
                module.receipt(bad, IDENTITY, PREPARED, **KWARGS)

    def test_merge_refuses_to_rewrite_a_recorded_build(self):
        registry = json.loads((ROOT / "boinc/project/native-build-trust.json").read_text())
        build = module.receipt(result(), IDENTITY, PREPARED, **KWARGS)["build"]
        merged = module.merge(registry, copy.deepcopy(build))
        build[IMAGE]["challenges"][0]["char_edits"] += 1
        with self.assertRaises(ValueError):
            module.merge(merged, build)

    def test_the_threshold_comes_from_the_recorded_runs_of_the_same_challenge(self):
        registry = json.loads((ROOT / "boinc/project/native-build-trust.json").read_text())
        self.assertEqual(module.threshold(registry, PREPARED["manifest_sha256"]), 0.08734328574534386)
        self.assertIsNone(module.threshold(registry, "0" * 64))

    def test_the_command_writes_the_receipt_and_merges_only_a_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            (tmp / "identity.json").write_text(json.dumps(IDENTITY))
            registry = tmp / "registry.json"
            registry.write_text((ROOT / "boinc/project/native-build-trust.json").read_text())
            for rate, code in ((0.09, 1), (0.07, 0)):
                (tmp / "result.json").write_text(json.dumps(result(rate)))
                argv = ["--result", str(tmp / "result.json"), "--identity", str(tmp / "identity.json"),
                        "--prepared", str(ROOT / "boinc/project/challenges/eng-d8cd7279.prepared.json"),
                        "--platform", "arm64-apple-darwin", "--run", "7", "--commit", "c" * 40,
                        "--artifact-sha256", "d" * 64, "--machine", "macOS 15",
                        "--out", str(tmp / "receipt.json"), "--merge", str(registry), "--registry", str(registry)]
                with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(module.main(argv), code)
                self.assertEqual(IMAGE in json.loads(registry.read_text())["builds"], code == 0)


if __name__ == "__main__":
    unittest.main()
