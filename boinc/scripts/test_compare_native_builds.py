"""compare-native-builds.py fails on any difference between two builds, and on a build that fails (#1835)."""
import importlib.util
import os
import stat
import sys
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("compare_native_builds", Path(__file__).with_name("compare-native-builds.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

# A stand-in engine: writes page.txt and page.xml next to the output base it is given, as
# `tesseract IMAGE BASE ... txt alto` does. MODE picks what it does wrong, if anything.
ENGINE = """#!{python}
import sys
mode = {mode!r}
base = sys.argv[2]
if mode == "fail":
    sys.stderr.write("engine failed")
    sys.exit(1)
if mode != "no_txt":
    open(base + ".txt", "w").write("text" + ("!" if mode == "text" else ""))
if mode != "no_xml":
    open(base + ".xml", "w").write("<alto" + (' conf="0.90"' if mode == "alto" else "") + "/>")
"""


class Compare(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        (self.root / "tessdata").mkdir()
        (self.root / "tessdata" / "eng.traineddata").write_bytes(b"model")
        (self.root / "page.png").write_bytes(b"not read by the stand-ins")
        self.log = []

    def engine(self, name, mode, extra=""):
        path = self.root / name
        path.write_text(ENGINE.format(python=sys.executable, mode=mode) + extra)
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return path

    def run_compare(self, old, new):
        return module.compare(old, new, self.root / "tessdata", [self.root / "page.png"], say=self.log.append)

    def test_identical_output_passes_and_prints_both_digests(self):
        self.assertEqual(self.run_compare(self.engine("old", "same"), self.engine("new", "same", "# patched build\n")), 0)
        self.assertEqual(sum(line.startswith(("old engine sha256:", "new engine sha256:")) for line in self.log), 2)

    def test_different_diagnostics_alone_do_not_fail(self):
        warn = "sys.stderr.write('a warning only the new build prints')\n"
        self.assertEqual(self.run_compare(self.engine("old", "same"), self.engine("new", "same", warn)), 0)

    def test_a_text_difference_fails(self):
        self.assertEqual(self.run_compare(self.engine("old", "same"), self.engine("new", "text")), 1)

    def test_an_alto_difference_fails(self):
        self.assertEqual(self.run_compare(self.engine("old", "same"), self.engine("new", "alto")), 1)

    def test_a_missing_output_file_fails_even_if_both_miss_it(self):
        self.assertEqual(self.run_compare(self.engine("old", "no_xml"), self.engine("new", "same")), 1)
        self.assertEqual(self.run_compare(self.engine("old2", "no_txt"), self.engine("new2", "no_txt", "# b\n")), 1)

    def test_a_nonzero_exit_fails_even_if_both_fail(self):
        self.assertEqual(self.run_compare(self.engine("old", "same"), self.engine("new", "fail")), 1)
        self.assertEqual(self.run_compare(self.engine("old2", "fail"), self.engine("new2", "fail", "# b\n")), 1)
        self.assertTrue(any("engine failed" in line for line in self.log))

    def test_the_same_file_twice_compares_nothing_and_fails(self):
        engine = self.engine("same", "same")
        self.assertEqual(self.run_compare(engine, engine), 1)

    def test_no_images_fails(self):
        old, new = self.engine("old", "same"), self.engine("new", "same", "# b\n")
        empty = self.root / "empty"
        empty.mkdir()
        self.assertEqual(module.compare(old, new, self.root / "tessdata", [empty], say=self.log.append), 1)

    def test_main_exit_status(self):
        old, new = self.engine("old", "same"), self.engine("new", "text")
        args = ["--old", str(old), "--new", str(new), "--tessdata", str(self.root / "tessdata"), "--images", str(self.root / "page.png")]
        self.assertEqual(module.main(args), 1)
        self.assertEqual(module.main(["--old", str(old), "--new", str(self.engine("new2", "same", "# b\n"))] + args[4:]), 0)


if __name__ == "__main__":
    unittest.main()
