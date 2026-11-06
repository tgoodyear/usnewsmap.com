"""Synthetic regression tests; run from the repository root with Python's unittest."""

import builtins
import io
import json
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import sys
import types
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parents[2] / "docs/reports/2026-11-gpu-studies/scripts"
JOBS = SCRIPTS / "jobs"


def module(name, **attributes):
    result = types.ModuleType(name)
    for key, value in attributes.items():
        setattr(result, key, value)
    return result


def run_job(path, args, modules):
    real_open = builtins.open
    opened = []

    def open_progress(file, *args, **kwargs):
        if str(file) == "/work/out/progress.txt":
            return io.StringIO()
        stream = real_open(file, *args, **kwargs)
        opened.append(stream)
        return stream

    try:
        with mock.patch.dict(sys.modules, modules), \
                mock.patch.object(sys, "argv", [str(path), *map(str, args)]), \
                mock.patch.object(sys, "path", sys.path.copy()), \
                mock.patch("builtins.open", side_effect=open_progress):
            runpy.run_path(str(path), run_name="__main__")
    finally:
        for stream in opened:
            stream.close()


class ScratchTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = Path(f".gpu-study-review-{os.getpid()}-{cls.__name__}")
        cls.workspace.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.workspace)

    def setUp(self):
        self.work = self.workspace / self._testMethodName
        self.work.mkdir()


class AdapterTests(ScratchTest):
    keys = (
        "module.q_proj.lora_A.weight",
        "module.q_proj.lora_B.weight",
        "module.v_proj.lora_A.weight",
        "module.v_proj.lora_B.weight",
    )

    def setUp(self):
        super().setUp()
        self.list_path = self.work / "list.jsonl"
        self.list_path.write_text("", encoding="utf-8")

    def context(self, expected, state, unexpected=()):
        model = mock.Mock()
        model.to.return_value = model
        merged = model.merge_and_unload.return_value
        processor = mock.Mock()
        getter = mock.Mock(return_value=expected)
        setter = mock.Mock(return_value=types.SimpleNamespace(
            missing_keys=["base_model.unloaded_base_weight"],
            unexpected_keys=list(unexpected),
        ))
        loader = mock.Mock(return_value=state)
        worker = module("worker", attach_lora=mock.Mock(return_value=model), PROMPT="read")
        torch = module("torch", bfloat16=object())
        peft = module("peft", get_peft_model_state_dict=getter, set_peft_model_state_dict=setter)
        tensors = module("safetensors.torch", load_file=loader)
        safetensors = module("safetensors", torch=tensors, __path__=[])
        transformers = module(
            "transformers",
            AutoProcessor=types.SimpleNamespace(from_pretrained=mock.Mock(return_value=processor)),
            Qwen3VLForConditionalGeneration=types.SimpleNamespace(from_pretrained=mock.Mock(return_value=model)),
            StoppingCriteria=object,
            StoppingCriteriaList=list,
        )
        modules = {
            "worker": worker, "torch": torch, "PIL": module("PIL", Image=object()),
            "peft": peft, "safetensors": safetensors, "safetensors.torch": tensors,
            "transformers": transformers,
        }
        return types.SimpleNamespace(
            modules=modules, model=model, merged=merged, processor=processor,
            getter=getter, setter=setter, loader=loader,
        )

    def run_adapter(self, script, context, use_adapter=True):
        if script == "lora_infer.py":
            args = [
                "--list", self.list_path, "--root", self.work,
                "--out", self.work / "inference.jsonl", "--min-pixels", 1, "--max-pixels", 2,
            ]
            if use_adapter:
                args += ["--adapter", self.work / "adapter.safetensors"]
        else:
            args = ["--base", "synthetic-base", "--adapter", "adapter.safetensors", "--out", self.work / "merged"]
        run_job(JOBS / script, args, context.modules)

    def assert_not_used(self, context):
        context.model.eval.assert_not_called()
        context.model.merge_and_unload.assert_not_called()
        context.merged.save_pretrained.assert_not_called()

    def test_rejects_empty_partial_extra_and_empty_expected_state_before_loading(self):
        full = dict.fromkeys(self.keys, 1)
        cases = (
            ("empty", full, {}),
            ("partial", full, {self.keys[0]: 1, self.keys[1]: 1}),
            ("extra", full, {**full, "other.weight": 1}),
            ("no_expected_tensors", {}, {}),
        )
        for script in ("lora_infer.py", "merge_lora.py"):
            for name, expected, supplied in cases:
                with self.subTest(script=script, case=name):
                    context = self.context(expected, supplied)
                    with self.assertRaisesRegex(SystemExit, "adapter keys do not fit"):
                        self.run_adapter(script, context)
                    context.setter.assert_not_called()
                    self.assert_not_used(context)

    def test_complete_state_loads_before_inference_or_merge(self):
        full = dict.fromkeys(self.keys, 1)
        for script in ("lora_infer.py", "merge_lora.py"):
            with self.subTest(script=script):
                context = self.context(full, full.copy())
                self.run_adapter(script, context)
                context.setter.assert_called_once_with(context.model, full)
                if script == "lora_infer.py":
                    context.model.eval.assert_called_once()
                    context.model.merge_and_unload.assert_not_called()
                else:
                    context.model.merge_and_unload.assert_called_once()
                    context.merged.save_pretrained.assert_called_once()
                    context.processor.save_pretrained.assert_called_once()

    def test_peft_unexpected_keys_prevent_inference_and_merge(self):
        full = dict.fromkeys(self.keys, 1)
        for script in ("lora_infer.py", "merge_lora.py"):
            with self.subTest(script=script):
                context = self.context(full, full, unexpected=["remapped.weight"])
                with self.assertRaisesRegex(SystemExit, "after loading"):
                    self.run_adapter(script, context)
                context.setter.assert_called_once_with(context.model, full)
                self.assert_not_used(context)

    def test_baseline_does_not_inspect_or_load_an_adapter(self):
        context = self.context({}, {})
        self.run_adapter("lora_infer.py", context, use_adapter=False)
        context.loader.assert_not_called()
        context.getter.assert_not_called()
        context.setter.assert_not_called()
        context.model.eval.assert_called_once()


class FakeImage:
    def __init__(self, width=8, height=8):
        self.width, self.height = width, height
        self.size = (width, height)

    def load(self):
        pass

    def convert(self, mode):
        return self

    def crop(self, box):
        return self

    def resize(self, size, method):
        return FakeImage(*size)


def block(text="read", error=None, skipped=False):
    return types.SimpleNamespace(html=text, error=error, skipped=skipped, bbox=(0, 0, 2, 2))


class SuryaTests(ScratchTest):
    def run_surya(self, mode, responses):
        input_list = self.work / "inputs.tsv"
        output = self.work / "output.jsonl"
        input_list.write_text("crop-1\timage.png\n", encoding="utf-8")
        manager = mock.Mock()
        recognizer = mock.Mock(side_effect=[[types.SimpleNamespace(blocks=bs)] for bs in responses])
        image = types.SimpleNamespace(
            MAX_IMAGE_PIXELS=1, LANCZOS=1, open=mock.Mock(return_value=FakeImage()),
        )
        surya_run = module(
            "run", CAPPED_LONG_SIDE=4,
            tiles=mock.Mock(return_value=[(0, 0, 4, 8, None), (4, 0, 8, 8, None)]),
            inside=mock.Mock(return_value=True), block_text=mock.Mock(side_effect=lambda html: html),
        )
        modules = {
            "run": surya_run, "PIL": module("PIL", Image=image), "surya": module("surya", __path__=[]),
            "surya.inference": module("surya.inference", SuryaInferenceManager=mock.Mock(return_value=manager)),
            "surya.recognition": module("surya.recognition", RecognitionPredictor=mock.Mock(return_value=recognizer)),
            "surya.layout": module("surya.layout", __path__=[]),
            "surya.layout.schema": module(
                "surya.layout.schema", LayoutBox=mock.Mock(), LayoutResult=mock.Mock(),
            ),
        }
        run_job(JOBS / "surya_read.py", ["--list", input_list, "--mode", mode, "--out", output], modules)
        manager.stop.assert_called_once()
        return json.loads(output.read_text(encoding="utf-8")), surya_run

    def test_any_block_error_fails_the_row_without_partial_text(self):
        for mode in ("line", "whole", "capped", "tiled"):
            for all_fail in (False, True):
                with self.subTest(mode=mode, all_fail=all_fail):
                    good = block()
                    bad = block(text="partial", error="failed", skipped=True)
                    if mode == "tiled":
                        responses = [[bad] if all_fail else [good], [bad]]
                    else:
                        responses = [[bad] if all_fail else [good, bad]]
                    row, runner = self.run_surya(mode, responses)
                    self.assertEqual(row["status"], "error: block")
                    self.assertNotIn("text", row)
                    self.assertNotIn(mock.call("partial"), runner.block_text.call_args_list)

    def test_skipped_blocks_remain_successful_and_are_not_read(self):
        for mode in ("whole", "capped", "tiled"):
            for all_skipped in (False, True):
                with self.subTest(mode=mode, all_skipped=all_skipped):
                    blocks = [block(text="skipped", skipped=True)]
                    if not all_skipped:
                        blocks.append(block())
                    responses = [blocks, blocks] if mode == "tiled" else [blocks]
                    row, runner = self.run_surya(mode, responses)
                    self.assertEqual(row["status"], "ok")
                    expected = "" if all_skipped else "read\nread" if mode == "tiled" else "read"
                    self.assertEqual(row["text"], expected)
                    self.assertNotIn(mock.call("skipped"), runner.block_text.call_args_list)


FAKE_SCORER = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
def arg(name):
    return args[args.index(name) + 1]

with open(os.environ["TRACE"], "a", encoding="utf-8") as trace:
    trace.write(args[0] + " " + arg("--out") + "\n")
rows = []
if args[0] == "score":
    refs = [json.loads(line) for line in Path(arg("--refs")).read_text().splitlines()]
    for i, value in enumerate(args):
        if value != "--hyp":
            continue
        name, path = args[i + 1].split("=", 1)
        hyps = [json.loads(line) for line in Path(path).read_text().splitlines()]
        by_id = {r["id"]: r["text"] for r in hyps if isinstance(r.get("text"), str)}
        missing = 0
        errors = 0
        for ref in refs:
            text = by_id.get(ref["id"])
            missing += text is None
            if text is None or text == "":
                errors += len(ref["ref"])
            elif text != ref["ref"]:
                raise ValueError("unexpected synthetic text")
        cer = errors / sum(len(r["ref"]) for r in refs)
        rows.append({"group": "all", "reader": name, "items": len(refs), "missing": missing,
                     "cer": cer, "cer_lo": cer, "cer_hi": cer, "wer": cer, "median_crop_cer": 0})
    baseline = next(row["cer"] for row in rows if row["reader"] == arg("--baseline"))
    for row in rows:
        if row["reader"] != arg("--baseline"):
            row["cer_diff"] = row["cer"] - baseline
elif args[0] == "invent":
    rows = [{"group": "all", "reader": "surya-line", "crops": 1, "major": 0,
             "invented_run": 0, "over_length_guard": 0, "under_length_guard": 0,
             "under_word_overlap_guard": 0}]
Path(arg("--out")).write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
"""


@unittest.skipUnless(shutil.which("jq"), "analyze-6.sh requires jq")
class NativeScoringTests(ScratchTest):
    def setUp(self):
        super().setUp()
        self.tool = self.work / "fake-scorer"
        self.tool.write_text(FAKE_SCORER, encoding="utf-8")
        self.tool.chmod(0o755)
        self.ids = [f"crop-{i:02}" for i in range(52)]

    def write_rows(self, path, rows):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")

    def fixture(self, name):
        root = self.work / name
        refs = [{"id": cid, "ref": "x", "page": cid} for cid in self.ids]
        self.write_rows(root / "analysis/s3/refs.jsonl", refs)
        all_hyps = [{"id": cid, "text": "x"} for cid in self.ids]
        for reader in ("surya", "qwen", "churro", "tesseract"):
            self.write_rows(root / f"analysis/s3/{reader}.jsonl", all_hyps)
        self.write_rows(root / "results/s6/surya-line.jsonl", [
            row for row in all_hyps if row["id"] != self.ids[5]
        ])
        for mode in ("whole", "capped"):
            self.write_rows(root / f"results/s6/surya-{mode}.jsonl", [
                {"id": "page", "status": "ok", "text": "x", "ms": 1000}
            ])
        return root

    def freeze(self, root, ids):
        path = root / "push/s6c/list.tsv"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(f"{cid}\tc/{cid}.png\n" for cid in ids), encoding="utf-8")

    def analyze(self, root):
        env = os.environ.copy()
        env.update(GPU_STUDY_ROOT=str(root.resolve()), TOOL=str(self.tool.resolve()), TRACE=str((root / "trace").resolve()))
        return subprocess.run(
            ["bash", str(SCRIPTS / "analyze-6.sh")], env=env, text=True,
            capture_output=True, timeout=30, check=False,
        )

    def scores(self, root):
        return {
            row["reader"]: row
            for row in map(json.loads, (root / "analysis/s6/native.jsonl").read_text().splitlines())
        }

    def test_frozen_denominator_includes_missing_and_error_rows_for_both_readers(self):
        root = self.fixture("partial")
        self.freeze(root, self.ids[:50])
        native = [
            {"id": self.ids[0], "text": "x"},
            {"id": self.ids[1], "status": "error: block"},
            {"id": self.ids[3], "status": "error: block", "text": "x"},
            {"id": self.ids[4], "status": "ok", "text": ""},
        ] + [{"id": cid, "status": "ok", "text": "x"} for cid in self.ids[5:50]]
        native.append({"id": self.ids[50], "status": "ok", "text": "x"})
        self.write_rows(root / "results/s6c/surya-native.jsonl", native)
        result = self.analyze(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        refs = [json.loads(line)["id"] for line in (root / "analysis/s6/native-refs.jsonl").read_text().splitlines()]
        self.assertEqual(refs, self.ids[:50])
        scores = self.scores(root)
        self.assertEqual((scores["native"]["items"], scores["native"]["missing"]), (50, 3))
        self.assertEqual((scores["gguf"]["items"], scores["gguf"]["missing"]), (50, 1))
        self.assertAlmostEqual(scores["native"]["cer"], 0.08)
        self.assertAlmostEqual(scores["native"]["cer_diff"], 0.06)
        summary = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("[")]
        self.assertIn(["native", 50, 3], [row[:3] for row in summary])

    def test_attempted_native_run_with_no_outputs_scores_all_as_missing(self):
        for mode in ("absent", "empty"):
            with self.subTest(mode=mode):
                root = self.fixture(mode)
                self.freeze(root, self.ids[:50])
                raw = root / "results/s6c/surya-native.jsonl"
                if mode == "empty":
                    self.write_rows(raw, [])
                result = self.analyze(root)
                self.assertEqual(result.returncode, 0, result.stderr)
                native = self.scores(root)["native"]
                self.assertEqual((native["items"], native["missing"], native["cer"]), (50, 50, 1.0))
                if mode == "absent":
                    self.assertFalse(raw.exists())

    def test_invalid_or_missing_frozen_list_fails_before_native_score(self):
        for mode in ("missing", "short", "duplicate", "unknown", "duplicate_ref", "malformed"):
            with self.subTest(mode=mode):
                root = self.fixture(mode)
                self.write_rows(root / "results/s6c/surya-native.jsonl", [{"id": self.ids[0], "text": "x"}])
                ids = self.ids[:50]
                if mode == "short":
                    ids = ids[:-1]
                elif mode == "duplicate":
                    ids = ids[:-1] + [ids[0]]
                elif mode == "unknown":
                    ids = ids[:-1] + ["unknown"]
                if mode != "missing":
                    self.freeze(root, ids)
                if mode == "malformed":
                    path = root / "push/s6c/list.tsv"
                    path.write_text(path.read_text().replace(f"{ids[0]}\tc/{ids[0]}.png", ids[0]), encoding="utf-8")
                if mode == "duplicate_ref":
                    path = root / "analysis/s3/refs.jsonl"
                    refs = [json.loads(line) for line in path.read_text().splitlines()]
                    refs[49] = refs[0]
                    self.write_rows(path, refs)
                result = self.analyze(root)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((root / "analysis/s6/native.jsonl").exists())

    def test_unattempted_optional_native_run_is_not_scored(self):
        root = self.fixture("not-attempted")
        result = self.analyze(root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((root / "analysis/s6/native.jsonl").exists())


if __name__ == "__main__":
    unittest.main()
