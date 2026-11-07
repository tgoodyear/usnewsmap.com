<?php
namespace Usnm\Boinc\Hold;
use BoincApp;
use BoincAppVersion;
// Stop new legacy dispatch without cancelling results or rewriting workunits.
$args = getopt("", ["project:", "app-version:", "apply"]);
if (!isset($args["project"], $args["app-version"]) || !preg_match('/^[1-9][0-9]*$/D', $args["app-version"])) {
    fwrite(STDERR, "Usage: php hold-legacy-qwen.php --project PATH --app-version ID [--apply]\n");
    exit(2);
}
$project = realpath($args["project"]);
if (!$project || !chdir("$project/html/inc")) {
    fwrite(STDERR, "Project not found\n"); exit(1);
}
require_once("boinc_db.inc");
$id = intval($args["app-version"]);
$version = BoincAppVersion::lookup_id($id);
$app = BoincApp::lookup("name='usnm_qwen'");
if (!$version || !$app || intval($version->appid) !== intval($app->id)
    || !in_array($version->plan_class, ["docker_mt", "docker_nvidia_cuda"], true)) {
    fwrite(STDERR, "Refusing: not a legacy Qwen container app version\n"); exit(1);
}
$before = [
    "app_version_id" => $id, "app" => "usnm_qwen",
    "version_num" => intval($version->version_num), "plan_class" => $version->plan_class,
    "deprecated_before" => intval($version->deprecated),
    "action" => isset($args["apply"]) ? "hold" : "preview",
    "results_and_workunits_unchanged" => true,
];
echo json_encode($before)."\n";
if (!isset($args["apply"])) {
    exit(0);
}
$journal = "$project/ops-issue-559";
if (!is_dir($journal) && !mkdir($journal, 0700)) {
    fwrite(STDERR, "Cannot create operator journal\n"); exit(1);
}
$record = "$journal/hold-app-version-$id.json";
if (is_link($record)) {
    fwrite(STDERR, "Refusing linked journal\n"); exit(1);
}
if (!file_exists($record)) {
    $bytes = json_encode($before, JSON_PRETTY_PRINT)."\n";
    $handle = fopen($record, "x");
    if (!$handle) {
        fwrite(STDERR, "Cannot exclusively create pre-mutation journal\n"); exit(1);
    }
    $complete = flock($handle, LOCK_EX)
        && fwrite($handle, $bytes) === strlen($bytes)
        && fflush($handle) && fsync($handle);
    fclose($handle);
    if (!$complete) {
        fwrite(STDERR, "Incomplete journal write; mutation refused\n"); exit(1);
    }
}
$saved = json_decode(file_get_contents($record, false, null, 0, 16384), true);
$valid = is_array($saved) && count($saved) === count($before);
foreach ($before as $key => $value) {
    if ($key === "deprecated_before") {
        $valid = $valid && in_array($saved[$key] ?? null, [0, 1], true);
    } else {
        $valid = $valid && ($saved[$key] ?? null) === $value;
    }
}
if (!$valid) {
    fwrite(STDERR, "Invalid or mismatched pre-mutation journal; mutation refused\n"); exit(1);
}
$handle = fopen($record, "r+");
if (!$handle) {
    fwrite(STDERR, "Cannot reopen journal for synchronization\n"); exit(1);
}
$durable = flock($handle, LOCK_EX) && fsync($handle);
fclose($handle);
if (!$durable) {
    fwrite(STDERR, "Journal synchronization failed; mutation refused\n"); exit(1);
}
if (!intval($version->deprecated) && !$version->update("deprecated=1")) {
    fwrite(STDERR, "Failed to deprecate app version\n"); exit(1);
}
$after = BoincAppVersion::lookup_id($id);
if (!$after || intval($after->deprecated) !== 1 || !touch("$project/reread_db")) {
    fwrite(STDERR, "Hold verification or feeder notification failed; inspect journal\n"); exit(1);
}
echo "Legacy app version held; existing results, workunits and quorum preserved\n";
