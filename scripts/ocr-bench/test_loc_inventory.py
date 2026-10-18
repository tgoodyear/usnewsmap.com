import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("loc_inventory", Path(__file__).with_name("loc_inventory.py"))
loc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loc)


def guide(content="Title (sn12345678) 1900-1902", total=20):
    return f"""<p>Total Reprocessed Pages to Date: {total}</p><table><tr>
    <td>2026-09-30</td><td>Contributor</td><td>test_one_ver02</td>
    <td>20</td><td>{content}</td></tr></table>""".encode()


class InventoryTests(unittest.TestCase):
    def test_count_and_pin(self):
        data = guide()
        result = loc.coverage(data)
        self.assertEqual((result["batch_count"], result["pages_reported"]), (1, 20))
        self.assertEqual(result["source_sha256"], loc.sha(data))
        self.assertEqual(result["latest_added"], "2026-09-30")

    def test_html_boundaries_and_title_specific_years(self):
        result = loc.coverage(guide('<a>Title (sn12345678)</a> 1900-1902'
                                    '<a>Other (sn87654321)</a> 1921'))
        self.assertEqual(loc.candidates(result, "sn12345678", 1901), ["test_one_ver02"])
        self.assertEqual(loc.candidates(result, "sn12345678", 1921), [])
        self.assertEqual(loc.candidates(result, "sn87654321", 1921), ["test_one_ver02"])

    def test_duplicate_or_total_conflict_is_error(self):
        with self.assertRaisesRegex(ValueError, "total"):
            loc.coverage(guide(total=21))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            loc.coverage(guide() + guide())

    def test_missing_table_is_error(self):
        with self.assertRaisesRegex(ValueError, "no reprocessed"):
            loc.coverage(b"<html>maintenance</html>")

    def test_unknown_year_is_candidate_not_absent(self):
        self.assertEqual(loc.candidates(loc.coverage(guide("Title (sn12345678)")),
                                        "sn12345678", 1921), ["test_one_ver02"])

    def test_batch_version_change_is_only_candidate(self):
        inventory = loc.coverage(guide())
        pages = [{"doc_id": "page-1", "lccn": "sn00000000", "date": "1920-01-01",
                  "batch": "test_one_ver01", "lang": "eng", "keep": True}]
        result = loc.repair_overlap(inventory, pages)
        self.assertEqual(result["statuses"], {"candidate-needs-page-verification": 1})
        with self.assertRaisesRegex(ValueError, "duplicate repair"):
            loc.repair_overlap(inventory, pages * 2)

    def test_benchmark_denominators_preserve_missing(self):
        samples = []
        for identity, url in [("yes", "https://www.loc.gov/resource/sn12345678/1901-01-01/ed-1/?sp=2"),
                              ("no", "https://www.loc.gov/resource/sn12345678/1921-01-01/ed-1/?sp=2"),
                              ("de", "https://example.org/german")]:
            samples.append({"id": identity, "source_page_id": identity, "source": "test",
                            "provenance": {"page_url": url}, "languages": ["eng"],
                            "image": {"sha256": "a" * 64}, "reference": {"sha256": "b" * 64}})
        result = loc.benchmark_overlap(loc.coverage(guide()),
                                      {"samples": samples, "subsets": {"all": {"members": ["yes", "no", "de"]}}})
        self.assertEqual(result["subsets"]["all"]["statuses"],
                         {"candidate-needs-page-verification": 1, "not-listed-in-guide": 1, "outside-ndnp": 1})
        self.assertEqual(result["rows"][0]["doc_id"], "sn12345678_1901-01-01_ed-1_seq-2")
        samples[0]["provenance"]["page_url"] = "https://www.loc.gov/resource/sn12345678/1901-01-01/ed-1/"
        with self.assertRaisesRegex(ValueError, "sequence"):
            loc.benchmark_overlap(loc.coverage(guide()), {"samples": samples})


if __name__ == "__main__":
    unittest.main()
