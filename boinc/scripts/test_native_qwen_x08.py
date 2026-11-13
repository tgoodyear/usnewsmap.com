"""native-qwen-cohort.php --plan-class x08: Xiaomi-OCR-0 range admission scoped by root (#1867)."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "boinc/scripts/native-qwen-cohort.php"
CLASSES = ("mt_x08", "apple_gpu_x08", "nvidia_vulkan_x08", "intel_vulkan_x08", "amd_vulkan_x08")

DB = """<?php
$GLOBALS['wus'] = json_decode(file_get_contents(__DIR__.'/../../workunits.json'));
class BoincApp {static function lookup($w){return (object)['id'=>6];}}
class BoincWorkunit {
 static function enum($where) {
  $out = [];
  if (preg_match("/name IN \\\\(([^)]*)\\\\)/", $where, $m)) {
   $names = array_map(fn($n) => trim($n, "'"), explode(',', $m[1]));
   foreach ($GLOBALS['wus'] as $w) if (in_array($w->name, $names, true)) $out[] = $w;
   return $out;
  }
  preg_match('/id>=(\\d+) AND id<=(\\d+)/', $where, $m);
  foreach ($GLOBALS['wus'] as $w)
   if ($w->batch == 108 && $w->id >= $m[1] && $w->id <= $m[2]) $out[] = $w;
  return $out;
 }
}
"""


@unittest.skipUnless(shutil.which("php"), "PHP unavailable")
class X08AdmissionTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.share = Path(tmp.name)
        self.project = self.share / "usnmocr"
        (self.project / "html/inc").mkdir(parents=True)
        (self.project / "config.xml").write_text("<boinc><config/></boinc>")
        (self.project / "html/inc/boinc_db.inc").write_text(DB)
        self.policy = self.project / "plan_class_spec.xml"
        self.policy.write_bytes((ROOT / "boinc/project/plan_class_spec.xml").read_bytes())
        self.wus = []

    def unit(self, root, number, wu_id, batch=108, model="xiaomi-ocr-0-q8", version=5):
        job_id = f"{number:064x}"
        unit = self.share / "qwen" / root / "units" / f"bundle-{number:05d}-x"
        unit.mkdir(parents=True)
        (unit / "qwen.json").write_text(json.dumps({"id": job_id, "model": model, "prompt_version": version}))
        (unit / "submitted").write_text("usnm_qwen_" + job_id)
        self.wus.append({"id": wu_id, "name": "usnm_qwen_" + job_id, "batch": batch, "min_quorum": 2,
                         "target_nresults": 2, "rsc_disk_bound": 12884901888})

    def run_helper(self, *args):
        (self.project / "workunits.json").write_text(json.dumps(self.wus))
        return subprocess.run(["php", str(HELPER), "--project", str(self.project), "--plan-class", "x08",
                               *args], capture_output=True, text=True)

    def windows(self):
        root = ET.fromstring(self.policy.read_bytes())
        return {name: (root.find(f"plan_class[name='{name}']").findtext("min_wu_id"),
                       root.find(f"plan_class[name='{name}']").findtext("max_wu_id")) for name in CLASSES}

    def test_benchmark_roots_open_together_over_their_own_range(self):
        for n, wu in ((1, 500), (2, 503)):
            self.unit("bench-968-x08-en-op", n, wu)
        self.unit("bench-968-x08-fr-52", 3, 507)
        # Another app's workunit (batch 0) inside the range is no batch-108 work, so it is fine.
        self.wus.append({"id": 504, "name": "usnm_quality_x", "batch": 0})
        before = self.policy.read_bytes()
        proc = self.run_helper("--root", "bench-968-x08-en-op", "--root", "bench-968-x08-fr-52")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        proof = json.loads(proc.stdout)
        self.assertEqual((proof["workunit_id"], proof["last_workunit_id"], proof["workunits"], proof["apply"]),
                         (500, 507, 3, False))
        self.assertEqual(self.policy.read_bytes(), before)
        proc = self.run_helper("--root", "bench-968-x08-en-op", "--root", "bench-968-x08-fr-52", "--apply")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(set(self.windows().values()), {("500", "507")})
        # Every other class is untouched.
        root, old = ET.fromstring(self.policy.read_bytes()), ET.fromstring(before)
        for node in old.findall("plan_class"):
            name = node.findtext("name")
            if name not in CLASSES:
                self.assertEqual(ET.tostring(root.find(f"plan_class[name='{name}']")), ET.tostring(node), name)
        proc = self.run_helper("--stop", "--apply")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(set(self.windows().values()), {("2", "1")})

    def test_production_roots_stay_closed_without_the_switch(self):
        self.unit("x08-fre-wf1415", 1, 600)
        before = self.policy.read_bytes()
        proc = self.run_helper("--root", "x08-fre-wf1415", "--apply")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("needs --production", proc.stderr)
        self.assertEqual(self.policy.read_bytes(), before)
        proc = self.run_helper("--root", "x08-fre-wf1415", "--production", "--apply")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(set(self.windows().values()), {("600", "600")})

    def test_a_foreign_batch_108_unit_inside_the_range_refuses_it(self):
        self.unit("bench-968-x08-en-op", 1, 500)
        self.unit("bench-968-x08-en-op", 2, 510)
        self.unit("x08-eng-pilot", 3, 505)  # a production unit between them, not named
        before = self.policy.read_bytes()
        proc = self.run_helper("--root", "bench-968-x08-en-op", "--apply")
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("lies inside the range and is not from a named root", proc.stderr)
        self.assertEqual(self.policy.read_bytes(), before)

    def test_only_xiaomi_roots_and_jobs_are_admitted(self):
        self.unit("qwen-bundle-v3-20261113", 1, 500)
        proc = self.run_helper("--root", "qwen-bundle-v3-20261113", "--apply")
        self.assertIn("is not a Xiaomi-OCR-0 benchmark", proc.stderr)
        self.unit("bench-968-x08-sv-52", 2, 501, model="qwen3-vl-8b-q4km", version=3)
        proc = self.run_helper("--root", "bench-968-x08-sv-52", "--apply")
        self.assertIn("is not a Xiaomi-OCR-0 request-version-5 job", proc.stderr)
        for extra in (("--workunit", "500"), ("--general",)):
            proc = self.run_helper("--root", "bench-968-x08-sv-52", *extra)
            self.assertNotEqual(proc.returncode, 0)
        proc = subprocess.run(["php", str(HELPER), "--project", str(self.project), "--plan-class", "nvidia_vulkan_8b",
                               "--general", "--root", "bench-968-x08-sv-52"], capture_output=True, text=True)
        self.assertIn("--root and --production are for --plan-class x08 only", proc.stderr)
        self.assertEqual(set(self.windows().values()), {("2", "1")})

    def test_a_unit_without_its_workunit_refuses_the_range(self):
        self.unit("bench-968-x08-en-op", 1, 500)
        self.unit("bench-968-x08-en-op", 2, 501)
        self.wus.pop()
        proc = self.run_helper("--root", "bench-968-x08-en-op", "--apply")
        self.assertIn("has no workunit", proc.stderr)


if __name__ == "__main__":
    unittest.main()
