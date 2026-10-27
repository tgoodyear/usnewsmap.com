"""Native disposable DB for running an immutable amd64 BOINC image on ARM hosts."""
import os
from pathlib import Path
import shutil
import signal
import time

import fixture

os.umask(0o022)
root = Path("/fixture")
initial = not (root / "data/mysql").is_dir()
if initial:
    fixture.certificate("ca")
    fixture.certificate("wrong-ca")
    fixture.run("openssl", "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=tls-db.test",
                "-keyout", "/fixture/server.key", "-out", "/fixture/server.csr")
    (root / "extensions").write_text("subjectAltName=DNS:tls-db.test\nextendedKeyUsage=serverAuth\n")
    fixture.run("openssl", "x509", "-req", "-in", "/fixture/server.csr", "-CA", "/fixture/ca.pem",
                "-CAkey", "/fixture/ca.key", "-CAcreateserial", "-days", "1",
                "-extfile", "/fixture/extensions", "-out", "/fixture/server.pem")
    fixture.run("mariadb-install-db", "--no-defaults", "--datadir=/fixture/data",
                "--auth-root-authentication-method=normal", "--skip-test-db")
shutil.copyfile(root / "ca.pem", fixture.CA)
db = fixture.start(True, ("--performance-schema=ON", "--max-connections=32"))
if initial:
    fixture.sql("CREATE USER 'fixture'@'127.0.0.1' IDENTIFIED BY 'fixture-only'; "
                "CREATE USER 'runtime'@'127.0.0.1' IDENTIFIED BY 'fixture-only'; "
                "GRANT ALL ON *.* TO 'fixture'@'127.0.0.1' WITH GRANT OPTION")
fixture.sql("TRUNCATE TABLE mysql.general_log; "
            "SET GLOBAL log_output='TABLE'; SET GLOBAL general_log=ON;")
signal.signal(signal.SIGTERM, lambda *_: fixture.stop(db))
(root / "ready").touch()
while db.poll() is None:
    time.sleep(0.5)
