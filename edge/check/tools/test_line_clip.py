"""Tests for line_clip.py and build_tasks.py --clip-band: python3 -m unittest discover -s edge/check/tools."""
import json
import os
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(__file__))
import line_clip  # noqa: E402

# chr-rare/rare-v1 .../sn83020866_1828-03-20_ed-1_seq-1_P1_TL00132: LoC's word boxes in the 110-pixel line image. Two of them
# reach into the next printed line, so the TextLine box (and the line image) covers two lines.
TL00132 = [[13, 13, 83, 28], [108, 21, 179, 66], [304, 15, 151, 82], [474, 28, 91, 19], [567, 18, 100, 33]]


class Rule(unittest.TestCase):
    def test_band_leaves_out_tall_boxes(self):
        self.assertEqual(line_clip.printed_band(TL00132), (18, 47, 33))

    def test_own_box_widens_the_band(self):
        self.assertEqual(line_clip.printed_band(TL00132, TL00132[0]), (13, 47, 33))
        self.assertEqual(line_clip.printed_band(TL00132, TL00132[2]), (18, 47, 33))  # a tall box does not

    def test_two_line_crop_is_cut_below(self):
        """The word 4ᏍᏗ: the crop was the whole 110-pixel line image; the top is within half a word height of the band."""
        top, bottom, cut = line_clip.clip_span(0, 110, line_clip.printed_band(TL00132, TL00132[0]))
        self.assertTrue(cut)
        self.assertEqual(top, 0)
        self.assertAlmostEqual(bottom, 47 + 3.3)

    def test_one_line_crop_is_kept(self):
        boxes = [[0, 6, 50, 20], [60, 7, 40, 20], [110, 4, 60, 24]]
        self.assertEqual(line_clip.clip_span(0, 34, line_clip.printed_band(boxes, boxes[0])), (0, 34, False))

    def test_small_cut_is_not_kept(self):
        """A side past REACH but a cut under SHRINK leaves the crop as it is."""
        band = (10, 90, 20)
        self.assertEqual(line_clip.clip_span(0, 101, band), (0, 101, False))
        self.assertEqual(line_clip.clip_span(0, 101, band, shrink=0.05), (0, 92, True))

    def test_no_boxes(self):
        self.assertIsNone(line_clip.printed_band([]))
        self.assertEqual(line_clip.clip_span(0, 50, None), (0, 50, False))


class BuildTasks(unittest.TestCase):
    def test_clip_band(self):
        try:
            from PIL import Image
        except ImportError:
            self.skipTest("Pillow not installed")
        with tempfile.TemporaryDirectory() as d:
            Image.new("L", (680, 110), 255).save(os.path.join(d, "l1.png"))
            texts = ["ᏜᏍᏗ", "ᏕᎦᎳᏫᎦᎬᏰᎵ", "ᏗᎪᏪᏢᏍᏆ", "ᏣᎭᎩᏃ", "ᎠᏥᏅ"]
            line = {"id": "p1_l1", "image": "l1.png", "page": "sn1/1828-03-20/ed-1/seq-1", "source": "https://example.org/", "label": "L",
                    "a": " ".join(texts), "a_words": [{"text": t, "box": b} for t, b in zip(texts, TL00132)],
                    "b": "ᏍᏍᏗ ᏕᎦᎳᏫᎦᎬᏰᎵ ᏗᎪᏪᏢᏍᏆ ᏣᎭᎩᏃ ᎠᏥᏅ", "words": [0]}
            with open(os.path.join(d, "lines.jsonl"), "w", encoding="utf-8") as f:
                f.write(json.dumps(line, ensure_ascii=False) + "\n")
            with open(os.path.join(d, "cfg.json"), "w", encoding="utf-8") as f:
                json.dump({"name": "Cherokee", "dir": "ltr"}, f)
            runs = {}
            for name, extra in (("plain", []), ("clip", ["--clip-band"])):
                out = os.path.join(d, name)
                subprocess.run([sys.executable, os.path.join(os.path.dirname(__file__), "build_tasks.py"), "--lines", os.path.join(d, "lines.jsonl"),
                                "--images", d, "--lang", "chr", "--set", "t1", "--a-is", "loc", "--b-is", "ours",
                                "--language-config", os.path.join(d, "cfg.json"), "--out-dir", out, *extra], check=True, capture_output=True)
                t = json.loads(open(os.path.join(out, "tasks.jsonl"), encoding="utf-8").readline())
                runs[name] = (t, Image.open(os.path.join(out, "clips", t["word"])).size)
            (plain, plain_size), (clip, clip_size) = runs["plain"], runs["clip"]
            self.assertEqual(plain["word"], "chr/t1/words/p1_l1_w0.jpg")
            self.assertEqual(clip["word"], "chr/t1/words/p1_l1_w0_b.jpg")
            self.assertEqual({k: v for k, v in plain.items() if k != "word"}, {k: v for k, v in clip.items() if k != "word"})
            self.assertEqual(plain_size, (83 + 2 * 7, 110))
            self.assertEqual(clip_size, (83 + 2 * 7, 51))


if __name__ == "__main__":
    unittest.main()
