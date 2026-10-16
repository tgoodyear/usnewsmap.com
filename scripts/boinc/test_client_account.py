"""Offline tests: never contact a client or send real account credentials."""
from contextlib import nullcontext, redirect_stdout
from copy import deepcopy
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

from scripts.boinc import client_account as account


def state(**changes):
    return {"attached": True, "user_id": 42, "host_id": 7,
            "expected_name_verified": True, "expected_user_verified": True,
            "pending_results": 0, "scheduler_rpc_in_progress": False,
            "no_new_tasks": False, "account_manager": False, **changes}


class ClientAccountTests(unittest.TestCase):
    def setUp(self):
        self.real_guard = account.network_guard
        self.real_protection = account.protect_preferences
        self.real_preflight = account.attach_preflight
        protection = patch.object(account, "protect_preferences")
        guard = patch.object(account, "network_guard", return_value=nullcontext())
        preflight = patch.object(account, "attach_preflight")
        protection.start()
        guard.start()
        preflight.start()
        self.addCleanup(protection.stop)
        self.addCleanup(guard.stop)
        self.addCleanup(preflight.stop)

    def test_gui_suspension_is_refused_before_any_client_mutation(self):
        rpc = Mock()
        with self.assertRaisesRegex(ValueError, "GUI-suspended"):
            self.real_preflight(rpc, state(suspended_via_gui=True))
        rpc.call.assert_not_called()

    def test_concurrent_apply_is_locked_per_client_not_per_journal(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with account.client_lock(root, True):
                inode = (root / ".usnm-account-rejoin.lock").stat().st_ino
                result = subprocess.run(
                    [sys.executable, account.__file__, "--data-dir", str(root),
                     "--expected-user-id", "42", "--expected-user-name", "example",
                     "--state-file", str(root / "different-state.json"),
                     "--key-file", str(root / "absent-key"), "--apply"],
                    capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Another account operation owns this client", result.stderr)
                self.assertFalse((root / "different-state.json").exists())
            with account.client_lock(root, True):
                self.assertEqual((root / ".usnm-account-rejoin.lock").stat().st_ino, inode)
            self.assertEqual((root / ".usnm-account-rejoin.lock").stat().st_mode & 0o777, 0o600)

    def test_attach_disabled_client_is_refused_before_any_project_mutation(self):
        rpc = Mock()
        rpc.call.return_value = ET.fromstring("<reply><disallow_attach/></reply>")
        with self.assertRaisesRegex(ValueError, "disallows attachment"):
            self.real_preflight(rpc, state())
        self.assertEqual([call.args[0] for call in rpc.call.call_args_list], ["get_cc_status"])

    def test_only_private_weak_key_for_expected_account(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "key"
            path.write_text("42_" + "a" * 32)
            path.chmod(0o600)
            self.assertTrue(account.weak_key(path, 42).startswith("42_"))
            with self.assertRaises(ValueError):
                account.weak_key(path, 43)
            path.chmod(0o640)
            with self.assertRaises(ValueError):
                account.weak_key(path, 42)
            path.chmod(0o600)
            path.write_text("a" * 32)
            with self.assertRaises(ValueError):
                account.weak_key(path, 42)
            link = path.with_name("link")
            link.symlink_to(path)
            with self.assertRaises(OSError):
                account.weak_key(link, 42)

    def test_pending_or_inflight_work_is_never_detached(self):
        for changed in ({"pending_results": 1}, {"scheduler_rpc_in_progress": True}):
            with self.subTest(changed=changed), patch.object(account, "snapshot", return_value=state(**changed)):
                rpc = Mock()
                result = account.rejoin(rpc, "synthetic", 42, "example")
                self.assertIn("draining", result["status"])
                self.assertEqual([call.args[0] for call in rpc.call.call_args_list], ["project_nomorework"])

    def test_late_result_before_detach_is_preserved(self):
        snapshots = [state(), state(), state(pending_results=1)]
        with patch.object(account, "snapshot", side_effect=snapshots), patch.object(account.time, "sleep"):
            rpc = Mock()
            result = account.rejoin(rpc, "synthetic", 42, "example")
            self.assertIn("draining", result["status"])
            self.assertNotIn("project_detach", [call.args[0] for call in rpc.call.call_args_list])

    def test_account_manager_not_modified(self):
        with patch.object(account, "snapshot", return_value=state(account_manager=True)):
            rpc = Mock()
            with self.assertRaises(ValueError):
                account.rejoin(rpc, "synthetic", 42, "example")
            rpc.call.assert_not_called()

    def test_drained_rejoin_uses_key_only_in_protocol(self):
        snapshots = [state()] * 3 + [state(attached=False)] + [state()]
        with patch.object(account, "snapshot", side_effect=snapshots), patch.object(account.time, "sleep"):
            rpc = Mock()
            result = account.rejoin(rpc, "synthetic", 42, "example")
            operations = [call.args[0] for call in rpc.call.call_args_list]
            self.assertEqual(operations, ["project_nomorework", "project_detach", "project_attach"])
            self.assertNotIn("synthetic", str(result))
            self.assertEqual(result["status"], "scheduler attribution verified")
            self.assertFalse(result["no_new_tasks"])

    def test_user_held_project_is_refused_before_any_mutation(self):
        with patch.object(account, "snapshot", return_value=state(no_new_tasks=True)):
            rpc = Mock()
            with self.assertRaisesRegex(ValueError, "User-held"):
                account.rejoin(rpc, "synthetic", 42, "example")
            rpc.call.assert_not_called()

    def test_drain_journal_preserves_original_preference_across_two_invocations(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = Path(directory) / "state.json"
            original = account.migration_state(journal, state(), 42, "a" * 64)
            self.assertFalse(original["original_no_new_tasks"])
            with patch.object(account, "snapshot", return_value=state(pending_results=1)):
                self.assertIn("draining", account.rejoin(Mock(), "synthetic", 42, "example",
                              original_no_work=original["original_no_new_tasks"])["status"])
            reloaded = account.migration_state(journal, state(no_new_tasks=True), 42, "a" * 64)
            self.assertFalse(reloaded["original_no_new_tasks"])
            snapshots = [state(no_new_tasks=True)] * 3 + [state(attached=False), state()]
            with patch.object(account, "snapshot", side_effect=snapshots):
                result = account.rejoin(Mock(), "synthetic", 42, "example",
                                        original_no_work=reloaded["original_no_new_tasks"])
            self.assertFalse(result["no_new_tasks"])
            self.assertEqual(journal.stat().st_mode & 0o777, 0o600)
            with self.assertRaises(ValueError):
                account.migration_state(journal, state(), 43, "a" * 64)
            with self.assertRaisesRegex(ValueError, "another client"):
                account.migration_state(journal, state(no_new_tasks=True), 42, "b" * 64)

    def test_network_guard_restores_prior_mode_after_exception(self):
        rpc = Mock()
        rpc.call.side_effect = [
            ET.fromstring("<reply><network_mode>2</network_mode><network_mode_perm>2</network_mode_perm>"
                          "<network_mode_delay>0</network_mode_delay></reply>"),
            ET.fromstring("<reply><success/></reply>"),
            ET.fromstring("<reply><network_mode>3</network_mode>"
                          "<network_suspend_reason>16</network_suspend_reason></reply>"),
            ET.fromstring("<reply><success/></reply>")]
        with self.assertRaisesRegex(RuntimeError, "injected"):
            with self.real_guard(rpc):
                raise RuntimeError("injected")
        self.assertEqual(rpc.call.call_args_list[1].args,
                         ("set_network_mode", {"never": None, "duration": 600}))
        self.assertEqual(rpc.call.call_args_list[-1].args,
                         ("set_network_mode", {"auto": None, "duration": 0}))

    def test_network_pause_acknowledgement_failure_still_restores_policy(self):
        rpc = Mock()
        rpc.call.side_effect = [
            ET.fromstring("<reply><network_mode>2</network_mode><network_mode_perm>2</network_mode_perm>"
                          "<network_mode_delay>0</network_mode_delay></reply>"),
            TimeoutError("injected lost pause acknowledgement"),
            ET.fromstring("<reply><success/></reply>")]
        with self.assertRaises(TimeoutError):
            with self.real_guard(rpc):
                self.fail("Failed pause must not reach detach")
        self.assertEqual(rpc.call.call_args_list[-1].args,
                         ("set_network_mode", {"auto": None, "duration": 0}))

    def test_prior_never_policy_restored_after_final_network_enabling_attach_rpc(self):
        rpc = Mock()
        rpc.call.return_value = ET.fromstring(
            "<reply><network_mode>3</network_mode><network_mode_perm>3</network_mode_perm>"
            "<network_mode_delay>0</network_mode_delay></reply>")
        with self.assertRaisesRegex(RuntimeError, "injected"):
            with account.network_policy(rpc):
                rpc.call("project_attach", {"authenticator": "synthetic"})
                rpc.call("project_update")
                raise RuntimeError("injected")
        self.assertEqual(rpc.call.call_args_list[-1].args,
                         ("set_network_mode", {"never": None, "duration": 0}))

    def test_resource_preferences_are_protected_and_original_override_backed_up(self):
        working = ET.fromstring("<reply><global_preferences><max_ncpus_pct>25</max_ncpus_pct>"
                                "<cpu_usage_limit>30</cpu_usage_limit><mod_time>123</mod_time>"
                                "<ram_max_used_busy_pct>50</ram_max_used_busy_pct>"
                                "<ram_max_used_idle_pct>50</ram_max_used_idle_pct>"
                                "<run_gpu_if_user_active>0</run_gpu_if_user_active>"
                                "<run_if_user_active>0</run_if_user_active>"
                                "<work_buf_min_days>0</work_buf_min_days>"
                                "<work_buf_additional_days>0</work_buf_additional_days>"
                                "<start_hour>9</start_hour><end_hour>17</end_hour>"
                                "<net_start_hour>20</net_start_hour><net_end_hour>22</net_end_hour>"
                                "</global_preferences></reply>")
        protected = deepcopy(working)
        account.materialize_schedules(protected.find("global_preferences"))
        for day in protected.findall(".//day_prefs"):
            for element in day:
                if element.tag != "day_of_week":
                    element.text = format(float(element.text), ".2f")
        rpc = Mock()
        rpc.call.side_effect = [working, ET.fromstring("<reply/>"), ET.fromstring("<reply/>"),
                                protected, protected]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            original = "<global_preferences><cpu_usage_limit>30</cpu_usage_limit></global_preferences>"
            (root / "global_prefs_override.xml").write_text(original)
            self.real_protection(rpc, root, root)
            backup = next(root.glob("global-prefs-before-rejoin-*.json"))
            self.assertEqual(json.loads(backup.read_text())["original_override"], original)
            self.assertEqual(backup.stat().st_mode & 0o777, 0o600)
            payload = rpc.call.call_args_list[1].args[1]["global_preferences"]
            self.assertEqual(payload.findtext("max_ncpus_pct"), "25")
            self.assertIsNone(payload.find("mod_time"))
            self.assertEqual(len(payload.findall("day_prefs")), 7)

    def test_inherited_weekly_schedules_override_new_scheduler_days(self):
        preferences = ET.fromstring(
            "<global_preferences><start_hour>9</start_hour><end_hour>17</end_hour>"
            "<net_start_hour>20</net_start_hour><net_end_hour>22</net_end_hour>"
            "<day_prefs><day_of_week>4</day_of_week><start_hour>23</start_hour>"
            "<end_hour>5</end_hour></day_prefs></global_preferences>")
        account.materialize_schedules(preferences)
        defaults, days = account.schedule_intervals(preferences, require_explicit=True)
        self.assertEqual(defaults, ((9, 17), (20, 22)))
        self.assertEqual(days[4], ((23, 5), (20, 22)))
        # Pinned parse_override applies only supplied nonempty weekly spans,
        # otherwise the server's new day survives. Every day must be explicit.
        server_days = {0: ((0, 24), (0, 24))}
        for element in preferences.findall("day_prefs"):
            cpu = tuple(float(element.findtext(name)) for name in ("start_hour", "end_hour"))
            network = tuple(float(element.findtext(name)) for name in ("net_start_hour", "net_end_hour"))
            self.assertNotEqual(cpu[0], cpu[1])
            self.assertNotEqual(network[0], network[1])
            server_days[int(element.findtext("day_of_week"))] = (cpu, network)
        self.assertEqual(server_days[0], ((9, 17), (20, 22)))
        self.assertEqual(set(server_days), set(range(7)))

    def test_unrestricted_and_never_schedules_keep_stock_semantics(self):
        preferences = ET.fromstring(
            "<global_preferences><start_hour>9.123456</start_hour><end_hour>9.123456</end_hour>"
            "<net_start_hour>24</net_start_hour><net_end_hour>0</net_end_hour></global_preferences>")
        account.materialize_schedules(preferences)
        _, days = account.schedule_intervals(preferences, require_explicit=True)
        self.assertEqual(days, (((0, 24), (24, 0)),) * 7)
        preferences.remove(preferences.find("day_prefs"))
        with self.assertRaisesRegex(ValueError, "every day"):
            account.schedule_intervals(preferences, require_explicit=True)

    def test_unrepresentable_weekly_precision_fails_closed(self):
        preferences = ET.fromstring(
            "<global_preferences><start_hour>9.016667</start_hour><end_hour>17</end_hour>"
            "<net_start_hour>0</net_start_hour><net_end_hour>24</net_end_hour></global_preferences>")
        with self.assertRaisesRegex(ValueError, "two-decimal"):
            account.materialize_schedules(preferences)

    def test_same_weak_key_can_be_detected_without_resetting_client(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "account_example.xml"
            path.write_text(f"<account><master_url>{account.MASTER}</master_url>"
                            "<authenticator>synthetic</authenticator></account>")
            self.assertTrue(account.already_using_key(Path(directory), "synthetic"))
            self.assertFalse(account.already_using_key(Path(directory), "different"))

    def test_unexpected_account_fails_closed(self):
        snapshots = [state(attached=False), state(user_id=43, expected_user_verified=False)]
        with patch.object(account, "snapshot", side_effect=snapshots), patch.object(account.time, "sleep"):
            rpc = Mock()
            with self.assertRaisesRegex(RuntimeError, "Unexpected scheduler account"):
                account.rejoin(rpc, "synthetic", 42, "example")
            self.assertEqual(rpc.call.call_args.args[0], "project_nomorework")
            self.assertNotIn("project_update", [call.args[0] for call in rpc.call.call_args_list])

    def test_snapshot_handles_presence_booleans_and_counts_all_project_results(self):
        rpc = Mock()
        rpc.call.side_effect = [
            ET.fromstring(f"""<reply><project><master_url>{account.MASTER}</master_url>
                <userid>42</userid><hostid>7</hostid><user_name>example</user_name>
                <scheduler_rpc_in_progress/><dont_request_more_work/></project></reply>"""),
            ET.fromstring(f"""<reply><results><result><project_url>{account.MASTER}</project_url></result>
                <result><project_url>https://other.example/</project_url></result></results></reply>""")]
        result = account.snapshot(rpc, 42, "example")
        self.assertEqual(result["pending_results"], 1)
        self.assertTrue(result["scheduler_rpc_in_progress"])
        self.assertTrue(result["no_new_tasks"])
        self.assertTrue(result["expected_name_verified"])
        self.assertNotIn("example", str(result))

    def test_duplicate_alias_refused_without_mutation(self):
        rpc = Mock()
        rpc.call.return_value = ET.fromstring(
            f"<reply><project><master_url>{account.MASTER}usnmocr/</master_url></project></reply>")
        with self.assertRaises(ValueError):
            account.snapshot(rpc, 42, "example")
        self.assertEqual(rpc.call.call_count, 1)

    def test_credential_bearing_debug_logging_is_rejected(self):
        rpc = Mock()
        runtime = ET.fromstring("<reply><msgs><msg><seqno>1</seqno><project/>"
                               "<body>log flags: file_xfer, sched_ops, task</body></msg></msgs></reply>")
        rpc.call.side_effect = [ET.fromstring("<reply><log_flags/></reply>"), runtime]
        account.check_private_logging(rpc)
        for name in ("gui_rpc_debug", "http_debug", "http_xfer_debug"):
            rpc.call.side_effect = [ET.fromstring(f"<reply><log_flags><{name}/></log_flags></reply>")]
            with self.assertRaises(ValueError):
                account.check_private_logging(rpc)
            unsafe = ET.fromstring(f"<reply><msgs><msg><project/><body>log flags: {name}</body>"
                                    "<seqno>1</seqno></msg></msgs></reply>")
            rpc.call.side_effect = [ET.fromstring("<reply/>"), unsafe]
            with self.assertRaises(ValueError):
                account.check_private_logging(rpc)
        rpc.call.side_effect = [ET.fromstring("<reply/>"), runtime]
        account.check_private_logging(rpc)
        rpc.call.side_effect = [ET.fromstring("<reply><cc_config><options/></cc_config></reply>"), runtime]
        account.check_private_logging(rpc)
        rpc.call.side_effect = [ET.fromstring("<reply/>"), ET.fromstring("<reply/>")]
        with self.assertRaisesRegex(ValueError, "No retained effective"):
            account.check_private_logging(rpc)
        truncated = ET.fromstring("<reply><msgs><msg><seqno>100</seqno><project/>"
                                 "<body>log flags: task</body></msg></msgs></reply>")
        rpc.call.side_effect = [ET.fromstring("<reply/>"), truncated]
        with self.assertRaisesRegex(ValueError, "truncated"):
            account.check_private_logging(rpc)

    def test_idempotent_cli_recovers_its_own_completed_hold_without_detach(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            key = root / "key"
            key.write_text("42_" + "a" * 32)
            key.chmod(0o600)
            (root / "gui_rpc_auth.cfg").write_text("synthetic-gui-secret")
            (root / "account_example.xml").write_text(f"<account><master_url>{account.MASTER}</master_url>"
                                                     f"<authenticator>{key.read_text()}</authenticator></account>")
            journal = root / "account-rejoin-31416.json"
            account.migration_state(journal, state(), 42, account.client_identity(root, 31416))
            rpc = Mock()
            output = io.StringIO()
            args = ["account", "--data-dir", str(root), "--expected-user-id", "42",
                    "--expected-user-name", "example", "--key-file", str(key), "--apply"]
            with patch("sys.argv", args), patch.object(account, "RPC", return_value=rpc), \
                    patch.object(account, "check_private_logging"), \
                    patch.object(account, "snapshot", side_effect=[state(no_new_tasks=True), state()]), \
                    redirect_stdout(output):
                account.main()
            self.assertEqual([call.args[0] for call in rpc.call.call_args_list], ["project_allowmorework"])
            self.assertFalse(journal.exists())
            self.assertNotIn(key.read_text(), output.getvalue())
            self.assertNotIn("example", output.getvalue())

    def test_nonzero_preference_status_is_rejected_even_with_unchanged_working_values(self):
        connection = Mock()
        connection.recv.side_effect = [b"<reply><nonce>challenge</nonce></reply>\x03",
                                        b"<reply><authorized/></reply>\x03",
                                        b"<reply><status>-1</status></reply>\x03"]
        with patch.object(account.socket, "create_connection", return_value=connection):
            rpc = account.RPC(31417, "synthetic")
            with self.assertRaisesRegex(RuntimeError, "error -1"):
                rpc.call("set_global_prefs_override", {"global_preferences": ET.Element("global_preferences")})
            rpc.close()

    def test_rpc_fixed_loopback_framing_and_private_error(self):
        connection = Mock()
        connection.recv.side_effect = [b"<reply><nonce>challenge</nonce></reply>\x03",
                                        b"<reply><authorized/></reply>\x03",
                                        b"<reply><error>private-value</error></reply>\x03"]
        with patch.object(account.socket, "create_connection", return_value=connection) as connect:
            rpc = account.RPC(31417, "private-password")
            connect.assert_called_once_with(("127.0.0.1", 31417), timeout=10)
            with self.assertRaises(RuntimeError) as error:
                rpc.call("get_project_status")
            self.assertNotIn("private-value", str(error.exception))
            for call in connection.sendall.call_args_list:
                self.assertTrue(call.args[0].endswith(b"\x03"))
                self.assertNotIn(b"private-password", call.args[0])
                self.assertNotIn(b" />", call.args[0])
            rpc.close()
