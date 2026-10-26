import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "check_doc_links", Path(__file__).with_name("check-doc-links.py")
)
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)


class DocLinksTest(unittest.TestCase):
    def test_named_paths_option_checks_the_requested_document(self):
        with tempfile.TemporaryDirectory() as tmp:
            doc = Path(tmp) / "note.md"
            doc.write_text("[gone](missing.json)\n")
            result = subprocess.run(
                [sys.executable, str(Path(__file__).with_name("check-doc-links.py")),
                 "--paths", str(doc)],
                capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("1 Markdown files; 1 broken local links", result.stdout)
            self.assertIn(f"{doc}:1: missing file: missing.json", result.stderr)

    def test_missing_file_and_anchor_are_reported_but_examples_are_not(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "target.md").write_text("# A heading\n## A heading\n")
            doc = root / "note.md"
            doc.write_text(
                "[good](target.md#a-heading-1)\n"
                "[bad](target.md#missing)\n"
                "[gone](missing.json)\n"
                "```md\n[example](example.md)\n```\n"
                "`[inline example](example.md)`\n"
                "[remote](https://example.org/missing)\n"
                "[ref]: target.md#a-heading\n"
            )
            self.assertEqual(
                CHECK.check([doc]),
                [
                    (doc, 2, "missing anchor: target.md#missing"),
                    (doc, 3, "missing file: missing.json"),
                ],
            )

    def test_encoded_paths_linked_code_labels_and_explicit_anchors(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "a file.md").write_text('<a id="custom"></a>\n# Text &amp; `Code`\n')
            doc = root / "note.md"
            doc.write_text(
                "[`label`](a%20file.md#text--code)\n"
                "[label](<a file.md#custom>)\n"
            )
            self.assertEqual(CHECK.check([doc]), [])


if __name__ == "__main__":
    unittest.main()
