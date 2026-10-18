<?php
// Aggregate task counts only: no host, account, workunit or result identities.
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

try {
    require_once("../inc/boinc_db.inc");
    require_once("../inc/cache.inc");
    $path = get_path("aggregate-v1", "work_summary.php");
    $file = fopen($path, "c+");
    if (!$file || !flock($file, LOCK_EX | LOCK_NB)) {
        throw new RuntimeException("Summary cache busy or unavailable");
    }
    $cached = stream_get_contents($file);
    $data = $cached ? json_decode($cached, true) : null;
    $timestamp = is_array($data) && is_string($data["updated_at"] ?? null)
        ? strtotime($data["updated_at"]) : false;
    if ($cached && (!is_array($data) || ($data["schema"] ?? null) !== 1 ||
            !is_array($data["apps"] ?? null) || $timestamp === false)) {
        error_log("BOINC work summary: invalid cache; refreshing from database");
        $data = null;
    }
    if (!$data || time() - $timestamp >= 120) {
        $db = BoincDb::get();
        if (!$db) {
            throw new RuntimeException("Summary database unavailable");
        }
        $rows = $db->do_query("
            SELECT /*+ MAX_EXECUTION_TIME(5000) */ a.name,
                SUM(r.server_state = 2) AS ready,
                SUM(r.server_state = 4) AS in_progress,
                SUM(r.server_state = 5 AND r.outcome = 1 AND r.validate_state = 1) AS validated
            FROM app a LEFT JOIN result r ON r.appid = a.id
            GROUP BY a.id, a.name ORDER BY a.id
        ");
        if (!$rows) {
            throw new RuntimeException("Summary query failed");
        }
        $apps = [];
        while ($row = $rows->fetch_assoc()) {
            $apps[] = [
                "name" => $row["name"],
                "ready" => (int) $row["ready"],
                "in_progress" => (int) $row["in_progress"],
                "validated" => (int) $row["validated"],
            ];
        }
        $data = ["schema" => 1, "updated_at" => gmdate("Y-m-d\TH:i:s\Z"), "apps" => $apps];
        $cached = json_encode($data, JSON_THROW_ON_ERROR);
        rewind($file);
        if (!ftruncate($file, 0) || fwrite($file, $cached) !== strlen($cached) || !fflush($file)) {
            throw new RuntimeException("Summary cache write failed");
        }
    }
    flock($file, LOCK_UN);
    fclose($file);
    echo $cached;
} catch (Throwable $error) {
    error_log("BOINC work summary refresh failed: " . get_class($error));
    http_response_code(503);
    header("Cache-Control: no-store");
    echo '{"error":"BOINC task counts are temporarily unavailable."}';
} finally {
    restore_error_handler();
}
