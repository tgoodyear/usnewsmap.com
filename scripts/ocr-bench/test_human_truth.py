import io
import json
import os
import tempfile
import unittest

import human_truth as h

try:
    from PIL import Image
except ImportError:  # the CI job installs it; locally the promote test skips
    Image = None


def r(reader, text, at, unreadable=False, qualified=True):
    return {"reader": reader, "text": text, "unreadable": unreadable, "at": at, "qualified": qualified}


def row(readings, **kw):
    base = {"id": "yi-truth/s/sn1_1913-09-01_x", "page": "sn1/1913-09-01/ed-1/seq-3", "line": "yi-truth/s/x.png", "readings": readings}
    base.update(kw)
    return base


class Rules(unittest.TestCase):
    def test_normalize_matches_the_worker(self):
        # the same cases as transcribe.test.mjs
        self.assertEqual(h.normalize("  א‏ב  \r\n\r\n ג  ד \n\n"), "אב\n\nג ד")
        self.assertEqual(h.normalize("a\x00b‮c"), "abc")
        self.assertEqual(h.normalize("ſ́x"), "ſ́x")

    def test_derive(self):
        self.assertEqual(h.derive([r("a", "x", 1)])[0], None)
        self.assertEqual(h.derive([r("a", "x", 1), r("b", "x ", 2)])[:2], ("agreed", "x"))
        self.assertEqual(h.derive([r("a", "x", 1), r("b", "y", 2)])[0], None)
        self.assertEqual(h.derive([r("a", "x", 1), r("b", "y", 2), r("c", "y", 3)])[:2], ("adjudicated", "y"))
        self.assertEqual(h.derive([r("a", "x", 1), r("a", "x", 2)])[0], None)  # one person twice
        self.assertEqual(h.derive([r("a", "x", 1), r("b", "x", 2, qualified=False), r("c", "x", 3)])[:2], ("agreed", "x"))

    def test_decide(self):
        ok = [r("a", "ab", 1), r("b", "ab", 2)]
        rng = [("1910-01-01", "1928-12-31")]
        self.assertTrue(h.decide(row(ok), {}, rng)[0])
        self.assertEqual(h.decide(row(ok, page="sn1/1935-01-01/ed-1/seq-1"), {}, rng)[1], "rights-unconfirmed")
        self.assertEqual(h.decide(row(ok), {row(ok)["id"]: {"ab"}}, rng)[1], "equals-an-engine-hint")
        self.assertEqual(h.decide(row([r("a", "", 1, True), r("b", "", 2, True)]), {}, rng)[1], "agreed-unreadable")
        many = [r("a", "a��", 1), r("b", "a��", 2)]
        self.assertEqual(h.decide(row(many), {}, rng)[1], "too-many-unreadable-marks")
        self.assertEqual(h.decide(row(ok, outcome="adjudicated"), {}, rng)[1], "export-disagrees-with-readings")


@unittest.skipIf(Image is None, "Pillow missing")
class Promote(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.conf = {"yi-truth": {"notice": "n", "attribution": "a", "image_license": "l", "languages": ["yid"], "scripts": ["Hebr"]}}

    def fake(self, base, key, images_dir):
        b = io.BytesIO()
        Image.new("L", (40, 10), 200).save(b, "PNG")
        return b.getvalue()

    def run_promote(self, release="v1.2.0", rows=None, releases=None):
        rows = rows or [row([r("a", "אב", 1), r("b", "אב", 2)]), row([r("a", "x", 1), r("b", "y", 2)], id="yi-truth/s/other")]
        return h.promote({"yi-truth": rows}, release, self.d, self.conf, [("1910-01-01", "1928-12-31")], {}, None, "u", releases, fetch=self.fake)

    def test_writes_inputs_and_holds_the_rest(self):
        res = self.run_promote()
        self.assertEqual((res["promoted"], res["held"]), (1, 1))
        base = os.path.join(self.d, "v1.2.0", "human-truth")
        frag = json.load(open(os.path.join(base, "spec-fragment.json")))
        sid = next(iter(frag["annotations"]))
        self.assertEqual(open(os.path.join(base, "usnm-human-yi-truth", "references", sid + ".txt"), encoding="utf-8").read(), "אב")
        self.assertEqual(frag["imports"][0]["subset"], "usnm-human-yi-truth/all-1")
        self.assertTrue(open(os.path.join(base, "INPUTS.sha256")).read().strip())
        self.assertIn("not-settled", open(os.path.join(base, "held.jsonl")).read())

    def test_refuses_frozen_existing_and_listed(self):
        for rel in ("v1.0.0", "v1.1.0"):
            with self.assertRaises(SystemExit):
                self.run_promote(rel)
        self.run_promote()
        with self.assertRaises(SystemExit):
            self.run_promote()
        lst = os.path.join(self.d, "releases.json")
        json.dump({"releases": [{"release": "v1.3.0"}]}, open(lst, "w"))
        with self.assertRaises(SystemExit):
            self.run_promote("v1.3.0", releases=lst)


if __name__ == "__main__":
    unittest.main()
