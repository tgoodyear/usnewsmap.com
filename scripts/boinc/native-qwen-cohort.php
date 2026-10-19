<?php
// Operator-only dispatch window; never changes account preferences or scientific rows.
// shared_8b opens Intel/NVIDIA Vulkan classes over the same audited, bounded workunit range.
const NATIVE_QWEN_CLASSES = ["nvidia_cuda_2b", "nvidia_cuda_8b", "intel_vulkan_8b", "nvidia_vulkan_8b", "amd_vulkan_8b"];
const SHARED_QWEN_CLASSES = ["intel_vulkan_8b", "nvidia_vulkan_8b"];

function native_qwen_cohort_xml($xml, $plan_class, $workunit_id, $last_id = null) {
    // AMD has an independent one-workunit canary. Neither path resets the other's windows.
    $classes = $plan_class === "amd_vulkan_8b" ? ["amd_vulkan_8b"]
        : array_values(array_diff(NATIVE_QWEN_CLASSES, ["amd_vulkan_8b"]));
    if ($plan_class === "shared_8b") $classes[] = "docker_nvidia_cuda_8b";
    $last_id = $last_id ?? $workunit_id;
    if ((!in_array($plan_class, $classes, true) && $plan_class !== "shared_8b")
        || $workunit_id < 0 || $workunit_id === 1 || $last_id < $workunit_id
        || $last_id - $workunit_id >= 10
        || ($plan_class !== "shared_8b" && $last_id !== $workunit_id)) {
        throw new RuntimeException("Unsupported native cohort");
    }
    $doc = new DOMDocument();
    if (!$doc->loadXML($xml, LIBXML_NONET)) throw new RuntimeException("Invalid plan XML");
    $xpath = new DOMXPath($doc);
    foreach ($classes as $name) {
        $nodes = $xpath->query("//plan_class[name='$name']");
        if ($nodes->length !== 1) throw new RuntimeException("Missing or duplicate native class");
        $node = $nodes->item(0);
        if ($xpath->query("project_prefs_tag", $node)->length) {
            throw new RuntimeException("Deploy project-participation policy first");
        }
        foreach (["min_wu_id", "max_wu_id"] as $tag) {
            $fields = $xpath->query($tag, $node);
            if ($fields->length === 0 && $name === "docker_nvidia_cuda_8b") {
                $node->appendChild($doc->createElement($tag));
                $fields = $xpath->query($tag, $node);
            }
            if ($fields->length !== 1) throw new RuntimeException("Missing bounded cohort field");
            $open = $workunit_id > 1 && ($name === $plan_class
                || ($plan_class === "shared_8b" && in_array($name, SHARED_QWEN_CLASSES, true)));
            $fields->item(0)->nodeValue = $open
                ? ($tag === "min_wu_id" ? $workunit_id : $last_id)
                : ($tag === "min_wu_id" ? 2 : 1);
        }
    }
    return $doc->saveXML();
}

function native_qwen_cohort_validate($workunit, $results, $plan_class) {
    if (!in_array($plan_class, NATIVE_QWEN_CLASSES, true)) {
        throw new RuntimeException("Unsupported native class");
    }
    $model = $plan_class === "nvidia_cuda_2b" ? "2b" : "8b";
    $batch = $model === "2b" ? 2 : 8;
    $disk = $model === "2b" ? 12884901888 : 19327352832;
    if (intval($workunit->batch) !== $batch || intval($workunit->rsc_disk_bound) !== $disk
        || intval($workunit->min_quorum) !== 2 || intval($workunit->target_nresults) !== 2
        || intval($workunit->canonical_resultid) !== 0 || intval($workunit->error_mask) !== 0
        || ($plan_class === "amd_vulkan_8b" ? (
            count($results) !== 0 || intval($workunit->transitioner_flags ?? 0) !== 2
            || intval($workunit->transition_time ?? 0) <= time()
            || intval($workunit->need_validate ?? 0) !== 0
        ) : count($results) !== 2)) {
        throw new RuntimeException("Expected unaccepted native quorum-2 work; AMD requires targeted staging without results");
    }
    foreach ($results as $result) {
        if (intval($result->server_state) !== 2 || intval($result->sent_time) !== 0) {
            throw new RuntimeException("Enable requires two unsent results; use --stop to close any campaign");
        }
    }
}

function native_qwen_cohort_write($project, $plan_class, $workunit_id, $apply, $last_id = null) {
    $path = "$project/plan_class_spec.xml";
    if (is_link($path)) throw new RuntimeException("Refusing symlinked plan XML");
    $lock_path = "$project/.native-qwen-cohort.lock";
    if (is_link($lock_path)) throw new RuntimeException("Refusing symlinked cohort lock");
    $lock = fopen($lock_path, "c");
    if (!$lock || !flock($lock, LOCK_EX)) throw new RuntimeException("Cannot lock cohort policy");
    $staged = null;
    try {
        $original = file_get_contents($path);
        if ($original === false) throw new RuntimeException("Cannot read plan XML");
        $updated = native_qwen_cohort_xml($original, $plan_class, $workunit_id, $last_id);
        $proof = [
            "plan_class" => $plan_class, "workunit_id" => $workunit_id,
            "last_workunit_id" => $last_id ?? $workunit_id,
            "other_managed_class_closed" => true,
            "independent_amd_window" => true, "apply" => $apply,
            "before_sha256" => hash("sha256", $original),
            "after_sha256" => hash("sha256", $updated),
        ];
        if ($apply && $original !== $updated) {
            $journal = "$project/ops-issue-559/native-cohort";
            if (is_link("$project/ops-issue-559") || is_link($journal)) {
                throw new RuntimeException("Refusing symlinked journal directory");
            }
            if (!is_dir($journal) && !mkdir($journal, 0700, true)) throw new RuntimeException("Cannot create journal");
            $id = bin2hex(random_bytes(16));
            $record = fopen("$journal/$id.json", "x");
            if (!$record) throw new RuntimeException("Cannot create journal");
            $bytes = json_encode($proof + ["original_xml" => $original], JSON_PRETTY_PRINT)."\n";
            if (fwrite($record, $bytes) !== strlen($bytes) || !fflush($record) || !fsync($record)) {
                fclose($record);
                throw new RuntimeException("Cannot persist original policy");
            }
            fclose($record);
            $staged = "$project/.native-qwen-cohort-$id";
            $stream = fopen($staged, "x");
            if (!$stream) throw new RuntimeException("Cannot stage cohort policy");
            if (fwrite($stream, $updated) !== strlen($updated) || !fflush($stream) || !fsync($stream)) {
                fclose($stream);
                throw new RuntimeException("Cannot persist cohort policy");
            }
            fclose($stream);
            if (!chmod($staged, fileperms($path) & 0777)) throw new RuntimeException("Cannot preserve plan permissions");
            if (hash_file("sha256", $path) !== $proof["before_sha256"]) throw new RuntimeException("Concurrent plan change");
            if (!rename($staged, $path)) throw new RuntimeException("Cannot replace cohort policy");
            $staged = null;
            if (hash_file("sha256", $path) !== $proof["after_sha256"]) throw new RuntimeException("Policy readback mismatch");
            $proof["journal"] = "$journal/$id.json";
        }
        return $proof;
    } finally {
        if ($staged !== null) unlink($staged);
        flock($lock, LOCK_UN);
        fclose($lock);
    }
}

function native_qwen_cohort_main() {
    $options = getopt("", ["project:", "db-project:", "plan-class:", "workunit:", "workunit-end:", "stop", "apply"]);
    $project = isset($options["project"]) ? realpath($options["project"]) : false;
    $db_project = isset($options["db-project"]) ? realpath($options["db-project"]) : $project;
    $plan_class = $options["plan-class"] ?? "nvidia_cuda_2b";
    $stop = array_key_exists("stop", $options);
    if (!$project || !is_dir($project) || !$db_project || !is_file("$db_project/config.xml")
        || (!in_array($plan_class, NATIVE_QWEN_CLASSES, true) && $plan_class !== "shared_8b")
        || (!$stop && !preg_match('/^[1-9][0-9]*$/', $options["workunit"] ?? ""))
        || (isset($options["workunit-end"]) && !preg_match('/^[1-9][0-9]*$/', $options["workunit-end"]))
        || ($stop && (isset($options["workunit"]) || isset($options["workunit-end"])))) {
        throw new RuntimeException("usage: --project PATH [--db-project PATH] [--stop | --plan-class ".implode("|", NATIVE_QWEN_CLASSES)."|shared_8b --workunit ID [--workunit-end ID]] [--apply]");
    }
    $id = $stop ? 0 : intval($options["workunit"]);
    $last_id = isset($options["workunit-end"]) ? intval($options["workunit-end"]) : $id;
    if ($last_id < $id || $last_id - $id >= 10
        || ($plan_class !== "shared_8b" && $last_id !== $id)) {
        throw new RuntimeException("Only shared_8b supports a contiguous range of at most ten workunits");
    }
    if (!$stop) {
        if ($id <= 1) throw new RuntimeException("Use a newly staged workunit ID greater than 1");
        chdir("$db_project/html/inc");
        require_once("boinc_db.inc");
        for ($current = $id; $current <= $last_id; $current++) {
            $wu = BoincWorkunit::lookup_id($current);
            $app = $wu ? BoincApp::lookup_id($wu->appid) : null;
            if (!$wu || !$app || $app->name !== "usnm_qwen") throw new RuntimeException("Not a Qwen workunit");
            native_qwen_cohort_validate($wu, BoincResult::enum("workunitid=$current"),
                $plan_class === "shared_8b" ? "nvidia_vulkan_8b" : $plan_class);
        }
    }
    echo json_encode(native_qwen_cohort_write(
        $project, $plan_class, $id, array_key_exists("apply", $options), $last_id
    ), JSON_PRETTY_PRINT)."\n";
}

if (realpath($_SERVER["SCRIPT_FILENAME"] ?? "") === __FILE__) {
    try {
        umask(0077);
        native_qwen_cohort_main();
    } catch (Throwable $error) {
        fwrite(STDERR, $error->getMessage()."\n");
        exit(1);
    }
}
