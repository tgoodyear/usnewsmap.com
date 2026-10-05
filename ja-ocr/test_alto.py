"""Tests for alto.py and the job's ALTO output (#151). Run: python3 -m unittest -q"""

import json
import tempfile
import unittest
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path

import alto
import jaocr

try:
    import pyarrow  # noqa: F401
except ImportError:
    pyarrow = None

A = "{http://www.loc.gov/standards/alto/ns-v3#}"

# NDLOCR-Lite's XML shape: TEXTBLOCKs with LINEs out of reading order, a LINE
# directly under PAGE, a BLOCK with no lines (a running head) and an empty line.
NDL = """<OCRDATASET>
<PAGE IMAGENAME="p.jpg" WIDTH="2400" HEIGHT="4000">
  <TEXTBLOCK CONF="0.5">
    <SHAPE><POLYGON POINTS="0,0,1,1"/></SHAPE>
    <LINE TYPE="本文" X="2000" Y="300" WIDTH="50" HEIGHT="400" CONF="0.8" ORDER="2" STRING="二行目" />
    <LINE TYPE="本文" X="2100" Y="300" WIDTH="50" HEIGHT="400" CONF="0.9" ORDER="1" STRING="一行目 &amp; &quot;見出&quot;" />
  </TEXTBLOCK>
  <LINE TYPE="本文" X="100" Y="3000" WIDTH="400" HEIGHT="60" CONF="1.2" ORDER="7" STRING="Camp News" />
  <TEXTBLOCK CONF="0.5">
    <LINE TYPE="本文" X="1500" Y="300" WIDTH="50" HEIGHT="300" CONF="0.7" ORDER="4" STRING="次の記事" />
    <LINE TYPE="本文" X="1400" Y="300" WIDTH="50" HEIGHT="300" CONF="0.7" ORDER="5" STRING="" />
  </TEXTBLOCK>
  <BLOCK TYPE="柱" X="10" Y="10" WIDTH="300" HEIGHT="40" CONF="0.6" />
</PAGE>
</OCRDATASET>""".encode()

LOC = b"""<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://schema.ccs-gmbh.com/ALTO"><Description><MeasurementUnit>inch1200</MeasurementUnit>
<OCRProcessing ID="OCR.0"><ocrProcessingStep><processingStepSettings>Lang:ja
Engine:Abbyy8
width:2477
height:4025
xdpi:300
ydpi:300
</processingStepSettings></ocrProcessingStep></OCRProcessing></Description><Layout>
<Page ID="PAGE.0" HEIGHT="16100" WIDTH="9908" PHYSICAL_IMG_NR="394"><PrintSpace/></Page></Layout></alto>"""

AT = datetime(2026, 10, 5, 12, 0, tzinfo=timezone.utc)


def convert(geometry=alto.Geometry()):
    xml = alto.ndlocr_to_alto(NDL, geometry, image_url="https://tile.loc.gov/a&b", engine="ndlocr-lite x",
                              seq=3, ocred_at=AT)
    return ET.fromstring(xml)


class Alto(unittest.TestCase):
    def test_loc_geometry(self):
        self.assertEqual(alto.loc_geometry(LOC), alto.Geometry(dpi=300, width=9908, height=16100))
        self.assertEqual(alto.loc_geometry(None), alto.Geometry())
        self.assertEqual(alto.loc_geometry(b"not xml"), alto.Geometry())
        # A page in another unit gives its dpi but not its size.
        other = LOC.replace(b"inch1200", b"pixel").replace(b"xdpi:300", b"xdpi:400")
        self.assertEqual(alto.loc_geometry(other), alto.Geometry(dpi=400))

    def test_blocks_and_lines_in_reading_order(self):
        root = convert()
        blocks = root.findall(f".//{A}TextBlock")
        text = [[s.get("CONTENT") for s in b.iter(f"{A}String")] for b in blocks]
        self.assertEqual(text, [['一行目 & "見出"', "二行目"], ["次の記事"], ["Camp News"]])
        self.assertEqual({b.get("LANG") for b in blocks}, {"jpn"})
        ids = [e.get("ID") for e in root.iter() if e.get("ID")]
        self.assertEqual(len(ids), len(set(ids)))

    def test_fractional_reading_order(self):
        ndl = (b'<OCRDATASET><PAGE WIDTH="100" HEIGHT="100"><TEXTBLOCK>'
               b'<LINE X="1" Y="1" WIDTH="5" HEIGHT="9" ORDER="0.8" STRING="c"/>'
               b'<LINE X="9" Y="1" WIDTH="5" HEIGHT="9" ORDER="0.2" STRING="b"/>'
               b'</TEXTBLOCK><LINE X="20" Y="1" WIDTH="5" HEIGHT="9" ORDER="0.1" STRING="a"/></PAGE></OCRDATASET>')
        root = ET.fromstring(alto.ndlocr_to_alto(ndl, alto.Geometry(), image_url="u", engine="e", seq=1,
                                                 ocred_at=AT))
        self.assertEqual([s.get("CONTENT") for s in root.iter(f"{A}String")], ["a", "b", "c"])

    def test_inch1200_at_the_scans_dpi(self):
        root = convert()
        page = root.find(f".//{A}Page")
        # 300 dpi: four units a pixel; the page from the image when LoC gives none.
        self.assertEqual((page.get("WIDTH"), page.get("HEIGHT")), ("9600", "16000"))
        first = root.find(f".//{A}TextLine")
        self.assertEqual([first.get(k) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")],
                         ["8400", "1200", "200", "1600"])
        block = root.find(f".//{A}TextBlock")
        self.assertEqual([block.get(k) for k in ("HPOS", "VPOS", "WIDTH", "HEIGHT")],
                         ["8000", "1200", "600", "1600"])
        self.assertEqual(root.findtext(f".//{A}MeasurementUnit"), "inch1200")
        # 400 dpi: three units a pixel.
        line = convert(alto.Geometry(dpi=400)).find(f".//{A}TextLine")
        self.assertEqual(line.get("HPOS"), "6300")

    def test_lines_are_clamped_to_locs_page(self):
        root = convert(alto.Geometry(dpi=300, width=8500, height=16100))
        page = root.find(f".//{A}Page")
        self.assertEqual(page.get("WIDTH"), "8500")
        first = root.find(f".//{A}TextLine")
        self.assertEqual((first.get("HPOS"), first.get("WIDTH")), ("8400", "100"))

    def test_provenance_and_confidence(self):
        root = convert()
        self.assertEqual(root.findtext(f".//{A}fileName"), "https://tile.loc.gov/a&b")
        self.assertEqual(root.findtext(f".//{A}softwareName"), "NDLOCR-Lite")
        self.assertEqual(root.findtext(f".//{A}softwareVersion"), "ndlocr-lite x")
        self.assertEqual(root.find(f".//{A}Page").get("PHYSICAL_IMG_NR"), "3")
        wc = [s.get("WC") for s in root.iter(f"{A}String")]
        self.assertEqual(wc, ["0.900", "0.800", "0.700", "1.000"])


@unittest.skipIf(pyarrow is None, "pyarrow not installed")
class OcrIssue(unittest.TestCase):
    def test_writes_alto_then_the_part(self):
        item = {"resources": [{"files": [
            [{"mimetype": "image/jp2", "url": "https://tile.loc.gov/storage-services/service/x/0001.jp2"},
             {"mimetype": "text/xml", "url": "https://tile.loc.gov/storage-services/service/x/0001.xml"}],
        ]}]}

        def fetch(url, pacer, attempts=5):
            if "fo=json" in url:
                return json.dumps(item).encode()
            if url.endswith("0001.xml"):
                return LOC
            return b"jpeg"

        def ndlocr(root, images, out):
            out.mkdir(parents=True, exist_ok=True)
            for img in images.glob("*.jpg"):
                (out / f"{img.stem}.txt").write_text("一行目\n二行目\n")
                (out / f"{img.stem}.xml").write_bytes(NDL)

        page = {"doc_id": "sn1_1945-01-01_ed-1_seq-1", "page_key": "sn1/1945-01-01/ed-1/seq-1", "lccn": "sn1",
                "date": "1945-01-01", "edition": 1, "seq": 1, "batch": "b", "loc_text": "missing"}
        orig = jaocr.fetch, jaocr.ndlocr
        jaocr.fetch, jaocr.ndlocr = fetch, ndlocr
        try:
            with tempfile.TemporaryDirectory() as d:
                cur = jaocr.LocalStore(d)
                n = jaocr.ocr_issue(cur, Path("/x"), "sn1_1945-01-01_ed-1", [page], jaocr.Pacer(0),
                                    jaocr.Pacer(0), "ndlocr-lite x")
                self.assertEqual(n, 1)
                xml = ET.fromstring(cur.read(f"ocr-ja/alto/{page['doc_id']}.xml"))
                # LoC's page size, and the image the text came from.
                self.assertEqual(xml.find(f".//{A}Page").get("WIDTH"), "9908")
                self.assertTrue(xml.findtext(f".//{A}fileName").endswith("/full/full/0/default.jpg"))
                self.assertTrue(cur.exists("ocr-ja/pages/sn1_1945-01-01_ed-1.parquet"))
                self.assertEqual(jaocr.alto_pages(cur), {page["doc_id"]})
        finally:
            jaocr.fetch, jaocr.ndlocr = orig

    def test_no_xml_means_no_part(self):
        item = {"resources": [{"files": [[{"mimetype": "image/jp2",
                                            "url": "https://tile.loc.gov/storage-services/service/x/0001.jp2"}]]}]}
        orig = jaocr.fetch, jaocr.ndlocr

        def ndlocr(root, images, out):
            out.mkdir(parents=True, exist_ok=True)
            for img in images.glob("*.jpg"):
                (out / f"{img.stem}.txt").write_text("一行目\n")

        jaocr.fetch = lambda url, pacer, attempts=5: json.dumps(item).encode() if "fo=json" in url else b"jpeg"
        jaocr.ndlocr = ndlocr
        page = {"doc_id": "d1", "page_key": "k1", "lccn": "sn1", "date": "1945-01-01", "edition": 1, "seq": 1,
                "batch": "b", "loc_text": "missing"}
        try:
            with tempfile.TemporaryDirectory() as d:
                cur = jaocr.LocalStore(d)
                self.assertEqual(jaocr.ocr_issue(cur, Path("/x"), "i", [page], jaocr.Pacer(0), jaocr.Pacer(0), "e"), 0)
                self.assertEqual(cur.list("ocr-ja/"), [])
        finally:
            jaocr.fetch, jaocr.ndlocr = orig


if __name__ == "__main__":
    unittest.main()
