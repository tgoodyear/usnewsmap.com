import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gold_pages as gp  # noqa: E402


class Gold(unittest.TestCase):
    def test_archive_from_jp2_url(self):
        u = "https://tile.loc.gov/storage-services/service/ndnp/ak/batch_ak_arctictern_ver01/data/sn96060045/00414185861/1902050101/0015.jp2"
        self.assertEqual(gp.archive_from_jp2_url(u), "ak_arctictern_ver01.tar.bz2")
        self.assertIsNone(gp.archive_from_jp2_url("https://x/y.jp2"))

    def test_en_op_pages_become_regions(self):
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "p.jsonl")
            with open(p, "w") as f:
                f.write(json.dumps({"page": "sn1/1900-01-01/ed-1/seq-1", "date": "1900-01-01", "alto": "/x/a.xml", "image": "/x/a.jpg",
                                    "crops": [{"id": "c1", "box": [1, 2, 3, 4], "truth": "The cat"}, {"id": "c2", "box": [0, 0, 1, 1]}]}) + "\n")
            got = gp.en_op_pages(p, "/data/op")
            self.assertEqual(got[0]["alto"], "/data/op/a.xml")
            self.assertEqual(got[0]["regions"], [{"id": "c1", "box": [1, 2, 3, 4], "text": "The cat"}])
            self.assertEqual(got[0]["page_id"], "sn1_1900-01-01_ed-1_seq-1")


if __name__ == "__main__":
    unittest.main()
