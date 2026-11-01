"""The parity report's arithmetic: no worker or scan is needed."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("parity", Path(__file__).with_name("native_ocr_parity.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class Distance(unittest.TestCase):
    def test_known_distances(self):
        for a, b, want in [("kitten", "sitting", 3), ("", "abc", 3), ("abc", "", 3),
                           ("abc", "abc", 0), ("flaw", "lawn", 2), ("Straße", "Strasse", 2)]:
            self.assertEqual(module.distance(a, b), want, (a, b))


def row(native=0, container=0, identical=True, cer=0.0):
    return {"native": {"status": native, "wall": 100.0, "cpu": 90.0},
            "container": {"status": container, "wall": 200.0, "cpu": 95.0},
            "text_identical": identical, "alto_identical": identical, "cer_between_builds": cer}


class Names(unittest.TestCase):
    def test_scans_differing_by_format_get_their_own_slots(self):
        self.assertNotEqual(module.slot_name("page.jpg"), module.slot_name("page.jp2"))


class Summary(unittest.TestCase):
    def test_a_failed_read_is_counted_but_not_compared(self):
        summary = module.group_summary([row(), row(identical=False, cer=0.5), row(native=1)])
        self.assertEqual((summary["pages"], summary["both_finished"]), (3, 2))
        self.assertEqual(summary["text_identical"], 1)
        self.assertEqual(summary["cer_between_builds_max"], 0.5)
        self.assertEqual(summary["pages_per_hour_native"], 36.0)
        self.assertEqual(summary["pages_per_hour_container"], 18.0)

    def test_no_finished_pages_gives_no_rates(self):
        self.assertIsNone(module.group_summary([row(native=1)])["pages_per_hour_native"])


if __name__ == "__main__":
    unittest.main()
