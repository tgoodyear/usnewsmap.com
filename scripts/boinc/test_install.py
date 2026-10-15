"""Installation tests: no server, wrapper execution, or registry is required."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("boinc_install", Path(__file__).with_name("install.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
IMAGE = "registry.example.org/usnm/boinc@sha256:" + "a" * 64


class InstallTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.project = self.root / "project"
        self.project.mkdir()
        (self.project / "config.xml").write_text("<boinc/>")
        self.wrapper = self.root / "docker_wrapper"
        self.wrapper.write_bytes(b"not-executed")

    def tearDown(self):
        self.tmp.cleanup()

    def test_app_files_templates_and_refusal_to_overwrite(self):
        target = module.install(self.project, self.wrapper, IMAGE, "1.0")
        self.assertEqual((target / "Dockerfile_usnm_ocr_1.0_x86_64-pc-linux-gnu_docker").read_text(), f"FROM {IMAGE}\n")
        tree = ET.parse(target / "version.xml")
        self.assertEqual(tree.find("file/main_program").tag, "main_program")
        self.assertTrue((self.project / "templates/usnm_ocr_out").is_file())
        with self.assertRaises(FileExistsError):
            module.install(self.project, self.wrapper, IMAGE, "1.0")

    def test_unpinned_and_injected_images_never_create_app(self):
        for image in ["image:latest", IMAGE + "\nRUN evil", "sha256:" + "a" * 64]:
            with self.assertRaises(ValueError):
                module.install(self.project, self.wrapper, image, "1.0")
        self.assertFalse((self.project / "apps").exists())

    def test_assessment_app_gets_its_templates(self):
        target = module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_assess")
        self.assertEqual(target.parts[-3], "usnm_assess")
        for name in ["usnm_assess_in", "usnm_assess_crash_in", "usnm_assess_out"]:
            self.assertTrue((self.project / "templates" / name).is_file())
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "2.0", "other")

    def test_gold_app_gets_its_output_template(self):
        target = module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_gold")
        self.assertEqual(target.parts[-3], "usnm_gold")
        self.assertTrue((self.project / "templates" / "usnm_gold_out").is_file())

    def test_surya_versions_per_plan_class(self):
        cpu = module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_surya", "docker_mt")
        gpu = module.install(self.project, self.wrapper, IMAGE, "1.1", "usnm_surya", "docker_nvidia_cuda")
        self.assertEqual(cpu.name, "x86_64-pc-linux-gnu__docker_mt")
        self.assertEqual(gpu.name, "x86_64-pc-linux-gnu__docker_nvidia_cuda")
        self.assertIn("use_gpu = true", (gpu / "job_usnm_surya_1.1_x86_64-pc-linux-gnu_docker_nvidia_cuda.toml").read_text())
        self.assertNotIn("use_gpu", (cpu / "job_usnm_surya_1.0_x86_64-pc-linux-gnu_docker_mt.toml").read_text())
        for name in ["usnm_surya_in", "usnm_surya_out"]:
            self.assertTrue((self.project / "templates" / name).is_file())
        # Each app only has the plan classes it was made for.
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "2.0", "usnm_surya", "docker")
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "2.0", "usnm_ocr", "docker_nvidia_cuda")

    def test_quality_and_qwen_apps(self):
        quality = module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_quality", "docker")
        self.assertEqual(quality.name, "x86_64-pc-linux-gnu__docker")
        self.assertNotIn("use_gpu", (quality / "job_usnm_quality_1.0_x86_64-pc-linux-gnu_docker.toml").read_text())
        for name in ["usnm_quality_in", "usnm_quality_out"]:
            self.assertTrue((self.project / "templates" / name).is_file())
        self.assertIn("<sticky/>", (self.project / "templates" / "usnm_quality_in").read_text())
        cpu = module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_qwen", "docker_mt")
        gpu = module.install(self.project, self.wrapper, IMAGE, "1.1", "usnm_qwen", "docker_nvidia_cuda")
        self.assertIn("use_gpu = true", (gpu / "job_usnm_qwen_1.1_x86_64-pc-linux-gnu_docker_nvidia_cuda.toml").read_text())
        self.assertNotIn("use_gpu", (cpu / "job_usnm_qwen_1.0_x86_64-pc-linux-gnu_docker_mt.toml").read_text())
        for name in ["usnm_qwen_in", "usnm_qwen_2b_in", "usnm_qwen_8b_in", "usnm_qwen_out"]:
            self.assertTrue((self.project / "templates" / name).is_file())
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "2.0", "usnm_quality", "docker_mt")
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "2.0", "usnm_qwen", "docker")

    def test_platforms_coexist_at_one_version_without_shared_download_collisions(self):
        staged_downloads = {}
        for platform in module.PLATFORMS:
            wrapper = self.root / f"wrapper_{platform}"
            wrapper.write_bytes(f"wrapper-payload-{platform}".encode())
            target = module.install(self.project, wrapper, IMAGE, "1.0", platform=platform)
            self.assertEqual(target.name, f"{platform}__docker")
            spec = ET.parse(target / "version.xml").getroot()
            mains = [f.findtext("physical_name") for f in spec.findall("file") if f.find("main_program") is not None]
            self.assertEqual(len(mains), 1)
            if platform == "windows_x86_64":
                self.assertTrue(mains[0].endswith(".exe"))
            else:
                self.assertFalse(mains[0].endswith(".exe"))
            logicals = {f.findtext("logical_name"): f.findtext("physical_name") for f in spec.findall("file") if f.find("logical_name") is not None}
            self.assertEqual(set(logicals), {"Dockerfile", "job.toml"})
            for f in spec.findall("file"):
                phys = f.findtext("physical_name")
                payload = (target / phys).read_bytes()
                if phys in staged_downloads:
                    self.assertEqual(staged_downloads[phys], payload)
                staged_downloads[phys] = payload
            with self.assertRaises(FileExistsError):
                module.install(self.project, wrapper, IMAGE, "1.0", platform=platform)
            if platform != "x86_64-pc-linux-gnu":
                with self.assertRaisesRegex(ValueError, "verified platform-specific --wrapper"):
                    module.install(self.project, module.DEFAULT_SERVER_WRAPPER, IMAGE, "2.0", platform=platform)
                with self.assertRaisesRegex(ValueError, "verified platform-specific --wrapper"):
                    module.install(self.project, None, IMAGE, "2.0", platform=platform)
        with self.assertRaises(ValueError):
            module.install(self.project, self.wrapper, IMAGE, "1.0", "usnm_surya", "docker_mt", platform="arm64-apple-darwin")

    def test_invalid_platform_does_not_create_files(self):
        with self.assertRaisesRegex(ValueError, "platform"):
            module.install(self.project, self.wrapper, IMAGE, "1.0", platform="../../other")
        self.assertFalse((self.project / "apps").exists())

    def test_native_quality_and_qwen_stage_expected_layout_and_sign_when_key_given(self):
        bin_unix = self.project / "usnm-quality-bin"
        bin_unix.write_bytes(b"\x7fELFnative")
        bin_win = self.project / "usnm-quality.exe"
        bin_win.write_bytes(b"MZnative")
        llama_srv = self.project / "llama-server"
        llama_srv.write_bytes(b"\x7fELFllama")
        llama_srv.chmod(0o644)
        llama_srv_win = self.project / "llama-server.exe"
        llama_srv_win.write_bytes(b"MZllama")
        llama_srv_win.chmod(0o644)
        llama_lock = self.project / "llama.lock"
        llama_lock.write_text("tag = 'b5000'\n")
        weights_lock = self.project / "weights.lock"
        weights_lock.write_text("model = 'qwen3-vl'\n")
        sign_key = self.project / "code_sign_private"
        sign_key.write_text("private-key\n")
        fake_crypt = self.project / "fake_crypt_prog"
        fake_crypt.write_text('#!/bin/sh\nprintf "sig:%s\\n" "$2"\n')
        fake_crypt.chmod(0o755)

        for platform, binary, ext in (
            ("x86_64-pc-linux-gnu", bin_unix, ""),
            ("aarch64-unknown-linux-gnu", bin_unix, ""),
            ("arm64-apple-darwin", bin_unix, ""),
            ("x86_64-apple-darwin", bin_unix, ""),
            ("windows_x86_64", bin_win, ".exe"),
        ):
            target = module.install(
                self.project,
                version="1.0",
                app="usnm_quality",
                plan_class="",
                platform=platform,
                kind="native",
                binary=binary,
                sign_key=sign_key,
                crypt_prog=fake_crypt,
            )
            self.assertEqual(target, self.project / f"apps/usnm_quality/1.0/{platform}")
            phys = f"usnm_quality_1.0_{platform}{ext}"
            self.assertTrue((target / phys).is_file())
            self.assertEqual((target / phys).stat().st_mode & 0o777, 0o755)
            self.assertTrue((target / f"{phys}.sig").is_file())
            xml_text = (target / "version.xml").read_text()
            self.assertIn(f"<physical_name>{phys}</physical_name>", xml_text)
            self.assertIn("<main_program />", xml_text)
            self.assertIn("<copy_file />", xml_text)
            self.assertNotIn("is_wrapper", xml_text)
            self.assertNotIn("dont_throttle", xml_text)
            self.assertEqual(
                sorted(p.name for p in target.iterdir()),
                sorted([phys, f"{phys}.sig", "version.xml"]),
            )

        # Native usnm_qwen without llama-server[.exe], llama.lock, and weights.lock fails before creating target dir
        for bad_extras in (
            None,
            [f"llama.lock={llama_lock}", f"weights.lock={weights_lock}"],
            [f"llama-server={llama_srv}", f"llama.lock={llama_lock}"],
        ):
            with self.assertRaisesRegex(ValueError, "requires --extra-file"):
                module.install(
                    self.project,
                    version="1.0",
                    app="usnm_qwen",
                    plan_class="apple_gpu_8b",
                    platform="arm64-apple-darwin",
                    kind="native",
                    binary=bin_unix,
                    extra_files=bad_extras,
                )
        self.assertFalse((self.project / "apps/usnm_qwen").exists())

        for plan_class, platform, gpu, expected_model_template in (
            ("mt", "x86_64-pc-linux-gnu", False, "usnm_qwen_2b_in"),
            ("apple_gpu_8b", "arm64-apple-darwin", True, "usnm_qwen_8b_in"),
            ("nvidia_cuda_8b", "x86_64-pc-linux-gnu", True, "usnm_qwen_8b_in"),
            ("nvidia_cuda_2b", "windows_x86_64", True, "usnm_qwen_2b_in"),
        ):
            bin_for_plat = bin_win if platform == "windows_x86_64" else bin_unix
            ext = ".exe" if platform == "windows_x86_64" else ""
            srv_logical = f"llama-server{ext}"
            srv_src = llama_srv_win if platform == "windows_x86_64" else llama_srv
            target = module.install(
                self.project,
                version="1.0",
                app="usnm_qwen",
                plan_class=plan_class,
                platform=platform,
                kind="native",
                binary=bin_for_plat,
                extra_files=[
                    f"{srv_logical}={srv_src}",
                    f"llama.lock={llama_lock}",
                    f"weights.lock={weights_lock}",
                ],
                sign_key=sign_key,
                crypt_prog=fake_crypt,
            )
            self.assertEqual(target, self.project / f"apps/usnm_qwen/1.0/{platform}__{plan_class}")
            suffix = f"usnm_qwen_1.0_{platform}_{plan_class}"
            main_phys = f"{suffix}{ext}"
            self.assertTrue((target / main_phys).is_file())
            self.assertTrue((target / f"{main_phys}.sig").is_file())
            self.assertTrue((target / f"{suffix}_{srv_logical}").is_file())
            self.assertEqual((target / f"{suffix}_{srv_logical}").stat().st_mode & 0o777, 0o755)
            self.assertTrue((target / f"{suffix}_{srv_logical}.sig").is_file())
            self.assertTrue((target / f"{suffix}_llama.lock").is_file())
            self.assertTrue((target / f"{suffix}_llama.lock.sig").is_file())
            self.assertTrue((target / f"{suffix}_weights.lock").is_file())
            self.assertTrue((target / f"{suffix}_weights.lock.sig").is_file())
            self.assertTrue((self.project / "templates" / expected_model_template).is_file())
            xml_text = (target / "version.xml").read_text()
            self.assertIn("<main_program />", xml_text)
            self.assertIn(f"<logical_name>{srv_logical}</logical_name>", xml_text)
            self.assertIn("<logical_name>llama.lock</logical_name>", xml_text)
            self.assertIn("<logical_name>weights.lock</logical_name>", xml_text)
            self.assertNotIn("is_wrapper", xml_text)
            if gpu:
                self.assertIn("<dont_throttle />", xml_text)
            else:
                self.assertNotIn("dont_throttle", xml_text)

        for bad_kwargs in (
            dict(wrapper=self.wrapper, image=IMAGE, version="1.0", kind="container", binary=bin_unix),
            dict(wrapper=self.wrapper, image=IMAGE, version="1.0", kind="native", binary=bin_unix),
            dict(version="1.0", kind="native"),
            dict(version="1.0", app="usnm_quality", plan_class="mt", kind="native", binary=bin_unix),
            dict(version="1.0", app="usnm_qwen", plan_class="", kind="native", binary=bin_unix),
            dict(
                version="1.0",
                app="usnm_qwen",
                plan_class="apple_gpu_8b",
                platform="x86_64-pc-linux-gnu",
                kind="native",
                binary=bin_unix,
            ),
            dict(
                version="1.0",
                app="usnm_qwen",
                plan_class="nvidia_cuda_8b",
                platform="arm64-apple-darwin",
                kind="native",
                binary=bin_unix,
            ),
        ):
            with self.subTest(**bad_kwargs), self.assertRaises(ValueError):
                module.install(self.project, **bad_kwargs)

    def test_app_release_script_accepts_native_and_new_platforms(self):
        import subprocess
        release = Path(__file__).resolve().parents[2] / "scripts/boinc-app-release.sh"
        binary = self.project / "usnm-quality"
        binary.write_bytes(b"native-bin")
        for args, expected_err in (
            (
                [
                    "--env", "prod", "--app", "usnm_quality", "--version", "1.0",
                    "--kind", "native", "--binary", str(binary), "--platform", "windows_x86_64",
                    "--signing-host", "bad host",
                ],
                "--signing-host must be a host name",
            ),
            (
                [
                    "--env", "prod", "--app", "usnm_qwen", "--version", "1.0",
                    "--kind", "native", "--binary", str(binary), "--plan-class", "apple_gpu_8b",
                    "--platform", "arm64-apple-darwin", "--signing-host", "bad host",
                ],
                "--signing-host must be a host name",
            ),
            (
                [
                    "--env", "prod", "--app", "usnm_qwen", "--version", "1.0",
                    "--kind", "native", "--binary", str(binary), "--plan-class", "apple_gpu_8b",
                    "--platform", "x86_64-pc-linux-gnu", "--signing-host", "signhost",
                ],
                "native --plan-class apple_gpu_8b is not supported on x86_64-pc-linux-gnu",
            ),
            (
                [
                    "--env", "prod", "--app", "usnm_qwen", "--version", "1.0",
                    "--kind", "native", "--binary", str(binary), "--plan-class", "apple_gpu_8b",
                    "--platform", "arm64-apple-darwin", "--signing-host", "signhost",
                ],
                "native usnm_qwen on arm64-apple-darwin requires --extra-file llama-server=PATH",
            ),
            (
                [
                    "--env", "prod", "--app", "usnm_ocr", "--version", "1.0",
                    "--kind", "container", "--platform", "windows_x86_64",
                    "--signing-host", "signhost",
                ],
                "Windows requires --wrapper, --wrapper-sha256 and --wrapper-source-pin from the reviewed build artifact",
            ),
            (
                [
                    "--env", "prod", "--app", "usnm_quality", "--version", "1.0",
                    "--kind", "container", "--binary", str(binary), "--signing-host", "signhost",
                ],
                "--binary and --extra-file are only valid with --kind native",
            ),
        ):
            res = subprocess.run(["bash", str(release), *args], capture_output=True, text=True)
            self.assertNotEqual(res.returncode, 0)
            self.assertIn(expected_err, res.stderr)


if __name__ == "__main__":
    unittest.main()
