import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_prefs", ROOT / "contrib-compute/images/server/prepare-prefs.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
REFRESH_SPEC = importlib.util.spec_from_file_location(
    "refresh_scheduler", ROOT / "contrib-compute/images/server/refresh-scheduler.py"
)
REFRESH = importlib.util.module_from_spec(REFRESH_SPEC)
REFRESH_SPEC.loader.exec_module(REFRESH)


class SchedulerRefresh(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(prefix=".qwen-scheduler-", dir=ROOT)
        self.addCleanup(self.folder.cleanup)
        self.root = Path(self.folder.name)
        self.source = self.root / "image-cgi"
        self.target = self.root / "project-cgi"
        self.source.write_bytes(b"reviewed scheduler batch and cohort gates\n")
        self.source.chmod(0o755)
        self.target.write_bytes(b"old scheduler\n")

    def test_replacement_preserves_inflight_bytes_and_is_idempotent(self):
        with self.target.open("rb") as inflight:
            self.assertTrue(REFRESH.refresh(self.source, self.target))
            self.assertEqual(inflight.read(), b"old scheduler\n")
        self.assertEqual(self.target.read_bytes(), self.source.read_bytes())
        self.assertTrue(self.target.stat().st_mode & 0o111)
        inode = self.target.stat().st_ino
        self.assertFalse(REFRESH.refresh(self.source, self.target))
        self.assertEqual(self.target.stat().st_ino, inode)
        self.assertEqual(list(self.root.glob(".usnm-scheduler-*")), [])

    def test_failed_replacement_preserves_original_and_cleans_owned_staging(self):
        for operation in ("fsync", "replace"):
            with self.subTest(operation=operation):
                with patch.object(REFRESH.os, operation, side_effect=OSError("operation rejected")):
                    with self.assertRaises(OSError):
                        REFRESH.refresh(self.source, self.target)
                self.assertEqual(self.target.read_bytes(), b"old scheduler\n")
                self.assertEqual(list(self.root.glob(".usnm-scheduler-*")), [])

    def test_staging_collision_never_deletes_another_file(self):
        occupied = self.root / ".usnm-scheduler-occupied"
        occupied.write_bytes(b"not ours")
        with patch.object(REFRESH.uuid, "uuid4") as uuid4:
            uuid4.return_value.hex = "occupied"
            with self.assertRaises(FileExistsError):
                REFRESH.refresh(self.source, self.target)
        self.assertEqual(occupied.read_bytes(), b"not ours")
        self.assertEqual(self.target.read_bytes(), b"old scheduler\n")

    def test_matching_bytes_with_missing_execute_permission_are_repaired(self):
        self.target.write_bytes(self.source.read_bytes())
        self.target.chmod(0o644)
        self.assertTrue(REFRESH.refresh(self.source, self.target))
        self.assertTrue(self.target.stat().st_mode & 0o111)

    def test_missing_empty_or_nonexecutable_source_fails_closed(self):
        for bad in ("missing", "empty", "nonexecutable"):
            with self.subTest(source=bad):
                if bad == "missing":
                    self.source.unlink()
                else:
                    self.source.write_bytes(b"" if bad == "empty" else b"not executable")
                    self.source.chmod(0o755 if bad == "empty" else 0o644)
                with self.assertRaises(ValueError):
                    REFRESH.refresh(self.source, self.target)
                self.assertEqual(self.target.read_bytes(), b"old scheduler\n")

    def test_startup_refreshes_before_serving_and_image_contains_helper(self):
        image = (ROOT / "contrib-compute/images/server/Containerfile").read_text()
        entry = (ROOT / "contrib-compute/images/server/entrypoint.sh").read_text()
        self.assertIn("images/server/refresh-scheduler.py", image)
        self.assertIn('"$TEMPLATES/refresh-scheduler.py" --source "$BOINC_SRC/sched/cgi" --target "$PROJ/cgi-bin/cgi"', entry)
        self.assertLess(entry.index("  project_tree\n"), entry.index("exec /usr/bin/supervisord"))


class CanaryPreferences(unittest.TestCase):
    def test_all_shipped_user_surfaces_describe_one_project_participation_policy(self):
        for name in (
            "scripts/boinc/setup_windows_volunteer.ps1",
            "docs/boinc-volunteer.md",
            "contrib-compute/boinc/html/user/about.php",
        ):
            with self.subTest(name=name):
                source = (ROOT / name).read_text()
                self.assertIn("production and experimental", source)
                self.assertNotIn("qwen_native_cuda_canary", source)
                self.assertIn("CPU/GPU", source)
        scheduler = (ROOT / "scripts/boinc/patch-server-apple-gpu.py").read_text()
        self.assertNotIn("qwen_native_cuda_canary", scheduler)
        driver = (ROOT / "crates/usnm-qwen-app/src/main.rs").read_text()
        self.assertNotIn("qwen_native_cuda_canary", driver)

    @unittest.skipUnless(shutil.which("php"), "PHP required")
    def test_operator_cohort_has_one_exact_workunit_stop_and_durable_preview(self):
        helper = ROOT / "scripts/boinc/native-qwen-cohort.php"
        plans = ROOT / "contrib-compute/boinc/plan_class_spec.xml"
        with tempfile.TemporaryDirectory(prefix=".qwen-cohort-", dir=ROOT) as folder:
            result = subprocess.run(
                ["php", "-d", "zend.assertions=1", "-d", "assert.exception=1", "-r", r"""
require $argv[1];
$xml = file_get_contents($argv[2]);
$project = $argv[3];
file_put_contents("$project/plan_class_spec.xml", $xml);
$preview = native_qwen_cohort_write($project, "nvidia_cuda_2b", 100, false);
assert(file_get_contents("$project/plan_class_spec.xml") === $xml);
assert(!is_dir("$project/ops-issue-559"));
$applied = native_qwen_cohort_write($project, "nvidia_cuda_2b", 100, true);
assert(is_file($applied["journal"]));
$journal = json_decode(file_get_contents($applied["journal"]), true);
assert($journal["original_xml"] === $xml);
$updated = simplexml_load_file("$project/plan_class_spec.xml");
$original = simplexml_load_string($xml);
foreach ($updated->plan_class as $node) {
    $name = (string)$node->name;
    if ($name === "nvidia_cuda_2b") {
        assert((string)$node->min_wu_id === "100" && (string)$node->max_wu_id === "100");
    } elseif (in_array($name, ["nvidia_cuda_8b", "intel_vulkan_8b"], true)) {
        assert((string)$node->min_wu_id === "2" && (string)$node->max_wu_id === "1");
    } else {
        $matching = $original->xpath("plan_class[name='$name']");
        assert($node->asXML() === $matching[0]->asXML());
    }
}
native_qwen_cohort_write($project, "nvidia_cuda_8b", 101, true);
$vulkan = native_qwen_cohort_write($project, "intel_vulkan_8b", 102, true);
$opened = simplexml_load_file("$project/plan_class_spec.xml");
foreach ($opened->plan_class as $node) {
    $name = (string)$node->name;
    if ($name === "intel_vulkan_8b") {
        assert((string)$node->min_wu_id === "102" && (string)$node->max_wu_id === "102");
    } elseif (in_array($name, ["nvidia_cuda_2b", "nvidia_cuda_8b"], true)) {
        assert((int)$node->min_wu_id > (int)$node->max_wu_id);
    }
}
native_qwen_cohort_write($project, "nvidia_cuda_2b", 0, true);
$closed = simplexml_load_file("$project/plan_class_spec.xml");
foreach ($closed->plan_class as $node) {
    if (in_array((string)$node->name, ["nvidia_cuda_2b","nvidia_cuda_8b","intel_vulkan_8b"], true)) {
        assert((int)$node->min_wu_id > (int)$node->max_wu_id);
    }
}
assert(count(glob("$project/.native-qwen-cohort-*")) === 0);
$wu = (object)["batch"=>2,"rsc_disk_bound"=>12884901888,"min_quorum"=>2,
    "target_nresults"=>2,"canonical_resultid"=>0,"error_mask"=>0];
$results = [(object)["server_state"=>2,"sent_time"=>0],(object)["server_state"=>2,"sent_time"=>0]];
native_qwen_cohort_validate($wu, $results, "nvidia_cuda_2b");
foreach (["batch"=>12,"rsc_disk_bound"=>34359738368,"min_quorum"=>1,
    "target_nresults"=>3,"canonical_resultid"=>10,"error_mask"=>1] as $key=>$value) {
    $bad = clone $wu; $bad->$key = $value; $refused = false;
    try { native_qwen_cohort_validate($bad, $results, "nvidia_cuda_2b"); }
    catch (RuntimeException $e) { $refused = true; }
    assert($refused);
}
$results[0]->server_state = 4;
$refused = false;
try { native_qwen_cohort_validate($wu, $results, "nvidia_cuda_2b"); }
catch (RuntimeException $e) { $refused = true; }
assert($refused);
$wu->batch = 8; $wu->rsc_disk_bound = 19327352832; $results[0]->server_state = 2;
native_qwen_cohort_validate($wu, $results, "nvidia_cuda_8b");
native_qwen_cohort_validate($wu, $results, "intel_vulkan_8b");
foreach ($results as $row) $row->sent_time = 1;
$refused = false;
try { native_qwen_cohort_validate($wu, $results, "nvidia_cuda_8b"); }
catch (RuntimeException $e) { $refused = true; }
assert($refused);
""", str(helper), str(plans), folder],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell required")
    def test_windows_checksum_append_is_portable_ascii_lf(self):
        workflow = (ROOT / ".github/workflows/boinc-quality-native.yml").read_text()
        append = next(line.strip() for line in workflow.splitlines()
                      if "[IO.File]::AppendAllText" in line)
        with tempfile.TemporaryDirectory(prefix=".qwen-sha-test-", dir=ROOT) as folder:
            root = Path(folder)
            literal = str(root).replace("'", "''")
            script = f"$out='{literal}'; $hash='{'a' * 64}'; $name='msvcp140.dll';\n{append}"
            result = subprocess.run(["pwsh", "-NoProfile", "-Command", script],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual((root / "SHA256SUMS").read_bytes(),
                             b"a" * 64 + b"  msvcp140.dll\n")

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell required")
    def test_windows_crt_workflow_enumerates_directory_contents_not_wildcard_entry(self):
        workflow = (ROOT / ".github/workflows/boinc-quality-native.yml").read_text()
        enumeration = "$crt = " + workflow.split("          $crt = ", 1)[1].split(
            "          foreach ($required", 1
        )[0]
        with tempfile.TemporaryDirectory(prefix=".qwen-crt-test-", dir=ROOT) as folder:
            root = Path(folder)
            crt = root / "x64/Microsoft.VC143.CRT"
            crt.mkdir(parents=True)
            for name in ("msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll"):
                (crt / name).write_bytes(b"fixture-not-a-runtime")
            literal = str(root).replace("'", "''")
            script = (
                "$ErrorActionPreference='Stop'; "
                f"$redist=[pscustomobject]@{{FullName='{literal}'}};\n"
                + enumeration
                + "\nif ($files.Count -ne 3) { throw 'CRT enumeration failed' }\n"
            )
            result = subprocess.run(["pwsh", "-NoProfile", "-Command", script],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_stock_extension_is_exact_and_fails_closed_on_upstream_change(self):
        names = ("default", "parse", "edit", "parse_form", "show")
        source = "\n".join(f"function project_specific_prefs_{name}() {{}}" for name in names)
        extended = MODULE.extend_stock(source)
        for name in names:
            self.assertIn(f"function usnm_stock_prefs_{name}()", extended)
        with self.assertRaises(ValueError):
            MODULE.extend_stock(source.replace("project_specific_prefs_edit", "upstream_changed"))
        with self.assertRaises(ValueError):
            MODULE.extend_stock(source + "\nfunction project_specific_prefs_show() {}")

    @unittest.skipUnless(shutil.which("php"), "PHP required")
    def test_project_participation_ignores_legacy_flags_and_preserves_stock(self):
        with tempfile.TemporaryDirectory(prefix=".qwen-canary-test-", dir=ROOT) as folder:
            folder = Path(folder)
            shutil.copyfile(
                ROOT / "contrib-compute/boinc/html/project/project_specific_prefs.inc",
                folder / "project_specific_prefs.inc",
            )
            (folder / "usnm_stock_prefs.inc").write_text("""<?php
function usnm_stock_prefs_default() { return "<max_jobs>1</max_jobs>"; }
function usnm_stock_prefs_parse($xml) { return (object)["max_jobs"=>1]; }
function usnm_stock_prefs_parse_form(&$error) { return "<max_jobs>1</max_jobs>"; }
function usnm_stock_prefs_edit($prefs, $error) { echo "stock_edit"; }
function usnm_stock_prefs_show($prefs, $columns) { echo "stock_show"; }
function row2($label, $value) { $GLOBALS["row2_label"] = $label; echo $value; }
function row_defs($label, $key, $a, $b, $prefs) { echo $key; }
""")
            result = subprocess.run(
                ["php", "-d", "zend.assertions=1", "-d", "assert.exception=1", "-r", """
require $argv[1];
assert(str_contains(project_specific_prefs_default(), '<max_jobs>1</max_jobs>'));
assert(!str_contains(project_specific_prefs_default(), 'qwen_native_cuda_canary'));
foreach (['', '0', '1', 'true', '01', ' 1', '1 ', '2'] as $old) {
    $prefs = project_specific_prefs_parse('<qwen_native_cuda_canary>'.$old.'</qwen_native_cuda_canary>');
    assert(!isset($prefs->qwen_native_cuda_canary) && $prefs->max_jobs === 1);
}
$error = new stdClass;
foreach ([[], ['qwen_native_cuda_canary'=>'0'], ['qwen_native_cuda_canary'=>'1'], ['qwen_native_cuda_canary'=>['1']]] as $get) {
    $_GET = $get;
    assert(project_specific_prefs_parse_form($error) === '<max_jobs>1</max_jobs>');
}
ob_start(); project_specific_prefs_edit($prefs); $edit = ob_get_clean();
assert(str_contains($edit, 'stock_edit') && str_contains($edit, 'production and experimental'));
assert(!str_contains($edit, 'qwen_native_cuda_canary'));
assert(str_contains($edit, 'normal CPU/GPU choices'));
ob_start(); project_specific_prefs_show($prefs); $show = ob_get_clean();
assert($show === 'stock_show');
ob_start(); project_specific_prefs_show($prefs, true); $show = ob_get_clean();
assert($show === 'stock_show');
""", str(folder / "project_specific_prefs.inc")],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
