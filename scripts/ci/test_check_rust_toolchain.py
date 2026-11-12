"""scripts/ci/check-rust-toolchain.py: every place that installs Rust uses rust-toolchain.toml's version."""

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("check", HERE / "check-rust-toolchain.py")
check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(check)

TOML = '[toolchain]\nchannel = "1.100.0"\ncomponents = ["rustfmt", "clippy"]\n'
READ = """      - id: rust-toolchain
        run: echo "channel=$(sed -n 's/^channel *= *"\\(.*\\)"/\\1/p' rust-toolchain.toml)" >> "$GITHUB_OUTPUT"
"""
WORKFLOW = """jobs:
  build:
    steps:
      - uses: actions/checkout@v4
""" + READ + """      - uses: dtolnay/rust-toolchain@abc # stable branch
        with:
          toolchain: ${{ steps.rust-toolchain.outputs.channel }}
          targets: x86_64-unknown-linux-musl
      - run: cargo build
"""


class Toolchain(unittest.TestCase):
    def tree(self, toml=TOML, workflow=WORKFLOW, containerfile=None):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(root))
        (root / "rust-toolchain.toml").write_text(toml)
        (root / ".github/workflows").mkdir(parents=True)
        (root / ".github/workflows/a.yml").write_text(workflow)
        if containerfile is not None:
            (root / "docker").mkdir()
            (root / "docker/Dockerfile.x").write_text(containerfile)
        return root

    def findings(self, **kwargs):
        return check.check(self.tree(**kwargs))

    def test_consistent_tree_passes(self):
        self.assertEqual(self.findings(), [])

    def test_the_repository_passes(self):
        self.assertEqual(check.check(check.ROOT), [])

    def test_the_command_line_exits_1_on_a_finding(self):
        root = self.tree(toml=TOML.replace("1.100.0", "stable"))
        run = subprocess.run([sys.executable, str(HERE / "check-rust-toolchain.py"), "--root", str(root)],
                             capture_output=True, text=True)
        self.assertEqual(run.returncode, 1)
        self.assertIn("not an exact release", run.stderr)

    def test_floating_channels_are_refused(self):
        for channel in ("stable", "beta", "nightly", "1.100", "1"):
            with self.subTest(channel=channel):
                found = self.findings(toml=TOML.replace("1.100.0", channel))
                self.assertTrue(any("not an exact release" in f for f in found), found)

    def test_missing_components_are_refused(self):
        found = self.findings(toml='[toolchain]\nchannel = "1.100.0"\ncomponents = ["rustfmt"]\n')
        self.assertEqual(found, ["rust-toolchain.toml: components lacks clippy"])

    def test_a_literal_stable_is_refused(self):
        found = self.findings(workflow=WORKFLOW.replace("${{ steps.rust-toolchain.outputs.channel }}", "stable"))
        self.assertEqual(len(found), 1)
        self.assertIn("'stable' is not", found[0])

    def test_a_literal_other_version_is_refused_and_the_same_one_passes(self):
        literal = "${{ steps.rust-toolchain.outputs.channel }}"
        self.assertEqual(len(self.findings(workflow=WORKFLOW.replace(literal, "1.99.1"))), 1)
        self.assertEqual(self.findings(workflow=WORKFLOW.replace(literal, "1.100.0")), [])

    def test_the_expression_needs_the_reading_step(self):
        found = self.findings(workflow=WORKFLOW.replace(READ, ""))
        self.assertEqual(len(found), 1)
        self.assertIn("no earlier step", found[0])

    def test_a_reading_step_that_ignores_the_file_is_refused(self):
        found = self.findings(workflow=WORKFLOW.replace("rust-toolchain.toml)", "x.toml)"))
        self.assertEqual(len(found), 1)

    def test_a_missing_toolchain_input_is_refused(self):
        found = self.findings(workflow=WORKFLOW.replace(
            "          toolchain: ${{ steps.rust-toolchain.outputs.channel }}\n", ""))
        self.assertEqual(len(found), 1)
        self.assertIn("without a toolchain input", found[0])

    def test_image_tags(self):
        self.assertEqual(self.findings(containerfile="FROM rust:1.100.0-bookworm AS build\n"), [])
        self.assertEqual(self.findings(containerfile="FROM rust:1-bookworm AS build\n"), [])
        self.assertEqual(self.findings(containerfile="FROM rust:bookworm@sha256:" + "a" * 64 + "\n"), [])
        found = self.findings(containerfile="FROM rust:1.99.1-bookworm AS build\n")
        self.assertEqual(len(found), 1)
        self.assertIn("Rust image 1.99.1", found[0])
        self.assertEqual(len(self.findings(containerfile="ARG RUST_IMAGE=rust:1.99\n")), 1)


if __name__ == "__main__":
    unittest.main()
