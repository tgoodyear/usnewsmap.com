"""build_resources.py's copy of the study's text handling and counts against the golden vectors
the study's own qe_lib wrote (crates/usnm-core/tests/data/qe-golden.json, scripts/qe/golden.py)."""
import json, os, sys, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import build_resources as b  # noqa: E402

GOLDEN = os.path.join(HERE, "..", "..", "crates", "usnm-core", "tests", "data", "qe-golden.json")


class GoldenTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(GOLDEN, encoding="utf-8") as f:
            cls.g = json.load(f)

    def test_text_handling(self):
        for c in self.g["cases"]:
            t = c["text"]
            self.assertEqual(b.norm_text(t), c["normalized"], t)
            self.assertEqual([b.fold(w, "eng") for w in b.WORD.findall(b.norm_text(t))], c["words"], t)
            self.assertEqual(b.lm_text(t, "eng"), c["lm_text"], t)
        for f in self.g["folds"]:
            self.assertEqual(b.fold(f["text"], f["lang"]), f["fold"])
            self.assertEqual(b.lm_text(f["text"], f["lang"]), f["lm_text"])

    def test_counts(self):
        cnt, ctx, types, vocab = b.train_counts(self.g["train"])
        self.assertEqual(vocab, self.g["vocab"])
        got = sorted([k, cnt.get(k, 0), ctx.get(k, 0), types.get(k, 0)] for k in set(cnt) | set(ctx))
        self.assertEqual(got, sorted(self.g["entries"]))


if __name__ == "__main__":
    unittest.main()
