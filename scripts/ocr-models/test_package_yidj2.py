import hashlib
import importlib.util
import json
import sys
import tarfile
import tempfile
import tomllib
import unittest
from pathlib import Path

_spec = importlib.util.spec_from_file_location("package_yidj2", Path(__file__).with_name("package_yidj2.py"))
pkg = importlib.util.module_from_spec(_spec)
sys.modules["package_yidj2"] = pkg
_spec.loader.exec_module(pkg)


def digest(data):
    return hashlib.sha256(data).hexdigest()


class SyntheticBuild(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.inputs = self.root / "inputs"
        recipe = self.inputs / "recipe"
        recipe.mkdir(parents=True)
        files = {
            "model.traineddata": b"\x00model bytes",
            "LICENSE": b"Apache License\n",
            "split.txt": b"a b c\n",
            "recipe/study10.sh": b"#!/bin/sh\n",
            "recipe/notes.sha256": b"x\n",
        }
        for name, data in files.items():
            (self.inputs / name).write_bytes(data)
        self.model_dir = self.root / "model"
        self.model_dir.mkdir()
        (self.model_dir / "NOTICE.md").write_text("notice\n")
        prov = {
            "model": {"file": "m.traineddata", "sha256": digest(files["model.traineddata"]),
                      "bytes": len(files["model.traineddata"])},
            "start_model": {"license_file": {"asset": "LICENSE-base.txt", "sha256": digest(files["LICENSE"])}},
            "training_data": {"page_list": {"asset": "split.sha256.txt", "sha256": digest(files["split.txt"])}},
            "recipe": {"asset": "recipe.tar", "files": {
                "study10.sh": digest(files["recipe/study10.sh"]),
                "notes.sha256": digest(files["recipe/notes.sha256"]),
            }},
            "release": {"assets": ["LICENSE-base.txt", "NOTICE.md", "SHA256SUMS", "m.traineddata",
                                   "provenance.json", "recipe.tar", "split.sha256.txt"]},
        }
        self.write_provenance(prov)
        (self.model_dir / pkg.SUMS).write_text("")

    def write_provenance(self, prov):
        text = json.dumps(prov, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
        (self.model_dir / "provenance.json").write_text(text)

    def build(self, out, update_sums=False):
        return pkg.build(self.inputs / "model.traineddata", self.inputs / "LICENSE", self.inputs / "split.txt",
                         self.inputs / "recipe", self.root / out, model_dir=self.model_dir,
                         update_sums=update_sums)

    def test_build_is_reproducible_and_verifies(self):
        self.build("first", update_sums=True)
        (self.inputs / "recipe/study8.sh").write_bytes(b"another study\n")
        self.build("second")
        first, second = self.root / "first", self.root / "second"
        names = sorted(p.name for p in first.iterdir())
        self.assertEqual(names, sorted(p.name for p in second.iterdir()))
        for name in names:
            self.assertEqual((first / name).read_bytes(), (second / name).read_bytes(), name)
        pkg.verify(first, model_dir=self.model_dir)
        with tarfile.open(first / "recipe.tar") as tar:
            members = tar.getmembers()
        self.assertEqual([m.name for m in members], ["recipe-study10/notes.sha256", "recipe-study10/study10.sh"])
        self.assertEqual([(m.mtime, m.uid, m.gid, m.mode) for m in members], [(0, 0, 0, 0o644), (0, 0, 0, 0o755)])

    def test_build_refuses_unpinned_inputs_and_drift(self):
        self.build("first", update_sums=True)
        (self.inputs / "split.txt").write_bytes(b"changed\n")
        with self.assertRaisesRegex(pkg.PackageError, "training split"):
            self.build("bad")
        (self.inputs / "split.txt").write_bytes(b"a b c\n")
        (self.inputs / "recipe/study10.sh").write_bytes(b"edited\n")
        with self.assertRaisesRegex(pkg.PackageError, "recipe file"):
            self.build("bad")
        (self.inputs / "recipe/study10.sh").write_bytes(b"#!/bin/sh\n")
        (self.model_dir / "NOTICE.md").write_text("notice, edited\n")
        with self.assertRaisesRegex(pkg.PackageError, "--update-sums"):
            self.build("drift")

    def test_verify_wants_exactly_the_committed_assets(self):
        self.build("out", update_sums=True)
        out = self.root / "out"
        (out / "extra").write_bytes(b"")
        with self.assertRaisesRegex(pkg.PackageError, "holds"):
            pkg.verify(out, model_dir=self.model_dir)
        (out / "extra").unlink()
        (out / "m.traineddata").write_bytes(b"other")
        with self.assertRaisesRegex(pkg.PackageError, "m.traineddata: sha256"):
            pkg.verify(out, model_dir=self.model_dir)

    def test_provenance_must_be_canonical(self):
        path = self.model_dir / "provenance.json"
        path.write_text(json.dumps(json.loads(path.read_text())))
        with self.assertRaisesRegex(pkg.PackageError, "canonical"):
            self.build("out")


class CommittedRecord(unittest.TestCase):
    """The committed notice, provenance, SHA256SUMS and catalog entry agree."""

    @classmethod
    def setUpClass(cls):
        cls.prov = pkg.load_provenance(pkg.MODEL_DIR)
        cls.sums = pkg.parse_sums((pkg.MODEL_DIR / pkg.SUMS).read_text(encoding="utf-8"))
        cls.catalog = tomllib.loads((pkg.ROOT / "ocr/engines/engines.toml").read_text(encoding="utf-8"))

    def test_sums_cover_the_release_and_the_committed_files(self):
        self.assertEqual(sorted([*self.sums, pkg.SUMS]), sorted(self.prov["release"]["assets"]))
        for name in ["NOTICE.md", "provenance.json"]:
            self.assertEqual(self.sums[name], pkg.file_sha256(pkg.MODEL_DIR / name), name)
        self.assertEqual(self.sums[self.prov["model"]["file"]], self.prov["model"]["sha256"])
        self.assertEqual(self.sums[self.prov["start_model"]["license_file"]["asset"]],
                         self.prov["start_model"]["license_file"]["sha256"])
        self.assertEqual(self.sums[self.prov["training_data"]["page_list"]["asset"]],
                         self.prov["training_data"]["page_list"]["sha256"])

    def test_catalog_entry_matches_provenance_and_stays_opt_in(self):
        tess = next(e for e in self.catalog["engine"] if e["id"] == "tesseract-5.5")
        name = self.prov["model"]["catalog"]["model"]
        model = next(m for m in tess["model"] if m["name"] == name)
        release = self.prov["release"]
        self.assertEqual(model["version"], release["tag"])
        self.assertEqual(model["source"], release["url"])
        self.assertTrue(release["url"].endswith(f"/releases/tag/{release['tag']}"))
        self.assertEqual(model["license"], self.prov["model"]["license"])
        self.assertEqual(model["terms"], self.prov["training_data"]["terms"])
        self.assertIn(self.prov["model"]["sha256"], model["note"])
        self.assertIn(self.prov["model"]["license_url"], model["credit"])
        self.assertIn(self.prov["training_data"]["attribution"], model["credit"])
        profile = next(p for p in tess["profile"] if p["id"] == self.prov["model"]["catalog"]["profile"])
        self.assertEqual(profile["models"], [name])
        ids = [p["id"] for p in tess["profile"]]
        self.assertLess(ids.index("yid"), ids.index(profile["id"]))
        lock = (pkg.ROOT / "ocr/engines/tesseract/models.lock").read_text(encoding="utf-8")
        self.assertNotIn(name, lock)

    def test_notice_names_the_pinned_sources(self):
        notice = (pkg.MODEL_DIR / "NOTICE.md").read_text(encoding="utf-8")
        for pin in [self.prov["training_data"]["commit"], self.prov["start_model"]["commit"],
                    self.prov["model"]["license_url"], self.prov["training_data"]["attribution"],
                    self.prov["start_model"]["license_file"]["asset"]]:
            self.assertIn(pin, notice)


if __name__ == "__main__":
    unittest.main()
