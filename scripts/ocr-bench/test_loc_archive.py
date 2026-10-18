import hashlib
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("loc_archive", Path(__file__).with_name("loc_archive.py"))
loc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loc)

ALTO = b"""<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
<Description><MeasurementUnit>pixel</MeasurementUnit><OCRProcessing>
<processingSoftware><softwareName>ndnp-open-ocr</softwareName>
<softwareVersion>1.2.0</softwareVersion></processingSoftware></OCRProcessing></Description>
<Layout><Page WIDTH="100" HEIGHT="200"/></Layout></alto>"""
PREFIX = "sn12345678/1900/01/01/ed-1/seq-2/ocr."


class ArchiveTests(unittest.TestCase):
    def archive(self, root, members):
        path = Path(root) / "ocr.tar.bz2"
        with tarfile.open(path, "w:bz2") as archive:
            for name, data in members:
                member = tarfile.TarInfo(name)
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
        return path, hashlib.sha256(path.read_bytes()).hexdigest()

    def test_canonical_page_and_embedded_version(self):
        with tempfile.TemporaryDirectory() as root:
            path, digest = self.archive(root, [(PREFIX + "xml", ALTO), (PREFIX + "txt", b"Text")])
            result = loc.inspect(path, digest)
            self.assertEqual(result["page_count"], 1)
            page = result["pages"][0]
            self.assertEqual(page["doc_id"], "sn12345678_1900-01-01_ed-1_seq-2")
            self.assertEqual(page["software"][0]["softwareVersion"], "1.2.0")
            self.assertEqual(page["source_image_identity"], "unverified")
            self.assertEqual(page["artifacts"]["xml"]["sha256"], hashlib.sha256(ALTO).hexdigest())
            with self.assertRaisesRegex(ValueError, "checksum"):
                loc.inspect(path, "a" * 64)

    def test_missing_pair_and_unexpected_member_rejected(self):
        for members, message in [([(PREFIX + "xml", ALTO)], "missing"),
                                 ([("../escape", b"x")], "unexpected"),
                                 ([(PREFIX + "xml", ALTO)] * 2, "duplicate")]:
            with self.subTest(message=message), tempfile.TemporaryDirectory() as root:
                path, digest = self.archive(root, members)
                with self.assertRaisesRegex(ValueError, message):
                    loc.inspect(path, digest)


if __name__ == "__main__":
    unittest.main()
