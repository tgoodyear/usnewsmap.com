import importlib.util
import io
import shutil
import tarfile
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "native", Path(__file__).with_name("setup-ja-native.py"))
native = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(native)


class NativeSetup(unittest.TestCase):
    def test_recipe_uses_image_pins_and_requirements_and_refuses_drift(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            for name in ("docker/Dockerfile.ja-ocr", "ocr/engines/ndlocr-lite/models.lock"):
                (root / name).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(native.ROOT / name, root / name)
            shutil.copytree(native.ROOT / "ocr/ja-ocr", root / "ocr/ja-ocr")
            plan = native.recipe(root)
            self.assertEqual(plan, native.recipe(native.ROOT))
            requirements = root / plan["requirements"]
            requirements.write_text(requirements.read_text() + "\nextra==1.0\n")
            self.assertNotEqual(native.recipe(root)["requirements_sha256"],
                                plan["requirements_sha256"])
            dockerfile = root / "docker/Dockerfile.ja-ocr"
            original = dockerfile.read_text()
            for old, new in [
                (plan["commit"], "0" * 40),
                ("grep -v '^flet'", "grep -v '^different'"),
                ("-r /opt/jaocr/requirements.txt", "-r /tmp/other.txt"),
            ]:
                with self.subTest(change=old):
                    dockerfile.write_text(original.replace(old, new))
                    with self.assertRaises(ValueError):
                        native.recipe(root)

    def test_archive_extraction_strips_root_and_refuses_unsafe_members(self):
        with tempfile.TemporaryDirectory() as work:
            root = Path(work)
            for name, kind in [
                ("ndlocr-lite-abc/src/ocr.py", tarfile.REGTYPE),
                ("ndlocr-lite-abc/../../escape", tarfile.REGTYPE),
                ("ndlocr-lite-abc/link", tarfile.SYMTYPE),
                ("/absolute", tarfile.REGTYPE),
            ]:
                with self.subTest(name=name):
                    archive = root / "source.tar.gz"
                    with tarfile.open(archive, "w:gz") as output:
                        member = tarfile.TarInfo(name)
                        member.type = kind
                        member.size = 2 if kind == tarfile.REGTYPE else 0
                        output.addfile(member, io.BytesIO(b"ok") if member.size else None)
                    destination = root / "engine"
                    if name.endswith("/src/ocr.py"):
                        native.extract(archive, destination, "abc")
                        self.assertEqual((destination / "src/ocr.py").read_bytes(), b"ok")
                    else:
                        with self.assertRaises(ValueError):
                            native.extract(archive, destination, "abc")


if __name__ == "__main__":
    unittest.main()
