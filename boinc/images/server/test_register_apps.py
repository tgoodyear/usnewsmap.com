"""register-apps.php: a new app gets its project.xml weight, which BOINC's xadd leaves at 1."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "register-apps.php"
PROJECT_XML = HERE.parents[1] / "project/project.xml"

# BoincApp as boinc_db.inc has it, over a JSON file. STUB_IGNORE_UPDATE drops updates, as a
# failed UPDATE would.
BOINC_DB = r"""<?php
class BoincApp {
    static function rows() { return json_decode(file_get_contents(getenv("STUB_DB")), true); }
    static function lookup($clause) {
        preg_match("/^name='([a-z0-9_]+)'$/", $clause, $m) or die("bad clause $clause\n");
        foreach (self::rows() as $row) {
            if ($row["name"] === $m[1]) { $app = new BoincApp; foreach ($row as $k => $v) $app->$k = $v; return $app; }
        }
        return null;
    }
    static function insert($name) {
        $rows = self::rows();
        $rows[] = ["id" => count($rows) + 1, "name" => $name, "weight" => "1", "deprecated" => "0", "versions" => 0];
        file_put_contents(getenv("STUB_DB"), json_encode($rows));
    }
    function update($clause) {
        if (getenv("STUB_IGNORE_UPDATE")) return true;
        if (preg_match("/^min_version = ([0-9]+)$/", $clause, $mv)) {
            $rows = self::rows();
            foreach ($rows as &$row) if ($row["name"] === $this->name) $row["min_version"] = $mv[1];
            file_put_contents(getenv("STUB_DB"), json_encode($rows));
            return true;
        }
        preg_match("/^weight=([0-9]+)$/", $clause, $m) or die("bad update $clause\n");
        $rows = self::rows();
        foreach ($rows as &$row) if ($row["name"] === $this->name) $row["weight"] = $m[1];
        file_put_contents(getenv("STUB_DB"), json_encode($rows));
        return true;
    }
}
class BoincAppVersion {
    static function lookup($clause) {
        preg_match("/^appid=([0-9]+)$/", $clause, $m) or die("bad clause $clause\n");
        foreach (BoincApp::rows() as $row) if ($row["id"] == $m[1] && $row["versions"] > 0) return (object)["appid" => $row["id"]];
        return null;
    }
}
"""

# tools/xadd's app half at BOINC 22bbebe: an app it lacks gets only a name, so weight 1.
XADD = r"""<?php
chdir("html/inc"); require_once("boinc_db.inc"); chdir("../..");
if (getenv("STUB_XADD_STATUS")) exit((int)getenv("STUB_XADD_STATUS"));
foreach (simplexml_load_file("project.xml")->app as $app) {
    $n = (string)$app->name;
    $a = BoincApp::lookup("name='$n'");
    // As tools/xadd: an existing app takes project.xml's min_version.
    if ($a && (int)($a->min_version ?? 0) != (int)$app->min_version) { $a->update("min_version = " . (int)$app->min_version); echo "updated min_version of app $n\n"; }
    elseif ($a) { echo "app $n already in DB\n"; } else { BoincApp::insert($n); echo "added app: $n\n"; }
}
"""


@unittest.skipUnless(shutil.which("php"), "PHP unavailable")
class RegisterApps(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.project = Path(self.temp.name)
        (self.project / "html/inc").mkdir(parents=True)
        (self.project / "bin").mkdir()
        (self.project / "html/inc/boinc_db.inc").write_text(BOINC_DB)
        xadd = self.project / "bin/xadd"
        xadd.write_text(f"#!{shutil.which('php')}\n" + XADD)
        xadd.chmod(0o755)
        shutil.copy(PROJECT_XML, self.project / "project.xml")
        self.db = self.project / "db.json"

    def tearDown(self):
        self.temp.cleanup()

    def run_script(self, rows, **env):
        self.db.write_text(json.dumps(rows))
        result = subprocess.run(["php", str(SCRIPT)], cwd=self.project, capture_output=True, text=True,
                                env={**os.environ, "STUB_DB": str(self.db), **env})
        return result, {row["name"]: row["weight"] for row in json.loads(self.db.read_text())}

    def live(self):
        # The registered apps as on 11 November: every app but usnm_ocr_native (#1780), weights as
        # declared. usnm_gold_native has had a version since 6 November (#1607).
        rows = []
        for name, weight in self.declared().items():
            if name != "usnm_ocr_native":
                # usnm_assess has no app version on the live server.
                rows.append({"id": len(rows) + 1, "name": name, "weight": str(weight), "deprecated": "0",
                             "versions": 0 if name == "usnm_assess" else 1})
        return rows

    def declared(self):
        return {app.findtext("name"): int(app.findtext("weight"))
                for app in ET.parse(PROJECT_XML).findall("app")}

    def test_a_new_app_gets_its_declared_weight_and_the_rest_are_unchanged(self):
        result, weights = self.run_script(self.live())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("added app: usnm_ocr_native", result.stdout)
        self.assertIn("set weight of app usnm_ocr_native, which has no app version, to 0", result.stdout)
        self.assertEqual(weights["usnm_ocr_native"], "0")
        self.assertEqual(weights, {name: str(weight) for name, weight in self.declared().items()})

    def test_an_operator_set_min_version_survives_registration(self):
        # #1687: app-version-hold.php --action min-version keeps 106 loaded beside 107; xadd
        # would set it back to project.xml's 0 on the next publication.
        rows = self.live()
        for row in rows:
            row["min_version"] = "106" if row["name"] == "usnm_ocr" else "0"
        result, _ = self.run_script(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("app usnm_ocr keeps min_version 106 from the database (project.xml says 0)", result.stdout)
        self.assertNotIn("updated min_version of app usnm_ocr", result.stdout)
        after = {row["name"]: row.get("min_version") for row in json.loads(self.db.read_text())}
        self.assertEqual(after["usnm_ocr"], "106")
        self.assertEqual(ET.parse(self.project / "project.xml").find("app[name='usnm_ocr']").findtext("min_version"), "106")
        self.assertEqual(list(self.project.glob("project.xml.register-apps.*")), [])
        # Without the preserving step, the stub xadd (as BOINC's) would have reset it.
        shutil.copy(PROJECT_XML, self.project / "project.xml")
        self.db.write_text(json.dumps(rows))
        subprocess.run([str(self.project / "bin/xadd")], cwd=self.project, capture_output=True, text=True,
                       env={**os.environ, "STUB_DB": str(self.db)}, check=True)
        reset = {row["name"]: row.get("min_version") for row in json.loads(self.db.read_text())}
        self.assertEqual(reset["usnm_ocr"], "0")

    def test_a_rerun_changes_nothing(self):
        rows = self.live() + [{"id": 99, "name": "usnm_ocr_native", "weight": "0", "deprecated": "0", "versions": 0}]
        result, weights = self.run_script(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("set weight", result.stdout)
        self.assertEqual(weights, {row["name"]: row["weight"] for row in rows})

    def test_a_registered_apps_differing_weight_is_reported_and_kept(self):
        rows = self.live()
        for row in rows:
            if row["name"] == "usnm_qwen":
                row["weight"] = "8"
        result, weights = self.run_script(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("app usnm_qwen has weight 8 in the database and 16 in project.xml; not changed, "
                      "it has app versions", result.stdout)
        self.assertEqual(weights["usnm_qwen"], "8")
        self.assertEqual(weights["usnm_ocr_native"], "0")

    def test_a_retry_after_a_failed_run_still_sets_the_weight(self):
        # xadd added the app at weight 1 and the run stopped before setting it: the app has no
        # version yet, so a rerun sets it.
        rows = self.live() + [{"id": 99, "name": "usnm_ocr_native", "weight": "1", "deprecated": "0", "versions": 0}]
        result, weights = self.run_script(rows)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("app usnm_ocr_native already in DB", result.stdout)
        self.assertEqual(weights["usnm_ocr_native"], "0")

    def test_a_weight_that_does_not_hold_fails(self):
        result, weights = self.run_script(self.live(), STUB_IGNORE_UPDATE="1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("app usnm_ocr_native has no app version, but its weight isn't 0", result.stderr)

    def test_an_xadd_failure_fails_before_any_weight_is_set(self):
        result, weights = self.run_script(self.live(), STUB_XADD_STATUS="3")
        self.assertEqual(result.returncode, 1)
        self.assertIn("bin/xadd exited 3", result.stderr)
        self.assertNotIn("usnm_ocr_native", weights)

    def test_the_step_and_a_new_project_register_through_it(self):
        bridge = (HERE / "bridge-job.sh").read_text()
        self.assertIn('exec php "$TEMPLATES/register-apps.php" ;;', bridge)
        self.assertNotIn("exec bin/xadd", bridge)
        self.assertIn('(cd "$PROJ" && php "$TEMPLATES/register-apps.php")', (HERE / "entrypoint.sh").read_text())
        self.assertIn("boinc/images/server/register-apps.php", (HERE / "Containerfile").read_text())

    def test_an_app_without_a_weight_is_refused_before_xadd_runs(self):
        xml = (self.project / "project.xml").read_text()
        (self.project / "project.xml").write_text(xml.replace("<weight>0</weight>", "", 1))
        result, weights = self.run_script(self.live())
        self.assertEqual(result.returncode, 1)
        self.assertIn("app usnm_ocr_native needs a <weight>", result.stderr)
        self.assertNotIn("usnm_ocr_native", weights)



class GoldOnboardShare(unittest.TestCase):
    """bridge-job.sh's gold-onboard tail: share_readable walks the whole download tree, so a pass
    that issued no unit skips it (10 Nov: ~11 minutes over NFS each hourly gold tick)."""

    def tail(self):
        text = (HERE / "bridge-job.sh").read_text()
        start = text.index('    stamp="$(mktemp)"\n    rc=0\n    issued=')
        end = text.index('    exit "$rc" ;;', start)
        return text[start:end]

    def run_tail(self, output, status):
        with tempfile.TemporaryDirectory(prefix="gold-onboard-") as directory:
            directory = Path(directory)
            log = directory / "log"
            stub = directory / "usnm-boinc"
            stub.write_text(f"#!/bin/sh\necho '{output}'\nexit {status}\n")
            stub.chmod(0o755)
            script = ("set -eu\n"
                      f"share_readable() {{ echo walked >> '{log}'; }}\n"
                      f"PROJ='{directory}'\ncontroller='{directory}/c.json'\n" + self.tail() + 'echo "rc=$rc"\n')
            done = subprocess.run(["bash", "-c", script], capture_output=True, text=True, check=True,
                                  env={**os.environ, "PATH": f"{directory}{os.pathsep}{os.environ['PATH']}"})
            return done.stdout.splitlines(), log.exists()

    def test_no_unit_issued_skips_the_walk(self):
        self.assertEqual(self.run_tail("[]", 0), (["[]", "rc=0"], False))

    def test_issued_units_or_a_failure_walk(self):
        self.assertEqual(self.run_tail("[4,9]", 0), (["[4,9]", "rc=0"], True))
        self.assertEqual(self.run_tail("[]", 3), (["[]", "rc=3"], True))


if __name__ == "__main__":
    unittest.main()
