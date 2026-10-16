#!/usr/bin/env python3
"""Private-file, loopback-only BOINC attachment and drain-safe account rejoin."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
from copy import deepcopy
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import socket
import stat
import time
import xml.etree.ElementTree as ET

MASTER = "https://boinc.usnewsmap.com/"


def weak_key(path, user_id):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "r", encoding="ascii") as stream:
        info = os.fstat(stream.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077:
            raise ValueError("Weak-key file must be regular and private (0600 or stricter)")
        key = stream.read(256).strip()
    if not re.fullmatch(r"[1-9][0-9]{0,19}_[0-9a-f]{32}", key):
        raise ValueError("Expected a lower-panel weak account key, never a full authenticator")
    if int(key.split("_", 1)[0]) != user_id:
        raise ValueError("Weak-key user ID does not match the explicitly expected account")
    return key


class RPC:
    def __init__(self, port, password):
        self.socket = socket.create_connection(("127.0.0.1", port), timeout=10)
        self.socket.settimeout(10)
        self.buffer = b""
        nonce = self.call("auth1").findtext(".//nonce")
        if not nonce:
            self.close()
            raise RuntimeError("Client did not return an authentication challenge")
        # Stock BOINC GUI-RPC mandates this nonce challenge, not password storage.
        # The protocol is restricted to loopback; substituting a KDF breaks it.
        digest = hashlib.md5((nonce + password).encode()).hexdigest()
        if self.call("auth2", {"nonce_hash": digest}).find(".//authorized") is None:
            self.close()
            raise RuntimeError("Client GUI-RPC authentication failed")

    def close(self):
        self.socket.close()

    def call(self, operation, fields=None):
        request = ET.Element("boinc_gui_rpc_request")
        command = ET.SubElement(request, operation)
        for name, value in (fields or {}).items():
            if isinstance(value, ET.Element):
                command.append(deepcopy(value))
            else:
                ET.SubElement(command, name).text = str(value) if value is not None else None
        # Stock match_req() does not recognize ElementTree's "<operation />".
        self.socket.sendall(ET.tostring(request).replace(b" />", b"/>") + b"\x03")
        while b"\x03" not in self.buffer:
            block = self.socket.recv(65536)
            if not block:
                raise RuntimeError("Client closed GUI-RPC connection")
            self.buffer += block
            if len(self.buffer) > 16 * 1024 * 1024:
                raise RuntimeError("Client GUI-RPC response exceeds bounded size")
        message, self.buffer = self.buffer.split(b"\x03", 1)
        reply = ET.fromstring(message)
        if reply.find(".//unauthorized") is not None or reply.find(".//error") is not None:
            raise RuntimeError(f"Client rejected {operation}; response withheld for privacy")
        for value in (reply.findtext(".//error_num", "0"), reply.findtext("status", "0")):
            try:
                error = int(value)
            except ValueError:
                raise RuntimeError("Malformed GUI-RPC status; response withheld for privacy") from None
            if error:
                raise RuntimeError(f"Client rejected {operation} (error {error})")
        return reply


def flag(element, name):
    value = element.find(name)
    return value is not None and value.text not in ("0", "false")


def snapshot(rpc, expected_id, expected_name):
    projects = rpc.call("get_project_status").findall(".//project")
    matching = [project for project in projects
                if project.findtext("master_url", "").rstrip("/") == MASTER.rstrip("/")]
    aliases = [project for project in projects
               if project.findtext("master_url", "").startswith(MASTER + "usnmocr")]
    if aliases or len(matching) > 1:
        raise ValueError("Legacy or duplicate project URL requires explicit reconciliation")
    project = matching[0] if matching else None
    pending = [result for result in rpc.call("get_results", {"active_only": 0}).findall(".//result")
               if result.findtext("project_url", "").rstrip("/") == MASTER.rstrip("/")]
    return {
        "attached": project is not None,
        "user_id": int(project.findtext("userid", "0")) if project is not None else 0,
        "host_id": int(project.findtext("hostid", "0")) if project is not None else 0,
        "expected_name_verified": project is not None and project.findtext("user_name") == expected_name,
        "expected_user_verified": project is not None and int(project.findtext("userid", "0")) == expected_id,
        "pending_results": len(pending),
        "scheduler_rpc_in_progress": project is not None and flag(project, "scheduler_rpc_in_progress"),
        "no_new_tasks": project is not None and flag(project, "dont_request_more_work"),
        "account_manager": project is not None and flag(project, "attached_via_acct_mgr"),
    }


def project_operation(rpc, operation):
    rpc.call("project_" + operation, {"project_url": MASTER})


def check_private_logging(rpc):
    configuration = rpc.call("get_cc_config")
    # Stock get_cc_config returns an empty reply when no file exists, and a
    # normal options-only file omits log_flags: both use default debug=false.
    for name in ("gui_rpc_debug", "http_debug", "http_xfer_debug"):
        if flag(configuration, ".//" + name):
            raise ValueError("Disable credential-bearing client debug logging before account attachment")
    messages = rpc.call("get_messages", {"seqno": 0}).findall(".//msg")
    latest = []
    latest_start = None
    previous_was_flags = False
    for index, message in enumerate(messages):
        body = message.findtext("body", "").strip()
        is_flags = not message.findtext("project", "") and body.startswith("log flags:")
        if is_flags:
            if not previous_was_flags:
                latest = []
                latest_start = index
            latest.extend(body.partition(":")[2].strip().split(", "))
        previous_was_flags = is_flags
    if not latest:
        raise ValueError("No retained effective runtime log-flag evidence; no credential sent")
    if latest_start == 0 and int(messages[0].findtext("seqno", "0")) > 1:
        raise ValueError("Runtime log-flag group may be truncated; no credential sent")
    if {"gui_rpc_debug", "http_debug", "http_xfer_debug"} & set(latest):
        raise ValueError("Effective runtime debug logging may expose credentials; no credential sent")


def already_using_key(data_dir, key):
    for path in data_dir.glob("account_*.xml"):
        if path.is_symlink() or not path.is_file():
            continue
        account = ET.parse(path).getroot()
        if account.findtext("master_url", "").rstrip("/") == MASTER.rstrip("/"):
            return hmac.compare_digest(account.findtext("authenticator", ""), key)
    return False


def client_identity(data_dir, port):
    authentication = (data_dir / "gui_rpc_auth.cfg").stat()
    values = (socket.gethostname(), str(data_dir.resolve()), port, authentication.st_dev,
              authentication.st_ino, authentication.st_mtime_ns, authentication.st_size)
    return hashlib.sha256(json.dumps(values).encode()).hexdigest()


def migration_state(path, current, expected_id, identity):
    if path.exists():
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(descriptor) as stream:
            if os.fstat(stream.fileno()).st_mode & 0o077:
                raise ValueError("Migration state must be private")
            state = json.load(stream)
        if (state.get("version") != 2 or state.get("client_identity") != identity
                or state.get("master_url") != MASTER or state.get("expected_user_id") != expected_id):
            raise ValueError("Migration state belongs to another client/target or unsupported legacy journal")
        if type(state.get("original_no_new_tasks")) is not bool:
            raise ValueError("Malformed original drain preference")
        return state
    if current["no_new_tasks"]:
        raise ValueError("User-held project refused: stock attach cannot atomically retain its hold")
    state = {"version": 2, "client_identity": identity, "master_url": MASTER, "expected_user_id": expected_id,
             "original_no_new_tasks": False}
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        json.dump(state, stream)
        stream.flush()
        os.fsync(stream.fileno())
    return state


@contextmanager
def network_policy(rpc):
    status = rpc.call("get_cc_status")
    mode = int(status.findtext(".//network_mode", "0"))
    permanent = int(status.findtext(".//network_mode_perm", "0"))
    delay = float(status.findtext(".//network_mode_delay", "-1"))
    names = {1: "always", 2: "auto", 3: "never"}
    if mode not in names or permanent not in names or not 0 <= delay <= 31536000:
        raise ValueError("Cannot safely preserve the client's current network policy")
    started = time.monotonic()
    try:
        yield
    finally:
        remaining = max(0, delay - (time.monotonic() - started))
        restore = mode if remaining else permanent
        rpc.call("set_network_mode", {names[restore]: None, "duration": remaining})


@contextmanager
def network_guard(rpc):
    with network_policy(rpc):
        rpc.call("set_network_mode", {"never": None, "duration": 600})
        for _ in range(25):
            held = rpc.call("get_cc_status")
            if (held.findtext(".//network_mode") == "3"
                    and int(held.findtext(".//network_suspend_reason", "0"))):
                break
            time.sleep(0.2)
        else:
            raise RuntimeError("Client did not establish the temporary network guard")
        yield


def protect_preferences(rpc, data_dir, backup_dir):
    preferences = rpc.call("get_global_prefs_working").find(".//global_preferences")
    if preferences is None:
        raise ValueError("Cannot preserve effective resource preferences; no detach performed")
    preferences = deepcopy(preferences)
    metadata = {"source_project", "source_scheduler", "mod_time", "override_file_present"}
    for child in list(preferences):
        if child.tag in metadata:
            preferences.remove(child)
    required = {"max_ncpus_pct", "cpu_usage_limit", "ram_max_used_busy_pct", "ram_max_used_idle_pct",
                "run_gpu_if_user_active", "run_if_user_active", "work_buf_min_days", "work_buf_additional_days"}
    if not required.issubset({child.tag for child in preferences}):
        raise ValueError("Incomplete effective resource preferences; no detach performed")
    original = data_dir / "global_prefs_override.xml"
    if original.is_symlink():
        raise ValueError("Owner-managed preference symlink requires explicit handling; no detach performed")
    backup = backup_dir / f"global-prefs-before-rejoin-{time.time_ns()}.json"
    with os.fdopen(os.open(backup, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as stream:
        json.dump({"original_override": original.read_text() if original.exists() else None,
                   "protected_effective": ET.tostring(preferences, encoding="unicode")}, stream)
    rpc.call("set_global_prefs_override", {"global_preferences": preferences})
    rpc.call("read_global_prefs_override")
    stored = rpc.call("get_global_prefs_override").find(".//global_preferences")
    effective = rpc.call("get_global_prefs_working").find(".//global_preferences")
    def values(root):
        if root is None:
            return None
        return [(element.tag, (element.text or "").strip(),
                 tuple(ET.tostring(child, encoding="unicode") for child in element))
                for element in root if element.tag not in metadata]
    if values(stored) != values(preferences) or values(effective) != values(preferences):
        raise RuntimeError("Effective resource preferences changed; no detach performed")


def rejoin(rpc, key, expected_id, expected_name, timeout=300, original_no_work=None,
           data_dir=None, backup_dir=None):
    current = snapshot(rpc, expected_id, expected_name)
    if current["account_manager"]:
        raise ValueError("Account-manager project must not be detached independently")
    if original_no_work is None:
        original_no_work = current["no_new_tasks"]
    if original_no_work:
        raise ValueError("User-held project refused: stock attach cannot atomically retain its hold")
    if current["attached"]:
        project_operation(rpc, "nomorework")
        current = snapshot(rpc, expected_id, expected_name)
        if current["pending_results"] or current["scheduler_rpc_in_progress"]:
            return {**current, "status": "draining; no detach performed"}
    protect_preferences(rpc, data_dir, backup_dir)
    if current["attached"]:
        # set_network_mode explicitly clears BOINC's recent-GUI-network grace.
        # No network-enabling GUI operations are allowed inside this guard.
        with network_guard(rpc):
            current = snapshot(rpc, expected_id, expected_name)
            if current["pending_results"] or current["scheduler_rpc_in_progress"]:
                return {**current, "status": "draining; no detach performed"}
            project_operation(rpc, "detach")
            if snapshot(rpc, expected_id, expected_name)["attached"]:
                raise RuntimeError("Project detach did not complete; no duplicate attach attempted")
    # Stock project_attach itself re-enables networking. Only originally
    # work-enabled clients are supported, never promise atomic hold preservation.
    rpc.call("project_attach", {"project_url": MASTER, "authenticator": key, "project_name": ""})
    deadline = time.monotonic() + timeout
    update_sent = False
    while time.monotonic() < deadline:
        current = snapshot(rpc, expected_id, expected_name)
        if current["attached"]:
            if current["user_id"] not in (0, expected_id):
                project_operation(rpc, "nomorework")
                raise RuntimeError("Unexpected scheduler account; stopped further work fetch")
            if current["expected_user_verified"] and not current["expected_name_verified"]:
                project_operation(rpc, "nomorework")
                raise RuntimeError("Unexpected scheduler account name; stopped further work fetch")
            if current["host_id"] and current["expected_user_verified"] and current["expected_name_verified"]:
                return {**current, "status": "scheduler attribution verified", "key_kind": "weak"}
            if not update_sent:
                project_operation(rpc, "update")
                update_sent = True
        time.sleep(2)
    if current["attached"]:
        project_operation(rpc, "nomorework")
    raise RuntimeError("Bounded scheduler-attribution wait expired; no further work requested")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", required=True, type=Path)
    parser.add_argument("--port", default=31416, type=int)
    parser.add_argument("--expected-user-id", required=True, type=int)
    parser.add_argument("--expected-user-name", required=True)
    parser.add_argument("--key-file", type=Path)
    parser.add_argument("--state-file", type=Path, help="Private durable drain journal; defaults beside the key file")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    key = None
    if args.apply:
        if not args.key_file:
            parser.error("--apply requires a protected --key-file, not a key argument")
        key = weak_key(args.key_file, args.expected_user_id)
    rpc = RPC(args.port, (args.data_dir / "gui_rpc_auth.cfg").read_text().strip())
    try:
        result = snapshot(rpc, args.expected_user_id, args.expected_user_name)
        if args.apply:
            check_private_logging(rpc)
            identity = client_identity(args.data_dir, args.port)
            state_file = args.state_file or args.key_file.with_name(f"account-rejoin-{args.port}.json")
            if (result["attached"] and result["expected_user_verified"] and result["expected_name_verified"]
                    and result["host_id"] and already_using_key(args.data_dir, key)):
                if state_file.exists():
                    state = migration_state(state_file, result, args.expected_user_id, identity)
                    if result["account_manager"]:
                        raise ValueError("Account-manager project recovery requires its owner; no change")
                    if not state["original_no_new_tasks"] and result["no_new_tasks"]:
                        project_operation(rpc, "allowmorework")
                        result = snapshot(rpc, args.expected_user_id, args.expected_user_name)
                        if result["no_new_tasks"]:
                            raise RuntimeError("Could not release helper-owned drain hold; journal retained")
                    state_file.unlink()
                result.update(status="already attached with supplied weak key", key_kind="weak")
            else:
                if result["account_manager"]:
                    raise ValueError("Account-manager project must not be detached independently")
                state = migration_state(state_file, result, args.expected_user_id, identity)
                with network_policy(rpc):
                    result = rejoin(rpc, key, args.expected_user_id, args.expected_user_name,
                                    original_no_work=state["original_no_new_tasks"],
                                    data_dir=args.data_dir, backup_dir=state_file.parent)
                if result["status"] == "scheduler attribution verified":
                    state_file.unlink()
        print(json.dumps({"master_url": MASTER, **result}, indent=2))
    finally:
        rpc.close()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, ET.ParseError) as error:
        raise SystemExit(str(error)) from None
