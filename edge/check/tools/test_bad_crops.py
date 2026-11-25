import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bad_crops  # noqa: E402

ROWS = [
    {"id": "chr-lines/s/p1~2", "set": "s", "page": "p1", "settled": True, "outcome": "bad_crop", "held": True, "hold_reason": "bad_crop",
     "clips": {"line": "chr-lines/s/l/p1.jpg", "word": "chr-lines/s/w/p1~2.jpg"}, "counts": {"a": 1, "b": 0, "neither": 0, "unsure": 0, "notext": 0, "badcrop": 2}},
    {"id": "chr-lines/s/p1~3", "set": "s", "page": "p1", "settled": True, "outcome": "a", "counts": {"a": 3}},
    {"id": "en-gutter/g/p~s1", "set": "g", "kind": "gutter", "settled": True, "outcome": "bad_crop", "clips": {"crop": "en-gutter/g/p~s1.jpg"},
     "counts": {"a": 0, "b": 0, "unsure": 0, "badcrop": 3}},
]


class BadCrops(unittest.TestCase):
    def test_lists_only_bad_crop_tasks_with_their_clip_keys(self):
        found = bad_crops.bad_crops(ROWS)
        self.assertEqual([t["id"] for t in found], ["chr-lines/s/p1~2", "en-gutter/g/p~s1"])
        self.assertEqual(found[0]["clips"], {"line": "chr-lines/s/l/p1.jpg", "word": "chr-lines/s/w/p1~2.jpg"})
        self.assertEqual((found[0]["kind"], found[1]["kind"]), ("pick", "gutter"))

    def test_command_line(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "export.jsonl")
            with open(path, "w", encoding="utf-8") as f:
                f.write("".join(json.dumps(r) + "\n" for r in ROWS))
            text = subprocess.run([sys.executable, "-I", os.path.join(HERE, "bad_crops.py"), "--export", path], capture_output=True, text=True, check=True).stdout
            self.assertIn("chr-lines/s/p1~2\tset=s\tpage=p1\tline=chr-lines/s/l/p1.jpg word=chr-lines/s/w/p1~2.jpg\tbadcrop=2/3", text)
            self.assertTrue(text.endswith("2 bad_crop task(s)\n"))
            js = subprocess.run([sys.executable, "-I", os.path.join(HERE, "bad_crops.py"), "--export", path, "--json"], capture_output=True, text=True, check=True).stdout
            self.assertEqual([json.loads(l)["id"] for l in js.splitlines()], ["chr-lines/s/p1~2", "en-gutter/g/p~s1"])


if __name__ == "__main__":
    unittest.main()
