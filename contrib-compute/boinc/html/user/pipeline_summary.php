<?php
// The pipeline dashboard's data (pipeline.html): what is waiting at each stage, per app.
// Aggregate counts and times only: no host, account, workunit or result identities, as
// work_summary.php. Cached for 120 s; every query is bounded by MAX_EXECUTION_TIME.
header("Content-Type: application/json; charset=utf-8");
header("Access-Control-Allow-Origin: *");
header("Cache-Control: public, max-age=120");
header("X-Content-Type-Options: nosniff");
$generating_xml = true;
set_error_handler(function ($severity, $message, $filename, $line) {
    if (!(error_reporting() & $severity)) {
        return false;
    }
    throw new ErrorException($message, 0, $severity, $filename, $line);
});

// The rows of one bounded query, or an exception.
function pipeline_rows($db, $sql) {
    $rows = $db->do_query($sql);
    if (!$rows) {
        throw new RuntimeException("Pipeline query failed");
    }
    $out = [];
    while ($row = $rows->fetch_assoc()) {
        $out[] = $row;
    }
    return $out;
}

function pipeline_int($value) {
    return $value === null ? 0 : (int) $value;
}

try {
    require_once("../inc/boinc_db.inc");
    require_once("../inc/cache.inc");
    if (web_stopped()) {
        throw new RuntimeException("Project down for maintenance");
    }
    $path = get_path("pipeline-v1", "pipeline_summary.php");
    $file = fopen($path, "c+");
    if (!$file || !flock($file, LOCK_EX | LOCK_NB)) {
        throw new RuntimeException("Pipeline cache busy or unavailable");
    }
    $cached = stream_get_contents($file);
    $data = $cached ? json_decode($cached, true) : null;
    $timestamp = is_array($data) && is_string($data["updated_at"] ?? null)
        ? strtotime($data["updated_at"]) : false;
    if ($cached && (!is_array($data) || ($data["schema"] ?? null) !== 1 ||
            !is_array($data["apps"] ?? null) || $timestamp === false)) {
        error_log("BOINC pipeline summary: invalid cache; refreshing from database");
        $data = null;
    }
    if (!$data || time() - $timestamp >= 120) {
        BoincDb::get_aux(0);
        $db = BoincDb::$instance;
        if (!$db) {
            throw new RuntimeException("Pipeline database unavailable");
        }
        $now = time();
        // Results by stage. Stranded: in progress on a host not heard from in a day, which
        // waits out its deadline before BOINC reissues it. Overdue: in progress past its report
        // deadline. Awaiting validation: a successful result whose quorum partner hasn't
        // returned yet.
        $apps = [];
        foreach (pipeline_rows($db, "
            SELECT /*+ MAX_EXECUTION_TIME(5000) */ a.id, a.name,
                SUM(r.server_state = 2) AS unsent,
                SUM(r.server_state = 4) AS in_progress,
                SUM(r.server_state = 4 AND r.report_deadline < $now) AS overdue,
                SUM(r.server_state = 4 AND (h.rpc_time IS NULL OR h.rpc_time < $now - 86400)) AS stranded,
                SUM(r.server_state = 5 AND r.outcome = 1 AND r.validate_state = 0) AS awaiting_validation,
                SUM(r.server_state = 5 AND r.received_time > $now - 86400) AS returned_24h,
                SUM(r.server_state = 5 AND r.received_time > $now - 86400 AND r.outcome <> 1) AS errors_24h,
                SUM(r.server_state = 5 AND r.outcome = 1 AND r.validate_state = 1) AS validated,
                MAX(r.received_time) AS last_returned
            FROM app a LEFT JOIN result r ON r.appid = a.id LEFT JOIN host h ON h.id = r.hostid
            GROUP BY a.id, a.name ORDER BY a.id
        ") as $row) {
            $last = pipeline_int($row["last_returned"]);
            $apps[(int) $row["id"]] = [
                "name" => $row["name"],
                "unsent" => pipeline_int($row["unsent"]),
                "in_progress" => pipeline_int($row["in_progress"]),
                "overdue" => pipeline_int($row["overdue"]),
                "stranded" => pipeline_int($row["stranded"]),
                "awaiting_validation" => pipeline_int($row["awaiting_validation"]),
                "awaiting_assimilation" => 0,
                "returned_24h" => pipeline_int($row["returned_24h"]),
                "errors_24h" => pipeline_int($row["errors_24h"]),
                "validated" => pipeline_int($row["validated"]),
                "last_returned_at" => $last > 0 ? gmdate("Y-m-d\TH:i:s\Z", $last) : null,
                "returned_by_hour" => array_fill(0, 24, 0),
            ];
        }
        // Workunits validated but not yet assimilated (collected into the project's outputs).
        foreach (pipeline_rows($db, "
            SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid, COUNT(*) AS n
            FROM workunit WHERE assimilate_state = 1 GROUP BY appid
        ") as $row) {
            if (isset($apps[(int) $row["appid"]])) {
                $apps[(int) $row["appid"]]["awaiting_assimilation"] = pipeline_int($row["n"]);
            }
        }
        // Results returned per hour over the last day, oldest hour first.
        foreach (pipeline_rows($db, "
            SELECT /*+ MAX_EXECUTION_TIME(5000) */ appid,
                FLOOR(($now - received_time) / 3600) AS hours_ago, COUNT(*) AS n
            FROM result WHERE server_state = 5 AND received_time > $now - 86400
            GROUP BY appid, hours_ago
        ") as $row) {
            $ago = (int) $row["hours_ago"];
            if (isset($apps[(int) $row["appid"]]) && $ago >= 0 && $ago < 24) {
                $apps[(int) $row["appid"]]["returned_by_hour"][23 - $ago] = pipeline_int($row["n"]);
            }
        }
        $hosts = pipeline_rows($db, "
            SELECT /*+ MAX_EXECUTION_TIME(5000) */
                SUM(rpc_time > $now - 3600) AS active_1h, SUM(rpc_time > $now - 86400) AS active_24h
            FROM host
        ")[0] ?? [];
        $data = [
            "schema" => 1,
            "updated_at" => gmdate("Y-m-d\TH:i:s\Z", $now),
            "hosts" => [
                "active_1h" => pipeline_int($hosts["active_1h"] ?? null),
                "active_24h" => pipeline_int($hosts["active_24h"] ?? null),
            ],
            "apps" => array_values($apps),
        ];
        $cached = json_encode($data, JSON_THROW_ON_ERROR);
        rewind($file);
        if (!ftruncate($file, 0) || fwrite($file, $cached) !== strlen($cached) || !fflush($file)) {
            throw new RuntimeException("Pipeline cache write failed");
        }
    }
    flock($file, LOCK_UN);
    fclose($file);
    echo $cached;
} catch (Throwable $error) {
    error_log("BOINC pipeline summary refresh failed: " . get_class($error));
    http_response_code(503);
    header("Cache-Control: no-store");
    echo '{"error":"BOINC pipeline counts are temporarily unavailable."}';
} finally {
    restore_error_handler();
}
