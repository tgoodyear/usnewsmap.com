"""Offline fail-closed preparation tests; never invoke host account/service tools."""
from datetime import datetime, timedelta, timezone
from contextlib import contextmanager, ExitStack
import hashlib
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from boinc.scripts import prepare_node as node


class PrepareNodeTests(unittest.TestCase):
    @contextmanager
    def host(self):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            root = Path(directory)
            for name, path in {"DATA": root / "home", "INSTALL": root / "client",
                               "UNIT_PATH": root / "canary.service",
                               "DROPIN_PATH": root / "canary.service.d",
                               "CGROUP_CONTROLLERS": root / "cgroup.controllers"}.items():
                stack.enter_context(patch.object(node, name, path))
            node.CGROUP_CONTROLLERS.touch()
            tables = (root / "subuid", root / "subgid")
            for table in tables:
                table.write_text("eng:655360:65536\n")
            stack.enter_context(patch.object(node, "SUBID_PATHS", tables))
            stack.enter_context(patch.object(node.os, "uname", return_value=SimpleNamespace(machine="x86_64")))
            stack.enter_context(patch.object(node.os, "geteuid", return_value=0))
            stack.enter_context(patch.object(node.shutil, "which", return_value="/mock/tool"))
            account = SimpleNamespace(pw_name="boinc", pw_uid=1234, pw_gid=1234, pw_dir=str(node.DATA))
            state = {"created": False, "fail": None, "service": {
                "LoadState": "not-found", "ActiveState": "inactive", "FragmentPath": "",
                "SourcePath": "", "UnitFileState": "", "DropInPaths": ""}}

            def lookup(name):
                if not state["created"]:
                    raise KeyError(name)
                return account

            def command(*args, **kwargs):
                if args[:1] == (state["fail"],):
                    raise RuntimeError("simulated host command failure")
                if args[:1] == ("useradd",):
                    state["created"] = True
                    node.DATA.mkdir()
                if args[:3] == ("systemctl", "show", "usnm-ocr-worker.service"):
                    return "123"
                if "--property=LoadState" in args:
                    return "\n".join(f"{key}={value}" for key, value in state["service"].items())
                if "--property=FragmentPath" in args:
                    return f"FragmentPath={node.UNIT_PATH}\nUser=boinc\nGroup=boinc"
                if "--property=ActiveState" in args:
                    return "inactive"
                if "--property=UnitFileState" in args:
                    return "disabled"
                if args[-1] == "--version":
                    return "8.2.13 x86_64-pc-linux-gnu"
                return ""

            stack.enter_context(patch.object(node.pwd, "getpwnam", side_effect=lookup))
            stack.enter_context(patch.object(node.pwd, "getpwall", return_value=[account]))
            commands = stack.enter_context(patch.object(node, "run", side_effect=command))
            stack.enter_context(patch.object(node, "validate_bundle", return_value={"binary": "verified"}))
            bundle = root / "bundle/install/bin"
            bundle.mkdir(parents=True)
            for name in ("boinc", "boinccmd"):
                (bundle / name).write_text("test binary")
            baseline = {"observed_at": datetime.now(timezone.utc).isoformat(),
                        "baseline_ready": True, "worker": {"MainPID": "123"}}
            yield root, state, commands, baseline

    def test_preview_and_failed_preconditions_never_create_or_start(self):
        with self.host() as (root, state, commands, baseline):
            node.prepare(root / "bundle", baseline)
            self.assertFalse(state["created"])
            self.assertFalse(node.UNIT_PATH.exists())
            self.assertTrue(all(call.args[0] == "systemctl" for call in commands.call_args_list))
            commands.reset_mock()
            with self.assertRaises(ValueError):
                node.prepare(root / "bundle", {**baseline, "baseline_ready": False}, True)
            self.assertFalse(state["created"])
            node.DROPIN_PATH.mkdir()
            with self.assertRaisesRegex(ValueError, "Refusing existing path"):
                node.prepare(root / "bundle", baseline, True)
            self.assertFalse(state["created"])

    def test_apply_with_private_umask_installs_traversable_binaries_without_activation(self):
        with self.host() as (root, state, commands, baseline):
            old = os.umask(0o077)
            try:
                node.prepare(root / "bundle", baseline, True)
            finally:
                os.umask(old)
            self.assertTrue(state["created"])
            for directory in (node.INSTALL, node.INSTALL / "bin"):
                self.assertEqual(directory.stat().st_mode & 0o777, 0o755)
            self.assertEqual(node.DATA.stat().st_mode & 0o777, 0o700)
            self.assertTrue(node.UNIT_PATH.read_text().startswith(node.MARKER))
            calls = [call.args for call in commands.call_args_list]
            self.assertFalse(any(args[:2] in (("systemctl", "start"), ("systemctl", "enable"))
                                 for args in calls))
            version = next(call for call in commands.call_args_list if call.args[-1] == "--version")
            self.assertEqual(version.kwargs["extra_groups"], [])
            self.assertEqual(version.kwargs["user"], 1234)

    def test_vendor_runtime_or_loaded_service_collision_prevents_account_creation(self):
        for edits in ({"LoadState": "loaded", "FragmentPath": "/usr/lib/systemd/system/canary.service"},
                      {"LoadState": "loaded", "FragmentPath": "/run/systemd/system/canary.service"},
                      {"LoadState": "loaded", "FragmentPath": "/run/systemd/transient/canary.service"},
                      {"ActiveState": "active"}, {"LoadState": "masked"},
                      {"DropInPaths": "/run/systemd/system/canary.service.d/override.conf"}):
            with self.subTest(edits=edits), self.host() as (root, state, commands, baseline):
                state["service"].update(edits)
                with self.assertRaisesRegex(ValueError, "installed or loaded"):
                    node.prepare(root / "bundle", baseline, True)
                self.assertFalse(state["created"])
                self.assertTrue(all(call.args[:2] == ("systemctl", "show")
                                    for call in commands.call_args_list))

    def test_failed_apply_retains_only_inactive_prerequisites(self):
        with self.host() as (root, state, commands, baseline):
            state["fail"] = "usermod"
            with self.assertRaisesRegex(RuntimeError, "simulated"):
                node.prepare(root / "bundle", baseline, True)
            self.assertTrue(state["created"])
            self.assertFalse(node.UNIT_PATH.exists())
            self.assertFalse(any(call.args[:2] == ("systemctl", "start")
                                 for call in commands.call_args_list))

    def test_rollback_rejects_unowned_or_aliased_resources_and_attempts_all_cleanup(self):
        with self.host() as (root, state, commands, baseline):
            state["created"] = True
            node.UNIT_PATH.write_text("unowned")
            with self.assertRaises(ValueError):
                node.stop()
            commands.assert_not_called()
            node.UNIT_PATH.write_text(node.UNIT)
            alias = SimpleNamespace(pw_name="usnm", pw_uid=1234)
            with patch.object(node.pwd, "getpwall", return_value=[alias]):
                with self.assertRaises(ValueError):
                    node.stop()
            commands.assert_not_called()
            state["fail"] = "loginctl"
            with self.assertRaisesRegex(RuntimeError, "every cleanup"):
                node.stop()
            calls = [call.args for call in commands.call_args_list
                     if call.args[:2] != ("systemctl", "show")]
            self.assertEqual(calls, [
                ("systemctl", "stop", node.SERVICE),
                ("systemctl", "disable", node.SERVICE),
                ("loginctl", "disable-linger", "boinc"),
                ("systemctl", "stop", "user@1234.service"),
            ])
            self.assertFalse(any("usnm-ocr-worker.service" in args for args in calls))

    def test_subordinate_ids_are_free_in_both_tables(self):
        self.assertEqual(node.allocation("eng:655360:65536", "other:1048576:65536"), 1114112)
        for value in ("boinc:100000:65536", "eng:-1:65536", "eng:100000:0"):
            with self.assertRaises(ValueError):
                node.allocation(value)

    def test_rollback_accepts_pre_migration_installed_unit(self):
        with self.host() as (root, state, commands, baseline):
            state["created"] = True
            node.UNIT_PATH.write_text(
                "# Managed by scripts/boinc/prepare_node.py; bounded canary, disabled by default.\n"
                + node.UNIT.split("\n", 1)[1])
            node.stop()
            cleanup = [call.args for call in commands.call_args_list
                       if call.args[:2] != ("systemctl", "show")]
            self.assertEqual(cleanup, [
                ("systemctl", "stop", node.SERVICE),
                ("systemctl", "disable", node.SERVICE),
                ("loginctl", "disable-linger", "boinc"),
                ("systemctl", "stop", "user@1234.service"),
            ])

    def test_only_fresh_healthy_same_worker_baseline_passes(self):
        now = datetime.now(timezone.utc)
        baseline = {"observed_at": now.isoformat(), "baseline_ready": True,
                    "worker": {"MainPID": "123"}}
        node.validate_baseline(baseline, "123", now)
        for edits in ({"baseline_ready": False},
                      {"observed_at": (now - timedelta(seconds=121)).isoformat()},
                      {"observed_at": (now + timedelta(seconds=1)).isoformat()},
                      {"worker": {"MainPID": "124"}}):
            with self.assertRaises(ValueError):
                node.validate_baseline({**baseline, **edits}, "123", now)

    def test_source_and_both_actual_binaries_must_verify(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "install/bin").mkdir(parents=True)
            (root / "source-commit").write_text(node.COMMIT)
            lines = []
            for name in ("boinc", "boinccmd"):
                binary = root / "install/bin" / name
                binary.write_bytes(b"fixture client")
                lines.append(hashlib.sha256(binary.read_bytes()).hexdigest() + "  install/bin/" + name)
            (root / "SHA256SUMS").write_text("\n".join(lines))
            self.assertEqual(len(node.validate_bundle(root)), 2)
            (root / "install/bin/boinccmd").write_bytes(b"changed")
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                node.validate_bundle(root)
            (root / "SHA256SUMS").write_text(lines[0])
            with self.assertRaisesRegex(ValueError, "manifest"):
                node.validate_bundle(root)
            (root / "source-commit").write_text("wrong")
            with self.assertRaisesRegex(ValueError, "source"):
                node.validate_bundle(root)

    def test_unit_requires_reviewed_profile_and_never_enables_itself(self):
        self.assertIn("User=boinc\nGroup=boinc", node.UNIT)
        self.assertIn("ConditionPathExists=/etc/systemd/system/" + node.SERVICE + ".d/", node.UNIT)
        self.assertIn("--gui_rpc_port 31417", node.UNIT)
        self.assertIn("Restart=no", node.UNIT)
        self.assertNotIn("usnmeng", node.UNIT)


if __name__ == "__main__":
    unittest.main()
