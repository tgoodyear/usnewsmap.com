import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from churro_trial import score_text


class ChurroScoreTextTests(unittest.TestCase):
    def test_omits_metadata_and_preserves_line_structure_without_xml_indentation(self) -> None:
        raw = (
            '<HistoricalDocument xmlns="urn:test">\n'
            '  <Metadata><Language>English</Language><Description>not OCR</Description></Metadata>\n'
            '  <Page><Body><Paragraph><Line>Tom &amp; Jerry</Line><Line>Second line</Line>'
            '<Line><Word>and</Word> <Word>friends</Word></Line></Paragraph>'
            '<Footer><PageNumber>8</PageNumber></Footer></Body></Page>\n'
            '</HistoricalDocument>'
        )

        self.assertEqual(
            score_text(raw),
            ("Tom & Jerry\nSecond line\nand friends\n8", "xml-page-text-metadata-excluded-lines-v2"),
        )

    def test_preserves_whitespace_between_inline_xml_elements(self) -> None:
        raw = "<Document><Page><Line><Word>Tom</Word> <Word>Jerry</Word></Line></Page></Document>"

        self.assertEqual(
            score_text(raw),
            ("Tom Jerry", "xml-page-text-metadata-excluded-lines-v2"),
        )

    def test_invalid_xml_uses_trimmed_raw_output(self) -> None:
        self.assertEqual(score_text("  not XML  "), ("not XML", "unparsed-raw-fallback"))


if __name__ == "__main__":
    unittest.main()
