#!/usr/bin/env python3
"""Require verified TLS in BOINC 22bbebe's Python, PHP and MariaDB C clients."""
import argparse
from pathlib import Path

CA = "/usr/local/share/usnm/mysql-ca.pem"
OPTIONS = "/usr/local/share/usnm/mysql-tls.cnf"
PHP_CONNECT = f"""\
function usnm_mysql_tls_connect($host, $user, $password, $database, $port) {{
    $conn = mysqli_init();
    // mysqlnd rejects MYSQLI_OPT_SSL_VERIFY_SERVER_CERT; use its connection flag.
    if (!$conn ||
        !mysqli_ssl_set($conn, null, null, "{CA}", null, null) ||
        !mysqli_real_connect($conn, $host, $user, $password, $database, $port,
            null, MYSQLI_CLIENT_SSL | MYSQLI_CLIENT_SSL_VERIFY_SERVER_CERT)) {{
        return false;
    }}
    $status = $conn->query("SHOW SESSION STATUS LIKE 'Ssl_cipher'");
    $row = $status ? $status->fetch_row() : null;
    if (!$row || !$row[1]) {{
        $conn->close();
        return false;
    }}
    return $conn;
}}

"""

PATCHES = {
    "py/Boinc/db_base.py": [
        (
            "    dbconnection = MySQLdb.connect(db=db, host=host, port=port, user=user,\n"
            "        passwd=passwd, cursorclass=MySQLdb.cursors.DictCursor)",
            f"""\
    # Bookworm's mysqlclient cannot set MariaDB ssl_mode; its explicit option
    # file sets ssl-enforce and ssl-verify-server-cert before connecting.
    if not os.path.isfile("{OPTIONS}"):
        raise RuntimeError("required database TLS options are missing")
    dbconnection = MySQLdb.connect(db=db, host=host, port=port, user=user,
        passwd=passwd, cursorclass=MySQLdb.cursors.DictCursor,
        ssl={{'ca': '{CA}'}},
        read_default_file='{OPTIONS}', read_default_group='client')
    cursor = dbconnection.cursor()
    cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
    status = cursor.fetchone()
    cursor.close()
    if not status or not status['Value']:
        dbconnection.close()
        dbconnection = None
        raise RuntimeError("database connection is not encrypted")""",
        ),
    ],
    "html/inc/db.inc": [
        ('define("MYSQLI", true);', PHP_CONNECT + 'define("MYSQLI", true);'),
        (
            "$mysqli = @new mysqli($host, $user, $pass, $dbname, $port);",
            "$mysqli = usnm_mysql_tls_connect($host, $user, $pass, $dbname, $port);\n"
            "        if (!$mysqli) return null;",
        ),
    ],
    "html/inc/db_conn.inc": [
        (
            "$this->db_conn = @new mysqli($host, $user, $passwd, $name, $port);",
            "$this->db_conn = usnm_mysql_tls_connect($host, $user, $passwd, $name, $port);\n"
            "            if (!$this->db_conn) return false;",
        ),
    ],
    "db/db_base.cpp": [
        (
            "    // CLIENT_FOUND_ROWS means",
            f"""\
    // This image is built against Bookworm's MariaDB Connector/C, not Oracle's
    // MYSQL_OPT_SSL_MODE API. No option group is read by upstream DB_CONN.
    my_bool tls_required = 1;
    if (mysql_options(mysql, MYSQL_OPT_SSL_CA, "{CA}")
        || mysql_options(mysql, MYSQL_OPT_SSL_ENFORCE, &tls_required)
        || mysql_options(mysql, MYSQL_OPT_SSL_VERIFY_SERVER_CERT, &tls_required)) {{
        mysql_close(mysql);
        mysql = 0;
        return ERR_DB_CANT_CONNECT;
    }}

    // CLIENT_FOUND_ROWS means""",
        ),
        (
            "    mysql = mysql2;",
            """\
    mysql = mysql2;
    if (!mysql_get_ssl_cipher(mysql)) {
        mysql_close(mysql);
        mysql = 0;
        return ERR_DB_CANT_CONNECT;
    }""",
        ),
    ],
}


def patched(source: Path) -> dict[Path, str]:
    result = {}
    for relative, replacements in PATCHES.items():
        path = source / relative
        text = path.read_text()
        for old, new in replacements:
            if text.count(old) != 1:
                raise ValueError(f"{relative}: expected exactly one patch anchor")
            text = text.replace(old, new, 1)
        result[path] = text
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, type=Path)
    args = parser.parse_args()
    for path, text in patched(args.source).items():
        path.write_text(text)


if __name__ == "__main__":
    main()
