"""ADR-0017 names outside ARM: no Azure or Proxmox calls."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

from scripts.test_env_scripts import Stubbed

ROOT = Path(__file__).resolve().parents[1]


class NodeNames(unittest.TestCase):
    def validate(self, name, env=""):
        return subprocess.run(["bash", "-c",
                               '. scripts/lib/names.sh; valid_ocr_node_name "$NAME" "$ENV_NAME"'],
                              cwd=ROOT, env={**os.environ, "NAME": name, "ENV_NAME": env},
                              capture_output=True, text=True).returncode

    def test_convention_env_and_node_id_limit(self):
        for env in ("prod", "dev", "x1", "xabcd"):
            for number in ("0", "1", "120"):
                self.assertEqual(self.validate(f"usnm-ocr-node{number}-arcs-{env}", env), 0)
        for name in ("usnm-ocr-node1", "usnm-ocr-node1-arcs-ci", "usnm-ocr-node1-arcs-staging",
                     "usnm-ocr-node1-arcs-xabcde", "usnm-ocr-node-1-arcs-prod",
                     "usnm-ocr-node1-arcs-Prod", "usnm-ocr-node123456789012345-arcs-prod"):
            self.assertNotEqual(self.validate(name), 0, name)
        self.assertNotEqual(self.validate("usnm-ocr-node1-arcs-prod", "dev"), 0)
        self.assertEqual(self.validate("usnm-ocr-node123456789-arcs-prod"), 0)
        self.assertNotEqual(self.validate("usnm-ocr-node1234567890-arcs-prod"), 0)

    def test_entrypoints_refuse_legacy_names_before_external_calls(self):
        for script, args in (
            ("onboard.sh", ["--env", "prod", "--host", "node.example.org", "--name", "usnm-ocr-node1"]),
            ("proxmox-create-vm.sh", ["--vmid", "120", "--name", "usnm-ocr-node1", "--ssh-key", "ssh-ed25519 test"]),
        ):
            run = subprocess.run(["bash", str(ROOT / "scripts/ocr-node" / script), *args],
                                 capture_output=True, text=True)
            self.assertEqual(run.returncode, 1, run.stderr)
            self.assertIn("ADR-0017", run.stderr)

    def test_standalone_proxmox_copy_uses_adjacent_names_helper(self):
        with tempfile.TemporaryDirectory(dir=ROOT) as directory:
            directory = Path(directory)
            shutil.copy(ROOT / "scripts/ocr-node/proxmox-create-vm.sh", directory)
            shutil.copy(ROOT / "scripts/lib/names.sh", directory)
            (directory / "id").write_text("#!/bin/sh\necho 1000\n")
            (directory / "id").chmod(0o755)
            run = subprocess.run(["bash", str(directory / "proxmox-create-vm.sh"), "--vmid", "120",
                                  "--name", "usnm-ocr-node1-arcs-prod", "--ssh-key", "ssh-ed25519 test"],
                                 env={**os.environ, "PATH": str(directory) + os.pathsep + os.environ["PATH"]},
                                 capture_output=True, text=True)
            self.assertEqual(run.returncode, 1, run.stderr)
            self.assertIn("run as root", run.stderr)
            self.assertNotIn("ADR-0017", run.stderr)


class NodeGroup(Stubbed):
    def setUp(self):
        super().setUp()
        self.settings_file('AZURE_TENANT_ID="tenant-1"\nUSNM_OCR_NODE_GROUP="group-id"\n')

    def group(self, *args, **extra):
        return subprocess.run(["bash", str(ROOT / "scripts/ocr-node/group.sh"), "--env", "dev", *args],
                              cwd=ROOT, env={**self.env, **extra}, capture_output=True, text=True)

    def test_saved_id_renamed_in_place_without_replacing_members(self):
        run = self.group()
        self.assertEqual(run.returncode, 0, run.stderr)
        calls = self.calls()
        patch = next(c for c in calls if c.startswith("rest --method patch"))
        self.assertIn("/groups/group-id", patch)
        self.assertIn('"displayName":"usnm-ocr-nodes-grp-dev"', patch)
        self.assertIn('"mailNickname":"usnm-ocr-nodes-grp-dev"', patch)
        self.assertFalse(any(c.startswith("ad group create") or c.startswith("ad group member add")
                             for c in calls))

    def test_dry_run_never_patches(self):
        run = self.group("--dry-run")
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertIn("would rename", run.stdout)
        self.assertFalse(any(c.startswith("rest --method patch") for c in self.calls()))

    def test_conventional_group_is_unchanged(self):
        run = self.group(STUB_NODE_GROUP_NAME="usnm-ocr-nodes-grp-dev",
                         STUB_NODE_GROUP_NICKNAME="usnm-ocr-nodes-grp-dev")
        self.assertEqual(run.returncode, 0, run.stderr)
        self.assertFalse(any(c.startswith("rest --method patch") for c in self.calls()))

    def test_unsaved_legacy_group_is_not_adopted_or_duplicated(self):
        self.settings_file('AZURE_TENANT_ID="tenant-1"\n')
        run = self.group(STUB_NODE_GROUP_IDS="legacy-id")
        self.assertEqual(run.returncode, 1, run.stderr)
        self.assertIn("USNM_OCR_NODE_GROUP is empty", run.stderr)
        calls = self.calls()
        lookup = next(c for c in calls if c.startswith("ad group list"))
        self.assertIn("usnm-ocr-nodes-dev", lookup)
        self.assertIn("usnm-ocr-nodes-grp-dev", lookup)
        self.assertFalse(any(c.startswith("ad group create") or c.startswith("rest --method patch")
                             for c in calls))
