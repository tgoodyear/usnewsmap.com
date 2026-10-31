"""Hermetic on-prem lifecycle, reviewed promotion and interrupted retry tests."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import unittest
from unittest.mock import Mock, patch
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bootstrap_pipeline as pipeline
import onprem_pipeline as onprem
import onprem_artifacts as artifacts


def manifest(kind="container"):
    return dict(schema=1, env="dev", host="fixture.example", ct="230",
                source_commit="a" * 40,
                release=dict(app="usnm_ocr", version="1.0", platform="x86_64-pc-linux-gnu",
                             plan_class="docker", kind=kind),
                images={**{k: "sha256:" + str(i) * 64 for i, k in enumerate(("base", "bin", "server", "bridge"), 1)},
                        "worker": "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "c" * 64},
                worker=dict(source_commit="b" * 40, run=123, attempt=1, receipt_sha256="d" * 64))


class OnpremTests(unittest.TestCase):
    def setUp(self):
        self.directory = pipeline.ROOT / "target" / ("onprem-test-" + uuid.uuid4().hex)
        self.directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.directory)
        self.acceptance = self.directory / "client.json"
        self.acceptance.write_text(json.dumps({"client": {"mode": "existing"}}))
        self.config_path = self.directory / "config.json"
        self.config = dict(env="dev", app="usnm_ocr", version="1.0", platform="x86_64-pc-linux-gnu",
                           plan_class="docker", signing_host="signer.example", acceptance_config=str(self.acceptance))
        self.config_path.write_text(json.dumps(self.config))
        self.args = argparse.Namespace(state_dir=self.directory, dry_run=False, approve_retry=None,
                                       coordination_claim="https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-123",
                                       onprem_host="fixture.example", onprem_ct="230", authorize_runtime_install=True)
        self.config, identity = onprem.load_onprem(self.config_path, self.args)
        self.p = onprem.OnpremPipeline(self.args, self.config, identity, Mock(return_value=""))

    def test_target_identity_and_binary_bytes_bound_to_resume(self):
        _, first = onprem.load_onprem(self.config_path, self.args)
        self.args.onprem_ct = "231"
        _, second = onprem.load_onprem(self.config_path, self.args)
        self.assertNotEqual(first, second)
        self.args.onprem_ct = "--bad"
        with self.assertRaises(pipeline.PipelineError):
            onprem.load_onprem(self.config_path, self.args)
        binary = self.directory / "binary"
        binary.write_bytes(b"native")
        self.args.onprem_ct = "230"
        config = {k: v for k, v in self.config.items() if k not in ("onprem_host", "onprem_ct")}
        config.update(app="usnm_quality", plan_class="", kind="native", binary=str(binary),
                      binary_sha256=hashlib.sha256(b"native").hexdigest(), binary_source="a" * 40)
        self.config_path.write_text(json.dumps(config))
        loaded, _ = onprem.load_onprem(self.config_path, self.args)
        self.assertEqual(loaded["kind"], "native")
        binary.write_bytes(b"replaced")
        with self.assertRaisesRegex(pipeline.PipelineError, "checksum"):
            onprem.load_onprem(self.config_path, self.args)

    def test_native_missing_qwen_payload_fails_before_lifecycle(self):
        config = dict(self.config, kind="native", app="usnm_qwen", plan_class="mt",
                      binary=str(self.config_path), binary_sha256=hashlib.sha256(self.config_path.read_bytes()).hexdigest(),
                      binary_source="a" * 40)
        with self.assertRaisesRegex(pipeline.PipelineError, "required reviewed extra-file"):
            onprem.verify_binary(config)

    def test_six_gb_release_requires_reviewed_vulkan_payload_and_linux_platform(self):
        binary = self.directory / "binary"
        binary.write_bytes(b"native fixture")
        entry = dict(path=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                     source_commit="a" * 40)
        extras = {name: dict(entry) for name in
                  ("llama-server", "llama.lock", "weights.lock", "libggml.so", "libggml-base.so", "libggml-vulkan.so")}
        config = {key: value for key, value in self.config.items() if key not in ("onprem_host", "onprem_ct")}
        config.update(kind="native", app="usnm_qwen", plan_class="nvidia_vulkan_6gb_8b",
                      binary=str(binary), binary_sha256=entry["sha256"], binary_source=entry["source_commit"],
                      extra_files=extras)
        self.config_path.write_text(json.dumps(config))
        loaded, _ = onprem.load_onprem(self.config_path, self.args)
        self.assertEqual(loaded["plan_class"], "nvidia_vulkan_6gb_8b")
        for missing in extras:
            with self.subTest(missing=missing), self.assertRaisesRegex(pipeline.PipelineError, "required reviewed extra-file"):
                onprem.verify_binary(dict(config, extra_files={key: value for key, value in extras.items() if key != missing}))
        config["platform"] = "windows_x86_64"
        self.config_path.write_text(json.dumps(config))
        with self.assertRaisesRegex(pipeline.PipelineError, "unsupported native"):
            onprem.load_onprem(self.config_path, self.args)

    def test_real_signer_manifest_gate_overrides_azure_and_rejects_other_release(self):
        text = (pipeline.ROOT / "scripts/boinc/boinc-app-release.sh").read_text()
        block = 'if [ -n "$ONPREM_MANIFEST$ARTIFACT_RECEIPT" ]; then' + text.split(
            'if [ -n "$ONPREM_MANIFEST$ARTIFACT_RECEIPT" ]; then', 1)[1].split(
                '[ -z "$(git status --porcelain -- "$BOINC_SETTINGS")"', 1)[0]
        path = self.directory / "manifest.json"
        path.write_text(json.dumps(manifest()))
        env = dict(os.environ, ONPREM_MANIFEST=str(path), ARTIFACT_RECEIPT=str(self.directory / "signed.json"),
                   SERVER_MODE="onprem", PREPARE_ONLY="true", ENV_NAME="dev", ONPREM_HOST="fixture.example",
                   ONPREM_CT="230", APP="usnm_ocr", VERSION="1.0", PLATFORM="x86_64-pc-linux-gnu",
                   PLAN_CLASS="docker", KIND="container", WORKER_IMAGE=manifest()["images"]["worker"],
                   BINARY="", SERVER_IMAGE="old.azurecr.io/image@sha256:" + "f" * 64)
        shell = 'set -euo pipefail\ndie() { echo "$*" >&2; exit 1; }\ngit() { :; }\nEXTRA_FILES=()\n'
        result = subprocess.run(["bash", "-c", shell + block + '\necho "$SERVER_IMAGE"'], cwd=pipeline.ROOT,
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), manifest()["images"]["bridge"])
        env["VERSION"] = "2.0"
        result = subprocess.run(["bash", "-c", shell + block], cwd=pipeline.ROOT,
                                env=env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("manifest does not match", result.stderr)

    def test_real_signer_writes_versioned_immutable_receipt_without_publication(self):
        text = (pipeline.ROOT / "scripts/boinc/boinc-app-release.sh").read_text()
        marker = '    if [ -n "$ARTIFACT_RECEIPT" ]; then'
        block = marker + text.split(marker, 1)[1].split('    echo "prepared signed', 1)[0]
        receipt = self.directory / "signed.json"
        result = subprocess.run(["bash", "-c", "set -euo pipefail\n" + block],
                                env=dict(os.environ, ARTIFACT_RECEIPT=str(receipt), appver_image="sha256:" + "e" * 64,
                                         sha="a" * 40, SERVER_IMAGE=manifest()["images"]["bridge"]),
                                cwd=pipeline.ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(receipt.read_text()), dict(schema=1, image="sha256:" + "e" * 64,
                         source_commit="a" * 40, server_image=manifest()["images"]["bridge"]))
        self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)

    def test_isolated_signer_receives_local_public_runtime_without_azure_fallback(self):
        text = (pipeline.ROOT / "scripts/boinc/boinc-app-release.sh").read_text()
        marker = 'if [ -n "$ONPREM_MANIFEST" ]; then\n  # Transfer'
        block = marker + text.split(marker, 1)[1].split('git archive --format=tar HEAD scripts/boinc/install.py', 1)[0]
        transfer = self.directory / "public-image"
        shell = """set -euo pipefail
die() { echo "$*" >&2; exit 1; }
baz() { exit 99; }
onprem_ct() { printf '%s' "$*"; }
ssh_host() {
  case "$*" in
    "podman load"*) cat > "$TRANSFER" ;;
    "podman image exists"*) ;;
    *) exit 98 ;;
  esac
}
"""
        result = subprocess.run(["bash", "-c", shell + block], cwd=pipeline.ROOT,
                                env=dict(os.environ, ONPREM_MANIFEST="reviewed.json", TRANSFER=str(transfer),
                                         SERVER_IMAGE=manifest()["images"]["bridge"]),
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(transfer.read_text(), "podman save '" + manifest()["images"]["bridge"] + "'")

    def test_native_private_stage_rejects_rebuilt_binary_or_auxiliary_before_transfer(self):
        text = (pipeline.ROOT / "scripts/boinc/boinc-app-release.sh").read_text()
        marker = 'if [ -n "$ONPREM_MANIFEST" ] && [ "$KIND" = native ]; then'
        block = marker + text.split(marker, 1)[1].split("# --- 2. Stage", 1)[0]
        work = self.directory / "stage"
        (work / "extra").mkdir(parents=True)
        binary = work / "binary_payload"
        binary.write_bytes(b"approved native")
        extra = work / "extra/llama-server"
        extra.write_bytes(b"approved helper")
        path = self.directory / "native-manifest.json"
        path.write_text(json.dumps({"worker": {
            "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "extra_files": {"llama-server": {"sha256": hashlib.sha256(extra.read_bytes()).hexdigest()}}}}))
        shell = 'set -euo pipefail\ndie() { echo "$*" >&2; exit 1; }\n'
        env = dict(os.environ, ONPREM_MANIFEST=str(path), KIND="native", work=str(work), binary_name="binary_payload")
        def verify():
            return subprocess.run(["bash", "-c", shell + block], cwd=pipeline.ROOT, env=env, capture_output=True, text=True)
        self.assertEqual(verify().returncode, 0)
        for changed in (binary, extra):
            old = changed.read_bytes()
            changed.write_bytes(b"concurrent rebuild")
            result = verify()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("nothing transferred or signed", result.stderr)
            changed.write_bytes(old)

    def test_startup_wait_tolerates_absent_or_old_container_but_is_bounded(self):
        with patch.object(artifacts, "verify_runtime", side_effect=[ValueError("not created"), ValueError("old"), None]) as verify, patch.object(
                artifacts.time, "sleep") as sleep:
            artifacts.wait_for_runtime(manifest(), self.directory)
        self.assertEqual(verify.call_count, 3)
        self.assertEqual(sleep.call_count, 2)
        with patch.object(artifacts, "verify_runtime", side_effect=ValueError("wrong image")), patch.object(
                artifacts.time, "monotonic", side_effect=[0, 181]), patch.object(artifacts.time, "sleep") as sleep:
            with self.assertRaisesRegex(ValueError, "wrong image"):
                artifacts.wait_for_runtime(manifest(), self.directory)
        sleep.assert_not_called()

    def test_systemd_job_reconciliation_keeps_waiting_on_pending_or_unknown_job(self):
        with patch.object(artifacts, "call", side_effect=["123", OSError("connection lost"), ""]), patch.object(
                artifacts.time, "sleep") as sleep:
            artifacts.wait_for_jobs()
        self.assertEqual(sleep.call_count, 2)

    def test_supervisor_keeps_exclusion_after_outer_helper_is_killed(self):
        program = self.directory / "child.py"
        ready, release = self.directory / "ready", self.directory / "release"
        program.write_text(f"""import argparse, os, time
from pathlib import Path
parser = argparse.ArgumentParser()
parser.add_argument('--operation')
parser.add_argument('--payload')
parser.add_argument('--lock-fd', type=int)
args = parser.parse_args()
Path({str(ready)!r}).write_text(str(os.getpid()))
while not Path({str(release)!r}).exists():
    time.sleep(.01)
os.close(args.lock_fd)
""")
        lock_path = self.directory / "operation.lock"
        wrapper = self.directory / "outer.py"
        wrapper.write_text(f"""import argparse, fcntl, sys
sys.path.insert(0, {str(Path(artifacts.__file__).parent)!r})
import onprem_artifacts as artifacts
with open({str(lock_path)!r}, 'a') as lock:
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    artifacts.supervise(argparse.Namespace(operation='verify', payload='{{}}'), lock, {str(program)!r})
""")
        process = subprocess.Popen([sys.executable, str(wrapper)], cwd=self.directory)
        child = None
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(ready.exists())
            child = int(ready.read_text())
            process.kill()
            process.wait(timeout=5)
            with lock_path.open("a") as lock:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                release.write_text("finished")
                deadline = time.monotonic() + 5
                while True:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                        break
                    except BlockingIOError:
                        if time.monotonic() >= deadline:
                            self.fail("completed operation did not release exclusion")
                        time.sleep(.01)
        finally:
            release.write_text("finished")
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            if child is not None:
                try:
                    os.kill(child, 15)
                except ProcessLookupError:
                    pass

    def test_ephemeral_azure_client_rejected_before_resource_operations(self):
        self.acceptance.write_text('{"client":{"mode":"ephemeral"}}')
        with self.assertRaisesRegex(pipeline.PipelineError, "independent existing"):
            onprem.load_onprem(self.config_path, self.args)

    def test_preflight_requires_install_authorization_and_never_uses_azure_mode(self):
        self.args.authorize_runtime_install = False
        with patch.object(pipeline.Pipeline, "preflight"):
            with self.assertRaisesRegex(pipeline.PipelineError, "authorize-runtime-install"):
                self.p.preflight()
        self.assertIn("onprem-", self.p.settings)

    def test_interrupted_process_reload_requires_exact_reconciled_phase(self):
        self.p.state.update(active="runtime", status="running", done=list(pipeline.PHASES[:4]))
        self.p.save()
        resumed = onprem.OnpremPipeline(self.args, self.config, self.p.identity, Mock(return_value=""))
        with patch.object(resumed, "preflight"), patch.object(resumed, "receipt"), patch.object(resumed, "phase") as phase:
            with self.assertRaisesRegex(pipeline.PipelineError, "approve-retry"):
                resumed.execute()
            phase.assert_not_called()
            self.args.approve_retry = "runtime"
            self.assertTrue(resumed.execute()["complete"])
        self.assertEqual([call.args[0] for call in phase.call_args_list], list(pipeline.PHASES[4:]))

    def test_manifest_rejects_azure_tags_missing_worker_and_changed_provenance(self):
        for edit in (lambda v: v["images"].update(bridge="registry.azurecr.io/image@sha256:" + "a" * 64),
                     lambda v: v["images"].update(server="localhost/server:latest"),
                     lambda v: v.update(worker=None),
                     lambda v: v["worker"].update(attempt=0)):
            value = manifest()
            edit(value)
            with self.assertRaises((ValueError, TypeError)):
                artifacts.validate(value)

    def test_installer_preserves_host_settings_and_reconciles_restart_failure(self):
        conf = self.directory / "etc"
        conf.mkdir()
        env = conf / "server.env"
        old = "SERVER_IMAGE=sha256:" + "f" * 64 + "\nOTHER=preserved\n"
        env.write_text(old)
        value = manifest()
        with patch.object(artifacts, "inspect"), patch.object(artifacts, "verify_runtime") as verify, patch.object(
                artifacts, "call", side_effect=RuntimeError("restart lost")):
            with self.assertRaises(RuntimeError):
                artifacts.install(value, conf)
            verify.assert_not_called()
        self.assertEqual(env.read_text(), "SERVER_IMAGE=" + value["images"]["bridge"] + "\nOTHER=preserved\n")
        self.assertEqual((conf / ("server.env.before-" + value["source_commit"])).read_text(), old)
        with patch.object(artifacts, "inspect"), patch.object(artifacts, "verify_runtime") as verify, patch.object(artifacts, "call") as call:
            artifacts.install(value, conf)
        call.assert_called_once_with("systemctl", "restart", "usnm-boinc-server.service", timeout=300)
        verify.assert_called_once_with(value, conf)

    def test_runtime_readback_refuses_pin_or_running_container_mismatch(self):
        conf = self.directory / "etc"
        conf.mkdir()
        env = conf / "server.env"
        value = manifest()
        env.write_text("SERVER_IMAGE=sha256:" + "f" * 64 + "\n")
        with patch.object(artifacts, "inspect"):
            with self.assertRaisesRegex(ValueError, "server.env"):
                artifacts.verify_runtime(value, conf)
            env.write_text("SERVER_IMAGE=" + value["images"]["bridge"] + "\n")
            with patch.object(artifacts, "call", return_value=json.dumps([{"Image": "f" * 64, "State": {"Running": True}}])):
                with self.assertRaisesRegex(ValueError, "running server"):
                    artifacts.verify_runtime(value, conf)

    def test_bad_local_artifact_cannot_replace_privileged_runtime_config(self):
        conf = self.directory / "etc"
        conf.mkdir()
        env = conf / "server.env"
        original = "SERVER_IMAGE=sha256:" + "f" * 64 + "\nOTHER=preserved\n"
        env.write_text(original)
        with patch.object(artifacts, "inspect", side_effect=ValueError("source mismatch")), patch.object(artifacts, "call") as call:
            with self.assertRaisesRegex(ValueError, "source mismatch"):
                artifacts.install(manifest(), conf)
        call.assert_not_called()
        self.assertEqual(env.read_text(), original)
        self.assertEqual(list(conf.iterdir()), [env])

    def test_rollback_and_new_pin_are_durable_before_restart_submission(self):
        conf = self.directory / "etc"
        conf.mkdir()
        (conf / "server.env").write_text("SERVER_IMAGE=sha256:" + "f" * 64 + "\n")
        synced = []
        def restart(*argv, **kwargs):
            self.assertEqual(synced, [conf, conf])
            self.assertTrue((conf / ("server.env.before-" + manifest()["source_commit"])).exists())
        with patch.object(artifacts, "inspect"), patch.object(artifacts, "sync_directory", side_effect=synced.append), patch.object(
                artifacts, "call", side_effect=restart), patch.object(artifacts, "verify_runtime"):
            artifacts.install(manifest(), conf)

    def test_partial_image_build_reuses_only_source_verified_ids(self):
        source = self.directory / "build/source"
        source.mkdir(parents=True)
        iid = source.parent / "base.iid"
        iid.write_text("1" * 64)
        ids = iter(str(i) * 64 for i in (2, 3, 4))
        def build(argv, **kwargs):
            self.assertEqual(argv[argv.index("--network") + 1], "host")
            Path(argv[argv.index("--iidfile") + 1]).write_text(next(ids))
        with patch.object(artifacts, "inspect", side_effect=lambda image, source: image), patch.object(
                artifacts.subprocess, "run", side_effect=build) as run:
            images = artifacts.build(source, "a" * 40)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(images["base"], "sha256:" + "1" * 64)
        with patch.object(artifacts, "inspect", side_effect=ValueError("source differs")), patch.object(artifacts.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                artifacts.build(source, "b" * 40)
        run.assert_not_called()

    def test_end_to_end_two_reviews_worker_receipt_sign_publish_acceptance_and_retry(self):
        self.lifecycle(interrupt=True)

    def test_native_lifecycle_reviews_binary_before_signing_and_publish(self):
        binary = self.directory / "native"
        binary.write_bytes(b"native worker")
        extra = self.directory / "libworker.so"
        extra.write_bytes(b"extra")
        self.config.update(kind="native", app="usnm_quality", plan_class="", binary=str(binary),
                           binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), binary_source="a" * 40,
                           extra_files={"libworker.so": dict(path=str(extra), sha256=hashlib.sha256(b"extra").hexdigest(),
                                                             source_commit="b" * 40)})
        self.p = onprem.OnpremPipeline(self.args, self.config, "native-identity", pipeline.run)
        self.lifecycle()

    def lifecycle(self, interrupt=False):
        repo = self.directory / "repo"
        repo.mkdir()
        def git(*argv, cwd=repo):
            if argv[0] == "push":
                return ""
            return pipeline.run(["git", *argv], cwd=cwd)
        git("init", "-b", "main")
        git("config", "user.email", "fixture@example.invalid")
        git("config", "user.name", "Offline Fixture")
        env = repo / self.p.environment_settings
        env.parent.mkdir(parents=True)
        env.write_text(json.dumps(dict(server="onprem", codeSignPublicKey="PUBLIC", images={})))
        scripts = repo / "scripts/boinc"
        scripts.mkdir(parents=True)
        # A fake isolated signer writes the real versioned receipt contract.
        release = scripts / "boinc-app-release.sh"
        release.write_text("""#!/usr/bin/env python3
import json, subprocess, sys
from pathlib import Path
get = lambda key: sys.argv[sys.argv.index(key) + 1]
assert '--prepare-only' in sys.argv
value = json.loads(Path(get('--onprem-manifest')).read_text())
assert value['images']['bridge'].startswith('sha256:')
if value['release']['kind'] == 'native':
    assert get('--extra-file').startswith('libworker.so=')
source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
Path(get('--artifact-receipt')).write_text(json.dumps(dict(schema=1, image='sha256:'+'e'*64,
 source_commit=source, server_image=value['images']['bridge'])))
""")
        release.chmod(0o755)
        (scripts / "acceptance.py").write_text("""import json, sys
from pathlib import Path
get = lambda key: sys.argv[sys.argv.index(key) + 1]
if '--dry-run' not in sys.argv:
    config = json.loads(Path(get('--config')).read_text())
    assert '--expected-image sha256:' in config['server_command'][-1]
    major, minor = get('--version').split('.')
    Path(get('--evidence')).write_text(json.dumps(dict(accepted=True, cleanup_complete=True,
      release=dict(app=get('--app'), version_num=int(major)*100+int(minor),
      platform=get('--platform'), plan_class=get('--plan-class')))))
""")
        git("add", ".")
        git("commit", "-m", "offline fixture")
        source = git("rev-parse", "HEAD")
        calls, prs = [], {}
        dispatched = False
        failed_once = False
        def gh(*argv):
            nonlocal dispatched
            if argv[:2] == ("run", "list"):
                return json.dumps([dict(databaseId=123, displayTitle="boinc-worker (bootstrap-" + self.p.state["id"] + ")",
                                        headSha=source, status="completed", conclusion="success")] if dispatched else [])
            if argv[:2] == ("workflow", "run"):
                dispatched = True
                return ""
            if argv[:2] == ("run", "view"):
                if "--attempt" in argv:
                    return json.dumps({"jobs": [dict(name="worker image (publish multi-architecture index)",
                                                    status="completed", conclusion="success")]})
                return json.dumps(dict(databaseId=123, headSha=source, attempt=1, status="completed", conclusion="success"))
            if argv[:2] == ("run", "download"):
                path = Path(argv[argv.index("--dir") + 1]) / "final.json"
                path.write_text(json.dumps(dict(schema=1, run_id="123", run_attempt="1", commit=source,
                                                model="worker", image=manifest()["images"]["worker"])))
                return ""
            if argv[:2] == ("pr", "list"):
                branch = argv[argv.index("--head") + 1]
                return json.dumps([dict(number=prs[branch], state="MERGED")] if branch in prs else [])
            if argv[:2] == ("pr", "create"):
                branch = argv[argv.index("--head") + 1]
                prs[branch] = len(prs) + 1
                git("merge", "--ff-only", branch)
                calls.append("review-" + ("runtime" if len(prs) == 1 else "app"))
                return ""
            if argv[:2] == ("pr", "edit"):
                return ""
            if argv[:2] == ("pr", "view"):
                return json.dumps(dict(baseRefName="main", reviewDecision="APPROVED",
                                       statusCheckRollup=[dict(conclusion="SUCCESS")]))
            self.fail(str(argv))
        def build(work):
            calls.append("build")
            value = manifest()
            value.update(source_commit=source, release=self.p.release())
            value["images"]["worker"] = None
            value["worker"] = None
            path = work / self.p.settings
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(value))
        def remote(work, action, value):
            nonlocal failed_once
            calls.append(action)
            artifacts.validate(value)
            if action == "install" and interrupt and not failed_once:
                failed_once = True
                raise pipeline.PipelineError("interrupted after env write")
            self.assertIn("review-runtime", calls)
            return {}
        def ct(command):
            if "--target publish" in command:
                self.assertIn("review-app", calls)
                self.assertIn("--image sha256:" + "e" * 64, command)
                calls.append("publish")
            return ""
        self.p.run = pipeline.run
        with patch.object(self.p, "git", side_effect=git), patch.object(self.p, "main", side_effect=lambda: git("rev-parse", "main")), patch.object(
                self.p, "preflight"), patch.object(self.p, "gh", side_effect=gh), patch.object(
                self.p, "has_open_findings", return_value=False), patch.object(self.p, "build", side_effect=build), patch.object(
                self.p, "remote", side_effect=remote), patch.object(self.p, "ct", side_effect=ct), patch.object(
                pipeline, "public_worker", return_value=manifest()["images"]["worker"]):
            for attempt in range(10):
                try:
                    self.p.execute()
                    break
                except pipeline.Blocked:
                    continue
                except pipeline.PipelineError:
                    if self.p.state.get("active") != "runtime":
                        raise
                    # Durable ambiguous intent must stop automatic retries.
                    with self.assertRaisesRegex(pipeline.PipelineError, "approve-retry"):
                        self.p.execute()
                    self.args.approve_retry = "runtime"
            else:
                self.fail("pipeline did not complete")
            self.p.execute()
        self.assertEqual(self.p.state["done"], list(pipeline.PHASES))
        self.assertEqual(len(prs), 2)
        self.assertEqual(calls.count("build"), 1)
        self.assertEqual(calls.count("publish"), 1)
        self.assertEqual(calls.count("install"), 2 if interrupt else 1)
        final = json.loads((repo / self.p.settings).read_text())
        artifacts.validate(final)
        self.assertEqual(final["images"]["appver"], "sha256:" + "e" * 64)
        self.assertTrue(json.loads((self.directory / "acceptance.json").read_text())["cleanup_complete"])


if __name__ == "__main__":
    unittest.main()
