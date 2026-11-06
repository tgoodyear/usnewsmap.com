"""Executable checks of Gold's scheduler binding, including old queued work."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "gold_patch", Path(__file__).with_name("patch-server-gold-routing.py")
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
BINDINGS = ROOT / "contrib-compute/boinc/gold-engine-platforms.json"
IMAGE = next(iter(json.loads(BINDINGS.read_text())))
BASES = json.loads(BINDINGS.read_text())[IMAGE]
JOB_ID = "a" * 64


class GoldRoutingTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.directory = Path(cls.temp.name)
        cls.binary = cls.directory / "routing"
        # Compile the real patched hook, not a separately implemented policy.
        source = r'''
#include <cstring>
#include <cstdio>
#include "gold-engine-routing.h"
struct WORKUNIT { const char* name; long appid = 1; };
struct APP { const char* name; };
struct AV { long platformid; const char* xml_doc; const char* plan_class; };
struct BEST_APP_VERSION { AV* avp; };
struct PLATFORM { const char* name; };
struct SHMEM {
    PLATFORM platform;
    APP app{"usnm_gold"};
    PLATFORM* lookup_platform_id(long) { return &platform; }
    APP* lookup_app(long) { return &app; }
} memory;
SHMEM* ssp = &memory;
struct CONFIG {
    const char* download_dir = "download";
    int download_path(const char* name, char* path) {
        std::snprintf(path, 4096, "download/2a/%s", name); return 0;
    }
} config;
struct LOGGER {
    template<class... T> void printf(int, const char* format, T... args) {
        std::fprintf(stderr, format, args...);
    }
} log_messages;
const int MSG_NORMAL = 0;
HOOK
const int ERR_NOT_FOUND = 1;
int assigned(WORKUNIT& wu, BEST_APP_VERSION* bavp) {
ASSIGNMENT
    return 0;
}
int resend(WORKUNIT& wu, APP* app, BEST_APP_VERSION* bavp) {
    bool can_resend = true;
    for (int result = 0; result < 1; ++result) {
RESEND
        return 0;
    }
    return 1;
}
int main(int argc, char** argv) {
    if (argc != 5) return 2;
    memory.platform.name = argv[2];
    const std::string xml = usnm_gold::read_file("version.xml");
    AV av{1, xml.c_str(), "docker"};
    BEST_APP_VERSION bav{std::strstr(argv[4], "anonymous") ? nullptr : &av};
    WORKUNIT wu{argv[1]}; APP app{argv[3]};
    memory.app.name = argv[3];
    if (!std::strcmp(argv[4], "assigned")) return assigned(wu, &bav);
    if (!std::strncmp(argv[4], "resend", 6)) return resend(wu, &app, &bav);
    return usnm_gold_is_infeasible(wu, app, bav) ? 1 : 0;
}
'''.replace("HOOK", MODULE.GATE).replace(
            "ASSIGNMENT", MODULE.ASSIGNMENT_PATCHED.partition(MODULE.ASSIGNMENT_ORIGINAL)[0]
        ).replace(
            "RESEND", MODULE.RESEND_GATE
        ).replace(
            '"/usr/local/lib/usnm-boinc/gold-engine-platforms.json"',
            json.dumps(str(BINDINGS)),
        )
        subprocess.run(
            [os.environ.get("CXX", "c++"), "-std=c++17", "-Wall", "-Wextra", "-Werror",
             "-I", str(Path(__file__).parent), "-x", "c++", "-", "-o", str(cls.binary)],
            input=source, text=True, check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.job = {"id": JOB_ID, "engine": {"image": IMAGE, "base": BASES["arm64-apple-darwin"]}}
        self.payload = f"FROM ghcr.io/tgoodyear/usnm-boinc-worker@{IMAGE}\n"
        self.version = (
            "<file_ref><file_name>wrapper</file_name><main_program/></file_ref>"
            "<file_ref><file_name>Dockerfile_gold</file_name><open_name>Dockerfile</open_name></file_ref>"
        )

    def run_gate(self, platform="arm64-apple-darwin", app="usnm_gold", mode="registered"):
        fanout = self.directory / "download/2a"
        fanout.mkdir(parents=True, exist_ok=True)
        (fanout / (JOB_ID + "_gold_json")).write_text(json.dumps(self.job))
        (fanout / "Dockerfile_gold").write_text("wrong hierarchical application path")
        (self.directory / "download/Dockerfile_gold").write_text(self.payload)
        (self.directory / "version.xml").write_text(self.version)
        return subprocess.run(
            [str(self.binary), "usnm_gold_" + JOB_ID, platform, app, mode],
            cwd=self.directory, capture_output=True, text=True,
        )

    def test_current_arm_and_amd64(self):
        for image, bases in json.loads(BINDINGS.read_text()).items():
            self.job["engine"]["image"] = image
            self.payload = f"FROM ghcr.io/tgoodyear/usnm-boinc-worker@{image}\n"
            for platform, base in bases.items():
                self.job["engine"]["base"] = base
                self.assertEqual(self.run_gate(platform).returncode, 0, (image, platform))
            self.job["engine"]["base"] = bases["arm64-apple-darwin"]
            self.assertEqual(self.run_gate("arm64-apple-darwin").returncode, 0)
            self.job["engine"]["base"] = bases["x86_64-pc-linux-gnu"]
            self.assertEqual(self.run_gate("arm64-apple-darwin").returncode, 1)

    def test_stale_job_and_wrong_architecture_are_refused(self):
        for base in ("sha256:" + "f" * 64, BASES["x86_64-pc-linux-gnu"]):
            self.job["engine"]["base"] = base
            result = self.run_gate()
            self.assertEqual(result.returncode, 1)
            self.assertIn("[Gold routing] refusing WU", result.stderr)
        self.job["engine"]["base"] = BASES["arm64-apple-darwin"]
        self.assertEqual(self.run_gate("x86_64-pc-linux-gnu").returncode, 1)

    def test_assignments_check_same_binding_before_result_creation(self):
        self.assertEqual(self.run_gate(mode="assigned").returncode, 0)
        self.job["engine"]["base"] = BASES["x86_64-pc-linux-gnu"]
        self.assertEqual(self.run_gate(mode="assigned").returncode, 1)
        self.assertEqual(self.run_gate(mode="assigned", app="usnm_quality").returncode, 0)

    def test_resends_refuse_incompatible_and_anonymous_without_expiration(self):
        self.assertEqual(self.run_gate(mode="resend").returncode, 0)
        self.assertEqual(self.run_gate(mode="resend-anonymous").returncode, 1)
        self.job["engine"]["base"] = BASES["x86_64-pc-linux-gnu"]
        self.assertEqual(self.run_gate(mode="resend").returncode, 1)
        self.assertNotIn("can_resend = false", MODULE.RESEND_GATE)
        source = '#include "sched_check.h"\n' + MODULE.RESEND_ORIGINAL
        self.assertIn(MODULE.RESEND_GATE, MODULE.patch_resend(source))
        with self.assertRaises(ValueError):
            MODULE.patch_resend(MODULE.patch_resend(source))

    def test_different_signed_worker_is_refused(self):
        self.payload = "FROM ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "b" * 64 + "\n"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("differs from registered app", result.stderr)

    def test_unreviewed_worker_and_platform_are_refused(self):
        self.assertEqual(self.run_gate("unknown-platform").returncode, 1)
        self.job["engine"]["image"] = "sha256:" + "c" * 64
        self.assertEqual(self.run_gate().returncode, 1)

    def test_missing_duplicate_and_unsafe_payload_are_refused(self):
        original = self.version
        for version in ("", original + original, original.replace("Dockerfile_gold", "../secret")):
            self.version = version
            self.assertEqual(self.run_gate().returncode, 1)

    def test_job_identity_mismatch_and_anonymous_app_are_refused(self):
        self.assertEqual(self.run_gate(mode="anonymous").returncode, 1)
        self.job["id"] = "b" * 64
        self.assertEqual(self.run_gate().returncode, 1)

    def native_version(self, image=IMAGE):
        name = "usnm_native_build_" + image.removeprefix("sha256:") + ".json"
        return (
            "<file_ref><file_name>usnm_ocr</file_name><main_program/></file_ref>"
            f"<file_ref><file_name>{name}</file_name>"
            "<open_name>usnm_native_build.json</open_name></file_ref>"
        )

    def test_native_app_needs_the_jobs_own_build(self):
        self.version = self.native_version()
        self.assertEqual(self.run_gate(app="usnm_gold_native").returncode, 0)
        self.assertEqual(self.run_gate(app="usnm_gold_native", mode="assigned").returncode, 0)
        self.version = self.native_version("sha256:" + "b" * 64)
        result = self.run_gate(app="usnm_gold_native")
        self.assertEqual(result.returncode, 1)
        self.assertIn("differs from registered native build", result.stderr)

    def test_native_app_keeps_the_reviewed_binding(self):
        self.version = self.native_version()
        self.job["engine"]["base"] = BASES["x86_64-pc-linux-gnu"]
        self.assertEqual(self.run_gate(app="usnm_gold_native").returncode, 1)
        self.assertEqual(self.run_gate(app="usnm_gold_native", mode="anonymous").returncode, 1)

    def test_the_apps_cant_run_each_others_units(self):
        # A container version in the native app, and a native version in the container app.
        self.assertEqual(self.run_gate(app="usnm_gold_native").returncode, 1)
        self.version = self.native_version()
        self.assertEqual(self.run_gate(app="usnm_gold").returncode, 1)

    def test_other_apps_preserve_normal_routing(self):
        self.job = {}
        self.assertEqual(self.run_gate(app="usnm_quality").returncode, 0)

    def test_patch_is_exact_and_fail_closed_on_source_drift(self):
        original = '#include "sched_customize.h"\n' + MODULE.ORIGINAL + "\nreturn false;\n}"
        self.assertIn('#include "gold-engine-routing.h"', MODULE.patch_source(original))
        for invalid in ("", original + original, MODULE.patch_source(original)):
            with self.assertRaises(ValueError):
                MODULE.patch_source(invalid)
        assignment = '#include "sched_assign.h"\n' + MODULE.ASSIGNMENT_ORIGINAL + "\ncreate_result();"
        patched = MODULE.patch_assignment(assignment)
        self.assertLess(patched.index("usnm_gold_is_infeasible"), patched.index("create_result"))
        for invalid in ("", assignment + assignment, patched):
            with self.assertRaises(ValueError):
                MODULE.patch_assignment(invalid)

    def test_filesystem_gate_is_not_added_to_locked_fast_check(self):
        fast_body = "\n// unchanged fast hook\nreturn false;\n}"
        original = '#include "sched_customize.h"\n' + MODULE.ORIGINAL + fast_body
        patched = MODULE.patch_source(original)
        self.assertTrue(patched.endswith(MODULE.ORIGINAL + fast_body))
        for patch, target in (
            (MODULE.patch_slow_check, "    WORKUNIT& wu = wu_result.workunit;"),
            (MODULE.patch_locality, "    APP* app = ssp->lookup_app(wu.appid);"),
        ):
            source = '#include "sched_check.h"\n' + target
            self.assertIn("usnm_gold_is_infeasible", patch(source))
            with self.assertRaises(ValueError):
                patch(patch(source))
        locality = MODULE.patch_locality(
            '#include "sched_check.h"\n    APP* app = ssp->lookup_app(wu.appid);'
        )
        self.assertIn("return INFEASIBLE_CUSTOM;", locality)
        self.assertNotIn("return ERR_NO_APP_VERSION;", locality)


if __name__ == "__main__":
    unittest.main()
