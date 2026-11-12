import json, os, tempfile, unittest
from transcribe_tasks import build


class T(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        open(os.path.join(self.d, "x.png"), "wb").write(b"png")

    def run_build(self, items, cal=()):
        out = os.path.join(self.d, "out")
        build(list(items), list(cal), self.d, "yi-truth", "s1", {"name": "Y", "min_examples": 3}, out)
        return out

    def test_task_and_calibration(self):
        out = self.run_build([{"id": "l/1", "image": "x.png", "page": "p", "source": "u", "label": "L"}],
                             [{"id": "c1", "image": "x.png", "truth": "abc"}])
        rows = [json.loads(l) for l in open(os.path.join(out, "tasks.jsonl"))]
        self.assertEqual(rows[0]["id"], "yi-truth/s1/l_1")
        self.assertNotIn("truth", rows[0])
        self.assertEqual(rows[1]["truth"], "abc")
        self.assertTrue(os.path.isfile(os.path.join(out, "clips", rows[0]["line"])))
        cfg = json.load(open(os.path.join(out, "language.json")))
        self.assertEqual((cfg["kind"], "min_examples" in cfg), ("transcribe", False))

    def test_refuses_machine_text(self):
        for k in ("a", "hints", "text", "loc"):
            with self.assertRaises(SystemExit):
                self.run_build([{"id": "1", "image": "x.png", k: "x"}])

    def test_duplicate(self):
        with self.assertRaises(SystemExit):
            self.run_build([{"id": "1", "image": "x.png"}, {"id": "1", "image": "x.png"}])


if __name__ == "__main__":
    unittest.main()
