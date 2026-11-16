"""LoC's text of one page for the native spot check's low-F1 scoring (native_spot_loc.py)."""
import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("native_spot_loc", Path(__file__).with_name("native_spot_loc.py"))
loc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(loc)

PAGE = "sn83030272/1901-12-15/ed-1/seq-32"
DOC = "sn83030272_1901-12-15_ed-1_seq-32"
ALTO = (b'<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Layout><Page><PrintSpace><TextBlock>'
        b'<TextLine><String CONTENT="The"/><SP/><String CONTENT="Evening"/></TextLine>'
        b'<TextLine><String CONTENT="Star"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>')


class LocTextTests(unittest.TestCase):
    def archive(self, directory, members, name="x_ver01.tar.bz2"):
        path = Path(directory) / name
        with tarfile.open(path, "w:bz2") as tar:
            for member, data in members.items():
                info = tarfile.TarInfo(member)
                info.size = len(data)
                tar.addfile(info, io.BytesIO(data))
        return path

    def test_the_alto_words_come_a_line_to_a_line(self):
        self.assertEqual(loc.alto_text(ALTO), "The Evening\nStar")

    def test_only_a_well_formed_page_key_names_a_document(self):
        self.assertEqual(loc.doc_id_of(PAGE), DOC)
        for bad in ("", "x'; drop table", "sn1/1901-12-15/ed-1", "../sn1/1901-12-15/ed-1/seq-1"):
            self.assertIsNone(loc.doc_id_of(bad))
        self.assertEqual(loc.member_suffixes(PAGE),
                         ("sn83030272/1901-12-15/ed-1/seq-32/ocr.xml", "sn83030272/1901/12/15/ed-1/seq-32/ocr.xml"))

    def test_the_page_is_read_from_its_archive_in_either_layout(self):
        for member in (f"batch_x/data/{PAGE}/ocr.xml", "batch_x/data/sn83030272/1901/12/15/ed-1/seq-32/ocr.xml"):
            with self.subTest(member), tempfile.TemporaryDirectory() as tmp:
                self.archive(tmp, {"batch_x/data/sn83030272/1901-12-15/ed-1/seq-3/ocr.xml": b"<alto/>", member: ALTO})
                asked = []
                text = loc.page_text(PAGE, archives=tmp, cache=Path(tmp) / "none",
                                     lookup=lambda doc: asked.append(doc) or "x_ver01.tar.bz2")
                self.assertEqual((text, asked), ("The Evening\nStar", [DOC]))

    def test_the_alto_cache_is_read_before_the_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp) / "cache" / "x_ver01.tar.bz2"
            cache.mkdir(parents=True)
            (cache / f"{DOC}.xml").write_bytes(ALTO)

            def never(*args):
                raise AssertionError("the archive was read")

            text = loc.page_text(PAGE, archives=tmp, cache=Path(tmp) / "cache", scan=never,
                                 lookup=lambda doc: "x_ver01.tar.bz2")
            self.assertEqual(text, "The Evening\nStar")

    def test_a_page_no_archive_has_gives_no_text(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.archive(tmp, {"b/sn1/1901-12-15/ed-1/seq-1/ocr.xml": ALTO})
            self.assertIsNone(loc.page_text(PAGE, archives=tmp, cache=tmp, lookup=lambda doc: "x_ver01.tar.bz2"))
            self.assertIsNone(loc.page_text(PAGE, archives=tmp, cache=tmp, lookup=lambda doc: None))
            self.assertIsNone(loc.page_text("not a page", archives=tmp, cache=tmp, lookup=lambda doc: "x"))

    def test_a_scan_past_its_time_raises_so_the_caller_has_no_text(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = self.archive(tmp, {"a/sn9/1901-12-15/ed-1/seq-1/ocr.xml": ALTO, f"a/{PAGE}/ocr.xml": ALTO})
            with self.assertRaises(TimeoutError):
                loc.scan_archive(path, loc.member_suffixes(PAGE), deadline=0)

    def test_an_archive_name_from_the_database_must_be_a_plain_bulk_archive(self):
        self.assertTrue(loc.ARCHIVE.fullmatch("sdhi_mango_ver02.tar.bz2"))
        for bad in ("../x.tar.bz2", "x.tar.gz", "a/b.tar.bz2", "x.tar.bz2;rm"):
            self.assertFalse(loc.ARCHIVE.fullmatch(bad), bad)


if __name__ == "__main__":
    unittest.main()
