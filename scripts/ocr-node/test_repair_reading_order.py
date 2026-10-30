"""Offline reading-order regression tests; no OCR engine or source access."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET


HERE = Path(__file__).parent
SPEC = importlib.util.spec_from_file_location("repair", HERE / "repair-reading-order.py")
repair = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(repair)
IMAGE = b"frozen regression scan"
IMAGE_SHA = repair.sha256(IMAGE)


def fixture():
    # One engine line runs across two columns. The headline legitimately spans both.
    xml = f"""<alto xmlns="{repair.NAMESPACE}">
      <Description><MeasurementUnit>pixel</MeasurementUnit>
        <sourceImageInformation><fileName>retained.jp2</fileName>
          <fileIdentifier fileIdentifierLocation="sha256">{IMAGE_SHA}</fileIdentifier>
        </sourceImageInformation>
        <OCRProcessing ID="original"><ocrProcessingStep>
          <processingSoftware><softwareName>original engine</softwareName></processingSoftware>
        </ocrProcessingStep></OCRProcessing>
      </Description>
      <Layout><Page ID="page" WIDTH="200" HEIGHT="300">
        <PrintSpace HPOS="0" VPOS="0" WIDTH="200" HEIGHT="300">
          <TextBlock ID="headline" HPOS="10" VPOS="10" WIDTH="180" HEIGHT="10">
            <TextLine ID="head" HPOS="10" VPOS="10" WIDTH="180" HEIGHT="10">
              <String ID="title" CONTENT="Spanning headline" HPOS="10" VPOS="10" WIDTH="180" HEIGHT="10"/>
            </TextLine>
          </TextBlock>
          <ComposedBlock ID="composed" HPOS="10" VPOS="50" WIDTH="160" HEIGHT="80">
            <TextBlock ID="body" LANG="deu" HPOS="10" VPOS="50" WIDTH="160" HEIGHT="80">
              <TextLine ID="mixed" HPOS="10" VPOS="50" WIDTH="160" HEIGHT="10">
                <String ID="left1" CONTENT="left" HPOS="10" VPOS="50" WIDTH="30" HEIGHT="10" WC="0.92"/>
                <SP WIDTH="80"/>
                <String ID="right1" CONTENT="right" HPOS="120" VPOS="50" WIDTH="30" HEIGHT="10" WC="0.73"/>
                <HYP CONTENT="-" HPOS="151" VPOS="50" WIDTH="5" HEIGHT="10"/>
              </TextLine>
              <TextLine ID="left-second" HPOS="10" VPOS="70" WIDTH="30" HEIGHT="10">
                <String ID="left2" CONTENT="continues" HPOS="10" VPOS="70" WIDTH="30" HEIGHT="10"/>
              </TextLine>
              <TextLine ID="right-second" HPOS="120" VPOS="70" WIDTH="30" HEIGHT="10">
                <String ID="right2" CONTENT="hand" HPOS="120" VPOS="70" WIDTH="30" HEIGHT="10"/>
              </TextLine>
            </TextBlock>
            <Illustration ID="photo" HPOS="20" VPOS="100" WIDTH="160" HEIGHT="30"/>
          </ComposedBlock>
        </PrintSpace>
      </Page></Layout>
    </alto>""".encode()
    layout = {
        "method": repair.VERSION, "direction": "ltr",
        "alto_sha256": repair.sha256(xml), "image_sha256": IMAGE_SHA, "page_size": [200, 300],
        "order_checks": [{"name": "left-column-continuation", "word_ids": ["left1", "left2"]}],
        "regions": [
            {"name": "headline", "box": [0, 0, 200, 40]},
            {"name": "left", "box": [0, 40, 100, 300]},
            {"name": "right", "box": [100, 40, 200, 300]},
        ],
    }
    return xml, layout


class ReadingOrder(unittest.TestCase):
    def test_fragment_row_order_uses_visual_band_not_small_top_edge_differences(self):
        data, layout = fixture()
        data = data.replace(b'ID="right2" CONTENT="hand" HPOS="120" VPOS="70"',
                            b'ID="right2" CONTENT="hand" HPOS="120" VPOS="69"')
        layout["alto_sha256"] = repair.sha256(data)
        layout["regions"] = [{"name": "whole-page", "box": [0, 0, 200, 300]}]
        layout["order_checks"] = [{"name": "visual-row", "word_ids": ["left2", "right2"]}]
        output, _, _ = repair.reflow(data, layout, IMAGE_SHA)
        words = ET.fromstring(output).findall(".//a:String", repair.NS)
        self.assertEqual([w.get("ID") for w in words], ["title", "left1", "right1", "left2", "right2"])

    def test_splits_merged_lines_but_keeps_spanning_heading_and_every_word_record(self):
        data, layout = fixture()
        output, text, counts = repair.reflow(data, layout, IMAGE_SHA)
        original, fixed = ET.fromstring(data), ET.fromstring(output)
        self.assertEqual(text, "Spanning headline\n\nleft\ncontinues\n\nright-\nhand\n")
        self.assertEqual(repair.word_signature(original), repair.word_signature(fixed))
        self.assertEqual(counts["original_lines_split"], 1)
        self.assertEqual(counts["words_preserved"], 5)
        self.assertFalse(counts["order_checks"][0]["original_consecutive"])
        self.assertTrue(counts["order_checks"][0]["derived_consecutive"])
        hyphen_line = next(line for line in fixed.findall(".//a:TextLine", repair.NS)
                           if line.find("a:HYP", repair.NS) is not None)
        self.assertEqual(hyphen_line.find("a:String", repair.NS).get("ID"), "right1")
        self.assertEqual(repair.rectangle(hyphen_line)[2], 156)
        hyphen_block = next(block for block in fixed.findall(".//a:TextBlock", repair.NS)
                            if hyphen_line in list(block))
        self.assertEqual(repair.rectangle(hyphen_block)[2], 156)
        self.assertEqual(fixed.find(".//a:Illustration", repair.NS).attrib,
                         original.find(".//a:Illustration", repair.NS).attrib)
        self.assertEqual(fixed.findtext(".//a:ocrProcessingStep//a:softwareName",
                                       namespaces=repair.NS), "original engine")
        settings = fixed.findtext(".//a:postProcessingStep/a:processingStepSettings",
                                  namespaces=repair.NS)
        self.assertIn("parent_alto_sha256=" + repair.sha256(data), settings)
        self.assertIn("publication_acceptance=not-claimed", settings)
        ids = [e.get("ID") for e in fixed.iter() if e.get("ID")]
        self.assertEqual(len(ids), len(set(ids)))

    def test_retains_unpositioned_hyphen_without_inventing_its_geometry(self):
        data, layout = fixture()
        data = data.replace(b'<HYP CONTENT="-" HPOS="151" VPOS="50" WIDTH="5" HEIGHT="10"/>',
                            b'<HYP CONTENT="-"/>')
        layout["alto_sha256"] = repair.sha256(data)
        output, _, _ = repair.reflow(data, layout, IMAGE_SHA)
        root = ET.fromstring(output)
        line = next(e for e in root.findall(".//a:TextLine", repair.NS)
                    if e.find("a:HYP", repair.NS) is not None)
        self.assertEqual(line.find("a:HYP", repair.NS).attrib, {"CONTENT": "-"})
        self.assertEqual(repair.rectangle(line)[2], 150)

    def test_refuses_missing_or_unrecovered_expected_order(self):
        data, layout = fixture()
        for checks in [[], [{"name": "unknown", "word_ids": ["missing", "left1"]}],
                       [{"name": "wrong", "word_ids": ["right1", "left2"]}]]:
            with self.subTest(checks=checks):
                bad = copy.deepcopy(layout)
                bad["order_checks"] = checks
                with self.assertRaises(ValueError):
                    repair.reflow(data, bad, IMAGE_SHA)

    def test_refuses_rotation_and_inherited_styles_instead_of_silently_flattening_them(self):
        data, layout = fixture()
        for marker, attribute in [
            (b'ID="composed"', b' ROTATION="90"'),
            (b'ID="body"', b' ROTATION="90"'),
            (b'ID="composed"', b' STYLEREFS="composed-font"'),
            (b'ID="mixed"', b' LANG="fra"'),
            (b'ID="mixed"', b' STYLEREFS="line-font"'),
        ]:
            with self.subTest(marker=marker, attribute=attribute):
                changed = data.replace(marker, marker + attribute)
                bad = copy.deepcopy(layout)
                bad["alto_sha256"] = repair.sha256(changed)
                with self.assertRaises(ValueError):
                    repair.reflow(changed, bad, IMAGE_SHA)

    def test_rejects_mismatched_alto_image_and_page_size(self):
        data, layout = fixture()
        for field, value in [("alto_sha256", "0" * 64), ("image_sha256", "0" * 64),
                             ("page_size", [200, 301])]:
            with self.subTest(field=field):
                bad = copy.deepcopy(layout)
                bad[field] = value
                with self.assertRaises(ValueError):
                    repair.reflow(data, bad, IMAGE_SHA)

    def test_rejects_overlapping_uncovered_and_invalid_regions(self):
        data, layout = fixture()
        for name, box in [("overlap", [90, 40, 200, 300]), ("gap", [140, 40, 200, 300]),
                          ("outside", [100, 40, 201, 300]), ("empty", [100, 40, 100, 300]),
                          ("nan", [float("nan"), 40, 200, 300])]:
            with self.subTest(name=name):
                bad = copy.deepcopy(layout)
                bad["regions"][-1]["box"] = box
                with self.assertRaises(ValueError):
                    repair.reflow(data, bad, IMAGE_SHA)

    def test_rejects_wrong_image_provenance_and_stale_cross_line_substitutions(self):
        data, layout = fixture()
        for changed in [
            data.replace(IMAGE_SHA.encode(), b"0" * 64),
            data.replace(b'ID="left1"', b'ID="left1" SUBS_TYPE="HypPart1" SUBS_CONTENT="lefthand"'),
        ]:
            bad = copy.deepcopy(layout)
            bad["alto_sha256"] = repair.sha256(changed)
            with self.assertRaises(ValueError):
                repair.reflow(changed, bad, IMAGE_SHA)

    def test_rejects_entity_and_non_utf8_xml(self):
        data, layout = fixture()
        for changed in [
            b'<!DOCTYPE alto [<!ENTITY x "expanded">]>' + data,
            data.decode().encode("utf-16"),
        ]:
            bad = copy.deepcopy(layout)
            bad["alto_sha256"] = repair.sha256(changed)
            with self.assertRaises(ValueError):
                repair.reflow(changed, bad, IMAGE_SHA)

    def test_cli_writes_hash_receipt_without_overwriting_retained_inputs_or_outputs(self):
        data, layout = fixture()
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            alto, image, annotation, out = [directory / name
                                            for name in ("input.xml", "scan.jp2", "layout.json", "fixed")]
            alto.write_bytes(data)
            image.write_bytes(IMAGE)
            annotation.write_text(json.dumps(layout))
            args = ["--alto", str(alto), "--image", str(image), "--layout", str(annotation),
                    "--out", str(out)]
            repair.main(args)
            receipt = json.loads((out / "receipt.json").read_text())
            self.assertEqual(receipt["output_alto_sha256"], repair.sha256((out / "page.alto.xml").read_bytes()))
            self.assertEqual(receipt["output_text_sha256"], repair.sha256((out / "page.txt").read_bytes()))
            self.assertFalse(receipt["publication_acceptance"])
            with self.assertRaises(FileExistsError):
                repair.main(args)
            self.assertEqual(alto.read_bytes(), data)
            self.assertEqual(image.read_bytes(), IMAGE)


if __name__ == "__main__":
    unittest.main()
