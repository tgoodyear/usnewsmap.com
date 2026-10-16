"""Disposable loopback-only MariaDB TLS matrix; categorical evidence, no raw errors."""
import json
import hashlib
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import time

ROOT = Path("/fixture")
CA = Path("/usr/local/share/usnm/mysql-ca.pem")
OPTIONS = CA.with_name("mysql-tls.cnf")


def run(*args, **kwargs):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=60, **kwargs)


def certificate(name):
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        "-subj", f"/CN={name}", "-keyout", str(ROOT / f"{name}.key"),
        "-out", str(ROOT / f"{name}.pem"))


def sql(query):
    return run("mariadb", "--no-defaults", "--socket=/fixture/db.sock", "-uroot",
               "--batch", "--skip-column-names", "-e", query).stdout.strip()


def start(tls):
    args = ["mariadbd", "--no-defaults", "--user=root", "--datadir=/fixture/data",
            "--socket=/fixture/db.sock", "--pid-file=/fixture/db.pid",
            "--bind-address=127.0.0.1", "--port=3306", "--skip-name-resolve",
            "--innodb-buffer-pool-size=32M", "--innodb-log-file-size=16M",
            "--max-connections=12", "--performance-schema=OFF",
            "--log-error=/fixture/server.log"]
    if tls:
        args += ["--require-secure-transport=ON", "--ssl-ca=/fixture/ca.pem",
                 "--ssl-cert=/fixture/server.pem", "--ssl-key=/fixture/server.key"]
    else:
        args += ["--require-secure-transport=OFF", "--skip-ssl"]
    process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(120):
        if process.poll() is not None:
            raise RuntimeError("fixture server exited; inspect local server.log")
        try:
            if sql("SELECT 1") == "1":
                assert sql("SELECT @@require_secure_transport") == ("1" if tls else "0")
                return process
        except (subprocess.SubprocessError, OSError):
            pass
        time.sleep(0.25)
    process.terminate()
    process.wait(timeout=20)
    raise RuntimeError("fixture readiness timed out")


def stop(process):
    process.terminate()
    process.wait(timeout=30)


def command(consumer, host):
    return {
        "python": ["python3", "/checks/consumer.py", "--host", host],
        "php-legacy": ["php", "/checks/consumer.php", "--api", "legacy", "--host", host],
        "php-DbConn": ["php", "/checks/consumer.php", "--api", "class", "--host", host],
        "cpp-DB_CONN": ["/checks/consumer", "--host", host],
        "cli": ["mariadb", "--no-defaults", "--ssl", "--ssl-verify-server-cert",
                f"--ssl-ca={CA}", "--protocol=tcp", f"--host={host}",
                "-ufixture", "-pfixture-only", "fixture", "--batch", "--skip-column-names",
                "-e", "SHOW SESSION STATUS LIKE 'Ssl_cipher'"],
    }[consumer]


def tls_rejection(case, consumer, result):
    if not result.returncode:
        return False
    if consumer in ("python", "php-legacy", "php-DbConn"):
        try:
            error = json.loads(result.stderr)
        except (ValueError, TypeError):
            return False
        if not isinstance(error, dict):
            return False
        if consumer == "python":
            return error.get("mysql_errno") == 2026
        if (error.get("mysql_errno") == 2002
                and error.get("message") == "Cannot connect to MySQL using SSL"):
            return True
        # mysqlnd reports an absent SSL handshake as server-gone; accept this
        # only for the independently authenticated, SSL-disabled peer.
        return (case == "plaintext-peer" and error.get("mysql_errno") == 2006
                and error.get("message") == "MySQL server has gone away")
    prefix = r"mysql_real_connect\(\): 2026: " if consumer == "cpp-DB_CONN" else r"ERROR 2026 \(HY000\): "
    return re.fullmatch(prefix + r"TLS/SSL error: [^\n]+\n?", result.stderr) is not None


def matrix(case, succeeds, host="tls-db.test"):
    with socket.create_connection((host, 3306), timeout=3):
        pass
    for consumer in ("python", "php-legacy", "php-DbConn", "cpp-DB_CONN", "cli"):
        result = subprocess.run(command(consumer, host), capture_output=True, text=True, timeout=15)
        cipher = result.stdout.strip().split()[-1] if result.stdout.strip() else ""
        if succeeds:
            if result.returncode or not cipher.startswith(("TLS_", "ECDHE-", "DHE-")):
                raise RuntimeError(f"{case}/{consumer}: expected encrypted connection, exit={result.returncode}")
        else:
            if not tls_rejection(case, consumer, result):
                raise RuntimeError(f"{case}/{consumer}: failure is not demonstrably TLS-related")
        print(json.dumps({"case": case, "consumer": consumer,
                          "outcome": "encrypted" if succeeds else "rejected",
                          "cipher": cipher if succeeds else None}), flush=True)


def main():
    os.umask(0o077)
    certificate("ca")
    certificate("wrong-ca")
    run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=tls-db.test",
        "-keyout", "/fixture/server.key", "-out", "/fixture/server.csr")
    Path("/fixture/extensions").write_text("subjectAltName=DNS:tls-db.test\nextendedKeyUsage=serverAuth\n")
    run("openssl", "x509", "-req", "-in", "/fixture/server.csr", "-CA", "/fixture/ca.pem",
        "-CAkey", "/fixture/ca.key", "-CAcreateserial", "-days", "1",
        "-extfile", "/fixture/extensions", "-out", "/fixture/server.pem")
    shutil.copyfile(ROOT / "ca.pem", CA)
    OPTIONS.write_text("[client]\nssl-enforce=1\nssl-verify-server-cert=1\n"
                       "ssl-ca=/usr/local/share/usnm/mysql-ca.pem\n")
    run("mariadb-install-db", "--no-defaults", "--datadir=/fixture/data",
        "--auth-root-authentication-method=normal", "--skip-test-db")
    process = start(True)
    try:
        sql("CREATE DATABASE fixture; CREATE USER 'fixture'@'127.0.0.1' IDENTIFIED BY 'fixture-only'; "
            "GRANT ALL ON fixture.* TO 'fixture'@'127.0.0.1'; "
            "GRANT ALL ON boinc_fixture.* TO 'fixture'@'127.0.0.1';")
        print(json.dumps({
            "server": sql("SELECT VERSION()"),
            "require_secure_transport": True,
            "boinc_commit": run("git", "-C", "/opt/boinc", "rev-parse", "HEAD").stdout.strip(),
            "patch_sha256": hashlib.sha256(Path("/checks/patch-server-db-tls.py").read_bytes()).hexdigest(),
            "php_client": run("php", "-r", "echo mysqli_get_client_info();").stdout.strip(),
            "python_client": run("python3", "-c", "import MySQLdb; print(MySQLdb.get_client_info())").stdout.strip(),
            "cpp_client": run("mariadb_config", "--cc_version").stdout.strip(),
        }), flush=True)
        print(run("python3", "/checks/schema.py", "--create").stdout.strip(), flush=True)
        matrix("valid", True)
        matrix("wrong-hostname", False, "wrong-db.test")
        shutil.copyfile(ROOT / "wrong-ca.pem", CA)
        matrix("wrong-ca", False)
        shutil.copyfile(ROOT / "ca.pem", CA)
        matrix("restored-ca", True)
        # A plaintext client must be rejected by the enforcing server too.
        plain = subprocess.run(["mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test",
                                "--skip-ssl", "-ufixture", "-pfixture-only", "fixture", "-e", "SELECT 1"],
                               capture_output=True, text=True, timeout=15)
        if plain.returncode == 0:
            raise RuntimeError("TLS-required server accepted plaintext")
        print(json.dumps({"case": "server-requires-tls", "consumer": "unpatched-cli",
                          "outcome": "rejected"}), flush=True)
    finally:
        stop(process)
    process = start(False)
    try:
        assert sql("SELECT @@have_ssl") == "DISABLED"
        # Prove the no-TLS peer is reachable and credentials/database work.
        run("mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test", "--skip-ssl",
            "-ufixture", "-pfixture-only", "fixture", "-e", "SELECT 1")
        matrix("plaintext-peer", False)
    finally:
        stop(process)
    process = start(True)
    try:
        matrix("tls-restart", True)
        print(run("python3", "/checks/schema.py").stdout.strip(), flush=True)
    finally:
        stop(process)
    print(json.dumps({"result": "PASS", "scope": "pinned-connectors; not full project lifecycle/migration"}))


if __name__ == "__main__":
    main()
