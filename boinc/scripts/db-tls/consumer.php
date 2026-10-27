<?php
// Invoke both original database APIs, including their patched call sites.
$options = getopt("", ["api:", "host:", "project-dir:", "database:", "user:"]);
chdir(($options["project-dir"] ?? "/opt/boinc")."/html/user");
require_once("../inc/db_conn.inc");
mysqli_report(MYSQLI_REPORT_ERROR | MYSQLI_REPORT_STRICT);
ini_set("display_errors", "0");
ini_set("log_errors", "0");
if (!isset($options["host"], $options["api"]) ||
    !in_array($options["api"], ["legacy", "class"], true)) exit(2);
try {
    if ($options["api"] === "legacy") {
        $conn = _mysql_connect($options["host"], $options["user"] ?? "fixture", "fixture-only", $options["database"] ?? "fixture");
    } else {
        $db = new DbConn;
        if (!$db->init_conn($options["user"] ?? "fixture", "fixture-only", $options["host"], $options["database"] ?? "fixture")) {
            fwrite(STDERR, json_encode(["mysql_errno" => mysqli_connect_errno(),
                "message" => mysqli_connect_error()]));
            exit(10);
        }
        $conn = $db->db_conn;
    }
} catch (mysqli_sql_exception $error) {
    fwrite(STDERR, json_encode(["mysql_errno" => $error->getCode(), "message" => $error->getMessage()]));
    exit(10);
}
if (!$conn) exit(11);
$row = $conn->query("SHOW SESSION STATUS LIKE 'Ssl_cipher'")->fetch_row();
if (!$row || !$row[1]) exit(12);
echo $row[1], "\n";
$conn->close();
