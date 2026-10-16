<?php
// Invoke both original database APIs, including their patched call sites.
chdir("/opt/boinc/html/user");
require_once("../inc/db_conn.inc");
mysqli_report(MYSQLI_REPORT_ERROR | MYSQLI_REPORT_STRICT);
if ($argv[1] === "legacy") {
    $conn = _mysql_connect($argv[2], "fixture", "fixture-only", "fixture");
} else {
    $db = new DbConn;
    if (!$db->init_conn("fixture", "fixture-only", $argv[2], "fixture")) {
        fwrite(STDERR, mysqli_connect_error() ?? "");
        exit(10);
    }
    $conn = $db->db_conn;
}
if (!$conn) exit(11);
$row = $conn->query("SHOW SESSION STATUS LIKE 'Ssl_cipher'")->fetch_row();
if (!$row || !$row[1]) exit(12);
echo $row[1], "\n";
$conn->close();
