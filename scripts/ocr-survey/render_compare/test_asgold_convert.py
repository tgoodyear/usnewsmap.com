"""render_compare/asgold_convert.py: the en-asgold set into a loader.py set (#2103)."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import paths  # noqa: E402,F401
import asgold_convert  # noqa: E402
import loader  # noqa: E402

SEL = "rank\tid\tpage\tca_page\n1\ts9-a\tamericanstories-gold-0015\tsn11111111_1902-05-01_ed-1_seq-1\n" \
      "2\ts9-b\tamericanstories-gold-0042\tsn22222222_1904-12-01_ed-1_seq-1\n" \
      "3\ts9-c\tamericanstories-gold-0004\t\n" \
      "4\ts9-d\tamericanstories-gold-0075\tsn33333333_1909-05-07_ed-1_seq-1\n"


def region(pid, text, transcribed=True):
    return {"id": pid, "page": "x", "region": "coco-1", "type": "article", "box": [0, 0, 10, 10], "text": text,
            "transcribed": transcribed}


class ConvertTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = Path(self.tmp.name)
        self.frozen, self.alto = t / "frozen", t / "alto"
        (self.frozen / "LICENSES").mkdir(parents=True)
        self.alto.mkdir()
        (self.frozen / "LICENSES" / "info.json").write_text("{}")
        (self.frozen / "selection.tsv").write_text(SEL)
        rows = [region("s9-a", "one two"), region("s9-a", "", False), region("s9-b", "three"), region("s9-c", "four"),
                region("s9-d", "five")]
        (self.frozen / "regions.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        for stem in ("sn11111111-s9-a", "sn22222222-s9-b", "s9-c"):  # s9-d has no ALTO held
            (self.alto / f"{stem}.xml").write_text("<alto/>")
            (self.alto / f"{stem}.jpg").write_bytes(b"x")
        self.exclude = t / "exclude.txt"
        self.exclude.write_text("sn22222222-00000000000-1904120101-0042\n")

    def tearDown(self):
        self.tmp.cleanup()

    def test_pages_used_and_dropped_with_reasons(self):
        m, c, rep = asgold_convert.convert(self.frozen, self.alto, asgold_convert.read_exclude(self.exclude))
        used = {p["id"]: p for p in rep["pages"]}
        self.assertEqual([p for p in used if used[p]["used"]], ["s9-a"])
        self.assertIn("no Chronicling America page", used["s9-c"]["reason"])
        self.assertIn("overlaps Overproof page sn22222222", used["s9-b"]["reason"])
        self.assertIn("not held", used["s9-d"]["reason"])
        self.assertEqual(len(m), 1)  # the untranscribed region is not a crop
        self.assertEqual(c[0]["transcription"], "one two")
        self.assertEqual(m[0]["source_image"], "sn11111111-s9-a.jpg")  # LCCN first: templates.py pools a title by it

    def test_reads_the_real_key_shapes(self):
        self.exclude.write_text("sn84024828-00211100606-1887061801-0101\nsn85066387-noissue-190001011-0003\n")
        ex = asgold_convert.read_exclude(self.exclude)
        self.assertEqual([(k["lccn"], k["date"], k["num"]) for k in ex],
                         [("sn84024828", "18870618", 101), ("sn85066387", "19000101", 3)])

    def test_overlap_needs_lccn_date_and_page(self):
        ex = asgold_convert.read_exclude(self.exclude)
        self.assertIsNotNone(asgold_convert.overlap("sn22222222_1904-12-01_ed-1_seq-1", 42, ex))
        self.assertIsNone(asgold_convert.overlap("sn22222222_1904-12-01_ed-1_seq-1", 43, ex))
        self.assertIsNone(asgold_convert.overlap("sn22222222_1904-12-02_ed-1_seq-3", 42, ex))

    def test_loader_accepts_the_output(self):
        out = Path(self.tmp.name) / "set"
        argv = ["--frozen-dir", str(self.frozen), "--alto-dir", str(self.alto), "--exclude-file", str(self.exclude),
                "--out", str(out), "--report", str(out / "r.json")]
        self.assertEqual(asgold_convert.main(argv), 0)
        manifest = [json.loads(x) for x in (out / "manifest.jsonl").read_text().splitlines()]
        loader.check_set(out, manifest)  # a licence file, an open licence, for_scoring true: no SystemExit
        self.assertTrue(loader.licence_allows(manifest[0]["source_license"]))


if __name__ == "__main__":
    unittest.main()
