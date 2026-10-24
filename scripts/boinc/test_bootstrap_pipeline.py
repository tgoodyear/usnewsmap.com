"""Offline pipeline tests. All Azure/GitHub/SSH interactions are fake."""
import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
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
        self.p.has_open_findings = Mock(return_value=False)
        self.p.successful_attempt = Mock(side_effect=lambda build, detail, model: detail["attempt"])
        self.work = self.directory / "work"
        self.work.mkdir()
        self.file = self.work / self.p.settings
        self.file.parent.mkdir(parents=True)
        self.file.write_text(json.dumps(settings()))
        self.p.state["workspaces"].update({label: {"path": str(self.work), "commit": "reviewed-source"}
                                           for label in ("artifacts", "signing")})

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
                return "\n".join((self.p.settings, "scripts/boinc/provision-boinc.sh", "scripts/boinc/build-boinc-images.sh", "scripts/boinc/boinc-app-release.sh"))
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
        with patch.object(self.p, "preflight"), patch.object(self.p, "receipt"), patch.object(self.p, "phase", side_effect=action):
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
        with patch.object(self.p, "preflight"), patch.object(self.p, "receipt"), patch.object(self.p, "phase"):
            self.assertTrue(self.p.execute()["complete"])

    def test_running_intent_after_crash_requires_approval(self):
        self.p.state.update(active="publish", status="running")
        with patch.object(self.p, "preflight"), patch.object(self.p, "phase") as action:
            with self.assertRaisesRegex(pipeline.PipelineError, "ambiguous/failed"):
                self.p.execute()
            action.assert_not_called()

    def test_base_skips_existing_resources_and_never_clears_pins(self):
        with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "assert_main_settings"), patch.object(self.p, "script") as script:
            self.p.phase("base")
        script.assert_not_called()
        value = settings()
        value["images"]["server"] = value["images"]["bridge"] = ""
        self.file.write_text(json.dumps(value))
        with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "assert_main_settings"), patch.object(self.p, "script") as script:
            self.p.phase("base")
        script.assert_called_once_with(self.work, "provision-boinc.sh")

    def test_base_and_server_reject_clean_but_unreviewed_settings_on_resume(self):
        reviewed = settings()
        reviewed["images"]["server"] = reviewed["images"]["bridge"] = ""
        changed = json.loads(json.dumps(reviewed))
        changed["projectUrl"] = "https://unreviewed.invalid/"
        self.file.write_text(json.dumps(changed))
        for phase in ("base", "server"):
            with patch.object(self.p, "workspace", return_value=self.work), patch.object(self.p, "main", return_value="main"), patch.object(self.p, "git", return_value=json.dumps(reviewed)), patch.object(self.p, "script") as script:
                with self.assertRaisesRegex(pipeline.PipelineError, "settings differ from reviewed main"):
                    self.p.phase(phase)
                script.assert_not_called()

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
        with patch.object(self.p, "gh", return_value=json.dumps([build])), patch.object(self.p, "worker_image", return_value=image) as public:
            self.p.worker(self.work)
        public.assert_called_once_with(build)
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
        with patch.object(self.p, "git", side_effect=self.review_git) as git, patch.object(self.p, "gh", side_effect=["[]", "created", "requested"]) as gh:
            with self.assertRaisesRegex(pipeline.Blocked, "PR created"):
                self.p.review("runtime", self.work, ("server", "bridge", "worker"))
        self.assertTrue(any(c.args[0] == "push" for c in git.call_args_list))
        self.assertTrue(any(c.args[:2] == ("pr", "create") for c in gh.call_args_list))
        self.assertEqual(gh.call_args.args[:2], ("pr", "edit"))
        self.assertFalse(any("merge" in c.args for c in gh.call_args_list))

    def test_merged_review_verifies_pins_without_recreating_deleted_branch(self):
        with patch.object(self.p, "git", side_effect=self.review_git) as git, patch.object(self.p, "main", return_value="main"), patch.object(self.p, "gh", side_effect=['[{"number": 99, "state": "MERGED"}]', json.dumps({"baseRefName": "main", "reviewDecision": "APPROVED", "statusCheckRollup": [{"conclusion": "SUCCESS"}]})]):
            self.p.review("runtime", self.work, ("server", "bridge", "worker"))
        self.assertEqual(self.p.state["reviews"]["runtime"]["pr"], 99)
        self.assertFalse(any(c.args[0] == "push" for c in git.call_args_list))

    def test_review_policy_accepts_exact_head_green_blue_or_approval_not_stale_yellow(self):
        head = "a" * 40
        for body, expected in (("### 🟢 Approval recommended", True), ("### 🔵 Needs a closer look", True), ("### 🟡 Changes recommended", False)):
            value = dict(reviewDecision="", headRefOid=head, reviews=[dict(author=dict(login="copilot-pull-request-reviewer"), state="COMMENTED", commit=dict(oid=head), body=body)])
            self.assertEqual(pipeline.approved_review(value), expected)
            value["reviews"][0]["commit"]["oid"] = "b" * 40
            self.assertFalse(pipeline.approved_review(value))
        value = dict(reviewDecision="", headRefOid=head, reviews=[dict(author=dict(login="maintainer"), state="APPROVED", commit=dict(oid=head))])
        self.assertFalse(pipeline.approved_review(value))
        self.assertTrue(pipeline.approved_review(value, lambda login: login == "maintainer"))
        value["reviews"].append(dict(author=dict(login="maintainer"), state="CHANGES_REQUESTED", commit=dict(oid=head)))
        self.assertFalse(pipeline.approved_review(value))

    def test_external_approval_cannot_override_configured_review_requirement(self):
        review = dict(author=dict(login="outside-user"), state="APPROVED", commit=dict(oid="a" * 40))
        value = dict(headRefOid="a" * 40, reviewDecision="REVIEW_REQUIRED", reviews=[review])
        authority = Mock(return_value=True)
        self.assertFalse(pipeline.approved_review(value, authority))
        authority.assert_not_called()
        value["reviewDecision"] = ""
        self.assertFalse(pipeline.approved_review(value, lambda _: False))
        self.assertTrue(pipeline.approved_review(value, lambda _: True))
        for permission, expected in (("read", False), ("triage", False), ("write", True), ("maintain", True), ("admin", True)):
            self.runner.return_value = json.dumps(dict(permission=permission))
            self.assertEqual(self.p.authorized_reviewer("outside-user"), expected)

    def test_partial_rerun_uses_selected_models_last_successful_attempt(self):
        build = dict(databaseId=123)
        detail = dict(attempt=2)
        success = dict(name="qwen worker image (build, push) (2b)", status="completed", conclusion="success")
        with patch.object(self.p, "gh", side_effect=[json.dumps(dict(jobs=[dict(name="qwen worker image (build, push) (8b)", status="completed", conclusion="success")])), json.dumps(dict(jobs=[success]))]) as gh:
            self.assertEqual(pipeline.Pipeline.successful_attempt(self.p, build, detail, "qwen-2b"), 1)
        self.assertEqual([c.args[c.args.index("--attempt") + 1] for c in gh.call_args_list], ["2", "1"])
        with patch.object(self.p, "gh", return_value=json.dumps(dict(jobs=[{**success, "conclusion": "failure"}]))):
            with self.assertRaisesRegex(pipeline.PipelineError, "latest job attempt did not succeed"):
                pipeline.Pipeline.successful_attempt(self.p, build, detail, "qwen-2b")

    def test_partial_rerun_downloads_selected_models_successful_receipt_attempt(self):
        self.config.update(app="usnm_qwen", plan_class="docker_mt_2b")
        del self.p.successful_attempt
        build = dict(databaseId=123, headSha="a" * 40)
        detail = {**build, "attempt": 2, "status": "completed", "conclusion": "success"}
        directory = self.directory / "worker-receipt-123-1"
        directory.mkdir()
        image = settings()["images"]["worker"]
        (directory / "final.json").write_text(json.dumps(dict(schema=1, run_id="123", run_attempt="1", commit=build["headSha"], model="qwen-2b", image=image)))
        def jobs(model):
            return json.dumps(dict(jobs=[dict(name="qwen worker image (build, push) (" + model + ")", status="completed", conclusion="success")]))
        with patch.object(self.p, "gh", side_effect=[json.dumps(detail), jobs("8b"), jobs("2b")]), patch.object(pipeline, "public_worker", return_value=image):
            self.assertEqual(self.p.worker_image(build), image)
        self.assertEqual(self.p.state["worker_attempt"], 1)

    def test_findings_query_fails_closed_on_unresolved_or_paginated_threads(self):
        for nodes, more, expected in (([], False, False), ([{"isResolved": True}], False, False),
                                      ([{"isResolved": False}], False, True), ([], True, True)):
            self.runner.return_value = json.dumps({"data": {"repository": {"pullRequest": {"reviewThreads": {
                "nodes": nodes, "pageInfo": {"hasNextPage": more}}}}}})
            self.assertEqual(pipeline.Pipeline.has_open_findings(self.p, 99), expected)
        argv = self.runner.call_args.args[0]
        self.assertEqual(argv[:3], ["gh", "api", "graphql"])
        self.assertNotIn("-R", argv)

    def test_merged_review_with_unresolved_findings_blocks(self):
        self.p.has_open_findings.return_value = True
        with patch.object(self.p, "git", side_effect=self.review_git), patch.object(self.p, "gh", side_effect=['[{"number": 99, "state": "MERGED"}]', json.dumps({"baseRefName": "main", "reviewDecision": "APPROVED", "statusCheckRollup": [{"conclusion": "SUCCESS"}]})]):
            with self.assertRaisesRegex(pipeline.Blocked, "unresolved findings"):
                self.p.review("runtime", self.work, ("server", "bridge", "worker"))

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
        self.full_pipeline()

    def test_qwen_end_to_end_preserves_ocr_default_and_reviews_app_specific_pin(self):
        self.config.update(app="usnm_qwen", plan_class="docker_mt_2b")
        self.p = pipeline.Pipeline(self.args, self.config, "identity", self.runner)
        self.p.has_open_findings = Mock(return_value=False)
        self.full_pipeline(qwen=True)

    def full_pipeline(self, qwen=False):
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
    if sys.argv[sys.argv.index('--app') + 1] == 'usnm_qwen':
        assert sys.argv[sys.argv.index('--worker-image') + 1] == 'ghcr.io/tgoodyear/usnm-boinc-worker@sha256:' + 'c' * 64
for key in keys:
    value['images'][key] = 'example.azurecr.io/usnm-boinc-' + key + '@sha256:' + 'b' * 64
if keys:
    settings.write_text(json.dumps(value))
"""
        for name in ("provision-boinc.sh", "build-boinc-images.sh", "boinc-app-release.sh"):
            script = scripts / "boinc" / name
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
        self.p.state["workspaces"] = {}
        dispatched = False
        created = {}
        def gh(*argv):
            nonlocal dispatched
            if argv[:2] == ("run", "list"):
                workflow = "boinc-qwen" if qwen else "boinc-worker"
                return json.dumps([dict(databaseId=123, displayTitle=workflow + " (bootstrap-" + self.p.state["id"] + ")", headSha=git("rev-parse", "main"), status="completed", conclusion="success")] if dispatched else [])
            if argv[:2] == ("run", "view"):
                if "--attempt" in argv:
                    job_name = "qwen worker image (build, push) (2b)" if qwen else "worker image (publish multi-architecture index)"
                    return json.dumps(dict(jobs=[dict(name=job_name, status="completed", conclusion="success")]))
                return json.dumps(dict(databaseId=123, headSha=git("rev-parse", "main"), attempt=1, status="completed", conclusion="success"))
            if argv[:2] == ("run", "download"):
                directory = Path(argv[argv.index("--dir") + 1])
                (directory / "final.json").write_text(json.dumps(dict(schema=1, run_id="123", run_attempt="1", commit=git("rev-parse", "main"), model="qwen-2b" if qwen else "worker", image="ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "c" * 64)))
                return ""
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
            if argv[:2] == ("pr", "edit"):
                return "review requested"
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
        if qwen:
            final = json.loads((repo / self.p.settings).read_text())
            self.assertEqual(final["images"]["worker"], settings()["images"]["worker"])
            pin = json.loads((repo / self.p.worker_pin).read_text())
            self.assertEqual(pin["image"], "ghcr.io/tgoodyear/usnm-boinc-worker@sha256:" + "c" * 64)
            self.assertEqual(pin["plan_class"], "docker_mt_2b")

    def test_completed_run_revalidates_immutable_durable_evidence(self):
        value = {"accepted": True, "cleanup_complete": True, "release": {
            "app": "usnm_ocr", "version_num": 100, "platform": self.config["platform"], "plan_class": "docker"}}
        evidence = self.directory / "acceptance.json"
        evidence.write_text(json.dumps(value))
        self.p.state["acceptance_sha256"] = self.p.receipt()
        self.p.state["done"] = list(pipeline.PHASES)
        with patch.object(self.p, "preflight"):
            self.assertTrue(self.p.execute()["complete"])
            evidence.write_text(json.dumps({**value, "unexpected": "changed"}))
            with self.assertRaisesRegex(pipeline.PipelineError, "changed after completion"):
                self.p.execute()
            evidence.unlink()
            with self.assertRaisesRegex(pipeline.PipelineError, "missing or unreadable"):
                self.p.execute()

    def test_workspace_rejects_staged_unstaged_and_untracked_code(self):
        self.p.state["workspaces"]["runtime"] = {"path": str(self.work), "commit": "a" * 40}
        for status in (" M infrastructure.bicep", "M  infrastructure.bicep", "?? injected.py"):
            def git(*argv, **_):
                if argv[0] == "rev-parse":
                    return str(self.work)
                if argv[0] == "status":
                    return status
                return ""
            with patch.object(self.p, "git", side_effect=git):
                with self.assertRaisesRegex(pipeline.PipelineError, "workspace code differs"):
                    self.p.workspace("runtime")

    def test_run_bound_worker_receipt_rejects_wrong_model_attempt_and_source(self):
        build = dict(databaseId=123, headSha="a" * 40)
        detail = {**build, "attempt": 2, "status": "completed", "conclusion": "success"}
        directory = self.directory / "worker-receipt-123-2"
        directory.mkdir()
        image = settings()["images"]["worker"]
        receipt = dict(schema=1, run_id="123", run_attempt="2", commit=build["headSha"], model="worker", image=image)
        for changes in ({"model": "qwen-2b"}, {"run_id": "456"}, {"run_attempt": "1"}, {"commit": "b" * 40}, {"image": image.replace("@sha256:", ":mutable-")}):
            (directory / "final.json").write_text(json.dumps({**receipt, **changes}))
            with patch.object(self.p, "gh", return_value=json.dumps(detail)), patch.object(pipeline, "public_worker") as public:
                with self.assertRaises(pipeline.PipelineError):
                    self.p.worker_image(build)
                public.assert_not_called()
        (directory / "final.json").write_text(json.dumps(receipt))
        with patch.object(self.p, "gh", return_value=json.dumps(detail)), patch.object(pipeline, "public_worker", return_value=image) as public:
            self.assertEqual(self.p.worker_image(build), image)
        public.assert_called_once_with("sha256:" + "a" * 64)
        self.assertEqual(self.p.state["worker_attempt"], 2)

    def test_old_workflow_without_final_receipt_fails_closed(self):
        build = dict(databaseId=123, headSha="a" * 40)
        detail = {**build, "attempt": 1, "status": "completed", "conclusion": "success"}
        with patch.object(self.p, "gh", side_effect=[json.dumps(detail), pipeline.PipelineError("artifact missing")]), patch.object(pipeline, "public_worker") as public:
            with self.assertRaisesRegex(pipeline.PipelineError, "artifact missing"):
                self.p.worker_image(build)
        public.assert_not_called()

    def test_workflow_receipts_are_after_validation_and_attempt_scoped(self):
        for name in ("boinc-worker.yml", "boinc-qwen.yml"):
            text = (pipeline.ROOT / ".github/workflows" / name).read_text()
            self.assertIn("github.run_attempt", text)
            self.assertIn("run_id:$run,run_attempt:$attempt,commit:$commit", text)
            self.assertIn("if-no-files-found: error", text)
            if name == "boinc-qwen.yml":
                self.assertLess(text.index("qwen_disk.py --image"), text.index("> release/final.json"))

    def test_forced_kill_only_after_cleanup_budget_expires(self):
        process = Mock(pid=123)
        process.communicate.side_effect = [subprocess.TimeoutExpired("fake", 1), subprocess.TimeoutExpired("fake", 2), ("", "")]
        with patch.object(pipeline.subprocess, "Popen", return_value=process), patch.object(pipeline.os, "killpg") as kill:
            with self.assertRaises(subprocess.TimeoutExpired):
                pipeline.run(["fake"], timeout=1, cleanup_timeout=2)
        self.assertEqual([call.args[1] for call in kill.call_args_list], [signal.SIGTERM, signal.SIGKILL])
        self.assertEqual([call.kwargs["timeout"] for call in process.communicate.call_args_list], [1, 2, 1])

    def test_interruption_during_execution_and_repeated_signal_during_teardown(self):
        child = self.directory / "fake_acceptance.py"
        child.write_text("""from pathlib import Path
import signal
import sys
import time
def finish(*_):
    Path('tearing-down').write_text('yes')
    time.sleep(.4)
    Path('cleaned').write_text('yes')
    sys.exit(0)
signal.signal(signal.SIGTERM, finish)
Path('ready').write_text('yes')
while True:
    time.sleep(.01)
""")
        wrapper = self.directory / "wrapper.py"
        wrapper.write_text(f"""import importlib.util
import signal
import sys
spec = importlib.util.spec_from_file_location('pipeline', {str(Path(pipeline.__file__))!r})
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)
def interrupted(*_):
    raise p.PipelineError('interrupted')
signal.signal(signal.SIGTERM, interrupted)
try:
    p.run([sys.executable, {str(child)!r}], cwd={str(self.directory)!r}, cleanup_timeout=2)
except p.PipelineError:
    sys.exit(0)
""")
        process = subprocess.Popen([sys.executable, str(wrapper)], cwd=self.directory,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            def wait_file(name):
                deadline = time.monotonic() + 5
                while not (self.directory / name).exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue((self.directory / name).exists(), name)
            wait_file("ready")
            process.send_signal(signal.SIGTERM)
            wait_file("tearing-down")
            process.send_signal(signal.SIGTERM)
            process.communicate(timeout=5)
            self.assertEqual(process.returncode, 0)
            self.assertTrue((self.directory / "cleaned").exists())
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate(timeout=3)

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

    def test_timeout_allows_graceful_process_group_cleanup(self):
        process = Mock(pid=123)
        process.communicate.side_effect = [subprocess.TimeoutExpired("fake", 1), ("", "")]
        with patch.object(pipeline.subprocess, "Popen", return_value=process), patch.object(pipeline.os, "killpg") as kill:
            with self.assertRaises(subprocess.TimeoutExpired):
                pipeline.run(["fake"], timeout=1)
        kill.assert_called_once_with(123, pipeline.signal.SIGTERM)
        self.assertEqual(process.communicate.call_args.kwargs["timeout"], 30)


if __name__ == "__main__":
    unittest.main()
