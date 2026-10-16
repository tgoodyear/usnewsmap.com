#!/usr/bin/env python3
"""Private-file, loopback-only BOINC attachment and drain-safe account rejoin."""
from __future__ import annotations

import argparse
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
        digest = hashlib.md5((nonce + password).encode()).hexdigest()  # lgtm[py/weak-sensitive-data-hashing]
        if self.call("auth2", {"nonce_hash": digest}).find(".//authorized") is None:
            self.close()
            raise RuntimeError("Client GUI-RPC authentication failed")

    def close(self):
        self.socket.close()

    def call(self, operation, fields=None):
        request = ET.Element("boinc_gui_rpc_request")
        command = ET.SubElement(request, operation)
        for name, value in (fields or {}).items():
            ET.SubElement(command, name).text = str(value)
        # Stock match_req() does not recognize ElementTree's "<operation />".
        self.socket.sendall(ET.tostring(request, short_empty_elements=False) + b"\x03")
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
        try:
            error = int(reply.findtext(".//error_num", "0"))
        except ValueError:
            raise RuntimeError("Malformed GUI-RPC error number; response withheld for privacy") from None
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


def already_using_key(data_dir, key):
    for path in data_dir.glob("account_*.xml"):
        if path.is_symlink() or not path.is_file():
            continue
        account = ET.parse(path).getroot()
        if account.findtext("master_url", "").rstrip("/") == MASTER.rstrip("/"):
            return hmac.compare_digest(account.findtext("authenticator", ""), key)
    return False


def rejoin(rpc, key, expected_id, expected_name, timeout=300):
    current = snapshot(rpc, expected_id, expected_name)
    restore_no_work = current["no_new_tasks"]
    if current["account_manager"]:
        raise ValueError("Account-manager project must not be detached independently")
    if current["attached"]:
        project_operation(rpc, "nomorework")
        # The stock client rejects hot project_attach; drain and prove a quiet
        # scheduler before the supported detach/rejoin, never edit its state XML.
        for _ in range(3):
            current = snapshot(rpc, expected_id, expected_name)
            if current["pending_results"] or current["scheduler_rpc_in_progress"]:
                return {**current, "status": "draining; no detach performed"}
            time.sleep(1)
        current = snapshot(rpc, expected_id, expected_name)
        if current["pending_results"] or current["scheduler_rpc_in_progress"]:
            return {**current, "status": "draining; no detach performed"}
        project_operation(rpc, "detach")
        if snapshot(rpc, expected_id, expected_name)["attached"]:
            raise RuntimeError("Project detach did not complete; no duplicate attach attempted")
    rpc.call("project_attach", {"project_url": MASTER, "authenticator": key, "project_name": ""})
    deadline = time.monotonic() + timeout
    update_sent = False
    while time.monotonic() < deadline:
        current = snapshot(rpc, expected_id, expected_name)
        if current["attached"]:
            if restore_no_work and not current["no_new_tasks"]:
                project_operation(rpc, "nomorework")
            if not update_sent:
                project_operation(rpc, "update")
                update_sent = True
            if current["user_id"] not in (0, expected_id):
                project_operation(rpc, "nomorework")
                raise RuntimeError("Unexpected scheduler account; stopped further work fetch")
            if current["expected_user_verified"] and not current["expected_name_verified"]:
                project_operation(rpc, "nomorework")
                raise RuntimeError("Unexpected scheduler account name; stopped further work fetch")
            if current["host_id"] and current["expected_user_verified"] and current["expected_name_verified"]:
                current["no_new_tasks"] = restore_no_work
                return {**current, "status": "scheduler attribution verified", "key_kind": "weak"}
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
            if (result["attached"] and result["expected_user_verified"] and result["expected_name_verified"]
                    and result["host_id"] and already_using_key(args.data_dir, key)):
                result.update(status="already attached with supplied weak key", key_kind="weak")
            else:
                result = rejoin(rpc, key, args.expected_user_id, args.expected_user_name)
        print(json.dumps({"master_url": MASTER, **result}, indent=2))
    finally:
        rpc.close()


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, ET.ParseError) as error:
        raise SystemExit(str(error)) from None
