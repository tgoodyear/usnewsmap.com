import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "boinc/scripts/hold-legacy-qwen.php"


@unittest.skipUnless(shutil.which("php"), "PHP required")
class HoldLegacyQwen(unittest.TestCase):
    def test_only_legacy_inference_versions_can_be_held_and_replay_is_safe(self):
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            inc = project / "html/inc"
            inc.mkdir(parents=True)
            state = project / "state.json"
            (inc / "boinc_db.inc").write_text("""
<?php
class BoincApp {
    static function lookup($q) { return (object)["id"=>6]; }
}
class BoincAppVersion {
    public $id, $appid, $plan_class, $version_num=100, $deprecated=0;
    static function lookup_id($id) {
        $v = new self; $v->id=$id;
        $v->appid = $id === 9 ? 7 : 6;
        $v->plan_class = [7=>"docker_mt", 9=>"docker_nvidia_train", 10=>"docker_mt_2b"][$id] ?? "mt";
        $p = dirname(__DIR__,2)."/state.json";
        if (file_exists($p)) $v->deprecated=json_decode(file_get_contents($p),true)["deprecated"];
        return $v;
    }
    function update($q) {
        if ($q !== "deprecated=1") throw new Exception("unexpected update");
        file_put_contents(dirname(__DIR__,2)."/state.json", '{"deprecated":1}');
        return true;
    }
}
""")
            def run(version, apply=False, prepend=None):
                return subprocess.run(
                    ["php"] + (["-d", f"auto_prepend_file={prepend}"] if prepend else [])
                    + [str(SCRIPT), "--project", str(project), "--app-version", str(version)]
                    + (["--apply"] if apply else []), capture_output=True, text=True,
                )
            for version in (9, 10, 11):
                self.assertNotEqual(run(version, True).returncode, 0)
            self.assertFalse(state.exists())
            self.assertEqual(run(7).returncode, 0)
            self.assertFalse(state.exists())
            journal = project / "ops-issue-559/hold-app-version-7.json"
            journal.parent.mkdir()
            for invalid in ("", "{", "{}", '{"app_version_id":9}'):
                journal.write_text(invalid)
                self.assertNotEqual(run(7, True).returncode, 0)
                self.assertFalse(state.exists())
            valid = json.loads(run(7).stdout)
            valid["action"] = "hold"
            for key, wrong in (("app_version_id", 9), ("plan_class", "docker_mt_2b"), ("deprecated_before", 2)):
                journal.write_text(json.dumps({**valid, key: wrong}))
                self.assertNotEqual(run(7, True).returncode, 0)
                self.assertFalse(state.exists())
            journal.unlink()
            short = project / "short.php"
            short.write_text("""<?php
namespace Usnm\\Boinc\\Hold;
function fwrite($handle, $data) {
    if ($handle === STDERR) return \\fwrite($handle, $data);
    return \\fwrite($handle, substr($data, 0, 5));
}
""")
            failed = run(7, True, short)
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("Incomplete journal write", failed.stderr)
            self.assertFalse(state.exists())
            self.assertNotEqual(run(7, True).returncode, 0)
            self.assertFalse(state.exists())
            journal.unlink()
            unsynced = project / "unsynced.php"
            unsynced.write_text("""<?php
namespace Usnm\\Boinc\\Hold;
function fsync($handle) { return false; }
""")
            self.assertNotEqual(run(7, True, unsynced).returncode, 0)
            self.assertIsInstance(json.loads(journal.read_text()), dict)
            self.assertFalse(state.exists())
            replay = run(7, True, unsynced)
            self.assertNotEqual(replay.returncode, 0)
            self.assertIn("Journal synchronization failed", replay.stderr)
            self.assertFalse(state.exists())
            journal.unlink()
            self.assertEqual(run(7, True).returncode, 0)
            self.assertEqual(json.loads(state.read_text()), {"deprecated": 1})
            first = journal.read_bytes()
            self.assertTrue((project / "reread_db").exists())
            self.assertEqual(run(7, True).returncode, 0)
            self.assertEqual(journal.read_bytes(), first)
            self.assertTrue(json.loads(first)["results_and_workunits_unchanged"])
