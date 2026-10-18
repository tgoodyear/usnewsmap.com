import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("loc_align", Path(__file__).with_name("loc_align.py"))
loc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loc)

ALTO = b"""<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
<Description><MeasurementUnit>inch1200</MeasurementUnit></Description>
<Layout><Page WIDTH="400" HEIGHT="800"><TextBlock>
<TextLine HPOS="0" VPOS="40" WIDTH="400" HEIGHT="40">
<String HPOS="0" VPOS="40" WIDTH="40" HEIGHT="40" CONTENT="outside"/>
<String HPOS="120" VPOS="40" WIDTH="40" HEIGHT="40" CONTENT="inside"/>
<String HPOS="360" VPOS="40" WIDTH="40" HEIGHT="40" CONTENT="edge"/>
</TextLine></TextBlock></Page></Layout></alto>"""
JPEG = b"\xff\xd8\xff\xc0\x00\x0b\x08\x00\xc8\x00\x64\x01\x01\x11\x00\xff\xd9"


class AlignTests(unittest.TestCase):
    def test_explicit_scale_and_boundary_policy(self):
        result = loc.align(ALTO, 100, 200, [10, 0, 95, 30], .25)
        self.assertEqual(result["text"], "inside")
        self.assertEqual(result["boundary_lines"], 1)
        self.assertEqual(result["policy"], "loc-word-centres-xml-order-v1")

    def test_jpeg_header(self):
        self.assertEqual(loc.jpeg_dimensions(JPEG), (100, 200))
        for invalid in (b"notjpeg", JPEG[:8], JPEG.replace(b"\x00\x0b", b"\x00\x01")):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                loc.jpeg_dimensions(invalid)

    def test_reject_ambiguous_geometry(self):
        cases = [
            (ALTO, .3),
            (ALTO.replace(b"inch1200", b"unknown"), .25),
            (ALTO.replace(b'HPOS="120"', b'HPOS="nan"'), .25),
            (ALTO.replace(b'HPOS="120"', b''), .25),
            (ALTO.replace(b'WIDTH="40" HEIGHT="40" CONTENT="inside"', b'WIDTH="0" HEIGHT="40" CONTENT="inside"'), .25),
            (ALTO.replace(b'HPOS="360"', b'HPOS="400"'), .25),
        ]
        for xml, scale in cases:
            with self.subTest(xml=xml, scale=scale), self.assertRaises((ValueError, KeyError)):
                loc.align(xml, 100, 200, [10, 0, 95, 30], scale)
        with self.assertRaisesRegex(ValueError, "crop"):
            loc.align(ALTO, 100, 200, [0, 0, 101, 30], .25)

    def test_cli_keeps_missing_and_rejects_wrong_source(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            (root / "source.jpg").write_bytes(JPEG)
            crop = JPEG.replace(b"\x00\xc8", b"\x00\x1e").replace(b"\x00\x64", b"\x00\x55")
            (root / "crop.jpg").write_bytes(crop)
            (root / "reference.txt").write_text("inside")
            (root / "alto.xml").write_bytes(ALTO)
            image_hash = hashlib.sha256(JPEG).hexdigest()
            sample = {"id": "one", "image": {"path": "crop.jpg", "sha256": hashlib.sha256(crop).hexdigest()},
                      "image_width": 85, "image_height": 30,
                      "reference": {"path": "reference.txt", "sha256": hashlib.sha256(b"inside").hexdigest()},
                      "reference_status": "human-verified",
                      "provenance": {"page_url": "https://example.org/page",
                                     "page_image_sha256": image_hash, "box": [10, 0, 95, 30]}}
            missing = {**sample, "id": "two"}
            (root / "manifest.json").write_text(json.dumps(
                {"samples": [sample, missing], "subsets": {"test": {"members": ["one", "two"]}}}))
            binding = {"id": "one", "source_page_url": "https://example.org/page",
                       "source_image": str(root / "source.jpg"), "source_image_sha256": image_hash,
                       "alto": str(root / "alto.xml"), "alto_sha256": hashlib.sha256(ALTO).hexdigest(),
                       "scale": .25, "artifact_url": "https://example.org/alto",
                       "retrieved_at": "2026-10-18", "version_evidence": "embedded test version",
                       "registration_evidence": "Synthetic same-scan, orientation and zero-offset fixture"}
            for index, expected in enumerate((0, 1)):
                if index:
                    binding["source_image_sha256"] = "0" * 64
                (root / "bindings.jsonl").write_text(json.dumps(binding) + "\n")
                out = root / str(index)
                result = subprocess.run([sys.executable, str(Path(loc.__file__)), "--release", str(root),
                                         "--subset", "test", "--baseline", "loc-refreshed-v1",
                                         "--bindings", str(root / "bindings.jsonl"), "--out-dir", str(out)],
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, expected, result.stderr)
                report = json.loads((out / "alignment.json").read_text())
                self.assertEqual(report["counts"]["missing"], 1)
                outputs = (out / "outputs.jsonl").read_text()
                if not index:
                    self.assertEqual(json.loads(outputs)["text"], "inside")
                    self.assertEqual(report["inference_cost"], "unknown")
                else:
                    self.assertEqual(outputs, "")
                    self.assertEqual(report["counts"]["error"], 1)


if __name__ == "__main__":
    unittest.main()
