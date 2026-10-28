<?php
// Operator-only dispatch window; never changes account preferences or scientific rows.
// shared_8b opens Intel/NVIDIA Vulkan classes over the same audited, bounded workunit range.
const NATIVE_QWEN_CLASSES = ["nvidia_cuda_2b", "nvidia_cuda_8b", "intel_vulkan_8b", "nvidia_vulkan_8b", "amd_vulkan_8b"];
const SHARED_QWEN_CLASSES = ["intel_vulkan_8b", "nvidia_vulkan_8b"];
const TIEBREAKER_QWEN_CLASSES = ["intel_vulkan_8b", "nvidia_vulkan_8b", "shared_8b"];
const AMD_GENERAL_MARKER = "usnm-amd-general-admission-v1";
// General admission: the class takes any of the app's work, with no workunit bounds. The
// marker records the operator's choice, so a plan-policy refresh keeps it (refresh-plan-policy.py).
// nvidia_vulkan_8b qualified on usnm-rtx-compute's 17 audited units (13 valid, none failed).
const GENERAL_MARKERS = [
    "amd_vulkan_8b" => AMD_GENERAL_MARKER,
    "nvidia_vulkan_8b" => "usnm-nvidia-general-admission-v1",
];

function native_qwen_general_xml($xml, $plan_class = "amd_vulkan_8b") {
    if (!isset(GENERAL_MARKERS[$plan_class])) throw new RuntimeException("No general admission for $plan_class");
    $marker = GENERAL_MARKERS[$plan_class];
    $doc = new DOMDocument();
    if (!$doc->loadXML($xml, LIBXML_NONET)) throw new RuntimeException("Invalid plan XML");
    $xpath = new DOMXPath($doc);
    $nodes = $xpath->query("//plan_class[name='$plan_class']");
    if ($nodes->length !== 1) throw new RuntimeException("Missing or duplicate $plan_class class");
    $node = $nodes->item(0);
    if ($xpath->query("project_prefs_tag", $node)->length) throw new RuntimeException("Unexpected $plan_class account gate");
    $markers = $xpath->query("comment()[normalize-space(.)='$marker']", $node);
    $low = $xpath->query("min_wu_id", $node);
    $high = $xpath->query("max_wu_id", $node);
    if ($markers->length === 1 && $low->length === 0 && $high->length === 0) return $xml;
    if ($markers->length !== 0 || $low->length !== 1 || $high->length !== 1
        || !preg_match('/^[1-9][0-9]*$/D', $low->item(0)->nodeValue)
        || !preg_match('/^[1-9][0-9]*$/D', $high->item(0)->nodeValue)
        || !(($low->item(0)->nodeValue === "2" && $high->item(0)->nodeValue === "1")
            || (intval($low->item(0)->nodeValue) > 1 && ($plan_class === "amd_vulkan_8b"
                // AMD's window is one workunit; NVIDIA's can be a shared_8b range (up to ten).
                ? $low->item(0)->nodeValue === $high->item(0)->nodeValue
                : intval($high->item(0)->nodeValue) >= intval($low->item(0)->nodeValue)
                    && intval($high->item(0)->nodeValue) - intval($low->item(0)->nodeValue) < 10)))) {
        throw new RuntimeException("Invalid $plan_class admission state");
    }
    $node->removeChild($low->item(0));
    $node->removeChild($high->item(0));
    $node->appendChild($doc->createComment($marker));
    return $doc->saveXML();
}

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
        $open_here = $workunit_id > 1 && ($name === $plan_class
            || ($plan_class === "shared_8b" && in_array($name, SHARED_QWEN_CLASSES, true)));
        if (isset(GENERAL_MARKERS[$name])) {
            $markers = $xpath->query("comment()[normalize-space(.)='".GENERAL_MARKERS[$name]."']", $node);
            if ($markers->length) {
                if ($markers->length !== 1 || $xpath->query("min_wu_id|max_wu_id", $node)->length) {
                    throw new RuntimeException("Invalid explicit $name general admission");
                }
                // A window for another class leaves this one generally admitted; --stop, or a
                // window for this class itself, replaces general admission with bounds.
                if ($name !== "amd_vulkan_8b" && $workunit_id > 1 && !$open_here) continue;
                $node->removeChild($markers->item(0));
                $node->appendChild($doc->createElement("min_wu_id"));
                $node->appendChild($doc->createElement("max_wu_id"));
            }
        }
        foreach (["min_wu_id", "max_wu_id"] as $tag) {
            $fields = $xpath->query($tag, $node);
            if ($fields->length === 0 && $name === "docker_nvidia_cuda_8b") {
                $node->appendChild($doc->createElement($tag));
                $fields = $xpath->query($tag, $node);
            }
            if ($fields->length !== 1) throw new RuntimeException("Missing bounded cohort field");
            $fields->item(0)->nodeValue = $open_here
                ? ($tag === "min_wu_id" ? $workunit_id : $last_id)
                : ($tag === "min_wu_id" ? 2 : 1);
        }
    }
    return $doc->saveXML();
}

// Whether every class in this operation's scope, other than the ones it opens, is closed
// (min_wu_id 2, max_wu_id 1) in the resulting policy. A preserved general admission is open.
// AMD's window is its own scope, so an AMD operation has no other class to close.
function native_qwen_other_closed($xml, $plan_class) {
    if ($plan_class === "amd_vulkan_8b") return true;
    $scope = array_values(array_diff(NATIVE_QWEN_CLASSES, ["amd_vulkan_8b"]));
    $opened = $plan_class === "shared_8b" ? SHARED_QWEN_CLASSES : [$plan_class];
    $doc = new DOMDocument();
    if (!$doc->loadXML($xml, LIBXML_NONET)) throw new RuntimeException("Invalid plan XML");
    $xpath = new DOMXPath($doc);
    foreach (array_diff($scope, $opened) as $name) {
        $node = $xpath->query("//plan_class[name='$name']")->item(0);
        if (!$node || $xpath->query("min_wu_id", $node)->item(0)?->nodeValue !== "2"
            || $xpath->query("max_wu_id", $node)->item(0)?->nodeValue !== "1") return false;
    }
    return true;
}

// An AMD ordinary canary admits one ordinary (non-assignment) workunit to AMD exactly as the
// NVIDIA/Intel windows do: two unsent generic replicas, so its peer is that workunit's normal
// replica on any eligible host. The default AMD window still requires targeted staging.
// A tiebreaker admits Intel/NVIDIA to the one unsent replica the transitioner created after an
// inconclusive quorum. Earlier results stay untouched; the normal validator compares the new one,
// and <one_result_per_host_per_wu/> keeps it on a host other than the inconclusive ones.
function native_qwen_tiebreaker_validate($workunit, $results, $plan_class, $assignments = 0) {
    if (!in_array($plan_class, TIEBREAKER_QWEN_CLASSES, true)) {
        throw new RuntimeException("Tiebreaker is for intel_vulkan_8b, nvidia_vulkan_8b, or shared_8b only");
    }
    $unsent = 0;
    $inconclusive = 0;
    foreach ($results as $result) {
        if (intval($result->server_state) === 2 && intval($result->sent_time) === 0
            && intval($result->outcome) === 0 && intval($result->validate_state) === 0) {
            $unsent++;
        } elseif (intval($result->server_state) === 5 && intval($result->outcome) === 1
            && intval($result->validate_state) === 4) {
            $inconclusive++;
        } else {
            throw new RuntimeException("Tiebreaker requires every reported result to be a successful inconclusive");
        }
    }
    if (intval($workunit->batch) !== 8 || intval($workunit->rsc_disk_bound) !== 19327352832
        || intval($workunit->min_quorum) !== 2 || intval($workunit->target_nresults) !== count($results)
        || intval($workunit->canonical_resultid) !== 0 || intval($workunit->error_mask) !== 0
        || intval($workunit->transitioner_flags ?? -1) !== 0 || intval($workunit->need_validate ?? -1) !== 0
        || intval($workunit->app_version_num ?? -1) !== 0 || intval($assignments) !== 0
        || $unsent !== 1 || $inconclusive < 2) {
        throw new RuntimeException("Tiebreaker requires one unassigned native 8b quorum-2 workunit with "
            ."one unsent replica and only inconclusive successes");
    }
}

// BOINC enables <one_result_per_host_per_wu> only as a self-closing flag or a numeric true
// value; empty pairs, CDATA and nested markup leave it off, so accept only <tag/> or <tag>1</tag>.
function native_qwen_one_result_per_host($path) {
    $reader = new XMLReader();
    if (!$reader->open($path, null, LIBXML_NONET)) return false;
    $enabled = 0;
    $found = 0;
    $stack = [];
    while (@$reader->read()) {
        if ($reader->nodeType === XMLReader::END_ELEMENT) {
            array_pop($stack);
        } elseif ($reader->nodeType === XMLReader::ELEMENT) {
            $here = array_merge($stack, [$reader->name]);
            if ($reader->name === "one_result_per_host_per_wu") {
                $found++;
                if ($here === ["boinc", "config", "one_result_per_host_per_wu"]) {
                    if ($reader->isEmptyElement) {
                        $enabled++;
                    } else {
                        $inner = $reader->readInnerXml();
                        if (preg_match('/^\s*1\s*$/D', $inner)) $enabled++;
                    }
                }
            }
            if (!$reader->isEmptyElement) $stack = $here;
        }
    }
    $reader->close();
    // BOINC reads the raw token in the first <config> block, without entity expansion.
    $raw = file_get_contents($path);
    return $found === 1 && $enabled === 1 && $raw !== false
        && preg_match_all('/<one_result_per_host_per_wu[\s>\/]/', $raw) === 1
        && preg_match_all('/<config[\s>\/]/', $raw) === 1
        && preg_match('/<config>(?:(?!<\/config>).)*(?:<one_result_per_host_per_wu\s*\/>|'
            .'<one_result_per_host_per_wu>\s*1\s*<\/one_result_per_host_per_wu>)/s', $raw) === 1;
}

function native_qwen_cohort_validate($workunit, $results, $plan_class, $ordinary_canary = false, $assignments = 0) {
    if (!in_array($plan_class, NATIVE_QWEN_CLASSES, true)) {
        throw new RuntimeException("Unsupported native class");
    }
    if ($ordinary_canary && $plan_class !== "amd_vulkan_8b") {
        throw new RuntimeException("Ordinary canary is for amd_vulkan_8b only");
    }
    $model = $plan_class === "nvidia_cuda_2b" ? "2b" : "8b";
    $batch = $model === "2b" ? 2 : 8;
    $disk = $model === "2b" ? 12884901888 : 19327352832;
    if (intval($workunit->batch) !== $batch || intval($workunit->rsc_disk_bound) !== $disk
        || intval($workunit->min_quorum) !== 2 || intval($workunit->target_nresults) !== 2
        || intval($workunit->canonical_resultid) !== 0 || intval($workunit->error_mask) !== 0
        || ($ordinary_canary && (count($results) !== 2 || intval($assignments) !== 0
            || intval($workunit->transitioner_flags ?? -1) !== 0
            || intval($workunit->need_validate ?? -1) !== 0
            || intval($workunit->app_version_num ?? -1) !== 0))
        || (!$ordinary_canary && ($plan_class === "amd_vulkan_8b" ? (
            count($results) !== 0 || intval($workunit->transitioner_flags ?? 0) !== 2
            || intval($workunit->transition_time ?? 0) <= time()
            || intval($workunit->need_validate ?? 0) !== 0
        ) : count($results) !== 2))) {
        throw new RuntimeException($ordinary_canary
            ? "AMD ordinary canary requires one unassigned, unbound quorum-2 workunit with two unsent replicas"
            : "Expected unaccepted native quorum-2 work; AMD requires targeted staging without results");
    }
    foreach ($results as $result) {
        if (intval($result->server_state) !== 2 || intval($result->sent_time) !== 0) {
            throw new RuntimeException("Enable requires two unsent results; use --stop to close any campaign");
        }
    }
}

function native_qwen_reservation_guard($original, $updated) {
    $doc = new DOMDocument();
    if (!$doc->loadXML($original, LIBXML_NONET)) throw new RuntimeException("Invalid plan XML");
    $xpath = new DOMXPath($doc);
    $markers = $xpath->query("//comment()[contains(.,'usnm-amd-first-reservation-v1')]");
    if ($markers->length === 0) return;
    if ($markers->length !== 1 || $markers->item(0)->parentNode !== $doc->documentElement
        || !preg_match('/^usnm-amd-first-reservation-v1:[a-f0-9]{32}:([1-9][0-9]*):[1-9][0-9]*$/D',
            trim($markers->item(0)->nodeValue), $match)) {
        throw new RuntimeException("Invalid AMD-first reservation marker");
    }
    $reserved = intval($match[1]);
    $after = new DOMDocument();
    if (!$after->loadXML($updated, LIBXML_NONET)) throw new RuntimeException("Invalid updated plan XML");
    $new = new DOMXPath($after);
    foreach (array_merge(NATIVE_QWEN_CLASSES, ["docker_nvidia_cuda_8b"]) as $name) {
        $node = $new->query("//plan_class[name='$name']")->item(0);
        $low = $node ? $new->query("min_wu_id", $node) : null;
        $high = $node ? $new->query("max_wu_id", $node) : null;
        if (!$low || $low->length !== 1 || $high->length !== 1
            || !ctype_digit($low->item(0)->nodeValue) || !ctype_digit($high->item(0)->nodeValue)) {
            throw new RuntimeException("AMD-first lease requires bounded native classes");
        }
        $a = intval($low->item(0)->nodeValue);
        $b = intval($high->item(0)->nodeValue);
        $allowed = $name === "amd_vulkan_8b"
            ? (($a === 2 && $b === 1) || ($a === $reserved && $b === $reserved))
            : (($a === 2 && $b === 1) || (2 <= $a && $a <= $b && $b - $a < 10
                && !($a <= $reserved && $reserved <= $b)));
        if (!$allowed) throw new RuntimeException("Release the AMD-first lease before opening competing work");
    }
}

function native_qwen_cohort_write($project, $plan_class, $workunit_id, $apply, $last_id = null, $general = false,
        $ordinary_canary = false, $tiebreaker = false) {
    if ($tiebreaker && (!in_array($plan_class, TIEBREAKER_QWEN_CLASSES, true) || $general || $ordinary_canary
            || $workunit_id <= 1 || ($last_id !== null && $last_id !== $workunit_id))) {
        throw new RuntimeException("Tiebreaker is one intel_vulkan_8b, nvidia_vulkan_8b, or shared_8b workunit");
    }
    if ($ordinary_canary && ($plan_class !== "amd_vulkan_8b" || $general || $workunit_id <= 1
            || ($last_id !== null && $last_id !== $workunit_id))) {
        throw new RuntimeException("Ordinary canary is one amd_vulkan_8b workunit");
    }
    if ($general && (!isset(GENERAL_MARKERS[$plan_class]) || $workunit_id !== 0 || ($last_id !== null && $last_id !== 0))) {
        throw new RuntimeException("General admission is for ".implode(" and ", array_keys(GENERAL_MARKERS))." only and has no workunit identity");
    }
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
        $updated = $general ? native_qwen_general_xml($original, $plan_class)
            : native_qwen_cohort_xml($original, $plan_class, $workunit_id, $last_id);
        native_qwen_reservation_guard($original, $updated);
        $proof = [
            "plan_class" => $plan_class, "workunit_id" => $workunit_id,
            "last_workunit_id" => $last_id ?? $workunit_id,
            "other_managed_class_closed" => !$general && native_qwen_other_closed($updated, $plan_class),
            "admission_mode" => $general ? "general"
                : ($workunit_id > 1 ? ($ordinary_canary ? "ordinary_canary" : ($tiebreaker ? "tiebreaker" : "bounded")) : "closed"),
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
    $options = getopt("", ["project:", "db-project:", "plan-class:", "workunit:", "workunit-end:", "stop", "general", "ordinary-canary", "tiebreaker", "apply"]);
    $project = isset($options["project"]) ? realpath($options["project"]) : false;
    $db_project = isset($options["db-project"]) ? realpath($options["db-project"]) : $project;
    $plan_class = $options["plan-class"] ?? "nvidia_cuda_2b";
    $stop = array_key_exists("stop", $options);
    $general = array_key_exists("general", $options);
    $ordinary_canary = array_key_exists("ordinary-canary", $options);
    $tiebreaker = array_key_exists("tiebreaker", $options);
    if (!$project || !is_dir($project) || !$db_project || !is_file("$db_project/config.xml")
        || (!in_array($plan_class, NATIVE_QWEN_CLASSES, true) && $plan_class !== "shared_8b")
        || (!$stop && !$general && !preg_match('/^[1-9][0-9]*$/', $options["workunit"] ?? ""))
        || (isset($options["workunit-end"]) && !preg_match('/^[1-9][0-9]*$/', $options["workunit-end"]))
        || ($stop && (isset($options["workunit"]) || isset($options["workunit-end"])))
        || ($general && ($stop || !isset(GENERAL_MARKERS[$plan_class])
            || isset($options["workunit"]) || isset($options["workunit-end"])))
        || ($ordinary_canary && ($stop || $general || $plan_class !== "amd_vulkan_8b"
            || !isset($options["workunit"]) || isset($options["workunit-end"])))
        || ($tiebreaker && ($stop || $general || $ordinary_canary
            || !in_array($plan_class, TIEBREAKER_QWEN_CLASSES, true)
            || !isset($options["workunit"]) || isset($options["workunit-end"])))) {
        throw new RuntimeException("usage: --project PATH [--db-project PATH] [--stop | --plan-class amd_vulkan_8b|nvidia_vulkan_8b --general | --plan-class amd_vulkan_8b --ordinary-canary --workunit ID | --plan-class intel_vulkan_8b|nvidia_vulkan_8b|shared_8b --tiebreaker --workunit ID | --plan-class ".implode("|", NATIVE_QWEN_CLASSES)."|shared_8b --workunit ID [--workunit-end ID]] [--apply]");
    }
    $id = ($stop || $general) ? 0 : intval($options["workunit"]);
    $last_id = isset($options["workunit-end"]) ? intval($options["workunit-end"]) : $id;
    if ($last_id < $id || $last_id - $id >= 10
        || ($plan_class !== "shared_8b" && $last_id !== $id)) {
        throw new RuntimeException("Only shared_8b supports a contiguous range of at most ten workunits");
    }
    if (!$stop && !$general) {
        if ($id <= 1) throw new RuntimeException("Use a newly staged workunit ID greater than 1");
        if ($tiebreaker) {
            // The scheduler reads the live project's config; a private --db-project copy must agree.
            if (!native_qwen_one_result_per_host("$project/config.xml")
                || !native_qwen_one_result_per_host("$db_project/config.xml")) {
                throw new RuntimeException("Tiebreaker requires one_result_per_host_per_wu");
            }
        }
        chdir("$db_project/html/inc");
        require_once("boinc_db.inc");
        for ($current = $id; $current <= $last_id; $current++) {
            $wu = BoincWorkunit::lookup_id($current);
            $app = $wu ? BoincApp::lookup_id($wu->appid) : null;
            if (!$wu || !$app || $app->name !== "usnm_qwen") throw new RuntimeException("Not a Qwen workunit");
            $results = BoincResult::enum("workunitid=$current");
            if ($tiebreaker) {
                native_qwen_tiebreaker_validate($wu, $results, $plan_class,
                    BoincDb::get()->count("assignment", "workunitid=$current"));
            } else {
                native_qwen_cohort_validate($wu, $results,
                    $plan_class === "shared_8b" ? "nvidia_vulkan_8b" : $plan_class, $ordinary_canary,
                    $ordinary_canary ? BoincDb::get()->count("assignment", "workunitid=$current") : 0);
            }
        }
    }
    echo json_encode(native_qwen_cohort_write(
        $project, $plan_class, $id, array_key_exists("apply", $options), $last_id, $general, $ordinary_canary,
        $tiebreaker
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
