"""Offline tests: never contact a client or send real account credentials."""
from pathlib import Path
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
        snapshots = [state()] * 4 + [state(pending_results=1)]
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

    def test_drained_rejoin_uses_key_only_in_protocol_and_restores_hold(self):
        snapshots = [state(no_new_tasks=True)] * 5 + [state(attached=False)] + [state(no_new_tasks=False)]
        with patch.object(account, "snapshot", side_effect=snapshots), patch.object(account.time, "sleep"):
            rpc = Mock()
            result = account.rejoin(rpc, "synthetic", 42, "example")
            operations = [call.args[0] for call in rpc.call.call_args_list]
            self.assertEqual(operations, ["project_nomorework", "project_detach", "project_attach",
                                          "project_nomorework", "project_update"])
            self.assertNotIn("synthetic", str(result))
            self.assertEqual(result["status"], "scheduler attribution verified")
            self.assertTrue(result["no_new_tasks"])

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
        rpc.call.return_value = ET.fromstring("<reply><log_flags/></reply>")
        account.check_private_logging(rpc)
        for name in ("gui_rpc_debug", "http_debug", "http_xfer_debug"):
            rpc.call.return_value = ET.fromstring(f"<reply><log_flags><{name}/></log_flags></reply>")
            with self.assertRaises(ValueError):
                account.check_private_logging(rpc)
        rpc.call.return_value = ET.fromstring("<reply/>")
        account.check_private_logging(rpc)
        rpc.call.return_value = ET.fromstring("<reply><cc_config><options/></cc_config></reply>")
        account.check_private_logging(rpc)

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
