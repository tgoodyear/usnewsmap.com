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
    def test_invalid_provenance_is_a_value_error(self):
        sample = {"provenance": {"page_url": "https://example.org/page", "page_image_sha256": "a" * 64}}
        binding = {"source_page_url": "https://example.org/page", "source_image_sha256": "a" * 64,
                   "registration_evidence": "verified", "artifact_url": "https://example.org/alto",
                   "retrieved_at": "2026-10-18", "version_evidence": "unknown"}
        for field in ("registration_evidence", "artifact_url", "retrieved_at", "version_evidence"):
            for value in (None, 123, [], {}, ""):
                with self.subTest(field=field, value=value), self.assertRaisesRegex(ValueError, field):
                    loc.adapt(sample, {**binding, field: value})

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
            binding = release(root, "https://example.org/page")
            for index, expected in enumerate((0, 1)):
                if index:
                    binding["source_image_sha256"] = "0" * 64
                result, report, outputs = run(root, binding, str(index))
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(report["counts"]["missing"], 1)
                if not index:
                    self.assertEqual(json.loads(outputs)["text"], "inside")
                    self.assertEqual(report["inference_cost"], "unknown")
                    self.assertNotIn("audit_inventory_sha256", report)
                else:
                    self.assertEqual(outputs, "")
                    self.assertEqual(report["counts"]["error"], 1)

    def test_cli_binds_alto_to_the_audited_package_member(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            binding = release(root, PAGE_URL)
            audit = inventory([("sn12345678_1901-01-01_ed-1_seq-2", ALTO)])
            (root / "audit.json").write_bytes(audit)
            pinned = ["--audit-inventory", str(root / "audit.json"),
                      "--audit-inventory-sha256", hashlib.sha256(audit).hexdigest()]
            result, report, outputs = run(root, binding, "aligned", pinned)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(outputs)["text"], "inside")
            self.assertEqual(report["audit_inventory_sha256"], hashlib.sha256(audit).hexdigest())
            self.assertEqual(report["audit_archive_sha256"], "c" * 64)
            self.assertIn("not to the source scan", report["limitations"])
            aligned = next(row for row in report["samples"] if row["status"] == "aligned")
            self.assertEqual(aligned["audit"]["doc_id"], "sn12345678_1901-01-01_ed-1_seq-2")
            self.assertEqual(aligned["audit"]["member"], "sn12345678/1901/01/01/ed-1/seq-2/ocr.xml")
            self.assertEqual(aligned["audit"]["software"], [{"softwareName": "synthetic"}])
            other = ALTO.replace(b"inside", b"insidx")
            cases = {"other-member": ([("sn12345678_1901-01-01_ed-1_seq-2", other)], PAGE_URL,
                                      "differs from the audited"),
                     "other-page": ([("sn12345678_1901-01-01_ed-1_seq-3", ALTO)], PAGE_URL,
                                    "not in the audited"),
                     "not-ndnp": ([("sn12345678_1901-01-01_ed-1_seq-2", ALTO)], "https://example.org/page",
                                  "needs an NDNP")}
            for name, (pages, url, reason) in cases.items():
                with self.subTest(name=name):
                    case = root / name
                    case.mkdir()
                    case_binding = release(case, url)
                    audit = inventory(pages)
                    (case / "audit.json").write_bytes(audit)
                    result, report, outputs = run(case, case_binding, "out", [
                        "--audit-inventory", str(case / "audit.json"),
                        "--audit-inventory-sha256", hashlib.sha256(audit).hexdigest()])
                    self.assertEqual(result.returncode, 1, result.stderr)
                    self.assertEqual(outputs, "")
                    errors = [row for row in report["samples"] if row["status"] == "error"]
                    self.assertEqual(len(errors), 1)
                    self.assertIn(reason, errors[0]["reason"])

    def test_audit_inventory_must_match_its_pin_and_format(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "audit.json"
            good = json.loads(inventory([("sn12345678_1901-01-01_ed-1_seq-2", ALTO)]))
            cases = [(good, "0" * 64, "checksum"),
                     ({**good, "format": "other/1"}, None, "is not"),
                     ({**good, "archive_sha256": "C" * 64}, None, "archive"),
                     ({**good, "pages": good["pages"] * 2, "page_count": 2}, None, "duplicate"),
                     ({**good, "page_count": 2}, None, "page count")]
            for audit, digest, reason in cases:
                data = json.dumps(audit).encode()
                path.write_bytes(data)
                with self.subTest(reason=reason), self.assertRaisesRegex(ValueError, reason):
                    loc.load_audit(path, digest or hashlib.sha256(data).hexdigest())
            data = json.dumps(good).encode()
            path.write_bytes(data)
            with self.assertRaisesRegex(ValueError, "page sequence"):
                loc.audited(loc.load_audit(path, hashlib.sha256(data).hexdigest()),
                            {"source_page_url": PAGE_URL.replace("?sp=2", "")})


PAGE_URL = "https://www.loc.gov/resource/sn12345678/1901-01-01/ed-1/?sp=2"


def inventory(pages):
    """A synthetic `loc_archive.py --out` inventory of (doc_id, ALTO bytes) pages."""
    rows = []
    for doc_id, xml in pages:
        lccn, issued, edition, seq = doc_id.split("_")
        member = "/".join([lccn, *issued.split("-"), edition, seq])
        rows.append({"doc_id": doc_id, "software": [{"softwareName": "synthetic"}],
                     "source_image_identity": "unverified",
                     "artifacts": {kind: {"path": f"{member}/ocr.{kind}",
                                          "sha256": hashlib.sha256(data).hexdigest(), "bytes": len(data)}
                                   for kind, data in (("xml", xml), ("txt", b"inside"))}})
    return json.dumps({"format": "usnm-loc-ocr-artifacts/1", "archive_sha256": "c" * 64,
                       "pages": rows, "page_count": len(rows)}).encode()


def release(root, page_url):
    """Write a one-aligned, one-missing synthetic release under root and return the binding."""
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
              "provenance": {"page_url": page_url, "page_image_sha256": image_hash, "box": [10, 0, 95, 30]}}
    missing = {**sample, "id": "two"}
    (root / "manifest.json").write_text(json.dumps(
        {"samples": [sample, missing], "subsets": {"test": {"members": ["one", "two"]}}}))
    return {"id": "one", "source_page_url": page_url,
            "source_image": str(root / "source.jpg"), "source_image_sha256": image_hash,
            "alto": str(root / "alto.xml"), "alto_sha256": hashlib.sha256(ALTO).hexdigest(),
            "scale": .25, "artifact_url": "https://example.org/alto",
            "retrieved_at": "2026-10-18", "version_evidence": "embedded test version",
            "registration_evidence": "Synthetic same-scan, orientation and zero-offset fixture"}


def run(root, binding, out, extra=()):
    (root / "bindings.jsonl").write_text(json.dumps(binding) + "\n")
    result = subprocess.run([sys.executable, str(Path(loc.__file__)), "--release", str(root),
                             "--subset", "test", "--baseline", "loc-refreshed-v1",
                             "--bindings", str(root / "bindings.jsonl"), "--out-dir", str(root / out),
                             *extra], capture_output=True, text=True)
    report = json.loads((root / out / "alignment.json").read_text())
    return result, report, (root / out / "outputs.jsonl").read_text()

if __name__ == "__main__":
    unittest.main()
