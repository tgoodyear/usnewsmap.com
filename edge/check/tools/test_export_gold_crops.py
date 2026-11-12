import unittest
from export_gold_crops import merge


class T(unittest.TestCase):
    def test_merge(self):
        tpl = [{"crop_id": "c1", "transcription": None, "transcriber": None, "reviewer": None}, {"crop_id": "c2", "transcription": None}]
        exp = [{"id": "de-968/de968/c1", "crop_id": "c1", "settled": True, "method": "single", "text": "Die ſchöne"},
               {"id": "de-968/de968/c2", "settled": False}]
        out, done = merge(tpl, exp)
        self.assertEqual(done, 1)
        self.assertEqual((out[0]["transcription"], out[0]["transcriber"], out[0]["reviewer"]), ("Die ſchöne", "maintainer", None))
        self.assertIsNone(out[1]["transcription"])


if __name__ == "__main__":
    unittest.main()
