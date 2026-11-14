#!/usr/bin/env python3
"""Checks of boinc_provenance.py's host rules:

  python3 -I scripts/onprem/quality/test_boinc_provenance.py
"""
import importlib.util
import os
import unittest

here = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("boinc_provenance", os.path.join(here, "boinc_provenance.py"))
bp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bp)

AV = {"7": "x86_64-pc-linux-gnu"}


def wu(canon=5, hostid="12", os_name="Linux", vendor="GenuineIntel"):
    return dict(canon=canon, hostid=hostid, avid="7", os=os_name, vendor=vendor)


class ResolveHost(unittest.TestCase):
    def test_a_known_vendor_gives_a_family(self):
        status, found = bp.resolve_host(wu(), AV)
        self.assertEqual((status, found["hostid"], found["family"]), ("resolved", 12, "amd64"))
        status, found = bp.resolve_host(wu(os_name="Darwin", vendor="Apple"), AV)
        self.assertEqual((status, found["family"]), ("resolved", "arm64"))

    def test_an_empty_or_unknown_vendor_keeps_the_host_with_no_family(self):
        for vendor in ("", "Unknown Vendor"):
            status, found = bp.resolve_host(wu(vendor=vendor), AV)
            self.assertEqual(status, "resolved", vendor)
            self.assertEqual(found["hostid"], 12)
            self.assertEqual(found["platform"], "x86_64-pc-linux-gnu")
            self.assertEqual(found["family"], "")  # written as \N, NULL in the table

    def test_no_canonical_result_or_host_stays_unresolved(self):
        self.assertEqual(bp.resolve_host(wu(canon=0), AV), ("no_canonical", {}))
        self.assertEqual(bp.resolve_host(wu(hostid=""), AV), ("result_missing", {}))


class ReadSkip(unittest.TestCase):
    def test_reads_doc_and_node_pairs_and_nothing_means_none(self):
        import tempfile

        self.assertEqual(bp.read_skip(None), set())
        with tempfile.NamedTemporaryFile("w", suffix=".tsv") as f:
            f.write("sn1_1900-01-01_ed-1_seq-1\tboinc-aaaaaaaaaaaa\nsn1_1900-01-01_ed-1_seq-2\tboinc-bbbbbbbbbbbb\textra\n\n")
            f.flush()
            self.assertEqual(
                bp.read_skip(f.name),
                {("sn1_1900-01-01_ed-1_seq-1", "boinc-aaaaaaaaaaaa"), ("sn1_1900-01-01_ed-1_seq-2", "boinc-bbbbbbbbbbbb")},
            )


if __name__ == "__main__":
    unittest.main()
