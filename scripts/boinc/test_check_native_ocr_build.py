"""A native OCR build must be the container worker's engine and models, file for file (#1418)."""
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_ocr_build", Path(__file__).with_name("check-native-ocr-build.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

def elf(e_type):
    return b"\x7fELF\x02\x01\x01" + b"\x00" * 9 + struct.pack("<HH", e_type, module.EM_X86_64)


ELF = elf(module.ET_EXEC)


class CheckNativeOcrBuild(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.lock = self.root / "models.lock"
        self.lock.write_text(f"# name url sha256\ntessdata/eng.traineddata https://example.org/eng "
                             f"{hashlib.sha256(b'model').hexdigest()}\n")
        self.containerfile = self.root / "Containerfile"
        self.containerfile.write_text("RUN apt-get install -y tesseract-ocr=5.5.0-1+b1 libtesseract5=5.5.0-1+b1\n")
        self.build = self.root / "build"
        self.build.mkdir()
        self.files = {"usnm_ocr": ELF + b"worker", "usnm_tesseract": ELF + b"engine", "tessdata_eng.traineddata": b"model"}
        for name, data in self.files.items():
            (self.build / name).write_bytes(data)
        self.write_manifest()

    def write_manifest(self, **changes):
        manifest = {"engine_version": "5.5.0", "platform": "x86_64-pc-linux-gnu", "v": 1, "files": {
            name: {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()} for name, data in self.files.items()}}
        (self.build / "usnm_native_build.json").write_text(json.dumps({**manifest, **changes}))

    def run_check(self, *names):
        argv = ["--binary", str(self.build / "usnm_ocr"), "--models-lock", str(self.lock),
                "--engine-containerfile", str(self.containerfile)]
        for name in names or ("usnm_native_build.json", "usnm_tesseract", "tessdata_eng.traineddata"):
            argv += ["--extra-file", f"{name}={self.build / name}"]
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = module.main(argv)
        return code, out.getvalue() + err.getvalue()

    def test_a_build_of_the_locked_engine_and_models_passes_and_names_its_identity(self):
        code, out = self.run_check()
        self.assertEqual(code, 0, out)
        image = hashlib.sha256((self.build / "usnm_native_build.json").read_bytes()).hexdigest()
        self.assertIn(f"engine.image sha256:{image}, engine.base sha256:{hashlib.sha256(ELF + b'engine').hexdigest()}", out)

    def test_refuses_another_engine_or_model_or_a_changed_file(self):
        self.write_manifest(engine_version="5.4.1")
        self.assertIn("the build's Tesseract is 5.4.1, the container worker's is 5.5.0", self.run_check()[1])
        self.write_manifest()
        self.lock.write_text(self.lock.read_text().replace(hashlib.sha256(b"model").hexdigest(), "0" * 64))
        self.assertIn("tessdata_eng.traineddata isn't the locked model", self.run_check()[1])
        self.lock.write_text(self.lock.read_text() + "tessdata/deu.traineddata https://example.org/deu " + "1" * 64 + "\n")
        self.assertIn("exactly the locked models", self.run_check()[1])

    def test_refuses_a_manifest_the_worker_would_not_read(self):
        # The worker reads v as a u32 and bytes as a u64, and refuses unknown fields.
        size = len(self.files["usnm_ocr"])
        digest = hashlib.sha256(self.files["usnm_ocr"]).hexdigest()
        for changes in ({"v": True}, {"v": 1.0}, {"extra": 1},
                        {"files": {**json.loads((self.build / "usnm_native_build.json").read_text())["files"],
                                   "usnm_ocr": {"bytes": float(size), "sha256": digest}}}):
            with self.subTest(changes=changes):
                self.write_manifest(**changes)
                self.assertIn("is not a build manifest for x86_64-pc-linux-gnu", self.run_check()[1])

    def test_refuses_files_that_are_not_the_manifests(self):
        self.assertIn("missing: tessdata_eng.traineddata", self.run_check("usnm_native_build.json", "usnm_tesseract")[1])
        (self.build / "usnm_tesseract").write_bytes(ELF + b"other")
        self.assertIn("usnm_tesseract differs from the build manifest", self.run_check()[1])
        # Neither a non-ELF file nor a relocatable object or core dump runs.
        for data in (b"not an executable", elf(1) + b"engine", elf(4) + b"engine"):
            self.files["usnm_tesseract"] = data
            (self.build / "usnm_tesseract").write_bytes(data)
            self.write_manifest()
            self.assertIn("usnm_tesseract is not an x86-64 ELF executable", self.run_check()[1])
        self.files["usnm_tesseract"] = elf(module.ET_DYN) + b"engine"
        (self.build / "usnm_tesseract").write_bytes(self.files["usnm_tesseract"])
        self.write_manifest()
        self.assertEqual(self.run_check()[0], 0)

    def test_the_worker_comes_only_from_binary(self):
        (self.build / "invalid").write_bytes(b"not the worker")
        argv = ["--binary", str(self.build / "invalid"), "--models-lock", str(self.lock),
                "--engine-containerfile", str(self.containerfile)]
        for name in ("usnm_native_build.json", "usnm_tesseract", "tessdata_eng.traineddata", "usnm_ocr"):
            argv += ["--extra-file", f"{name}={self.build / name}"]
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertEqual(module.main(argv), 1)
        self.assertIn("the worker is --binary, not --extra-file usnm_ocr", err.getvalue())

    def test_the_repository_locks_parse(self):
        self.assertRegex(module.locked_engine(module.ROOT / "ocr/engines/tesseract/Containerfile"), r"^[0-9]+\.[0-9]+\.[0-9]+$")
        self.assertIn("tessdata_eng.traineddata", module.locked_models(module.ROOT / "ocr/engines/tesseract/models.lock"))

    def test_refuses_a_lock_digest_that_is_not_a_sha256(self):
        lock = self.root / "bad.lock"
        for digest in ("abc", "A" * 64, "g" * 64, "a" * 65):
            lock.write_text(f"tessdata/eng.traineddata https://example.invalid/eng {digest}\n")
            with self.subTest(digest=digest), self.assertRaisesRegex(ValueError, "not a sha256"):
                module.locked_models(lock)

    def test_refuses_a_lock_that_locks_a_model_twice(self):
        lock = self.root / "twice.lock"
        lock.write_text("".join(f"tessdata/eng.traineddata https://example.invalid/eng {c * 64}\n" for c in "ab"))
        with self.assertRaisesRegex(ValueError, "model locked twice"):
            module.locked_models(lock)

    def test_an_extra_file_needs_a_name_and_a_path(self):
        for spec in ("usnm_tesseract", "=" + str(self.build / "usnm_tesseract"), "usnm_tesseract="):
            argv = ["--binary", str(self.build / "usnm_ocr"), "--extra-file", spec]
            err = io.StringIO()
            with self.subTest(spec=spec), contextlib.redirect_stderr(err), self.assertRaises(SystemExit):
                module.main(argv)
            self.assertIn("--extra-file must be LOGICAL=PATH, both non-empty", err.getvalue())


if __name__ == "__main__":
    unittest.main()
