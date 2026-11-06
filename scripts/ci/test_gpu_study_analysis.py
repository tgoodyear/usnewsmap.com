"""Synthetic Study 3 analysis regressions; run from the repository root with unittest."""

import base64
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import unittest


REPO = Path(__file__).resolve().parents[2]
SCRIPTS = REPO / "docs/reports/2026-11-gpu-studies/scripts"
WORK = Path.cwd() / f".gpu-study-analysis-tests-{os.getpid()}"

FAKE_SCORER = r"""#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

args = sys.argv[1:]
def arg(name):
    return args[args.index(name) + 1]

refs = [json.loads(line) for line in Path(arg("--refs")).read_text().splitlines()]
hyps = {}
for index, value in enumerate(args):
    if value == "--hyp":
        name, path = args[index + 1].split("=", 1)
        hyps[name] = {row["id"]: row["text"] for row in
                      map(json.loads, Path(path).read_text().splitlines())}
american = hyps["american-stories"]
present = [ref["id"] for ref in refs if ref["id"] in american]
event = {"command": args[0], "fold": "--fold" in args, "present": present,
         "empty": [item for item in present if american[item] == ""],
         "missing": len(refs) - len(present),
         "qwen_present": [ref["id"] for ref in refs if ref["id"] in hyps["qwen-8b"]]}
with Path(os.environ["TRACE"]).open("a", encoding="utf-8") as trace:
    trace.write(json.dumps(event) + "\n")

if args[0] == "score":
    rows = [{"group": "source:synthetic", "reader": name, "items": len(refs),
             "pages": len(refs), "missing": sum(ref["id"] not in values for ref in refs),
             "cer": 0, "cer_lo": 0, "cer_hi": 0, "median_crop_cer": 0, "wer": 0}
            for name, values in hyps.items()]
elif present:
    rows = [{"group": "all", "reader": "american-stories", "crops": len(present),
             "major": 0, "invented_run": 0, "over_length_guard": 0,
             "under_length_guard": len(event["empty"]), "under_word_overlap_guard": 0}]
else:
    rows = []
Path(arg("--out")).write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
"""


def jsonl(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("".join(json.dumps(row, ensure_ascii=False) + "\n" for row in rows), encoding="utf-8")


def encoded(value):
    return base64.b64encode(value).decode("ascii")


def as_row(item, page, seconds=4):
    payload = json.dumps(page, ensure_ascii=False).encode("utf-8")
    return f"{item}\t{seconds}\t{encoded(payload)}\n"


@unittest.skipUnless(shutil.which("jq"), "Study 3 analysis requires jq")
class Study3AnalysisTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.root = self.work / "study"
        self.root.mkdir()
        (self.root / "scripts").symlink_to(SCRIPTS, target_is_directory=True)
        self.local = self.work / "dev root/local/out"
        self.local.mkdir(parents=True)
        self.raw = self.local / "as.b64"
        self.retained = self.work / "retained-american-stories.jsonl"
        jsonl(self.retained, [])
        self.trace = self.work / "scorer-trace.jsonl"
        self.tool = self.work / "fake-scorer"
        self.tool.write_text(FAKE_SCORER, encoding="utf-8")
        self.tool.chmod(0o755)
        self.env = os.environ.copy()
        self.env.pop("AMERICAN_STORIES_B64", None)
        self.env.pop("AMERICAN_STORIES_JSONL", None)
        self.env.update(
            GPU_STUDY_ROOT=str(self.root),
            GPU_STUDY_DEV=str(self.work / "dev root"),
            TOOL=str(self.tool),
            TRACE=str(self.trace),
            AMERICAN_STORIES_REFERENCE=str(self.retained),
            PYTHONDONTWRITEBYTECODE="1",
        )
        refs = [
            {"id": f"crop:{i}", "page": f"page-{i}", "source": "synthetic", "ref": "x"}
            for i in range(1, 5)
        ]
        for name, ref in zip(("op-100", "rz-100", "ml-52", "nzz-360"), refs):
            jsonl(self.root / f"refs/{name}.jsonl", [ref])
        response = {"choices": [{"message": {"content": "x"}, "finish_reason": "stop"}],
                    "usage": {"completion_tokens": 1}}
        qwen = self.root / "results/s3-qwen/readings.b64"
        qwen.parent.mkdir(parents=True)
        qwen.write_text(f"crop:1\t5\t{encoded(json.dumps(response).encode())}\n", encoding="utf-8")
        for name, item in (("op-100-psm6", "crop:1"), ("ml-52-psm6", "crop:3")):
            (self.local / f"{name}.b64").write_text(f"{item}\t{encoded(b'x')}\n", encoding="utf-8")
        jsonl(self.root / "analysis/s1/frak2021-psm6.jsonl", [
            {"id": "crop:2", "text": "x"}, {"id": "crop:4", "text": "x"},
        ])
        jsonl(self.root / "results/s3-surya/surya-crops.jsonl", [{"id": "crop:1", "text": "x"}])
        jsonl(self.root / "results/s3-churro/churro.jsonl", [{"id": "crop:1", "text": "<Page>x</Page>"}])
        jsonl(self.root / "results/s3-hf/got.jsonl", [])

    def analyze(self, **extra_env):
        return subprocess.run(
            ["bash", str(SCRIPTS / "analyze-3.sh")],
            cwd=REPO, env={**self.env, **extra_env}, capture_output=True, text=True,
            timeout=30, check=False,
        )

    def events(self):
        return list(map(json.loads, self.trace.read_text(encoding="utf-8").splitlines()))

    def test_regenerates_reference_rows_and_keeps_empty_distinct_from_missing(self):
        text = " café $(touch should-not-exist) "
        self.raw.write_text(
            as_row("crop:1", {"bboxes": [{"raw_text": "superseded"}]})
            + as_row("crop:1", {"bboxes": [
                {"raw_text": text}, {"raw_text": " \u00a0 "}, {},
                {"raw_text": "next\nline"},
            ], "full articles": [{"article": "not used"}]})
            + as_row("crop:2", {"bboxes": []})
            + as_row("crop:4", {"bboxes": [{"class": "image"}]})
            + as_row("extra", {"bboxes": [{"raw_text": "unreferenced"}]}),
            encoding="utf-8",
        )
        jsonl(self.retained, [
            {"id": "crop:4", "text": ""},
            {"id": "crop:2", "text": ""},
            {"id": "crop:1", "text": text + "\n\nnext\nline"},
        ])
        result = self.analyze()
        self.assertEqual(result.returncode, 0, result.stderr)
        output = self.root / "analysis/s3/american-stories.jsonl"
        self.assertEqual(list(map(json.loads, output.read_text(encoding="utf-8").splitlines())), [
            {"id": "crop:1", "text": "superseded"},
            {"id": "crop:1", "text": text + "\n\nnext\nline"},
            {"id": "crop:2", "text": ""},
            {"id": "crop:4", "text": ""},
        ])
        self.assertFalse((self.root / "should-not-exist").exists())
        events = self.events()
        self.assertEqual([(e["command"], e["fold"]) for e in events],
                         [("score", False), ("score", True), ("invent", False)])
        for event in events:
            self.assertEqual(event["present"], ["crop:1", "crop:2", "crop:4"])
            self.assertEqual(event["empty"], ["crop:2", "crop:4"])
            self.assertEqual(event["missing"], 1)
            self.assertEqual(event["qwen_present"], ["crop:1"])
        invent = list(map(json.loads, (self.root / "analysis/s3/invent.jsonl").read_text().splitlines()))
        self.assertEqual((invent[0]["crops"], invent[0]["under_length_guard"]), (3, 2))

    def test_existing_empty_input_scores_every_crop_as_missing(self):
        self.raw.write_text("", encoding="utf-8")
        result = self.analyze()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "analysis/s3/american-stories.jsonl").read_text(), "")
        self.assertTrue(all(event["missing"] == 4 and not event["present"] for event in self.events()))

    def test_missing_input_fails_without_scoring_stale_output(self):
        stale = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(stale, [{"id": "crop:1", "text": "stale"}])
        result = self.analyze()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("American Stories decode failed", result.stderr)
        self.assertFalse(self.trace.exists())
        self.assertEqual(list(map(json.loads, stale.read_text().splitlines())), [
            {"id": "crop:1", "text": "stale"},
        ])

    def test_explicit_input_path_is_used(self):
        self.raw.write_text("not a record\n", encoding="utf-8")
        alternate = self.work / "input path/as.b64"
        alternate.parent.mkdir()
        alternate.write_text(as_row("crop:1", {"bboxes": [{"raw_text": "selected"}]}), encoding="utf-8")
        jsonl(self.retained, [{"id": "crop:1", "text": "selected"}])
        result = self.analyze(AMERICAN_STORIES_B64=str(alternate))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.events()[0]["present"], ["crop:1"])
        row = json.loads((self.root / "analysis/s3/american-stories.jsonl").read_text())
        self.assertEqual(row, {"id": "crop:1", "text": "selected"})

    def test_decoder_uses_named_input_and_output_flags(self):
        self.raw.write_text(as_row("crop:1", {"bboxes": [{"raw_text": "synthetic"}]}), encoding="utf-8")
        out = self.work / "named-output.jsonl"
        command = ["python3", str(SCRIPTS / "decode-american-stories.py")]
        result = subprocess.run(
            [*command, "--input", str(self.raw), "--output", str(out)],
            cwd=REPO, env=self.env, capture_output=True, text=True, timeout=30, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(out.read_text()), {"id": "crop:1", "text": "synthetic"})
        for args in ([str(self.raw), str(out)], ["--input", str(self.raw)]):
            with self.subTest(args=args):
                out.write_text("unchanged\n", encoding="utf-8")
                result = subprocess.run(
                    [*command, *args], cwd=REPO, env=self.env,
                    capture_output=True, text=True, timeout=30, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(out.read_text(), "unchanged\n")

    def test_opt_in_retained_jsonl_regenerates_without_raw_receipts(self):
        rows = [{"id": "crop:1", "text": "synthetic café"}, {"id": "crop:2", "text": ""}]
        jsonl(self.retained, rows)
        result = self.analyze(AMERICAN_STORIES_JSONL=str(self.retained))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.raw.exists())
        output = self.root / "analysis/s3/american-stories.jsonl"
        self.assertEqual(list(map(json.loads, output.read_text(encoding="utf-8").splitlines())), rows)
        for event in self.events():
            self.assertEqual(event["present"], ["crop:1", "crop:2"])
            self.assertEqual(event["empty"], ["crop:2"])
            self.assertEqual(event["missing"], 2)

    def test_jsonl_filters_unreferenced_rows_when_input_is_the_output_path(self):
        jsonl(self.retained, [{"id": "crop:1", "text": "latest"}, {"id": "crop:2", "text": ""}])
        output = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(output, [
            {"id": "crop:1", "text": "older"},
            {"id": "crop:1", "text": "latest", "unused": "metadata"},
            {"id": "crop:2", "text": ""},
            {"id": "extra", "text": "unreferenced"},
        ])
        result = self.analyze(AMERICAN_STORIES_JSONL=str(output))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(list(map(json.loads, output.read_text().splitlines())), [
            {"id": "crop:1", "text": "older"},
            {"id": "crop:1", "text": "latest"},
            {"id": "crop:2", "text": ""},
        ])
        self.assertEqual(self.events()[0]["missing"], 2)

    def test_jsonl_missing_mismatched_or_invalid_input_never_scores(self):
        expected = {"id": "crop:1", "text": "expected"}
        jsonl(self.retained, [expected])
        source = self.work / "precomputed input.jsonl"
        previous = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(previous, [{"id": "crop:1", "text": "previous"}])
        valid_line = (json.dumps(expected) + "\n").encode()
        cases = {
            "missing": None,
            "mismatch": b'{"id":"crop:1","text":"different"}\n',
            "missing_selected_id": b'',
            "unexpected_selected_id": valid_line + b'{"id":"crop:3","text":"x"}\n',
            "bad_json": valid_line + b'not-json\n',
            "bad_id": valid_line + b'{"id":"bad/id","text":"x"}\n',
            "missing_id": valid_line + b'{"text":"x"}\n',
            "nonstring_text": valid_line + b'{"id":"extra","text":null}\n',
            "bad_utf8": valid_line + b'{"id":"extra","text":"\xff"}\n',
            "surrogate": valid_line + b'{"id":"extra","text":"\\ud800"}\n',
        }
        for name, contents in cases.items():
            with self.subTest(case=name):
                if contents is None:
                    source.unlink(missing_ok=True)
                else:
                    source.write_bytes(contents)
                result = self.analyze(AMERICAN_STORIES_JSONL=str(source))
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.trace.exists())
                self.assertEqual(list(map(json.loads, previous.read_text().splitlines())), [
                    {"id": "crop:1", "text": "previous"},
                ])

    def test_input_modes_are_exclusive_and_explicit_paths_cannot_be_empty(self):
        self.raw.write_text("", encoding="utf-8")
        previous = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(previous, [{"id": "crop:1", "text": "previous"}])
        for env, diagnostic in (
            ({"AMERICAN_STORIES_JSONL": str(self.retained), "AMERICAN_STORIES_B64": str(self.raw)},
             "Set only one"),
            ({"AMERICAN_STORIES_JSONL": ""}, "AMERICAN_STORIES_JSONL must name"),
            ({"AMERICAN_STORIES_B64": ""}, "AMERICAN_STORIES_B64 must name"),
            ({"AMERICAN_STORIES_REFERENCE": ""}, "AMERICAN_STORIES_REFERENCE must name"),
        ):
            with self.subTest(env=env):
                result = self.analyze(**env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(diagnostic, result.stderr)
                self.assertFalse(self.trace.exists())
                self.assertEqual(list(map(json.loads, previous.read_text().splitlines())), [
                    {"id": "crop:1", "text": "previous"},
                ])

    def test_retained_reference_cannot_be_clobbered_by_intermediate_output(self):
        source = self.work / "precomputed.jsonl"
        jsonl(source, [{"id": "crop:1", "text": "new"}])
        intermediate = self.root / "analysis/s3/american-stories-all.jsonl"
        for case in ("missing", "existing"):
            with self.subTest(case=case):
                if case == "existing":
                    jsonl(intermediate, [{"id": "crop:1", "text": "retained"}])
                    before = intermediate.read_bytes()
                result = self.analyze(
                    AMERICAN_STORIES_JSONL=str(source), AMERICAN_STORIES_REFERENCE=str(intermediate),
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.trace.exists())
                if case == "missing":
                    self.assertFalse(intermediate.exists())
                else:
                    self.assertEqual(intermediate.read_bytes(), before)

        intermediate.unlink()
        jsonl(self.retained, [{"id": "crop:1", "text": "retained"}])
        before = self.retained.read_bytes()
        intermediate.symlink_to(self.retained)
        result = self.analyze(AMERICAN_STORIES_JSONL=str(source))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.retained.read_bytes(), before)
        self.assertFalse(self.trace.exists())
        intermediate.unlink()

        retained = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(retained, [{"id": "crop:1", "text": "retained", "unused": "preserve"}])
        before = retained.read_bytes()
        result = self.analyze(AMERICAN_STORIES_JSONL=str(retained), AMERICAN_STORIES_REFERENCE=str(retained))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(retained.read_bytes(), before)
        self.assertEqual(self.events()[0]["present"], ["crop:1"])

    def test_default_snapshot_is_relative_to_the_analysis_script(self):
        self.raw.write_text("", encoding="utf-8")
        report = self.work / "report"
        script = report / "scripts/analyze-3.sh"
        script.parent.mkdir(parents=True)
        shutil.copyfile(SCRIPTS / "analyze-3.sh", script)
        jsonl(report / "data/s3/american-stories.jsonl", [])
        env = self.env.copy()
        del env["AMERICAN_STORIES_REFERENCE"]
        result = subprocess.run(
            ["bash", str(script)], cwd=REPO, env=env, capture_output=True,
            text=True, timeout=30, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.events()), 3)

    def test_retained_mismatch_or_missing_snapshot_fails_before_scoring(self):
        self.raw.write_text(as_row("crop:1", {"bboxes": [{"raw_text": "decoded"}]}), encoding="utf-8")
        previous = self.root / "analysis/s3/american-stories.jsonl"
        jsonl(previous, [{"id": "crop:1", "text": "previous"}])
        for case in ("mismatch", "missing"):
            with self.subTest(case=case):
                if case == "mismatch":
                    jsonl(self.retained, [{"id": "crop:1", "text": "different"}])
                else:
                    self.retained.unlink()
                result = self.analyze()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("refusing to score", result.stderr)
                self.assertFalse(self.trace.exists())
                self.assertEqual(list(map(json.loads, previous.read_text().splitlines())), [
                    {"id": "crop:1", "text": "previous"},
                ])

    def test_empty_and_rejected_qwen_receipts_score_as_missing(self):
        self.raw.write_text("", encoding="utf-8")
        receipt = self.root / "results/s3-qwen/readings.b64"
        no_text = encoded(json.dumps({"choices": []}).encode("utf-8"))
        for case, contents in (
            ("empty", ""),
            ("no_matching_rows", "not a receipt\ncrop:1\tnot-a-time\tYWJj\n"),
            ("no_text", f"crop:1\t5\t{no_text}\n"),
            ("invalid_json", f"crop:1\t5\t{encoded(b'not JSON')}\n"),
        ):
            with self.subTest(case=case):
                receipt.write_text(contents, encoding="utf-8")
                if self.trace.exists():
                    self.trace.unlink()
                result = self.analyze()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.root / "analysis/s3/qwen.jsonl").read_text(), "")
                self.assertTrue(all(not event["qwen_present"] for event in self.events()))
                scores = list(map(json.loads, (self.root / "analysis/s3/compare.jsonl").read_text().splitlines()))
                qwen = next(row for row in scores if row["reader"] == "qwen-8b")
                self.assertEqual((qwen["items"], qwen["missing"]), (4, 4))

    def test_qwen_empty_text_remains_present_among_rejected_rows(self):
        self.raw.write_text("", encoding="utf-8")
        response = {"choices": [{"message": {"content": ""}}]}
        payload = encoded(json.dumps(response).encode("utf-8"))
        receipt = self.root / "results/s3-qwen/readings.b64"
        receipt.write_text(f"invalid receipt\ncrop:1\t5\t{payload}\nextra\t5\t{payload}\n", encoding="utf-8")
        result = self.analyze()
        self.assertEqual(result.returncode, 0, result.stderr)
        qwen_rows = list(map(json.loads, (self.root / "analysis/s3/qwen.jsonl").read_text().splitlines()))
        self.assertEqual([(row["id"], row["text"]) for row in qwen_rows], [("crop:1", "")])
        self.assertTrue(all(event["qwen_present"] == ["crop:1"] for event in self.events()))
        scores = list(map(json.loads, (self.root / "analysis/s3/compare.jsonl").read_text().splitlines()))
        qwen = next(row for row in scores if row["reader"] == "qwen-8b")
        self.assertEqual((qwen["items"], qwen["missing"]), (4, 3))

    def test_qwen_decode_preserves_grep_and_pipeline_failures(self):
        receipt = self.root / "results/s3-qwen/readings.b64"
        out = self.work / "qwen.jsonl"

        def decode(source, env):
            return subprocess.run(
                ["bash", str(SCRIPTS / "decode.sh"), "--kind", "qwen",
                 "--in", str(source), "--out", str(out)],
                cwd=REPO, env=env, capture_output=True, text=True, timeout=30, check=False,
            )

        read_error = decode(self.work / "no-such-receipt", self.env)
        self.assertNotEqual(read_error.returncode, 0)
        self.assertIn("grep", read_error.stderr)

        fake_bin = self.work / "bin"
        fake_bin.mkdir()
        grep = fake_bin / "grep"
        real_grep = shlex.quote(shutil.which("grep"))
        grep.write_text(f"#!/usr/bin/env bash\nexec {real_grep} -aE '[' \"$3\"\n", encoding="utf-8")
        grep.chmod(0o755)
        bad_regex = decode(receipt, {**self.env, "PATH": str(fake_bin) + os.pathsep + self.env["PATH"]})
        self.assertNotEqual(bad_regex.returncode, 0)
        self.assertIn("grep", bad_regex.stderr)

        receipt.write_text("crop:1\t5\tA\n", encoding="utf-8")
        jq_error = decode(receipt, self.env)
        self.assertNotEqual(jq_error.returncode, 0)
        self.assertIn("jq", jq_error.stderr)

    def test_invalid_records_fail_closed_without_replacing_output(self):
        valid = as_row("crop:1", {"bboxes": []})
        bad = {
            "fields": "crop:2\tbroken\n",
            "id": as_row("bad/id", {"bboxes": []}),
            "elapsed": "crop:2\t-1\te30=\n",
            "base64": "crop:2\t1\t%%%\n",
            "json": f"crop:2\t1\t{encoded(b'not JSON')}\n",
            "utf8": 'crop:2\t1\t' + encoded(b'{"bboxes":[],"x":"' + bytes([255]) + b'"}') + '\n',
            "page": as_row("crop:2", []),
            "missing_boxes": as_row("crop:2", {}),
            "wrong_boxes": as_row("crop:2", {"bboxes": None}),
            "box": as_row("crop:2", {"bboxes": ["not an object"]}),
            "text": as_row("crop:2", {"bboxes": [{"raw_text": None}]}),
            "surrogate": 'crop:2\t1\t' + encoded(b'{"bboxes":[{"raw_text":"\\ud800"}]}') + '\n',
        }
        out = self.work / "decoded.jsonl"
        for name, row in bad.items():
            with self.subTest(case=name):
                self.raw.write_text(valid + row, encoding="utf-8")
                out.write_text("unchanged\n", encoding="utf-8")
                result = subprocess.run(
                    ["bash", str(SCRIPTS / "decode.sh"), "--kind", "american-stories",
                     "--in", str(self.raw), "--out", str(out)],
                    cwd=REPO, env=self.env, capture_output=True, text=True, timeout=30, check=False,
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("line 2:", result.stderr)
                self.assertEqual(out.read_text(encoding="utf-8"), "unchanged\n")


if __name__ == "__main__":
    unittest.main()
