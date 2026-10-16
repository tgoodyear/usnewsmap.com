"""TLS patch guards and connector wiring; live TLS acceptance is a separate dev gate."""
import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import types
import unittest

SPEC = importlib.util.spec_from_file_location("tls_patch", Path(__file__).with_name("patch-server-db-tls.py"))
patch = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(patch)


class DbTlsPatch(unittest.TestCase):
    def test_all_anchors_checked_before_any_write(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for relative, replacements in patch.PATCHES.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("\n".join(old for old, _ in replacements))
            result = patch.patched(root)
            self.assertEqual(len(result), 4)
            self.assertIn("MYSQL_OPT_SSL_ENFORCE", result[root / "db/db_base.cpp"])
            broken = root / "html/inc/db_conn.inc"
            original = (root / "py/Boinc/db_base.py").read_text()
            broken.write_text("upstream changed")
            with self.assertRaises(ValueError):
                patch.patched(root)
            self.assertEqual((root / "py/Boinc/db_base.py").read_text(), original)

    def test_python_enforces_options_and_rejects_empty_cipher(self):
        new = patch.PATCHES["py/Boinc/db_base.py"][0][1]
        calls = []
        closed = []

        class Connection:
            def cursor(self):
                return self

            def execute(self, query):
                self.query = query

            def fetchone(self):
                return {"Value": cipher}

            def close(self):
                closed.append(True)

        def connect(**kwargs):
            calls.append(kwargs)
            return Connection()

        ns = {
            "MySQLdb": types.SimpleNamespace(connect=connect, cursors=types.SimpleNamespace(DictCursor=dict)),
            "os": types.SimpleNamespace(path=types.SimpleNamespace(isfile=lambda _: True)),
        }
        exec("def run(db, host, port, user, passwd):\n    global dbconnection\n" + new, ns)
        cipher = "TLS_AES_256_GCM_SHA384"
        ns["run"]("db", "db.example.org", 3306, "user", "secret")
        self.assertEqual(calls[0]["ssl"], {"ca": patch.CA})
        self.assertEqual(calls[0]["read_default_file"], patch.OPTIONS)
        self.assertEqual(calls[0]["read_default_group"], "client")
        cipher = ""
        with self.assertRaisesRegex(RuntimeError, "not encrypted"):
            ns["run"]("db", "db.example.org", 3306, "user", "secret")
        self.assertIsNone(ns["dbconnection"])

    @unittest.skipUnless(shutil.which("php"), "PHP unavailable")
    def test_php_both_paths_share_verified_connection_and_reject_plaintext(self):
        script = """<?php
namespace DbTlsTest;
const MYSQLI_OPT_SSL_VERIFY_SERVER_CERT = 21;
const MYSQLI_CLIENT_SSL = 2048;
$cipher = 'cipher';
class Conn {
    function query($q) { return $this; }
    function fetch_row() { global $cipher; return ['Ssl_cipher', $cipher]; }
    function close() {}
}
function mysqli_init() { return new Conn; }
function mysqli_options($c, $o, $v) { return $o === 21 && $v === true; }
function mysqli_ssl_set($c, $k, $cert, $ca, $p, $cipher) {
    return $ca === '/usr/local/share/usnm/mysql-ca.pem';
}
function mysqli_real_connect($c, $h, $u, $p, $d, $port, $socket, $flags) {
    return $flags === MYSQLI_CLIENT_SSL && $h === 'db.example.org';
}
""" + patch.PHP_CONNECT + """
if (!usnm_mysql_tls_connect('db.example.org', 'u', 'p', 'db', 3306)) exit(1);
$cipher = '';
if (usnm_mysql_tls_connect('db.example.org', 'u', 'p', 'db', 3306)) exit(2);
"""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "check.php"
            path.write_text(script)
            subprocess.run(["php", "-n", str(path)], check=True, capture_output=True)

    @unittest.skipUnless(shutil.which("c++"), "C++ unavailable")
    def test_cpp_sets_and_checks_all_options_before_connection(self):
        options = patch.PATCHES["db/db_base.cpp"][0][1].split("    // CLIENT_FOUND_ROWS")[0]
        cipher = patch.PATCHES["db/db_base.cpp"][1][1]
        source = """
typedef bool my_bool;
const int MYSQL_OPT_SSL_CA=1, MYSQL_OPT_SSL_ENFORCE=2, MYSQL_OPT_SSL_VERIFY_SERVER_CERT=3;
const int ERR_DB_CANT_CONNECT=10;
int mysql=1, mysql2=1, fail=0, seen=0;
bool encrypted=true;
int mysql_options(int, int option, const void*) { seen |= 1 << option; return option==fail; }
void mysql_close(int) {}
const char* mysql_get_ssl_cipher(int) { return encrypted ? "cipher" : 0; }
int connect() {
""" + options + cipher + """
return 0;
}
int main() {
 if (connect() || seen != 14) return 1;
 fail=2; mysql=1;
 if (connect()!=10 || mysql!=0) return 2;
 fail=0; mysql=1; encrypted=false;
 if (connect()!=10 || mysql!=0) return 3;
 return 0;
}
"""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "check.cpp"
            path.write_text(source)
            binary = Path(tmp) / "check"
            subprocess.run(["c++", str(path), "-o", str(binary)], check=True, capture_output=True)
            subprocess.run([str(binary)], check=True)


if __name__ == "__main__":
    unittest.main()
