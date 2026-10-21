#!/usr/bin/env python3
"""Unit tests for scripts/boinc/setup_mac_volunteer.py."""
from __future__ import annotations

from pathlib import Path
import tempfile
import unittest
from unittest import mock

try:
    from setup_mac_volunteer import (
        DEFAULT_PROJECT_URL,
        MAC_PREFS,
        MacSetupError,
        detect_host_resources,
        is_project_attached,
        merge_prefs_xml,
        setup_mac,
    )
except ModuleNotFoundError:
    from scripts.boinc.setup_mac_volunteer import (
        DEFAULT_PROJECT_URL,
        MAC_PREFS,
        MacSetupError,
        detect_host_resources,
        is_project_attached,
        merge_prefs_xml,
        setup_mac,
    )


class TestMacVolunteerSetup(unittest.TestCase):
    def test_merge_prefs_xml_creates_and_preserves_custom_tags(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            p = Path(tmp) / "global_prefs_override.xml"
            p.write_text(
                "<global_preferences>\n  <custom_tag>42</custom_tag>\n  <max_ncpus_pct>25</max_ncpus_pct>\n</global_preferences>\n",
                encoding="utf-8",
            )
            merged = merge_prefs_xml(p, MAC_PREFS)
            self.assertIn("<custom_tag>42</custom_tag>", merged)
            self.assertIn("<leave_apps_in_memory>1</leave_apps_in_memory>", merged)
            self.assertIn("<max_ncpus_pct>75</max_ncpus_pct>", merged)

    def test_is_project_attached_detects_url(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "client_state.xml").write_text(
                "<client_state><project><master_url>https://boinc.usnewsmap.com/</master_url></project></client_state>",
                encoding="utf-8",
            )
            self.assertTrue(is_project_attached(d, DEFAULT_PROJECT_URL))
            self.assertTrue(
                is_project_attached(d, "https://boinc.usnewsmap.com")
            )
            self.assertFalse(
                is_project_attached(d, "https://boinc.usnewsmap.com/usnewsmap/")
            )
            self.assertFalse(
                is_project_attached(d, "https://example.invalid/boinc/")
            )

    def test_default_project_url_is_the_master_url(self) -> None:
        # The volunteer guide, client_account.py and the Windows helper all
        # attach to the project root; a different default would never match
        # an attached host and would attach a new one to a URL that 404s.
        self.assertEqual(DEFAULT_PROJECT_URL, "https://boinc.usnewsmap.com/")

    def test_setup_mac_check_only_and_apply(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            fake_boinccmd = d / "boinccmd"
            fake_boinccmd.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            (d / "gui_rpc_auth.cfg").write_text("secret123\n", encoding="utf-8")

            preview = setup_mac(
                data_dir=d,
                boinccmd_path=fake_boinccmd,
                apply=False,
            )
            self.assertEqual(preview["mode"], "check-only")
            self.assertFalse((d / "global_prefs_override.xml").exists())

            with mock.patch("subprocess.run") as run_mock:
                run_mock.return_value = mock.Mock(returncode=0, stdout="17179869184\n", stderr="")
                applied = setup_mac(
                    data_dir=d,
                    boinccmd_path=fake_boinccmd,
                    account_key="acct_abc",
                    apply=True,
                )
            self.assertEqual(applied["mode"], "apply")
            self.assertTrue((d / "global_prefs_override.xml").is_file())
            self.assertTrue(applied["project_attached"])

    def test_apple_gpu_eligibility_needs_the_workunit_disk_bound(self) -> None:
        # ocr/boinc/usnm_qwen_8b_in advertises 18 GiB; 12 GiB is the 2B bound.
        gib = 1024 ** 3
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch("platform.machine", return_value="arm64"), \
                mock.patch("subprocess.run") as run_mock, \
                mock.patch("shutil.disk_usage") as usage_mock:
            run_mock.return_value = mock.Mock(returncode=0, stdout=f"{16 * gib}\n", stderr="")
            usage_mock.return_value = mock.Mock(free=12 * gib)
            info = detect_host_resources(Path(tmp))
            self.assertEqual(info["boinc_platform"], "arm64-apple-darwin")
            self.assertEqual(info["eligible_apps"], ["usnm_quality (native CPU)"])
            usage_mock.return_value = mock.Mock(free=18 * gib)
            info = detect_host_resources(Path(tmp))
            self.assertIn("usnm_qwen (native apple_gpu_8b Metal GPU)", info["eligible_apps"])

    def test_setup_mac_rejects_missing_boinc_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            fake_boinccmd = d / "boinccmd"
            fake_boinccmd.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            with self.assertRaises(MacSetupError):
                setup_mac(
                    data_dir=d / "missing",
                    boinccmd_path=fake_boinccmd,
                    apply=False,
                )


if __name__ == "__main__":
    unittest.main()
