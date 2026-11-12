"""Per-build trust for native OCR builds (#1418, docs/boinc/apps/native-ocr.md)."""
import contextlib
import copy
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("build_trust", Path(__file__).with_name("check-build-trust.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

ROOT = Path(__file__).resolve().parents[2]
BINDINGS = ROOT / "boinc/project/gold-engine-platforms.json"
LINUX = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1"
LINUX_BASE = "sha256:21877bc34785b43a6e405c8f41ee62261fb59ee377a1cc104d3577c792ba9904"
MACOS = "sha256:78065a0a43d8f8bf67c00c014ae2b0540d51d0038e4050dbbd509e14b63fa836"
MACOS_BASE = "sha256:4af660d0a8181c9734c1ac577f4d91089b5ceb17930b8a0a5a217beb99c82032"
WINDOWS = "sha256:" + "7" * 64
WINDOWS_BASE = "sha256:" + "6" * 64
CHALLENGE = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f"


def sorted_keys(value):
    """Whether every object in `value` has its keys in ascending order."""
    if isinstance(value, dict):
        return list(value) == sorted(value) and all(sorted_keys(v) for v in value.values())
    if isinstance(value, list):
        return all(sorted_keys(v) for v in value)
    return True


class BuildTrust(unittest.TestCase):
    def setUp(self):
        self.registry = module.load(module.REGISTRY)

    def with_windows(self, edits):
        registry = copy.deepcopy(self.registry)
        registry["builds"][WINDOWS] = {
            "challenges": [] if edits is None else [{
                "char_edits": edits, "crops": 30, "evidence": "a fake Windows run",
                "manifest_sha256": CHALLENGE, "max_cer": 0.08734328574534386, "ref_chars": 82794}],
            "engine_base": WINDOWS_BASE, "platform": "windows_x86_64",
            "source": {"artifact_sha256": "5" * 64, "commit": "4" * 40, "run": 1}}
        module.validate(registry)
        return registry

    def test_the_file_is_valid_with_sorted_keys(self):
        self.assertTrue(sorted_keys(json.loads(module.REGISTRY.read_text())))

    def test_linux_is_unchanged_and_macos_has_its_own_record(self):
        self.assertIn("CER 0.0716", module.check(self.registry, LINUX, LINUX_BASE, "x86_64-pc-linux-gnu"))
        self.assertIn("CER 0.0719", module.check(self.registry, MACOS, MACOS_BASE, "arm64-apple-darwin"))
        # A build is recorded for one platform and one engine only.
        with self.assertRaises(ValueError):
            module.check(self.registry, MACOS, LINUX_BASE, "arm64-apple-darwin")
        with self.assertRaises(ValueError):
            module.check(self.registry, MACOS, MACOS_BASE, "x86_64-apple-darwin")

    def test_a_build_without_a_passing_run_is_refused(self):
        with self.assertRaisesRegex(ValueError, "not recorded"):
            module.check(self.registry, WINDOWS, WINDOWS_BASE, "windows_x86_64")
        for edits in (None, 7300):
            with self.assertRaisesRegex(ValueError, "no challenge run"):
                module.check(self.with_windows(edits), WINDOWS, WINDOWS_BASE, "windows_x86_64")
        # Recorded and under the threshold, a Windows build takes the same path as Linux.
        self.assertIn("CER 0.0725", module.check(self.with_windows(6000), WINDOWS, WINDOWS_BASE, "windows_x86_64"))

    def test_the_command_line_exits_nonzero_when_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trust.json"
            path.write_text(json.dumps(self.with_windows(7300)))
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                rc = module.main(["--image", WINDOWS, "--base", WINDOWS_BASE, "--platform", "windows_x86_64",
                                  "--registry", str(path)])
            self.assertEqual(rc, 1)
            self.assertIn("no challenge run", err.getvalue())
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = module.main(["--image", MACOS, "--base", MACOS_BASE, "--platform", "arm64-apple-darwin"])
            self.assertEqual(rc, 0)

    def test_invalid_files_are_refused(self):
        def bad(edit):
            registry = copy.deepcopy(self.registry)
            edit(registry)
            with self.assertRaises(ValueError):
                module.validate(registry)
        bad(lambda r: r.update(v=2))
        bad(lambda r: r["builds"][LINUX].update(platform="aarch64-unknown-linux-gnu"))
        bad(lambda r: r["builds"][LINUX]["challenges"][0].update(ref_chars=0))
        bad(lambda r: r["builds"][LINUX].update(extra=1))
        bad(lambda r: r["equivalent"][0].update(digests=[LINUX, "sha256:" + "0" * 64]))
        bad(lambda r: r["equivalent"].append(copy.deepcopy(r["equivalent"][0])))

    def test_each_native_binding_is_a_recorded_build_with_a_passing_run(self):
        # A worker image binds every platform it's built for. A native build is one platform's
        # own digest, so a binding with one platform is a native build: it may be bound only
        # once its challenge is recorded, and only for the platform and engine recorded.
        bindings = json.loads(BINDINGS.read_text())
        native = {image: platforms for image, platforms in bindings.items() if len(platforms) == 1}
        self.assertIn(LINUX, native)
        for image, platforms in native.items():
            with self.subTest(image=image):
                (platform, base), = platforms.items()
                module.check(self.registry, image, base, platform)
        for image in self.registry["builds"]:
            if image in bindings:
                self.assertIn(image, native)
        # macOS is recorded but not bound: no host can earn trust for it yet.
        self.assertNotIn(MACOS, bindings)

    def test_native_units_stay_single_replica(self):
        # No native result is ever paired with another host's by BOINC's validator: each unit has
        # one result, assigned to one host, pinned to the app version of its build. If that ever
        # changes, pairing must stay within one build (homogeneous_app_version), and this fails
        # first.
        apps = {app.findtext("name"): app for app in ET.parse(ROOT / "boinc/project/project.xml").iter("app")}
        for name in ("usnm_ocr_native", "usnm_gold_native"):
            with self.subTest(app=name):
                self.assertEqual(apps[name].findtext("target_nresults"), "1")


if __name__ == "__main__":
    unittest.main()
