<?php
namespace Usnm\Boinc\VersionHold;
use BoincApp;
use BoincAppVersion;
use BoincDb;
use BoincPlatform;
// Hold (deprecate) or release (un-deprecate) one usnm_ocr or usnm_gold container app version (#1687)
// or native usnm_qwen app version (#1672),
// journaled before the change, as hold-legacy-qwen.php does for legacy Qwen. Or set usnm_ocr's
// min_version, which keeps every version from it up loaded. It never cancels results or
// rewrites workunits.
//
// The feeder loads, of each app's versions not deprecated in the database, every one at or
// above the app's min_version when it is set, and otherwise only the highest in its platform
// and plan class (stock sched_shmem.cpp). A workunit pinned with app_version_num gets only a
// loaded version of that number, so a change that stops a pinned number loading strands its
// unfinished workunits: this refuses that unless --accept-stranded. A usnm_gold release also needs
// the running server's gold-engine-platforms.json to bind the version's worker on its platform.
const APPS = ["usnm_ocr", "usnm_gold"];
// Native Qwen versions publish held too (#1672): a new platform waits for a real client run.
const QWEN_NATIVE_CLASSES = ["nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_2b", "intel_vulkan_8b",
                             "amd_vulkan_8b", "nvidia_cuda_8b", "nvidia_cuda_2b", "apple_gpu_8b", "mt"];
const BINDINGS = "/usr/local/lib/usnm-boinc/gold-engine-platforms.json";
$args = getopt("", ["project:", "app-version:", "action:", "min-version:", "journal-dir:",
                    "gold-bindings:", "accept-stranded", "apply"]);
$usage = "Usage: php app-version-hold.php --project PATH --journal-dir DIR"
    . " (--app-version ID --action hold|release | --action min-version --min-version N)"
    . " [--gold-bindings FILE] [--accept-stranded] [--apply]\n";
$action = $args["action"] ?? "";
$valid = isset($args["project"], $args["journal-dir"]) && in_array($action, ["hold", "release", "min-version"], true)
    && ($action === "min-version"
        ? isset($args["min-version"]) && !isset($args["app-version"]) && preg_match('/^(0|[1-9][0-9]{0,8})$/D', $args["min-version"])
        : isset($args["app-version"]) && !isset($args["min-version"]) && preg_match('/^[1-9][0-9]{0,9}$/D', $args["app-version"]));
if (!$valid || !str_starts_with($args["journal-dir"], "/")) {
    fwrite(STDERR, $usage); exit(2);
}
$project = realpath($args["project"]);
if (!$project || !chdir("$project/html/inc")) {
    fwrite(STDERR, "Project not found\n"); exit(1);
}
require_once("boinc_db.inc");

function refuse($message) {
    fwrite(STDERR, "Refusing: $message\n"); exit(1);
}

// {"platform plan_class" => [version numbers loaded]} for the app's rows.
function loaded($rows, $minimum) {
    $groups = [];
    foreach ($rows as $row) {
        if (intval($row->deprecated)) continue;
        $groups[$row->platformid . " " . $row->plan_class][] = intval($row->version_num);
    }
    $out = [];
    foreach ($groups as $key => $numbers) {
        $out[$key] = $minimum ? array_values(array_filter($numbers, fn($n) => $n >= $minimum)) : [max($numbers)];
    }
    return $out;
}


// Unfinished workunits of the app pinned to each version number: {number => count}. Unfinished
// is anything that may still need a result sent: no canonical result and no error yet (unsent,
// in progress, awaiting validation or a retry), a validation pending, or not yet assimilated.
// A result that fails or times out gets a replacement, which needs the pinned version loaded.
function pinned_unfinished($appid) {
    $db = BoincDb::get();
    $result = $db->do_query("SELECT /*+ MAX_EXECUTION_TIME(10000) */ w.app_version_num AS n,"
        . " COUNT(*) AS c FROM workunit w"
        . " WHERE w.appid=" . intval($appid) . " AND w.app_version_num>0"
        . " AND (w.assimilate_state<>2 OR w.need_validate<>0 OR (w.canonical_resultid=0 AND w.error_mask=0))"
        . " GROUP BY w.app_version_num");
    if (!$result) refuse("pinned workunit read failed");
    $out = [];
    while ($row = $result->fetch_assoc()) $out[intval($row["n"])] = intval($row["c"]);
    return $out;
}

// The worker digest a usnm_gold version's Dockerfile pins, or null.
function gold_worker($project, $version) {
    if (!preg_match_all('/<file_ref>([\s\S]*?)<\/file_ref>/', $version->xml_doc, $refs)) return null;
    $names = [];
    foreach ($refs[1] as $ref) {
        if (preg_match('/<open_name>\s*Dockerfile\s*<\/open_name>/', $ref)
                && preg_match('/<file_name>\s*([A-Za-z0-9_.-]+)\s*<\/file_name>/', $ref, $name)) {
            $names[] = $name[1];
        }
    }
    if (count($names) !== 1) return null;
    $payload = @file_get_contents("$project/download/" . $names[0], false, null, 0, 4096);
    if ($payload === false || !preg_match('/^FROM ghcr\.io\/tgoodyear\/usnm-boinc-worker@(sha256:[0-9a-f]{64})\n?$/D', $payload, $m)) {
        return null;
    }
    return $m[1];
}

if ($action === "min-version") {
    $app = BoincApp::lookup("name='usnm_ocr'");
    if (!$app) refuse("no usnm_ocr app");
    $target = intval($args["min-version"]);
    $before = intval($app->min_version);
    $versions = BoincAppVersion::enum("appid=" . intval($app->id));
    $after = loaded($versions, $target);
    if ($target && count(array_filter($after, fn($numbers) => count($numbers) > 0)) === 0) {
        refuse("min_version $target would leave no usnm_ocr version loaded");
    }
    $record = ["action" => "min-version", "app" => "usnm_ocr", "app_id" => intval($app->id),
               "min_version_before" => $before, "min_version" => $target];
    $subject = "min-version-usnm_ocr";
    $change = fn() => $app->update("min_version=$target");
    $check = fn() => intval(BoincApp::lookup("name='usnm_ocr'")->min_version) === $target;
    $unchanged = $before === $target;
} else {
    $id = intval($args["app-version"]);
    $version = BoincAppVersion::lookup_id($id);
    $app = $version ? BoincApp::lookup_id(intval($version->appid)) : null;
    $container = $app && in_array($app->name, APPS, true) && in_array($version->plan_class, ["docker", "docker_lines"], true);
    $qwen = $app && $app->name === "usnm_qwen" && in_array($version->plan_class, QWEN_NATIVE_CLASSES, true);
    if (!$version || !$app || !($container || $qwen)) {
        // docker_lines: the Cherokee line-job versions (#1621), batch 1621 only.
        refuse("only usnm_ocr and usnm_gold container (docker or docker_lines) and native usnm_qwen app versions");
    }
    $platform = BoincPlatform::lookup_id(intval($version->platformid));
    if (!$platform) refuse("app version platform missing");
    $target = $action === "hold" ? 1 : 0;
    $versions = BoincAppVersion::enum("appid=" . intval($app->id));
    $after = [];
    foreach ($versions as $row) {
        $copy = clone $row;
        if (intval($copy->id) === $id) $copy->deprecated = $target;
        $after[] = $copy;
    }
    $after = loaded($after, intval($app->min_version));
    $record = ["action" => $action, "app" => $app->name, "app_version_id" => $id,
               "version_num" => intval($version->version_num), "platform" => $platform->name,
               "plan_class" => $version->plan_class, "deprecated_before" => intval($version->deprecated)];
    $subject = "$action-app-version-$id";
    $change = fn() => $version->update("deprecated=$target");
    $check = fn() => intval(BoincAppVersion::lookup_id($id)->deprecated) === $target;
    $unchanged = intval($version->deprecated) === $target;
    if ($app->name === "usnm_gold" && $action === "release") {
        $worker = gold_worker($project, $version);
        $file = $args["gold-bindings"] ?? BINDINGS;
        $bindings = json_decode(@file_get_contents($file, false, null, 0, 1048576), true);
        $record["worker"] = $worker;
        if (!$worker) refuse("can't read the worker this usnm_gold version's Dockerfile pins");
        if (!is_array($bindings) || !is_array($bindings[$worker] ?? null) || !is_string($bindings[$worker][$platform->name] ?? null)) {
            refuse("the running server's gold-engine-platforms.json has no $platform->name binding for $worker;"
                . " the scheduler would refuse every gold unit for this version. Switch the server image first");
        }
    }
}
// A pinned number loaded now on a platform and plan class that the change stops loading there:
// its unfinished workunits get no more results on those hosts (on 9 Nov, every Linux host).
$unsent = pinned_unfinished(intval($app->id));
$stranded = [];
$where = [];
foreach (loaded($versions, intval($app->min_version)) as $key => $numbers) {
    foreach ($numbers as $number) {
        if (isset($unsent[$number]) && !in_array($number, $after[$key] ?? [], true)) {
            $stranded[strval($number)] = $unsent[$number];
            $where[intval(explode(" ", $key)[0])] = true;
        }
    }
}
$platforms = array_map(fn($id) => BoincPlatform::lookup_id($id)->name ?? "platform $id", array_keys($where));
sort($platforms);
$record["stranded_workunits"] = (object)$stranded;
$record["stranded_platforms"] = $platforms;
$record["results_and_workunits_unchanged"] = true;
echo json_encode($record + ["apply" => isset($args["apply"]), "unchanged" => $unchanged]) . "\n";
if ($stranded && !isset($args["accept-stranded"])) {
    refuse("unfinished workunits pinned to version " . implode(", ", array_keys($stranded))
        . " (unsent, in progress, awaiting validation, a retry or assimilation) would get no more results on "
        . implode(", ", $platforms) . " hosts. Wait for them to finish, set usnm_ocr's min_version"
        . " (--action min-version), or pass --accept-stranded");
}
if (!isset($args["apply"])) {
    exit(0);
}
if ($unchanged) {
    // Already applied, perhaps by a run that stopped before the notice: no change and no new
    // journal, but the feeder is told again.
    if (!touch("$project/reread_db") || !file_exists("$project/reread_db")) {
        fwrite(STDERR, "Already applied, but the feeder notice (reread_db) failed; touch it by hand\n"); exit(1);
    }
    echo "Already applied: $subject; the feeder notice was sent again\n";
    exit(0);
}
$journal = $args["journal-dir"];
if (!is_dir($journal) && !mkdir($journal, 0700, true)) refuse("cannot create the operator journal");
if (is_link($journal)) refuse("linked journal directory");
$path = "$journal/$subject-" . gmdate("Ymd\THis\Z") . "-" . bin2hex(random_bytes(4)) . ".json";
$bytes = json_encode($record, JSON_PRETTY_PRINT) . "\n";
$handle = @fopen($path, "x");
if (!$handle) refuse("cannot exclusively create the pre-mutation journal $path");
$complete = flock($handle, LOCK_EX) && fwrite($handle, $bytes) === strlen($bytes) && fflush($handle) && fsync($handle);
fclose($handle);
if (!$complete) refuse("incomplete journal write; nothing changed");
if (!$change() || !$check()) {
    fwrite(STDERR, "Change failed or didn't verify; inspect $path\n"); exit(1);
}
if (!touch("$project/reread_db") || !file_exists("$project/reread_db")) {
    fwrite(STDERR, "Changed, but the feeder notice (reread_db) failed; rerun the same --apply, which sends it again\n"); exit(1);
}
echo "Done: $subject, journaled in $path; the feeder rereads the database\n";
