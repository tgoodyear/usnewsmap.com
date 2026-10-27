"""Actual pinned BOINC consumers against a separate private MySQL 8.4 server."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

import fixture


def observe():
    version, required = fixture.run(
        *fixture.command("cli", "tls-db.test")[:-2], "-e",
        "SELECT VERSION(), @@require_secure_transport").stdout.strip().split("\t")
    if not version.startswith("8.4.") or required != "1":
        raise RuntimeError("expected MySQL 8.4 with require_secure_transport=ON")
    print(json.dumps({"server": version, "require_secure_transport": True,
                      "patch_sha256": hashlib.sha256(
                          Path("/checks/patch-server-db-tls.py").read_bytes()).hexdigest()}),
          flush=True)


def plaintext_rejected(result):
    return result.returncode != 0 and result.stderr.startswith("ERROR 3159 (HY000): ")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("prepare", "matrix", "restart"))
    args = parser.parse_args()
    if args.phase == "prepare":
        fixture.prepare_tls()
        return
    shutil.copyfile(fixture.ROOT / "ca.pem", fixture.CA)
    fixture.OPTIONS.write_text("[client]\nssl-enforce=1\nssl-verify-server-cert=1\n"
                               f"ssl-ca={fixture.CA}\n")
    observe()
    if args.phase == "restart":
        fixture.matrix("mysql84-tls-restart", True)
        print(json.dumps({"result": "PASS", "scope": "MySQL 8.4 pinned connectors; not Azure rollout"}),
              flush=True)
        return
    fixture.matrix("mysql84-valid", True)
    fixture.matrix("mysql84-wrong-hostname", False, "wrong-db.test")
    shutil.copyfile(fixture.ROOT / "wrong-ca.pem", fixture.CA)
    fixture.matrix("mysql84-wrong-ca", False)
    shutil.copyfile(fixture.ROOT / "ca.pem", fixture.CA)
    fixture.matrix("mysql84-restored-ca", True)
    plain = subprocess.run(
        ["mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test", "--skip-ssl",
         "-ufixture", "-pfixture-only", "fixture", "-e", "SELECT 1"],
        capture_output=True, text=True, timeout=15)
    if not plaintext_rejected(plain):
        raise RuntimeError("plaintext control was not rejected by the server TLS policy")
    print(json.dumps({"case": "mysql84-server-requires-tls", "consumer": "unpatched-cli",
                      "outcome": "rejected", "mysql_errno": 3159}), flush=True)


if __name__ == "__main__":
    main()
