"""scripts/ocr-survey/loc_column.py: the LoC column of the reader comparison (#968).

The batch logic runs against a stand-in for `usnm-ocr-survey alto-crop`. The end-to-end test runs
the real binary when USNM_OCR_SURVEY_BIN names one (or `usnm-ocr-survey` is on PATH) and is skipped
otherwise. All inputs are synthetic.
"""

import hashlib
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import loc_column  # noqa: E402

# A JPEG header for a 100 x 200 image (the SOF0 segment is all loc_align.jpeg_dimensions reads).
JPEG = b"\xff\xd8\xff\xc0\x00\x0b\x08\x00\xc8\x00\x64\x01\x01\x11\x00\xff\xd9"

# LoC-style ALTO in inch1200, page 400 x 800 units: a scale of 0.25 to the 100 x 200 image.
# Line 1 ends with a hyphen; "edge" has its centre outside the test box.
ALTO = """<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
<Description><MeasurementUnit>inch1200</MeasurementUnit>
<OCRProcessing ID="OCR_0"><ocrProcessingStep><processingSoftware>
<softwareCreator>Synthetic</softwareCreator><softwareName>Fixture</softwareName><softwareVersion>1</softwareVersion>
</processingSoftware></ocrProcessingStep></OCRProcessing></Description>
<Layout><Page ID="P1" WIDTH="400" HEIGHT="800" PHYSICAL_IMG_NR="1"><PrintSpace HPOS="0" VPOS="0" WIDTH="400" HEIGHT="800">
<TextBlock ID="B1" HPOS="0" VPOS="0" WIDTH="400" HEIGHT="200">
<TextLine HPOS="0" VPOS="40" WIDTH="400" HEIGHT="40">
<String HPOS="40" VPOS="40" WIDTH="80" HEIGHT="40" CONTENT="The"/><SP/>
<String HPOS="140" VPOS="40" WIDTH="120" HEIGHT="40" CONTENT="inter"/><HYP CONTENT="-"/>
</TextLine>
<TextLine HPOS="0" VPOS="100" WIDTH="400" HEIGHT="40">
<String HPOS="40" VPOS="100" WIDTH="120" HEIGHT="40" CONTENT="national"/><SP/>
<String HPOS="340" VPOS="100" WIDTH="60" HEIGHT="40" CONTENT="edge"/>
</TextLine>
</TextBlock></PrintSpace></Page></Layout></alto>
"""

# A stand-in for `usnm-ocr-survey alto-crop`: echoes its arguments, so the test can check them.
FAKE = """#!/usr/bin/env python3
import json, sys
a = dict(zip(sys.argv[2::2], sys.argv[3::2]))
assert sys.argv[1] == "alto-crop", sys.argv
assert open(a["--text"]).read() == ""
if "fail" in a["--alto"]:
    sys.exit("refused")
print(json.dumps({"alto_text": a["--alto"].rsplit("/", 1)[-1] + " " + a["--box"],
                  "frame": [float(a["--image-width"]), float(a["--image-height"])], "clipped_words": 2}))
"""


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def write_jsonl(p: Path, rows: list[dict]) -> None:
    p.write_text("".join(json.dumps(r) + "\n" for r in rows), encoding="utf-8")


class Fixture:
    """A manifest with one row of each kind, the pages under one directory, and a fake binary."""

    def __init__(self, root: Path):
        self.root = root
        self.loc = root / "loc"
        self.loc.mkdir()
        self.pages = {}
        for stem in ("sn1-reel1-0001-0001", "sn84026749_1900-01-01_ed-1_seq-2", "sn1-reel1-fail-0002"):
            img = JPEG + stem.encode()  # distinct bytes per page, same header
            (self.loc / f"{stem}.jpg").write_bytes(img)
            (self.loc / f"{stem}.xml").write_text(ALTO, encoding="utf-8")
            self.pages[stem] = sha(img)
        ok, own, fail = self.pages
        self.rows = [
            {"crop_id": "pub-overproof-ca-001", "page_id": f"overproof-ca-{ok}", "source_dataset": "overproof-ca",
             "box": [10, 0, 95, 40], "page_image_sha256": self.pages[ok]},
            {"crop_id": "eng-01", "page_id": own, "box": [5, 5, 50, 60], "page_image_sha256": self.pages[own]},
            {"crop_id": "pub-bln600-001", "page_id": "bln600-0001", "source_dataset": "bln600",
             "box": None, "page_image_sha256": None},
            {"crop_id": "pub-overproof-ca-002", "page_id": "overproof-ca-sn1-reel1-0001-0009",
             "source_dataset": "overproof-ca", "box": [0, 0, 10, 10], "page_image_sha256": "0" * 64},
            {"crop_id": "pub-overproof-ca-003", "page_id": f"overproof-ca-{ok}", "source_dataset": "overproof-ca",
             "box": [0, 0, 10, 10], "page_image_sha256": "f" * 64},
            {"crop_id": "pub-overproof-ca-004", "page_id": f"overproof-ca-{fail}", "source_dataset": "overproof-ca",
             "box": [0, 0, 10, 10], "page_image_sha256": self.pages[fail]},
        ]
        self.manifest = root / "manifest.jsonl"
        write_jsonl(self.manifest, self.rows)
        self.fake = root / "fake-usnm-ocr-survey"
        self.fake.write_text(FAKE, encoding="utf-8")
        self.fake.chmod(0o755)


class Batch(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.f = Fixture(Path(self.tmp.name))
        self.out = Path(self.tmp.name) / "out"

    def tearDown(self):
        self.tmp.cleanup()

    def run_tool(self, *extra: str) -> int:
        return loc_column.main(["--manifest", str(self.f.manifest), "--alto-dir", str(self.f.loc),
                                "--out", str(self.out), "--alto-crop-bin", str(self.f.fake), *extra])

    def test_page_stems(self):
        self.assertEqual(loc_column.page_stem(self.f.rows[0]), "sn1-reel1-0001-0001")
        self.assertEqual(loc_column.page_stem(self.f.rows[1]), "sn84026749_1900-01-01_ed-1_seq-2")
        self.assertIsNone(loc_column.page_stem(self.f.rows[2]))
        # A public set's row is never taken for an own crop, whatever its page_id looks like.
        self.assertIsNone(loc_column.page_stem({"page_id": "sn1_1900-01-01_ed-1_seq-1", "source_dataset": "nzz"}))

    def test_outputs_statuses_and_exit(self):
        self.assertEqual(self.run_tool(), 1)  # a missing page, a bad checksum and a refusal
        outputs = [json.loads(x) for x in (self.out / "outputs.jsonl").read_text().splitlines()]
        self.assertEqual(outputs, [
            {"crop_id": "pub-overproof-ca-001", "id": "pub-overproof-ca-001",
             "text": "sn1-reel1-0001-0001.xml 10,0,95,40"},
            {"crop_id": "eng-01", "id": "eng-01", "text": "sn84026749_1900-01-01_ed-1_seq-2.xml 5,5,50,60"},
            {"crop_id": "pub-overproof-ca-002", "id": "pub-overproof-ca-002", "text": ""},
            {"crop_id": "pub-overproof-ca-003", "id": "pub-overproof-ca-003", "text": ""},
            {"crop_id": "pub-overproof-ca-004", "id": "pub-overproof-ca-004", "text": ""},
        ])
        report = json.loads((self.out / "loc-column.json").read_text())
        self.assertEqual(report["counts"], {"aligned": 2, "error": 2, "missing": 1, "not-chronicling-america": 1})
        by = {c["crop_id"]: c for c in report["crops"]}
        self.assertEqual(by["pub-overproof-ca-001"]["image_size"], [100, 200])
        self.assertEqual(by["pub-overproof-ca-001"]["frame"], [100.0, 200.0])
        self.assertEqual(by["pub-overproof-ca-001"]["alto_sha256"], sha(ALTO.encode()))
        self.assertEqual(by["pub-overproof-ca-001"]["alto_software"][0]["softwareName"], "Fixture")
        self.assertEqual(by["pub-overproof-ca-002"]["reason"], "no ALTO")
        self.assertIn("page image checksum mismatch", by["pub-overproof-ca-003"]["reason"])
        self.assertIn("refused", by["pub-overproof-ca-004"]["reason"])
        self.assertEqual(report["manifest_sha256"], sha(self.f.manifest.read_bytes()))
        self.assertEqual(report["outputs_sha256"], sha((self.out / "outputs.jsonl").read_bytes()))
        self.assertEqual(report["alto_crop_sha256"], sha(self.f.fake.read_bytes()))

    def test_clean_manifest_exits_zero(self):
        write_jsonl(self.f.manifest, self.f.rows[:3])
        self.assertEqual(self.run_tool(), 0)

    def test_separate_image_dir(self):
        images = Path(self.tmp.name) / "images"
        images.mkdir()
        for p in self.f.loc.glob("*.jpg"):
            p.rename(images / p.name)
        write_jsonl(self.f.manifest, self.f.rows[:2])
        self.assertEqual(self.run_tool("--image-dir", str(images)), 0)

    def test_refuses_existing_out_and_duplicate_ids(self):
        self.out.mkdir()
        self.assertEqual(self.run_tool(), 2)
        self.out.rmdir()
        write_jsonl(self.f.manifest, [self.f.rows[0], self.f.rows[0]])
        self.assertEqual(self.run_tool(), 2)
        self.assertFalse(self.out.exists())


def real_binary() -> str | None:
    return os.environ.get("USNM_OCR_SURVEY_BIN") or shutil.which("usnm-ocr-survey")


@unittest.skipUnless(real_binary(), "needs usnm-ocr-survey (set USNM_OCR_SURVEY_BIN)")
class EndToEnd(unittest.TestCase):
    def test_real_alto_crop(self):
        with tempfile.TemporaryDirectory() as tmp:
            f = Fixture(Path(tmp))
            # Box 10..95 x 0..40 in image pixels is 40..380 x 0..160 in ALTO units: "edge" (centre
            # x 370) is in, so all four words are; "The" centre x 80 is in too.
            write_jsonl(f.manifest, [f.rows[0], {**f.rows[0], "crop_id": "narrow", "box": [10, 0, 80, 40]}])
            out = Path(tmp) / "out"
            code = loc_column.main(["--manifest", str(f.manifest), "--alto-dir", str(f.loc), "--out", str(out),
                                    "--alto-crop-bin", real_binary()])
            self.assertEqual(code, 0)
            texts = {r["crop_id"]: r["text"] for r in map(json.loads, (out / "outputs.jsonl").read_text().splitlines())}
            self.assertEqual(texts["pub-overproof-ca-001"], "The inter-\nnational edge")
            # x < 80 px is x < 320 units: "edge" (centre 370) is cut, the hyphen stays with "inter".
            self.assertEqual(texts["narrow"], "The inter-\nnational")


if __name__ == "__main__":
    unittest.main()
