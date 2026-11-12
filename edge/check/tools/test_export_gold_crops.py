import unittest
from export_gold_crops import merge


class T(unittest.TestCase):
    def test_merge(self):
        tpl = [{"crop_id": "c1", "transcription": None, "transcriber": None, "reviewer": None}, {"crop_id": "c2", "transcription": None}]
        exp = [{"id": "de-968/de968/c1", "crop_id": "c1", "settled": True, "method": "single", "text": "Die ſchöne \ufffd"},
               {"id": "de-968/de968/c2", "settled": False}]
        out, done = merge(tpl, exp)
        self.assertEqual(done, 1)
        self.assertEqual((out[0]["transcription"], out[0]["transcriber"], out[0]["reviewer"]), ("Die ſchöne [?]", "maintainer", None))
        self.assertIsNone(out[1]["transcription"])
        exp[0].update(method="near-agree", inter_cer=0.004, readings=[{"text": "Die ſchöne \ufffd"}, {"text": "Die ſchöne ?"}])
        out, _ = merge(tpl, exp)
        self.assertEqual((out[0]["settle_method"], out[0]["inter_reader_cer"], len(out[0]["readings"]), out[0]["reviewer"]), ("near-agree", 0.004, 2, "check.usnewsmap.com second reader"))


if __name__ == "__main__":
    unittest.main()
