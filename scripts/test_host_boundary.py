"""Application onboarding must not own lab OS subscriptions or hypervisors."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts/ocr-node"


class HostBoundary(unittest.TestCase):
    def test_no_subscription_credential_flow(self):
        for name in ("onboard.sh", "node-setup.sh", "offboard.sh"):
            text = (SCRIPTS / name).read_text()
            for forbidden in ("rhel-org-id", "rhel-activation-key", "RH_ORG",
                              "RH_KEY", "subscription-manager register",
                              "subscription-manager unregister",
                              "insights-client --register",
                              "insights-client --unregister"):
                self.assertNotIn(forbidden, text, f"{name}: {forbidden}")

    def test_rhel_prerequisite_precedes_azure_changes(self):
        text = (SCRIPTS / "onboard.sh").read_text()
        self.assertLess(text.index("subscription-manager identity"),
                        text.index('step "Resource providers"'))
        self.assertIn("infrastructure operator", text)

    def test_no_lab_provisioning_entrypoints(self):
        for name in ("rhel-image.sh", "proxmox-create-vm.sh", "pve-ocr-storage.sh",
                     "scanfetch-ct.sh", "ocrdash-ct.sh", "hoststats.sh"):
            self.assertFalse((SCRIPTS / name).exists(), name)
