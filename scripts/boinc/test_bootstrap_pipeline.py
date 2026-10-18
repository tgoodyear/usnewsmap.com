"""Offline pipeline tests. All Azure/GitHub/SSH interactions are fake."""
import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import unittest
from unittest.mock import Mock, patch
import uuid

SPEC = importlib.util.spec_from_file_location("pipeline", Path(__file__).with_name("bootstrap_pipeline.py"))
pipeline = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pipeline)


def settings():
    return {"codeSignPublicKey": "PUBLIC ONLY", "images": {
        key: ("ghcr.io/tgoodyear/usnm-boinc-worker" if key == "worker" else
              "example.azurecr.io/usnm-boinc-" + key) + "@sha256:" + "a" * 64
        for key in ("server", "bridge", "worker", "appver")}, "projectUrl": "https://boinc-dev.usnewsmap.com/"}


class PipelineTests(unittest.TestCase):
    def setUp(self):
        self.directory = pipeline.ROOT / "target" / ("pipeline-test-" + uuid.uuid4().hex)
        self.directory.mkdir(parents=True)
        self.addCleanup(shutil.rmtree, self.directory)
        self.acceptance = self.directory / "acceptance-config.json"
        self.acceptance.write_text("{}")
        self.config = {"env": "dev", "app": "usnm_ocr", "version": "1.0",
                       "platform": "x86_64-pc-linux-gnu", "plan_class": "docker",
                       "signing_host": "offline-signing.example", "acceptance_config": str(self.acceptance)}
        self.config_path = self.directory / "config.json"
        self.config_path.write_text(json.dumps(self.config))
        self.args = argparse.Namespace(state_dir=self.directory, dry_run=False,
                                       approve_retry=None, coordination_claim="https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-123")
        self.runner = Mock(return_value="")
        self.p = pipeline.Pipeline(self.args, self.config, "identity", self.runner)
        self.work = self.directory / "work"
        self.work.mkdir()
        self.file = self.work / self.p.settings
        self.file.parent.mkdir(parents=True)
        self.file.write_text(json.dumps(settings()))

    def test_configuration_rejects_missing_or_unknown_inputs(self):
        for change in ({"env": "wrong"}, {"signing_host": "-unsafe"}, {"version": "1.001"},
                       {"platform": "windows_x86_64"}, {"app": "unknown"}, {"extra": "bad"},
                       {"acceptance_config": "missing.json"}, {"plan_class": ["bad"]}):
            with self.subTest(change=change):
                self.config_path.write_text(json.dumps({**self.config, **change}))
                with self.assertRaises(pipeline.PipelineError):
                    pipeline.load(self.config_path)

    def test_identity_binds_acceptance_config_bytes(self):
        _, first = pipeline.load(self.config_path)
        self.acceptance.write_text('{"fixture":"changed"}')
        _, second = pipeline.load(self.config_path)
        self.assertNotEqual(first, second)

    def test_qwen_cpu_gpu_contracts(self):
        for plan in ("docker_mt_2b", "docker_nvidia_cuda_8b"):
            self.config_path.write_text(json.dumps({**self.config, "app": "usnm_qwen", "plan_class": plan}))
            self.assertEqual(pipeline.load(self.config_path)[0]["plan_class"], plan)

    def test_immutable_pins_reject_tags_or_wrong_repositories(self):
        for key in ("server", "bridge", "worker", "appver"):
            for value in ("", "example.azurecr.io/image:latest", "ghcr.io/evil/image@sha256:" + "a" * 64):
                images = settings()["images"]
                images[key] = value
                with self.assertRaises(pipeline.PipelineError):
                    pipeline.check_images(images, (key,))
        pipeline.check_images(settings()["images"], tuple(settings()["images"]))

    def test_state_private_and_identity_mismatch_fails(self):
        self.p.save()
        self.assertEqual(self.p.path.stat().st_mode & 0o777, 0o600)
        with self.assertRaises(pipeline.PipelineError):
            pipeline.Pipeline(self.args, self.config, "changed", self.runner)

    def test_dry_run_has_no_phases_or_state_write(self):
        self.args.dry_run = True
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase") as action:
            result = self.p.execute()
        self.assertTrue(result["dry_run"])
        action.assert_not_called()
        self.assertFalse(self.p.path.exists())

    def test_missing_acceptance_on_main_fails_before_mutation(self):
        def git(*argv, **_):
            if argv[0] == "remote":
                return "https://github.com/tgoodyear/usnewsmap.com.git"
            if argv[0] == "ls-tree":
                return "\n".join((self.p.settings, "scripts/provision-boinc.sh", "scripts/build-boinc-images.sh", "scripts/boinc-app-release.sh"))
            return ""
        with patch.object(self.p, "git", side_effect=git), patch.object(self.p, "main", return_value="main"), patch.object(self.p, "phase") as action:
            with self.assertRaisesRegex(pipeline.PipelineError, "#807/#548"):
                self.p.execute()
        action.assert_not_called()

    def test_dirty_source_fails_immediately(self):
        with patch.object(self.p, "git", return_value=" M dirty"):
            with self.assertRaisesRegex(pipeline.PipelineError, "clean source"):
                self.p.preflight()

    def test_entire_pipeline_resumes_and_never_repeats_completed_phases(self):
        actions = []
        def action(phase):
            actions.append(phase)
            if phase == "review-runtime" and actions.count(phase) == 1:
                raise pipeline.Blocked("review pending")
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase", side_effect=action):
            with self.assertRaises(pipeline.Blocked):
                self.p.execute()
            self.assertEqual(self.p.state["status"], "blocked")
            self.p.execute()
            self.p.execute()
        self.assertEqual(actions, list(pipeline.PHASES[:4]) + list(pipeline.PHASES[3:]))
        persisted = pipeline.Pipeline(self.args, self.config, "identity", self.runner)
        self.assertEqual(persisted.state["done"], list(pipeline.PHASES))

    def test_failure_requires_exact_explicit_retry(self):
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase", side_effect=pipeline.PipelineError("locked")):
            with self.assertRaises(pipeline.PipelineError):
                self.p.execute()
        for retry in (None, "sign"):
            self.args.approve_retry = retry
            with patch.object(self.p, "preflight"), patch.object(self.p, "phase") as action:
                with self.assertRaisesRegex(pipeline.PipelineError, "explicit --approve-retry"):
                    self.p.execute()
                action.assert_not_called()
        self.args.approve_retry = "base"
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase"):
            self.assertTrue(self.p.execute()["complete"])

    def test_running_intent_after_crash_requires_approval(self):
        self.p.state.update(active="publish", status="running")
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase") as action:
            with self.assertRaisesRegex(pipeline.PipelineError, "ambiguous/failed"):
                self.p.execute()
            action.assert_not_called()

    def test_base_skips_existing_resources_and_never_clears_pins(self):
        with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "script") as script:
            self.p.phase("base")
        script.assert_not_called()
        value = settings()
        value["images"]["server"] = value["images"]["bridge"] = ""
        self.file.write_text(json.dumps(value))
        with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "script") as script:
            self.p.phase("base")
        script.assert_called_once_with(self.work, "provision-boinc.sh")

    def test_all_lifecycle_commands_reject_dirty_workspace(self):
        with patch.object(self.p, "git", return_value=" M unreviewed.bicep"):
            for name in ("provision-boinc.sh", "build-boinc-images.sh", "boinc-app-release.sh"):
                with self.assertRaisesRegex(pipeline.PipelineError, "clean lifecycle workspace"):
                    self.p.script(self.work, name)
        self.runner.assert_not_called()

    def test_locked_signing_never_bypasses_prepare_only(self):
        with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "assert_reviewed"), patch.object(self.p, "git", return_value=""), patch.object(self.p, "script", side_effect=pipeline.PipelineError("locked")) as script:
            with self.assertRaisesRegex(pipeline.PipelineError, "never unlock or bypass"):
                self.p.phase("sign")
        argv = script.call_args.args
        self.assertIn("--prepare-only", argv)
        self.assertNotIn("--acceptance-config", argv)
        self.assertFalse(any("private" in str(x) or "unlock" in str(x) for x in argv))

    def test_worker_dispatch_is_not_repeated_on_resume(self):
        with patch.object(self.p, "gh", side_effect=["[]", "", "[]"]) as gh:
            for _ in range(2):
                with self.assertRaises(pipeline.Blocked):
                    self.p.worker(self.work)
        self.assertEqual(sum(c.args[:2] == ("workflow", "run") for c in gh.call_args_list), 1)
        self.assertTrue(self.p.state["worker_dispatched"])

    def test_worker_running_failed_ambiguous_and_success(self):
        title = "boinc-worker (bootstrap-" + self.p.state["id"] + ")"
        build = {"databaseId": 123, "displayTitle": title, "status": "completed", "conclusion": "success", "headSha": "a" * 40}
        for changes, error in (({"status": "in_progress"}, pipeline.Blocked), ({"conclusion": "failure"}, pipeline.PipelineError)):
            self.p.state.pop("worker_run", None)
            with patch.object(self.p, "gh", return_value=json.dumps([{**build, **changes}])):
                with self.assertRaises(error):
                    self.p.worker(self.work)
        self.p.state.pop("worker_run", None)
        with patch.object(self.p, "gh", return_value=json.dumps([build, build])):
            with self.assertRaisesRegex(pipeline.PipelineError, "ambiguous"):
                self.p.worker(self.work)
        image = settings()["images"]["worker"]
        with patch.object(self.p, "gh", return_value=json.dumps([build])), patch.object(pipeline, "public_worker", return_value=image) as public:
            self.p.worker(self.work)
        public.assert_called_once_with("a" * 40)
        self.assertEqual(self.p.state["worker_build"], 123)

    def test_worker_resume_uses_persisted_run_id_not_truncated_listing(self):
        self.p.state["worker_run"] = 123
        build = {"databaseId": 123, "displayTitle": "boinc-worker (bootstrap-" + self.p.state["id"] + ")",
                 "status": "in_progress", "conclusion": "", "headSha": "a" * 40}
        with patch.object(self.p, "gh", return_value=json.dumps(build)) as gh:
            with self.assertRaises(pipeline.Blocked):
                self.p.worker(self.work)
        self.assertEqual(gh.call_args.args[:3], ("run", "view", "123"))

    def test_review_rejects_non_artifact_configuration_changes(self):
        baseline = settings()
        candidate = settings()
        candidate["projectUrl"] = "https://unexpected/"
        self.file.write_text(json.dumps(candidate))
        with patch.object(self.p, "git", return_value=json.dumps(baseline)):
            with self.assertRaisesRegex(pipeline.PipelineError, "outside allowed artifact"):
                self.p.review("runtime", self.work, ("server", "bridge", "worker"))

    def test_review_refuses_other_dirty_files(self):
        def git(*argv, **_):
            return json.dumps(settings()) if argv[0] == "show" else " M other-file"
        with patch.object(self.p, "git", side_effect=git):
            with self.assertRaisesRegex(pipeline.PipelineError, "allowlist"):
                self.p.review("runtime", self.work, ("server", "bridge", "worker"))

    def review_git(self, *argv, **_):
        if argv[0] == "show":
            return json.dumps(settings())
        if argv[0] == "branch":
            return self.p.state["reviews"]["runtime"]["branch"]
        return ""

    def test_review_opens_pr_then_blocks_without_auto_merge(self):
        with patch.object(self.p, "git", side_effect=self.review_git) as git, patch.object(self.p, "gh", side_effect=["[]", "created"]) as gh:
            with self.assertRaisesRegex(pipeline.Blocked, "PR created"):
                self.p.review("runtime", self.work, ("server", "bridge", "worker"))
        self.assertTrue(any(c.args[0] == "push" for c in git.call_args_list))
        self.assertEqual(gh.call_args.args[:2], ("pr", "create"))
        self.assertFalse(any("merge" in c.args for c in gh.call_args_list))

    def test_merged_review_verifies_pins_without_recreating_deleted_branch(self):
        with patch.object(self.p, "git", side_effect=self.review_git) as git, patch.object(self.p, "main", return_value="main"), patch.object(self.p, "gh", side_effect=['[{"number": 99, "state": "MERGED"}]', json.dumps({"baseRefName": "main", "reviewDecision": "APPROVED", "statusCheckRollup": [{"conclusion": "SUCCESS"}]})]):
            self.p.review("runtime", self.work, ("server", "bridge", "worker"))
        self.assertEqual(self.p.state["reviews"]["runtime"]["pr"], 99)
        self.assertFalse(any(c.args[0] == "push" for c in git.call_args_list))

    def test_merged_without_approval_or_green_ci_never_promotes(self):
        for changes in ({"reviewDecision": "REVIEW_REQUIRED"}, {"baseRefName": "dev"},
                        {"statusCheckRollup": []}, {"statusCheckRollup": [{"conclusion": "FAILURE"}]},
                        {"statusCheckRollup": [{"status": "IN_PROGRESS"}]}):
            approval = {"baseRefName": "main", "reviewDecision": "APPROVED", "statusCheckRollup": [{"conclusion": "SUCCESS"}], **changes}
            with patch.object(self.p, "git", side_effect=self.review_git), patch.object(self.p, "gh", side_effect=['[{"number": 99, "state": "MERGED"}]', json.dumps(approval)]):
                with self.assertRaisesRegex(pipeline.Blocked, "approval and successful CI"):
                    self.p.review("runtime", self.work, ("server", "bridge", "worker"))

    def test_unmerged_review_blocks(self):
        for state in ("OPEN", "CLOSED"):
            with patch.object(self.p, "git", side_effect=self.review_git), patch.object(self.p, "gh", return_value=json.dumps([{"number": 99, "state": state}])):
                with self.assertRaises(pipeline.Blocked):
                    self.p.review("runtime", self.work, ("server", "bridge", "worker"))

    def test_stale_review_cannot_publish(self):
        self.p.state["reviews"]["app"] = {"settings": settings()}
        changed = settings()
        changed["images"]["appver"] = changed["images"]["appver"].replace("a", "b")
        with patch.object(self.p, "main", return_value="main"), patch.object(self.p, "git", return_value=json.dumps(changed)), patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "script") as script:
            with self.assertRaisesRegex(pipeline.PipelineError, "refusing stale"):
                self.p.phase("publish")
        script.assert_not_called()

    def test_acceptance_interface_and_evidence(self):
        evidence = {"accepted": True, "cleanup_complete": True, "release": {
            "app": "usnm_ocr", "version_num": 100, "platform": self.config["platform"], "plan_class": "docker"}}
        (self.directory / "acceptance.json").write_text(json.dumps(evidence))
        self.p.acceptance(self.work)
        argv = self.runner.call_args.args[0]
        self.assertEqual(argv[1], str(self.work / "scripts/boinc/acceptance.py"))
        self.assertIn("--coordination-claim", argv)
        self.assertNotIn("--signing-host", argv)
        for key in ("accepted", "cleanup_complete", "release"):
            bad = {**evidence, key: False}
            (self.directory / "acceptance.json").write_text(json.dumps(bad))
            with self.assertRaisesRegex(pipeline.PipelineError, "evidence failed"):
                self.p.acceptance(self.work)

    def test_end_to_end_real_git_with_fake_provision_sign_publish_and_acceptance(self):
        repo = self.directory / "fixture-repo"
        repo.mkdir()
        def git(*argv, cwd=repo):
            if argv[0] == "push":
                return ""  # Never contact an actual remote from the integration fixture.
            return pipeline.run(["git", *argv], cwd=cwd)
        git("init", "-b", "main")
        git("config", "user.email", "fixture@example.invalid")
        git("config", "user.name", "Offline Fixture")
        initial = repo / self.p.settings
        initial.parent.mkdir(parents=True)
        initial.write_text(json.dumps(settings()))
        scripts = repo / "scripts"
        (scripts / "boinc").mkdir(parents=True)
        fake = """#!/usr/bin/env python3
import json
from pathlib import Path
import sys
settings = Path('contrib-compute/infra/boinc/env/dev.json')
value = json.loads(settings.read_text())
name = Path(sys.argv[0]).name
keys = ['server', 'bridge'] if name == 'build-boinc-images.sh' else ['appver'] if name == 'boinc-app-release.sh' else []
if name == 'boinc-app-release.sh':
    assert '--prepare-only' in sys.argv
for key in keys:
    value['images'][key] = 'example.azurecr.io/usnm-boinc-' + key + '@sha256:' + 'b' * 64
if keys:
    settings.write_text(json.dumps(value))
"""
        for name in ("provision-boinc.sh", "build-boinc-images.sh", "boinc-app-release.sh"):
            script = scripts / name
            script.write_text(fake)
            script.chmod(0o755)
        (scripts / "boinc/acceptance.py").write_text("""import json
from pathlib import Path
import sys
args = sys.argv
get = lambda name: args[args.index('--' + name) + 1]
if '--dry-run' not in args:
    major, minor = get('version').split('.')
    release = dict(app=get('app'), version_num=int(major)*100+int(minor), platform=get('platform'), plan_class=get('plan-class'))
    Path(get('evidence')).write_text(json.dumps(dict(accepted=True, cleanup_complete=True, release=release)))
""")
        git("add", ".")
        git("commit", "-m", "offline fixture")
        self.p.run = pipeline.run
        dispatched = False
        created = {}
        def gh(*argv):
            nonlocal dispatched
            if argv[:2] == ("run", "list"):
                return json.dumps([dict(databaseId=123, displayTitle="boinc-worker (bootstrap-" + self.p.state["id"] + ")", headSha=git("rev-parse", "main"), status="completed", conclusion="success")] if dispatched else [])
            if argv[:2] == ("workflow", "run"):
                dispatched = True
                return ""
            if argv[:2] == ("pr", "list"):
                branch = argv[argv.index("--head") + 1]
                return json.dumps([dict(number=created[branch], state="MERGED")] if branch in created else [])
            if argv[:2] == ("pr", "create"):
                branch = argv[argv.index("--head") + 1]
                created[branch] = len(created) + 1
                git("merge", "--ff-only", branch)
                return "fake PR created and independently approved/merged"
            if argv[:2] == ("pr", "view"):
                return json.dumps(dict(baseRefName="main", reviewDecision="APPROVED", statusCheckRollup=[dict(conclusion="SUCCESS")]))
            self.fail("unexpected external command: " + str(argv))
        with patch.object(self.p, "git", side_effect=git), patch.object(self.p, "main", side_effect=lambda: git("rev-parse", "main")), patch.object(self.p, "preflight"), patch.object(self.p, "gh", side_effect=gh), patch.object(pipeline, "public_worker", return_value="ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "c" * 64):
            for _ in range(8):
                try:
                    self.p.execute()
                    break
                except pipeline.Blocked:
                    continue
            else:
                self.fail("offline pipeline never completed")
        self.assertEqual(self.p.state["done"], list(pipeline.PHASES))
        self.assertEqual(len(created), 2)
        self.assertTrue(json.loads((self.directory / "acceptance.json").read_text())["accepted"])

    def test_subprocess_preserves_git_porcelain_leading_columns(self):
        process = Mock(returncode=0)
        process.communicate.return_value = (" M settings.json\n", "")
        with patch.object(pipeline.subprocess, "Popen", return_value=process):
            self.assertEqual(pipeline.run(["git", "status"]), " M settings.json")

    def test_subprocess_failure_suppresses_secret_output(self):
        process = Mock(returncode=1)
        process.communicate.return_value = ("SECRET STDOUT", "SECRET STDERR")
        with patch.object(pipeline.subprocess, "Popen", return_value=process):
            with self.assertRaises(pipeline.PipelineError) as error:
                pipeline.run(["fake"])
        self.assertNotIn("SECRET", str(error.exception))

    def test_timeout_kills_process_group(self):
        process = Mock(pid=123)
        process.communicate.side_effect = [subprocess.TimeoutExpired("fake", 1), ("", "")]
        with patch.object(pipeline.subprocess, "Popen", return_value=process), patch.object(pipeline.os, "killpg") as kill:
            with self.assertRaises(subprocess.TimeoutExpired):
                pipeline.run(["fake"], timeout=1)
        kill.assert_called_once_with(123, pipeline.signal.SIGKILL)


if __name__ == "__main__":
    unittest.main()
