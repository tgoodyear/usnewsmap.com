"""Offline synthetic regression tests for pilot preparation and resumption."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "docs/reports/2026-11-gpu-studies/scripts/prep-pilot.sh"
FIXTURES = ROOT / f".gpu-study-prep-pilot-tests-{os.getpid()}"


BOINC = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
if args[:2] != ["qwen", "local-unit"]:
    raise SystemExit("unexpected usnm-boinc invocation")
def value(flag):
    return args[args.index(flag) + 1]
def trace(event):
    with open(os.environ["TRACE"], "a") as f:
        f.write(json.dumps(event) + "\n")
image, unit = pathlib.Path(value("--image")), pathlib.Path(value("--dir"))
trace({"tool": "local-unit", "args": args, "image": str(image), "dir": str(unit)})
page = image.read_bytes()  # The real local-unit reads before replacing page.jpg.
unit.mkdir(parents=True, exist_ok=True)
def atomic(name, data):
    staged = unit / (".synthetic-" + name)
    with open(staged, "xb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(staged, unit / name)
    fd = os.open(unit, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    trace({"tool": "atomic", "name": name, "dir": str(unit)})
atomic("page.jpg", page)
if os.environ.get("FAIL_UNIT") == "1":
    raise SystemExit(41)
if os.environ.get("UNIT_MISSING_JOB") == "1":
    raise SystemExit(0)
atomic("qwen.json", b'{"synthetic":true}\n')
'''


STUDY = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["TRACE"], "a") as f:
    f.write(json.dumps({"tool": args[0], "args": args}) + "\n")
def value(flag):
    return args[args.index(flag) + 1]
if args[0] == "jpeg":
    pathlib.Path(value("--out")).write_bytes(b"jpeg:" + pathlib.Path(value("--input")).read_bytes())
    raise SystemExit(42 if os.environ.get("FAIL_JPEG") == "1" else 0)
if args[0] != "cut":
    raise SystemExit("unexpected gpu-study invocation")
units, out = pathlib.Path(value("--units")), pathlib.Path(value("--out"))
out.mkdir(parents=True, exist_ok=True)
if os.environ.get("FAIL_CUT") == "1":
    (out / "partial.png").write_bytes(b"partial cut")
    raise SystemExit(43)
if os.environ.get("CUT_REMOVE_JOB"):
    (units / os.environ["CUT_REMOVE_JOB"] / "qwen.json").unlink()
(out / "crops.jsonl").write_text('{"id":"synthetic"}\n')
'''


OPENJPEG = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["TRACE"], "a") as f:
    f.write(json.dumps({"tool": "opj_decompress", "args": args}) + "\n")
pathlib.Path(args[args.index("-o") + 1]).write_bytes(
    b"decoded:" + pathlib.Path(args[args.index("-i") + 1]).read_bytes()
)
raise SystemExit(44 if os.environ.get("FAIL_OPJ") == "1" else 0)
'''


def executable(path, content):
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


class Fixture:
    def __init__(self, base):
        # CI checkouts contain /work/; paths are passed as arguments, never globally rewritten.
        self.root = base / "runner/work/repo with spaces"
        self.root.mkdir(parents=True)
        self.work = self.root / "pilot work"
        self.scans = self.root / "scans"
        self.alto = self.root / "alto"
        self.dev = self.root / "dev"
        self.bin = self.dev / "target/release"
        self.fake = self.root / "fake bin"
        for path in (self.work / "units", self.work / "pages", self.scans, self.alto, self.bin, self.fake):
            path.mkdir(parents=True)
        self.pages = self.root / "frozen pages.csv"
        self.trace = self.root / "trace.jsonl"
        executable(self.bin / "usnm-boinc", BOINC)
        executable(self.bin / "gpu-study", STUDY)
        executable(self.fake / "opj_decompress", OPENJPEG)
        self.env = os.environ.copy()
        self.env.update(GPU_STUDY_DEV=str(self.dev), PATH=str(self.fake) + os.pathsep + os.environ["PATH"],
                        TRACE=str(self.trace), PYTHONDONTWRITEBYTECODE="1")

    def select(self, *docs, newline=True):
        self.pages.write_text("\n".join(f"{doc},bad,title" for doc in docs) + ("\n" if newline else ""))

    def scan(self, doc, data=b"synthetic jpeg", ext="jpg"):
        (self.scans / f"{doc}.{ext}").write_bytes(data)
        return data

    def add_alto(self, doc):
        (self.alto / f"{doc}.xml").write_text("<alto>synthetic</alto>")

    def unit(self, doc, page=None, manifest=None):
        path = self.work / "units" / doc
        path.mkdir()
        if page is not None:
            (path / "page.jpg").write_bytes(page)
        if manifest is not None:
            (path / "qwen.json").write_bytes(manifest)
        return path

    def run(self, use_bin=True, **overrides):
        env = self.env.copy()
        env.update(overrides)
        args = ["bash", str(SCRIPT), "--pages", str(self.pages), "--scans", str(self.scans),
                "--alto", str(self.alto), "--work", str(self.work)]
        if use_bin:
            args += ["--bin", str(self.bin)]
        return subprocess.run(args, cwd=ROOT, env=env, text=True, capture_output=True, timeout=30, check=False)

    def events(self, kind):
        if not self.trace.exists():
            return []
        return [event for event in (json.loads(line) for line in self.trace.read_text().splitlines())
                if event["tool"] == kind]


class PrepPilotTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        FIXTURES.mkdir()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(FIXTURES)

    def setUp(self):
        self.base = FIXTURES / self._testMethodName
        self.base.mkdir()

    def fixture(self, name="case"):
        return Fixture(self.base / name)

    def succeeds(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PREP-DONE", result.stdout.splitlines())

    def fails(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("PREP-DONE", result.stdout)
        self.assertIn(message, result.stderr)

    def test_complete_unit_is_preserved_and_original_cut_paths_are_used(self):
        f = self.fixture()
        f.select("complete", newline=False)
        unit = f.unit("complete", b"existing page", b"existing manifest")
        (unit / "notes.txt").write_text("keep")
        before = {p.name: (p.read_bytes(), p.stat().st_ino, p.stat().st_mtime_ns) for p in unit.iterdir()}
        (f.work / "prep.log").write_text("previous log\n")
        self.succeeds(f.run(use_bin=False))
        self.assertEqual({p.name: (p.read_bytes(), p.stat().st_ino, p.stat().st_mtime_ns)
                          for p in unit.iterdir()}, before)
        self.assertEqual(f.events("local-unit"), [])
        self.assertEqual(f.events("cut")[0]["args"], [
            "cut", "--units", str(f.work / "units"), "--out", str(f.work / "crops")
        ])
        self.assertTrue((f.work / "prep.log").read_text().startswith("previous log\n"))
        self.assertIn("/runner/work/", str(f.work))
        self.assertFalse(list((f.work / "pages").glob(".prep-pilot-*")))

    def test_page_only_unit_is_rebuilt_with_the_synced_writer_and_rerun_skips_it(self):
        f = self.fixture()
        f.select("a")
        page = f.scan("a", b"interrupted page")
        f.add_alto("a")
        saved = f.work / "pages/a.jpg"
        saved.write_bytes(page)
        saved_stat = saved.stat()
        unit = f.unit("a", page=page)
        (unit / "notes.txt").write_text("keep")

        self.succeeds(f.run())
        event = f.events("local-unit")[0]
        self.assertEqual(event["image"], str(unit / "page.jpg"))
        self.assertEqual(event["dir"], str(unit))
        self.assertIn("/runner/work/", event["image"])
        self.assertNotIn("--crop-seed", event["args"])
        self.assertNotIn("--k-crops", event["args"])
        self.assertEqual([(e["name"], e["dir"]) for e in f.events("atomic")],
                         [("page.jpg", str(unit)), ("qwen.json", str(unit))])
        self.assertEqual((unit / "page.jpg").read_bytes(), page)
        self.assertTrue((unit / "qwen.json").is_file())
        self.assertEqual((unit / "notes.txt").read_text(), "keep")
        self.assertEqual((saved.read_bytes(), saved.stat().st_ino, saved.stat().st_mtime_ns),
                         (page, saved_stat.st_ino, saved_stat.st_mtime_ns))

        (f.scans / "a.jpg").unlink()
        (f.alto / "a.xml").unlink()
        self.succeeds(f.run(use_bin=False))
        self.assertEqual(len(f.events("local-unit")), 1)
        self.assertEqual(len(f.events("cut")), 2)

    def test_ambiguous_manifest_and_mismatched_pages_are_preserved(self):
        f = self.fixture("manifest-only")
        f.select("a")
        unit = f.unit("a", manifest=b"existing manifest")
        self.fails(f.run(), "ambiguous qwen.json")
        self.assertEqual((unit / "qwen.json").read_bytes(), b"existing manifest")
        self.assertFalse((unit / "page.jpg").exists())
        self.assertEqual(f.events("local-unit"), [])
        self.assertEqual(f.events("cut"), [])

        for mismatch in ("unit", "saved"):
            with self.subTest(mismatch=mismatch):
                f = self.fixture(mismatch)
                f.select("a")
                f.scan("a", b"current scan")
                f.add_alto("a")
                page = b"old unit page" if mismatch == "unit" else b"current scan"
                unit = f.unit("a", page=page)
                saved = f.work / "pages/a.jpg"
                saved.write_bytes(b"old saved page" if mismatch == "saved" else page)
                self.fails(f.run(), "page differs")
                self.assertEqual((unit / "page.jpg").read_bytes(), page)
                self.assertEqual(saved.read_bytes(), b"old saved page" if mismatch == "saved" else page)
                self.assertFalse((unit / "qwen.json").exists())
                self.assertEqual(f.events("local-unit"), [])

    def test_missing_inputs_and_conversion_failures_prevent_completion(self):
        for missing in ("alto", "scan"):
            with self.subTest(missing=missing):
                f = self.fixture("missing-" + missing)
                f.select("complete", "a")
                done = f.unit("complete", page=b"complete page", manifest=b"complete manifest")
                orphan = f.unit("a", page=b"synthetic jpeg")
                (f.work / "pages/a.jpg").write_bytes(b"synthetic jpeg")
                if missing != "alto":
                    f.add_alto("a")
                if missing != "scan":
                    f.scan("a")
                self.fails(f.run(), "no-" + missing)
                self.assertEqual((done / "qwen.json").read_bytes(), b"complete manifest")
                self.assertEqual((orphan / "page.jpg").read_bytes(), b"synthetic jpeg")
                self.assertFalse((orphan / "qwen.json").exists())
                self.assertEqual(f.events("local-unit"), [])
                self.assertEqual(f.events("cut"), [])

        f = self.fixture("jp2")
        f.select("a")
        f.add_alto("a")
        f.scan("a", b"jp2", ext="jp2")
        (f.work / "pages/a.png").write_bytes(b"unrelated png")
        self.fails(f.run(FAIL_OPJ="1"), "decompression-failed")
        self.fails(f.run(FAIL_JPEG="1"), "jpeg-failed")
        self.assertEqual((f.work / "pages/a.png").read_bytes(), b"unrelated png")
        self.assertFalse((f.work / "units/a").exists())
        self.assertEqual(f.events("local-unit"), [])
        self.assertEqual(f.events("cut"), [])
        self.succeeds(f.run())
        self.assertEqual((f.work / "units/a/page.jpg").read_bytes(), b"jpeg:decoded:jp2")

    def test_build_cut_and_incomplete_output_failures_are_resumable(self):
        f = self.fixture("build")
        f.select("a")
        page = f.scan("a")
        f.add_alto("a")
        unit = f.unit("a", page=page)
        (unit / "notes.txt").write_text("keep")
        self.fails(f.run(FAIL_UNIT="1"), "unit-failed")
        self.assertEqual((unit / "page.jpg").read_bytes(), page)
        self.assertEqual((unit / "notes.txt").read_text(), "keep")
        self.assertFalse((unit / "qwen.json").exists())
        self.assertEqual(f.events("cut"), [])
        self.succeeds(f.run())
        self.assertTrue((unit / "qwen.json").exists())

        f = self.fixture("incomplete")
        f.select("a")
        f.scan("a")
        f.add_alto("a")
        self.fails(f.run(UNIT_MISSING_JOB="1"), "local-unit produced an incomplete unit")
        self.assertTrue((f.work / "units/a/page.jpg").exists())
        self.assertFalse((f.work / "units/a/qwen.json").exists())
        self.assertEqual(f.events("cut"), [])
        self.succeeds(f.run())

        f = self.fixture("cut")
        f.select("a")
        unit = f.unit("a", page=b"page", manifest=b"manifest")
        self.fails(f.run(FAIL_CUT="1"), "cut-failed")
        self.assertEqual((unit / "qwen.json").read_bytes(), b"manifest")
        self.assertTrue((f.work / "crops/partial.png").exists())
        self.succeeds(f.run())

        f = self.fixture("removed-during-cut")
        f.select("a")
        f.unit("a", page=b"page", manifest=b"manifest")
        self.fails(f.run(CUT_REMOVE_JOB="a"), "frozen unit is incomplete")


if __name__ == "__main__":
    unittest.main()
