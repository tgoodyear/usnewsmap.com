"""app-version-hold.php: holding and releasing native usnm_qwen versions (#1672)."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "boinc/scripts/app-version-hold.php"

# A fake BOINC database: apps 1 usnm_ocr, 2 usnm_qwen; versions in state.json, keyed by id.
FAKE_DB = r"""<?php
function state_path() { return dirname(__DIR__, 2) . "/state.json"; }
function state() { return json_decode(file_get_contents(state_path()), true); }
class BoincApp {
    static function lookup_id($id) {
        $names = [1 => "usnm_ocr", 2 => "usnm_qwen"];
        return isset($names[$id]) ? (object)["id" => $id, "name" => $names[$id], "min_version" => 0] : null;
    }
    static function lookup($q) { return null; }
}
class BoincPlatform {
    static function lookup_id($id) { return (object)["id" => $id, "name" => $id === 3 ? "windows_x86_64" : "x86_64-pc-linux-gnu"]; }
}
class BoincAppVersion {
    public $id, $appid, $platformid, $plan_class, $version_num, $deprecated;
    static function make($id, $row) {
        $v = new self; $v->id = $id;
        foreach ($row as $key => $value) $v->$key = $value;
        return $v;
    }
    static function lookup_id($id) {
        $rows = state();
        return isset($rows[strval($id)]) ? self::make($id, $rows[strval($id)]) : null;
    }
    static function enum($q) {
        preg_match('/appid=(\d+)/', $q, $m);
        $out = [];
        foreach (state() as $id => $row) if ($row["appid"] === intval($m[1])) $out[] = self::make(intval($id), $row);
        return $out;
    }
    function update($q) {
        preg_match('/^deprecated=([01])$/', $q, $m) || throw new Exception("unexpected update $q");
        $rows = state();
        $rows[strval($this->id)]["deprecated"] = intval($m[1]);
        file_put_contents(state_path(), json_encode($rows));
        return true;
    }
}
class FakeResult { function fetch_assoc() { return null; } }
class BoincDb {
    static function get() { return new self; }
    function do_query($q) { return new FakeResult; }
}
"""


@unittest.skipUnless(shutil.which("php"), "PHP required")
class AppVersionHold(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.project = Path(self.folder.name)
        (self.project / "html/inc").mkdir(parents=True)
        (self.project / "html/inc/boinc_db.inc").write_text(FAKE_DB)
        self.journal = self.project / "journal"
        self.state = self.project / "state.json"
        self.state.write_text(json.dumps({
            # Linux nvidia_vulkan_8b, live; the new Windows one, published held.
            "70": {"appid": 2, "platformid": 1, "plan_class": "nvidia_vulkan_8b", "version_num": 117, "deprecated": 0},
            "71": {"appid": 2, "platformid": 3, "plan_class": "nvidia_vulkan_8b", "version_num": 117, "deprecated": 1},
            "72": {"appid": 2, "platformid": 1, "plan_class": "docker_mt", "version_num": 100, "deprecated": 0},
        }))

    def tearDown(self):
        self.folder.cleanup()

    def run_tool(self, version, action, apply=False):
        return subprocess.run(
            ["php", str(SCRIPT), "--project", str(self.project), "--journal-dir", str(self.journal),
             "--app-version", str(version), "--action", action] + (["--apply"] if apply else []),
            capture_output=True, text=True)

    def deprecated(self, version):
        return json.loads(self.state.read_text())[str(version)]["deprecated"]

    def test_a_held_native_qwen_version_is_released_and_held_again(self):
        preview = self.run_tool(71, "release")
        self.assertEqual(preview.returncode, 0, preview.stderr)
        record = json.loads(preview.stdout)
        self.assertEqual((record["app"], record["platform"], record["plan_class"], record["deprecated_before"]),
                         ("usnm_qwen", "windows_x86_64", "nvidia_vulkan_8b", 1))
        self.assertEqual(self.deprecated(71), 1)
        done = self.run_tool(71, "release", apply=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.deprecated(71), 0)
        self.assertTrue((self.project / "reread_db").exists())
        self.assertEqual(len(list(self.journal.glob("release-app-version-71-*.json"))), 1)
        held = self.run_tool(71, "hold", apply=True)
        self.assertEqual(held.returncode, 0, held.stderr)
        self.assertEqual(self.deprecated(71), 1)

    def test_a_qwen_container_class_is_refused(self):
        refused = self.run_tool(72, "hold", apply=True)
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("native usnm_qwen", refused.stderr)
        self.assertEqual(self.deprecated(72), 0)


if __name__ == "__main__":
    unittest.main()
