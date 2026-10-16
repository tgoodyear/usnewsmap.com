import importlib.util
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_prefs", ROOT / "contrib-compute/images/server/prepare-prefs.py"
)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class CanaryPreferences(unittest.TestCase):
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
    def test_authenticated_form_defaults_denies_exact_opt_in_and_preserves_stock(self):
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
assert(str_contains(project_specific_prefs_default(), '<qwen_native_cuda_canary>0</qwen_native_cuda_canary>'));
foreach (['', '0', 'true', '01', ' 1', '1 ', '2'] as $bad) {
    assert(project_specific_prefs_parse('<qwen_native_cuda_canary>'.$bad.'</qwen_native_cuda_canary>')->qwen_native_cuda_canary === 0);
}
$prefs = project_specific_prefs_parse('<qwen_native_cuda_canary>1</qwen_native_cuda_canary>');
assert($prefs->qwen_native_cuda_canary === 1 && $prefs->max_jobs === 1);
$error = new stdClass;
foreach ([[], ['qwen_native_cuda_canary'=>'0'], ['qwen_native_cuda_canary'=>['1']]] as $get) {
    $_GET = $get;
    assert(project_specific_prefs_parse_form($error) === '<max_jobs>1</max_jobs><qwen_native_cuda_canary>0</qwen_native_cuda_canary>'."\\n");
}
$_GET = ['qwen_native_cuda_canary'=>'1'];
assert(str_contains(project_specific_prefs_parse_form($error), '<qwen_native_cuda_canary>1</qwen_native_cuda_canary>'));
ob_start(); project_specific_prefs_edit($prefs); $edit = ob_get_clean();
assert(str_contains($edit, 'stock_edit') && str_contains($edit, ' checked'));
assert($GLOBALS["row2_label"] === '<label for="qwen_native_cuda_canary">'.QWEN_CANARY_LABEL.'</label>');
assert(str_contains($edit, 'id="qwen_native_cuda_canary"'));
$prefs->qwen_native_cuda_canary = 0;
ob_start(); project_specific_prefs_edit($prefs); $edit = ob_get_clean();
assert(!str_contains($edit, ' checked'));
$prefs->qwen_native_cuda_canary = 1;
ob_start(); project_specific_prefs_show($prefs); $show = ob_get_clean();
assert($show === 'stock_showyes');
ob_start(); project_specific_prefs_show($prefs, true); $show = ob_get_clean();
assert($show === 'stock_showqwen_native_cuda_canary');
""", str(folder / "project_specific_prefs.inc")],
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
