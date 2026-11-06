"""Tests for the private-address check's handling of package-version pins."""

import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("no-private-addresses.sh")


def scan(files):
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        subprocess.run(["git", "init", "-q", str(root)], check=True)
        for name, content in files.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        subprocess.run(["git", "-C", str(root), "add", "."], check=True)
        return subprocess.run([str(SCRIPT), "--repo", str(root)], capture_output=True, text=True)


class PrivateAddresses(unittest.TestCase):
    def test_four_part_package_version_in_pip_freeze_is_not_an_address(self):
        version = ".".join(("10", "4", "0", "35"))
        result = scan({"data/pip-freeze.txt": f"nvidia-curand=={version}\n"})
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_addresses_and_other_text_are_still_checked(self):
        addresses = [
            ".".join(("10", "4", "0", "35")),
            ".".join(("172", "16", "2", "3")),
            ".".join(("192", "168", "4", "5")),
        ]
        cases = [
            ("data/pip-freeze.txt", f"--index-url=https://{addresses[0]}/simple\n"),
            ("data/pip-freeze.txt", f"package=={addresses[0]} # {addresses[1]}\n"),
            ("data/other.txt", f"package=={addresses[0]}\n"),
            ("data/pip-freeze.txt", f"package=={addresses[0]}\n{addresses[2]}\n"),
        ]
        for path, content in cases:
            with self.subTest(path=path, content=content):
                result = scan({path: content})
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("Private (RFC 1918) addresses", result.stdout)


if __name__ == "__main__":
    unittest.main()
