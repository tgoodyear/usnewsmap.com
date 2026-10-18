"""Actual make_project, Apache/CGI and daemon lifecycle on an existing reviewed image."""
import hashlib
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import time
import urllib.request
import xml.etree.ElementTree as ET

import fixture

ROOT = Path("/fixture")
PROJECT = ROOT / "share/usnmocr"
SRC = Path("/usr/local/boinc")
TEMPLATES = Path("/usr/local/lib/usnm-boinc")
PROCESSES = []
DAEMONS = (
    ("feeder", ["--allapps", "-d", "3"]),
    ("transitioner", ["-d", "3"]),
    ("sample_trivial_validator", ["--app", "usnm_ocr", "-d", "3"]),
    ("batch_collect_assimilator", ["--app", "usnm_ocr", "-d", "3"]),
)


def run(*args, **kwargs):
    result = subprocess.run(args, capture_output=True, text=True, timeout=120, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{Path(args[0]).name} failed: "
                           + (result.stdout + result.stderr).replace("fixture-only", "[fixture password]")[-3000:])
    return result


def emit(case, **fields):
    print(json.dumps({"case": case, **fields}), flush=True)


def setup():
    os.umask(0o022)
    shutil.copyfile("/dbfixture/ca.pem", fixture.CA)
    fixture.OPTIONS.write_text("[client]\nssl-enforce=1\nssl-verify-server-cert=1\n"
                               "ssl-ca=/usr/local/share/usnm/mysql-ca.pem\n")
    fixture.sql = lambda query: run(
        "mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test",
        "--ssl", "--ssl-verify-server-cert", "--ssl-ca=/dbfixture/ca.pem",
        "-ufixture", "-pfixture-only", "--batch", "--skip-column-names", "-e", query).stdout.strip()


def init_project():
    (ROOT / "share").mkdir()
    result = run("python3", str(SRC / "tools/make_project"), "--no_query", "--srcdir", str(SRC),
                 "--project_root", str(PROJECT), "--key_dir", "/fixture/generated-keys",
                 "--url_base", "http://localhost/", "--html_user_url", "http://localhost/usnmocr/",
                 "--html_ops_url", "http://localhost/usnmocr_ops/", "--project_host", "localhost",
                 "--db_host", "tls-db.test", "--db_name", "full_fixture", "--db_user", "fixture",
                 "--db_passwd", "fixture-only", "--user_name", "boincadm", "usnmocr", "Local TLS fixture",
                 cwd=SRC, env={**os.environ, "USER": "root"})
    (ROOT / "make-project.log").write_text(result.stdout + result.stderr)
    assert (PROJECT / "html/inc/boinc_db.inc").is_file()
    assert (PROJECT / "bin/feeder").is_file()
    tables = int(fixture.sql(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema='full_fixture'"))
    assert tables == 47, "incomplete pinned BOINC schema"
    emit("actual-make-project", tables=tables)
    fixture.sql("GRANT SELECT,INSERT,UPDATE,DELETE ON full_fixture.* TO 'runtime'@'127.0.0.1'")
    # Render the production template using the real current runtime function.
    secrets = Path("/dev/shm/boinc")
    secrets.mkdir(parents=True)
    (secrets / "db-runtime").write_text("fixture-only")
    (secrets / "keys").mkdir()
    Path("/fixture-keys").chmod(0o700)
    for name in ("upload_private", "upload_public", "code_sign_public"):
        shutil.copyfile(ROOT / f"generated-keys/{name}", secrets / f"keys/{name}")
        shutil.copyfile(ROOT / f"generated-keys/{name}", Path("/fixture-keys") / name)
        (Path("/fixture-keys") / name).chmod(0o600)
    secure_keys()
    shutil.rmtree(ROOT / "generated-keys")
    config = PROJECT / "config.xml"
    config.unlink()
    run("bash", "-c", ". /usr/local/lib/usnm-boinc/lib.sh; render_config")
    # HTTP is private container loopback only; no public DNS/certificates.
    rendered = secrets / "config.xml"
    rendered.write_text(rendered.read_text().replace("https://localhost/", "http://localhost/"))
    shutil.copyfile(TEMPLATES / "project.xml", PROJECT / "project.xml")
    shutil.copyfile(TEMPLATES / "plan_class_spec.xml", PROJECT / "plan_class_spec.xml")
    run("bin/xadd", cwd=PROJECT)
    run("chown", "-R", "boincadm:boincadm", str(PROJECT))
    for i in range(256):
        (PROJECT / f"html/cache/{i:02x}").mkdir(exist_ok=True)
    run("chmod", "-R", "g+w", str(PROJECT / "html/cache"))
    assert {"usnm_ocr", "usnm_assess"} <= set(fixture.sql(
        "SELECT name FROM full_fixture.app").splitlines())
    emit("actual-xadd", apps=int(fixture.sql("SELECT COUNT(*) FROM full_fixture.app")))


def start_apache(database_failure=False):
    config = ET.parse(PROJECT / "config.xml")
    logdir = Path(config.findtext("./config/log_dir"))
    logdir.mkdir(exist_ok=True)
    shutil.chown(logdir, user="boincadm", group="boincadm")
    logdir.chmod(0o775)
    for folder in ("apache/run", "apache/lock", "apache/log", "apache/sessions", "php"):
        (ROOT / folder).mkdir(parents=True, exist_ok=True)
    (ROOT / "apache/lock").chmod(0o1777)
    (ROOT / "apache/sessions").chmod(0o1777)
    (ROOT / "php/99-fixture.ini").write_text(
        "opcache.lockfile_path=/fixture/apache/lock\nsession.save_path=/fixture/apache/sessions\n")
    conf = (TEMPLATES / "apache-project.conf.template").read_text()
    for key, value in {"PROJ": str(PROJECT), "PROJECT": "usnmocr", "MASTER_HOST": "localhost"}.items():
        conf = conf.replace(f"@{key}@", value)
    (ROOT / "apache.conf").write_text(
        "Include /etc/apache2/apache2.conf\n" + conf)
    env = {**os.environ, "APACHE_RUN_USER": "www-data", "APACHE_RUN_GROUP": "www-data",
           "APACHE_RUN_DIR": "/fixture/apache/run", "APACHE_LOCK_DIR": "/fixture/apache/lock",
           "APACHE_PID_FILE": "/fixture/apache/run/pid", "APACHE_LOG_DIR": "/fixture/apache/log",
           "PHP_INI_SCAN_DIR": "/etc/php/8.2/apache2/conf.d:/fixture/php"}
    apache = subprocess.Popen(["apache2", "-f", "/fixture/apache.conf", "-DFOREGROUND"], env=env,
                              stdout=subprocess.DEVNULL, stderr=open(ROOT / "apache.log", "a"))
    PROCESSES.append(apache)
    for _ in range(60):
        if apache.poll() is not None:
            logs = "\n".join(p.read_text(errors="replace")[-2000:]
                             for p in (ROOT / "apache.log", ROOT / "apache/log/error.log")
                             if p.exists())
            raise RuntimeError(f"Apache exited {apache.returncode}: " + logs)
        try:
            if database_failure:
                with socket.create_connection(("127.0.0.1", 80), timeout=2):
                    pass
                break
            with urllib.request.urlopen("http://127.0.0.1/get_project_config.php", timeout=2) as response:
                ET.fromstring(response.read())
            break
        except (OSError, ET.ParseError):
            time.sleep(0.5)
    else:
        logs = "\n".join(p.read_text(errors="replace")[-2000:]
                         for p in (ROOT / "apache.log", ROOT / "apache/log/error.log") if p.exists())
        raise RuntimeError("Apache project config readiness timed out: " + logs)


def secure_keys():
    keys = Path("/dev/shm/boinc/keys")
    keys.chmod(0o750)
    shutil.chown(keys, user="root", group="boincadm")
    for key in keys.iterdir():
        key.chmod(0o640)
        shutil.chown(key, user="root", group="boincadm")


def start_services():
    start_apache()
    # Start real daemon binaries individually so each PID/exit can be bounded.
    for name, args in DAEMONS:
        log = open(ROOT / f"{name}.log", "a")
        process = subprocess.Popen([str(PROJECT / "bin" / name), *args], cwd=PROJECT,
                                   stdout=log, stderr=log, user="boincadm", group="boincadm")
        PROCESSES.append(process)
    time.sleep(4)
    for process in PROCESSES:
        if process.poll() is not None:
            logs = "\n".join(p.name + ": " + p.read_text(errors="replace")[-1200:]
                             for p in ROOT.glob("*.log"))
            raise RuntimeError(f"actual service exited {process.returncode}: {logs}")
    emit("actual-daemons-running", names=["feeder", "transitioner", "sample_trivial_validator",
                                         "batch_collect_assimilator"])


def web():
    for path in ("get_project_config.php", "server_status.php", "index.php"):
        with urllib.request.urlopen(f"http://127.0.0.1/{path}", timeout=10) as response:
            data = response.read()
            validate_page(path, response.status, data)
        emit("actual-web", endpoint=path, bytes=len(data))
    scheduler_rpc()


def validate_page(path, status, data):
    assert status == 200 and data, "missing successful web response"
    lowered = data.lower()
    assert not any(message in lowered for message in (
        b"database error", b"can't connect to database", b"cannot connect to database",
        b"unable to connect", b"fatal error", b"uncaught",
    )), "web endpoint returned an error page"
    if path == "get_project_config.php":
        assert ET.fromstring(data).tag == "project_config"


def scheduler_rpc(case=None):
    request = urllib.request.Request("http://127.0.0.1/usnmocr_cgi/cgi",
                                    data=b"<scheduler_request><core_client_major_version>8</core_client_major_version>"
                                         b"<core_client_minor_version>2</core_client_minor_version>"
                                         b"<core_client_release>13</core_client_release>"
                                         b"<platform_name>x86_64-pc-linux-gnu</platform_name>"
                                         b"<host_info><p_ncpus>1</p_ncpus></host_info></scheduler_request>",
                                    headers={"Content-Type": "text/xml"})
    with urllib.request.urlopen(request, timeout=20) as response:
        tree = ET.fromstring(response.read())
        assert tree.tag == "scheduler_reply"
        assert not tree.findall(".//workunit") and not tree.findall(".//result")
        if case:
            messages = " ".join(tree.itertext()).lower()
            assert tree.find("project_is_down") is not None and "database" in messages, (
                "scheduler did not explicitly reject database access: " + messages[:300])
        else:
            assert tree.find("project_is_down") is None, (
                "scheduler is down, not a valid positive: " + " ".join(tree.itertext())[:500])
    emit(case or "actual-scheduler-rpc", consumer="scheduler", reply=tree.tag, dispatched_work=0,
         outcome="rejected" if case else "accepted")


def connections():
    rows = fixture.sql(
        "SELECT t.PROCESSLIST_ID,s.VARIABLE_VALUE "
        "FROM performance_schema.threads t JOIN performance_schema.status_by_thread s "
        "ON s.THREAD_ID=t.THREAD_ID WHERE t.PROCESSLIST_DB='full_fixture' "
        "AND s.VARIABLE_NAME='Ssl_cipher'").splitlines()
    assert len(rows) == 4, f"expected four live daemon TLS sessions, found {len(rows)}"
    observed = set()
    for row in rows:
        connection, cipher = row.split("\t")
        assert cipher.startswith(("TLS_", "ECDHE-", "DHE-")), "empty daemon TLS cipher"
        queries = fixture.sql(f"SELECT GROUP_CONCAT(argument SEPARATOR ';') "
                             f"FROM mysql.general_log WHERE thread_id={int(connection)} "
                             "AND command_type='Query'")
        role = daemon_role(queries)
        assert role not in observed, "duplicate daemon role"
        observed.add(role)
        emit("actual-daemon-session", consumer=role, connection=int(connection), cipher=cipher)
    assert observed == {"feeder", "transitioner", "sample_trivial_validator", "batch_collect_assimilator"}
    count = int(fixture.sql(
        "SELECT COUNT(*) FROM mysql.general_log WHERE command_type='Connect' "
        "AND argument LIKE '%using SSL/TLS%'"))
    assert count >= 8, "missing independently observed TLS web/daemon connections"
    emit("tls-required-connections", count=count)


def daemon_role(queries):
    compact = re.sub(r"\s+", "", queries.lower())
    for name, marker in (
        ("feeder", "fromplatformwheredeprecated=0"),
        ("transitioner", "wu.transition_time<"),
        ("sample_trivial_validator", "wu.need_validate>0"),
        ("batch_collect_assimilator", "assimilate_state=1"),
    ):
        if marker in compact:
            return name
    raise RuntimeError("live TLS connection lacks an actual daemon-specific query")


def daemon_tls_rejection(result):
    return bool(result.returncode and re.search(
        r"(?m)^mysql_real_connect\(\): 2026: TLS/SSL error: [^\n]+$",
        result.stderr + result.stdout))


def copied_consumers(case, succeeds, host="tls-db.test"):
    for name, command in (
        ("python", ["python3", "/checks/consumer.py"]),
        ("php-legacy", ["php", "/checks/consumer.php", "--api", "legacy"]),
        ("php-DbConn", ["php", "/checks/consumer.php", "--api", "class"]),
    ):
        result = subprocess.run(
            [*command, "--host", host, "--database", "full_fixture", "--project-dir", str(PROJECT),
             "--user", "runtime"],
            capture_output=True, text=True, timeout=20)
        if succeeds:
            cipher = result.stdout.strip()
            assert not result.returncode and cipher.startswith(("TLS_", "ECDHE-", "DHE-"))
            emit(case, consumer="copied-" + name, cipher=cipher, outcome="encrypted")
        else:
            assert fixture.tls_rejection(case, name, result), f"{case}/{name}: not a TLS rejection"
            emit(case, consumer="copied-" + name, outcome="rejected")


def negatives():
    copied_consumers("valid-copied-consumers", True)
    plaintext = subprocess.run(
        ["mariadb", "--no-defaults", "--protocol=tcp", "--host=tls-db.test", "--skip-ssl",
         "-uruntime", "-pfixture-only", "-e", "SELECT 1"],
        capture_output=True, text=True, timeout=10)
    assert fixture.sql("SELECT @@require_secure_transport") == "1"
    assert plaintext.returncode and plaintext.stderr.startswith("ERROR 1045 (28000):")
    emit("plaintext-client", outcome="rejected")
    config = Path("/dev/shm/boinc/config.xml")
    original = config.read_text()
    for case in ("wrong-host", "wrong-ca"):
        # Keep the previously authenticated feeder alive: otherwise stock CGI
        # short-circuits before attempting its own new database connection.
        start_services()
        stop_apache = PROCESSES.pop(0)
        stop_apache.terminate()
        stop_apache.wait(timeout=15)
        host = "wrong-db.test" if case == "wrong-host" else "tls-db.test"
        if case == "wrong-host":
            config.write_text(original.replace("tls-db.test", host))
        else:
            shutil.copyfile("/dbfixture/wrong-ca.pem", fixture.CA)
        try:
            copied_consumers(case, False, host)
            start_apache(database_failure=True)
            scheduler_rpc(case)
            for name, args in DAEMONS:
                result = subprocess.run([str(PROJECT / "bin" / name), *args], cwd=PROJECT,
                                        capture_output=True, text=True, timeout=20,
                                        user="boincadm", group="boincadm")
                assert daemon_tls_rejection(result), (
                    f"{case}/{name}: not a confirmed TLS rejection: {result.stderr[-300:]}")
                emit(case, consumer=name, outcome="rejected")
            stop_services()
        finally:
            stop_services()
            config.write_text(original)
            shutil.copyfile("/dbfixture/ca.pem", fixture.CA)
        copied_consumers(case + "-recovery", True)
    start_services()
    web()
    connections()
    stop_services()


def stop_services():
    for process in reversed(PROCESSES):
        if process.poll() is None:
            process.terminate()
    for process in PROCESSES:
        try:
            process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)
    PROCESSES.clear()


def rehearse_code_migration():
    global PROJECT
    original = PROJECT
    copied = ROOT / "migration-copy"
    shutil.copytree(original, copied, symlinks=True)
    run("chown", "-R", "--no-dereference", "boincadm:boincadm", str(copied))
    for name in ("stop", "status"):
        link = copied / "bin" / name
        if link.is_symlink():
            link.unlink()
            link.symlink_to(copied / "bin/start")
    try:
        PROJECT = copied
        # Simulate stale copied libraries without replacing their connection implementation.
        stale = copied / "py/Boinc/db_base.py"
        stale.write_bytes(stale.read_bytes() + b"\n# copied-project stale version fixture\n")
        binary = copied / "bin/transitioner"
        binary.write_bytes(binary.read_bytes() + b"\nstale copied binary fixture\n")
        before = {str(p.relative_to(copied)): hashlib.sha256(p.read_bytes()).hexdigest()
                  for tree in ("bin", "cgi-bin", "py", "html/inc")
                  for p in (copied / tree).rglob("*") if p.is_file() and not p.is_symlink() and p.suffix != ".pyc" and "__pycache__" not in p.parts}
        state = {name: hashlib.sha256((copied / name).read_bytes()).hexdigest()
                 for name in ("config.xml", "project.xml", "plan_class_spec.xml")}
        backup = ROOT / "migration-code-backup"
        helper = str(TEMPLATES / "refresh-project-code.py")
        run("python3", helper, "apply", "--project", str(copied), "--backup", str(backup), "--ack-server-stopped")
        copied_consumers("migration-copied-consumers", True)
        start_services()
        web()
        connections()
        stop_services()
        manifest = json.loads((backup / "manifest.json").read_text())
        assert all(hashlib.sha256((copied / row["path"]).read_bytes()).hexdigest() == row["after"]
                   for row in manifest["files"] if "after" in row)
        run("python3", helper, "rollback", "--project", str(copied), "--backup", str(backup), "--ack-server-stopped")
        assert all(hashlib.sha256((copied / rel).read_bytes()).hexdigest() == value for rel, value in before.items())
        assert all(hashlib.sha256((copied / name).read_bytes()).hexdigest() == value for name, value in state.items())
        copied_consumers("rollback-copied-consumers", True)
        start_services()
        web()
        connections()
        stop_services()
        emit("copied-project-code-migration-rollback", inventoried=len(manifest["files"]), unchanged_state=len(state))
    finally:
        stop_services()
        PROJECT = original


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--phase", required=True, choices=("init", "restart"))
    args = parser.parse_args()
    os.environ.update(KEY_VAULT_NAME="unused-local", DB_HOST="tls-db.test", DB_NAME="full_fixture",
                      DB_RUNTIME_USER="runtime", MASTER_HOST="localhost", PROJECT_DIR="/fixture/share",
                      PROJECT="usnmocr")
    setup()
    emit("runtime", server_image=os.environ["ACCEPTANCE_SERVER_IMAGE"],
         require_secure_transport=fixture.sql("SELECT @@require_secure_transport"),
         database_version=fixture.sql("SELECT VERSION()"))
    try:
        if args.phase == "init":
            init_project()
            rehearse_code_migration()
            hashes = {str(p.relative_to(PROJECT)): hashlib.sha256(p.read_bytes()).hexdigest()
                      for p in (PROJECT / "html/inc/db.inc", PROJECT / "html/inc/db_conn.inc",
                                PROJECT / "py/Boinc/db_base.py", PROJECT / "bin/feeder")}
            (ROOT / "copied-hashes.json").write_text(json.dumps(hashes))
        else:
            # Runtime secrets are ephemeral, unlike the project share.
            Path("/dev/shm/boinc/keys").mkdir(parents=True)
            for name in ("upload_private", "upload_public", "code_sign_public"):
                shutil.copyfile(Path("/fixture-keys") / name, Path("/dev/shm/boinc/keys") / name)
            secure_keys()
            run("bash", "-c", "mkdir -p /dev/shm/boinc/keys; printf fixture-only > /dev/shm/boinc/db-runtime; "
                ". /usr/local/lib/usnm-boinc/lib.sh; render_config")
        (PROJECT / ".usnm-initialised").touch()
        run("bash", "-c", "set -- --ensure-project; . /usr/local/lib/usnm-boinc/entrypoint.sh; project_tree")
        start_services()
        web()
        connections()
        stop_services()
        negatives()
        hashes = json.loads((ROOT / "copied-hashes.json").read_text())
        assert all(hashlib.sha256((PROJECT / p).read_bytes()).hexdigest() == digest
                   for p, digest in hashes.items())
        emit("persisted-project-" + args.phase, unchanged_copied_files=len(hashes))
        emit("result", outcome="PASS", scope="local image full project, not Azure or upgrade rollback")
    finally:
        stop_services()


if __name__ == "__main__":
    main()
