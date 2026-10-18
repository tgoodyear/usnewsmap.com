import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from churro_trial import collect_samples, score_text


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
            ("Tom & Jerry\nSecond line\nand friends\n8", "xml-page-text-metadata-excluded-lines-v3"),
        )

    def test_preserves_whitespace_between_inline_xml_elements(self) -> None:
        raw = "<Document><Page><Line><Word>Tom</Word> <Word>Jerry</Word></Line></Page></Document>"

        self.assertEqual(
            score_text(raw),
            ("Tom Jerry", "xml-page-text-metadata-excluded-lines-v3"),
        )

    def test_normalizes_multiline_whitespace_between_inline_xml_elements(self) -> None:
        raw = "<Document><Page><Line><Word>Tom</Word>\n  <Word>Jerry</Word></Line></Page></Document>"

        self.assertEqual(
            score_text(raw),
            ("Tom Jerry", "xml-page-text-metadata-excluded-lines-v3"),
        )


    def test_preserves_boundary_whitespace_in_text_bearing_nodes(self) -> None:
        raw = "<Document><Page><Line>  A  B\u00a0C </Line></Page></Document>"

        self.assertEqual(
            score_text(raw),
            ("  A  B\u00a0C ", "xml-page-text-metadata-excluded-lines-v3"),
        )

    def test_preserves_repeated_spaces_and_nonbreaking_spaces_in_text_nodes(self) -> None:
        raw = "<Document><Page><Line>A  B\u00a0C</Line></Page></Document>"

        self.assertEqual(
            score_text(raw),
            ("A  B\u00a0C", "xml-page-text-metadata-excluded-lines-v3"),
        )

    def test_trims_line_boundary_indentation_but_keeps_inline_spaces(self) -> None:
        raw = (
            "<Document><Page>\n"
            "  <Line>\n  <Word>Tom</Word>   <Word>Jerry</Word>\n</Line>\n"
            "  <Line>\n    <Word>and friends</Word>  \n</Line>\n"
            "</Page></Document>"
        )

        self.assertEqual(
            score_text(raw),
            ("Tom Jerry\nand friends", "xml-page-text-metadata-excluded-lines-v3"),
        )

    def test_preserves_completed_outputs_when_later_sample_fails(self) -> None:
        samples = [{"id": "first"}, {"id": "broken"}, {"id": "not-run"}]

        def infer_one(_index: int, sample: dict[str, str]) -> tuple[str, int | None]:
            if sample["id"] == "broken":
                raise RuntimeError("failure details are intentionally not included in receipts")
            return f"<Line>{sample['id']}</Line>", 1

        raw_rows, scored_rows, _timings, _statuses, failures = collect_samples(samples, infer_one)

        self.assertEqual(raw_rows, [{"id": "first", "text": "<Line>first</Line>"}])
        self.assertEqual(scored_rows, [{"id": "first", "text": "first"}])
        self.assertEqual([(row["id"], row["error_type"]) for row in failures], [("broken", "RuntimeError")])
        self.assertIsInstance(failures[0]["elapsed_seconds"], float)
        self.assertEqual(len(scored_rows), 1)

    def test_invalid_xml_uses_trimmed_raw_output(self) -> None:
        self.assertEqual(score_text("  not XML  "), ("not XML", "unparsed-raw-fallback"))


if __name__ == "__main__":
    unittest.main()
