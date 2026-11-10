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


VERSION_HOLD = ROOT / "boinc/scripts/app-version-hold.php"
FAKE_DB = r"""<?php
// The tables app-version-hold.php reads and writes, in state.json beside the project.
function usnm_state() { return json_decode(file_get_contents(dirname(__DIR__, 2) . "/state.json")); }
function usnm_save($s) { file_put_contents(dirname(__DIR__, 2) . "/state.json", json_encode($s)); }
class BoincApp {
    public $id, $name, $min_version;
    static function make($row) { $a = new self; foreach ($row as $k => $v) $a->$k = $v; return $a; }
    static function lookup($q) {
        foreach (usnm_state()->apps as $a) if ("name='$a->name'" === $q) return self::make($a);
        return null;
    }
    static function lookup_id($id) {
        foreach (usnm_state()->apps as $a) if ($a->id === $id) return self::make($a);
        return null;
    }
    function update($q) {
        if (!preg_match('/^min_version=([0-9]+)$/', $q, $m)) throw new Exception("unexpected update $q");
        $s = usnm_state();
        foreach ($s->apps as $a) if ($a->id === $this->id) $a->min_version = intval($m[1]);
        usnm_save($s); return true;
    }
}
class BoincPlatform {
    static function lookup_id($id) { return (object)["id" => $id, "name" => [1 => "x86_64-pc-linux-gnu", 2 => "arm64-apple-darwin"][$id]]; }
}
class BoincAppVersion {
    public $id, $appid, $platformid, $version_num, $plan_class, $deprecated, $xml_doc;
    static function make($row) { $v = new self; foreach ($row as $k => $x) $v->$k = $x; return $v; }
    static function lookup_id($id) {
        foreach (usnm_state()->versions as $v) if ($v->id === $id) return self::make($v);
        return null;
    }
    static function enum($q) {
        if (!preg_match('/^appid=([0-9]+)$/', $q, $m)) throw new Exception("unexpected enum $q");
        return array_map([self::class, "make"], array_values(array_filter(usnm_state()->versions, fn($v) => $v->appid === intval($m[1]))));
    }
    function update($q) {
        if (!preg_match('/^deprecated=([01])$/', $q, $m)) throw new Exception("unexpected update $q");
        $s = usnm_state();
        foreach ($s->versions as $v) if ($v->id === $this->id) $v->deprecated = intval($m[1]);
        usnm_save($s); return true;
    }
}
class UsnmResult {
    public $rows;
    function fetch_assoc() { return array_shift($this->rows); }
}
class BoincDb {
    static function get() { return new self; }
    function do_query($q) {
        // Unfinished: unsent, in progress, awaiting validation, a retry or assimilation.
        foreach (["w.assimilate_state<>2", "w.need_validate<>0", "w.canonical_resultid=0 AND w.error_mask=0"] as $term)
            if (!str_contains($q, $term)) throw new Exception("unexpected query");
        if (preg_match('/UPDATE|DELETE|INSERT/i', $q)) throw new Exception("unexpected query");
        preg_match('/w\.appid=([0-9]+)/', $q, $m);
        $r = new UsnmResult;
        $r->rows = array_values(array_map(fn($row) => (array)$row,
            array_filter(usnm_state()->unsent, fn($row) => $row->appid === intval($m[1]))));
        $r->rows = array_map(fn($row) => ["n" => strval($row["n"]), "c" => strval($row["c"])], $r->rows);
        return $r;
    }
}
"""


@unittest.skipUnless(shutil.which("php"), "PHP required")
class AppVersionHold(unittest.TestCase):
    """#1687: hold or release usnm_ocr and usnm_gold versions, and usnm_ocr's min_version."""

    WORKER = "sha256:" + "8" * 64

    def setUp(self):
        self.folder = tempfile.TemporaryDirectory()
        self.addCleanup(self.folder.cleanup)
        self.project = Path(self.folder.name)
        (self.project / "html/inc").mkdir(parents=True)
        (self.project / "html/inc/boinc_db.inc").write_text(FAKE_DB)
        (self.project / "download").mkdir()
        (self.project / "download/usnm_gold_dockerfile_1").write_text(
            f"FROM ghcr.io/tgoodyear/usnm-boinc-worker@{self.WORKER}\n")
        gold_xml = ("<app_version><file_ref><file_name>usnm_gold_dockerfile_1</file_name>"
                    "<open_name>Dockerfile</open_name></file_ref></app_version>")
        self.journal = self.project / "journal"
        self.bindings = self.project / "bindings.json"
        self.bindings.write_text(json.dumps({self.WORKER: {"x86_64-pc-linux-gnu": "sha256:" + "e" * 64}}))
        self.state = {
            "apps": [{"id": 1, "name": "usnm_ocr", "min_version": 0}, {"id": 2, "name": "usnm_gold", "min_version": 0},
                     {"id": 3, "name": "usnm_qwen", "min_version": 0}],
            "versions": [
                {"id": 82, "appid": 1, "platformid": 1, "version_num": 106, "plan_class": "docker", "deprecated": 0, "xml_doc": ""},
                {"id": 83, "appid": 1, "platformid": 2, "version_num": 106, "plan_class": "docker", "deprecated": 0, "xml_doc": ""},
                {"id": 85, "appid": 1, "platformid": 1, "version_num": 107, "plan_class": "docker", "deprecated": 1, "xml_doc": ""},
                {"id": 80, "appid": 2, "platformid": 1, "version_num": 105, "plan_class": "docker", "deprecated": 0, "xml_doc": ""},
                {"id": 88, "appid": 2, "platformid": 1, "version_num": 106, "plan_class": "docker", "deprecated": 1, "xml_doc": gold_xml},
                {"id": 89, "appid": 2, "platformid": 2, "version_num": 106, "plan_class": "docker", "deprecated": 1, "xml_doc": gold_xml},
                {"id": 90, "appid": 3, "platformid": 1, "version_num": 300, "plan_class": "docker", "deprecated": 0, "xml_doc": ""},
            ],
            "unsent": [{"appid": 1, "n": 106, "c": 90}],
        }
        self.save()

    def save(self):
        (self.project / "state.json").write_text(json.dumps(self.state))

    def load(self):
        return json.loads((self.project / "state.json").read_text())

    def deprecated(self, version_id):
        return next(v["deprecated"] for v in self.load()["versions"] if v["id"] == version_id)

    def hold(self, *args, apply=False):
        return subprocess.run(
            ["php", str(VERSION_HOLD), "--project", str(self.project), "--journal-dir", str(self.journal),
             "--gold-bindings", str(self.bindings), *args] + (["--apply"] if apply else []),
            capture_output=True, text=True)

    def journals(self):
        return sorted(self.journal.glob("*.json")) if self.journal.exists() else []

    def test_releasing_a_newer_ocr_version_refuses_to_strand_pinned_unsent_work(self):
        # The 9 Nov case: 90 unsent bundles pinned to 106 when 107 became the newest.
        preview = self.hold("--app-version", "85", "--action", "release")
        self.assertEqual(preview.returncode, 1)
        self.assertEqual(json.loads(preview.stdout.splitlines()[0])["stranded_workunits"], {"106": 90})
        self.assertIn("pinned to version 106 (unsent, in progress, awaiting validation, a retry or assimilation) would get no more results", preview.stderr)
        self.assertNotEqual(self.hold("--app-version", "85", "--action", "release", apply=True).returncode, 0)
        self.assertEqual((self.deprecated(85), self.journals()), (1, []))
        # min_version 106 keeps 106 loaded beside 107: the release then strands nothing.
        done = self.hold("--action", "min-version", "--min-version", "106", apply=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.load()["apps"][0]["min_version"], 106)
        record = json.loads(self.journals()[0].read_text())
        self.assertEqual((record["min_version_before"], record["min_version"]), (0, 106))
        done = self.hold("--app-version", "85", "--action", "release", apply=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.deprecated(85), 0)
        self.assertTrue((self.project / "reread_db").exists())
        released = [json.loads(p.read_text()) for p in self.journals() if p.name.startswith("release-")]
        self.assertEqual([(r["app_version_id"], r["deprecated_before"], r["version_num"]) for r in released],
                         [(85, 1, 107)])
        # Raising min_version past 106 now would strand the same units again.
        raised = self.hold("--action", "min-version", "--min-version", "107", apply=True)
        self.assertNotEqual(raised.returncode, 0)
        self.assertEqual(self.load()["apps"][0]["min_version"], 106)
        # Releasing again changes nothing and journals nothing, but tells the feeder again: a
        # run that stopped between the change and its notice is finished by its retry.
        count = len(self.journals())
        (self.project / "reread_db").unlink()
        again = self.hold("--app-version", "85", "--action", "release", apply=True)
        self.assertEqual(again.returncode, 0, again.stderr)
        self.assertIn("the feeder notice was sent again", again.stdout)
        self.assertEqual(len(self.journals()), count)
        self.assertTrue((self.project / "reread_db").exists())
        # A preview of the same never notifies.
        (self.project / "reread_db").unlink()
        self.assertEqual(self.hold("--app-version", "85", "--action", "release").returncode, 0)
        self.assertFalse((self.project / "reread_db").exists())

    def test_accept_stranded_and_drained_work_let_a_release_through(self):
        done = self.hold("--app-version", "85", "--action", "release", "--accept-stranded", apply=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.deprecated(85), 0)
        # Holding 107 again brings 106 back: nothing pinned to 107 strands.
        held = self.hold("--app-version", "85", "--action", "hold", apply=True)
        self.assertEqual(held.returncode, 0, held.stderr)
        self.assertEqual(self.deprecated(85), 1)
        # Once the pinned units are sent, the release needs no flag.
        self.state = self.load()
        self.state["unsent"] = []
        self.save()
        self.assertEqual(self.hold("--app-version", "85", "--action", "release", apply=True).returncode, 0)
        # Holding the plan's own version strands its units.
        self.state = self.load()
        self.state["unsent"] = [{"appid": 1, "n": 107, "c": 3}]
        self.save()
        held = self.hold("--app-version", "85", "--action", "hold", apply=True)
        self.assertNotEqual(held.returncode, 0)
        self.assertIn("unfinished workunits pinned to version 107", held.stderr)
        self.assertIn("in progress, awaiting validation, a retry or assimilation", held.stderr)
        self.assertEqual(self.deprecated(85), 0)
        self.assertEqual(self.hold("--app-version", "85", "--action", "hold", "--accept-stranded", apply=True).returncode, 0)

    def test_gold_release_needs_the_running_servers_binding(self):
        self.assertEqual(self.hold("--app-version", "88", "--action", "release", apply=True).returncode, 0)
        self.assertEqual(self.deprecated(88), 0)
        # 89 is arm64, which the running bindings don't list.
        unbound = self.hold("--app-version", "89", "--action", "release", apply=True)
        self.assertNotEqual(unbound.returncode, 0)
        self.assertIn("no arm64-apple-darwin binding for " + self.WORKER, unbound.stderr)
        self.assertEqual(self.deprecated(89), 1)
        self.bindings.write_text("{")
        self.assertNotEqual(self.hold("--app-version", "89", "--action", "release", apply=True).returncode, 0)
        (self.project / "download/usnm_gold_dockerfile_1").write_text("FROM somewhere/else\n")
        self.assertIn("can't read the worker", self.hold("--app-version", "89", "--action", "release").stderr)
        # Holding needs no binding.
        self.assertEqual(self.hold("--app-version", "88", "--action", "hold", apply=True).returncode, 0)

    def test_only_ocr_and_gold_container_versions_and_named_flags(self):
        self.assertNotEqual(self.hold("--app-version", "90", "--action", "hold", apply=True).returncode, 0)
        self.state["versions"][0]["plan_class"] = "docker_mt"
        self.save()
        self.assertNotEqual(self.hold("--app-version", "82", "--action", "hold", apply=True).returncode, 0)
        for args in ([], ["--app-version", "85"], ["--app-version", "85", "--action", "drop"],
                     ["--action", "min-version"], ["--action", "min-version", "--min-version", "-1"],
                     ["--action", "min-version", "--min-version", "106", "--app-version", "85"]):
            with self.subTest(args=args):
                self.assertEqual(self.hold(*args).returncode, 2)
        relative = subprocess.run(["php", str(VERSION_HOLD), "--project", str(self.project), "--journal-dir",
                                   "journal", "--app-version", "85", "--action", "hold"], capture_output=True, text=True)
        self.assertEqual(relative.returncode, 2)
        self.assertEqual(self.load()["versions"], self.state["versions"])
        self.assertFalse((self.project / "reread_db").exists())

    def test_the_journal_is_written_before_the_change(self):
        short = self.project / "short.php"
        short.write_text("""<?php
namespace Usnm\\Boinc\\VersionHold;
function fwrite($handle, $data) {
    if ($handle === STDERR) return \\fwrite($handle, $data);
    return \\fwrite($handle, substr($data, 0, 5));
}
""")
        failed = subprocess.run(["php", "-d", f"auto_prepend_file={short}", str(VERSION_HOLD), "--project",
                                 str(self.project), "--journal-dir", str(self.journal), "--app-version", "88",
                                 "--action", "hold", "--apply"], capture_output=True, text=True)
        self.assertEqual(failed.returncode, 0, failed.stderr)  # 88 is already held: nothing to write
        failed = subprocess.run(["php", "-d", f"auto_prepend_file={short}", str(VERSION_HOLD), "--project",
                                 str(self.project), "--journal-dir", str(self.journal), "--app-version", "80",
                                 "--action", "hold", "--apply"], capture_output=True, text=True)
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("incomplete journal write", failed.stderr)
        self.assertEqual(self.deprecated(80), 0)
