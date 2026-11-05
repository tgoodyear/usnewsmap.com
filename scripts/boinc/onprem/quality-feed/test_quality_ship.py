"""Exercise the shipper's Qwen admission forwarding without network or live state."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
import uuid

HERE = Path(__file__).resolve().parent


class ShipperTest(unittest.TestCase):
    def setUp(self):
        self.root = HERE / (".ship-test-" + uuid.uuid4().hex)
        self.root.mkdir()
        self.addCleanup(shutil.rmtree, self.root)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = {**os.environ, "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
                    "SHIP_TEST_ROOT": str(self.root)}
        self.stub("curl", """#!/bin/sh
cat <<EOF
<server_status><database_file_states>
<results_ready_to_send>${SHIP_TEST_READY:-0}</results_ready_to_send>
<tasks_by_app><app><name>usnm_quality</name><unsent>${SHIP_TEST_QUALITY_READY:-0}</unsent></app>
<app><name>usnm_qwen</name><unsent>${SHIP_TEST_READY:-0}</unsent></app>
</tasks_by_app></database_file_states></server_status>
EOF
""")
        self.stub("tar", "#!/bin/sh\nexit 0\n")
        self.stub("mktemp", "#!/bin/sh\necho \"$SHIP_TEST_ROOT/step.err\"\n")
        self.stub("ssh", """#!/usr/bin/env python3
import json, os, shlex, sys
from pathlib import Path
root = Path(os.environ["SHIP_TEST_ROOT"])
args = sys.argv[1:]
remote = next(i for i, arg in enumerate(args) if arg.startswith("root@")) + 1
# OpenSSH joins remote arguments before invoking a shell: do not accidentally
# validate a local argv list that production will never receive.
args = args[:remote] + shlex.split(" ".join(args[remote:]))
with (root / "calls.jsonl").open("a") as f:
    f.write(json.dumps(args) + "\\n")
if "--max-units" in args:
    sys.exit(int(os.environ.get("SHIP_TEST_SUBMIT_EXIT", "0")))
locked = os.environ.get("SHIP_TEST_LOCKED_STEP")
if locked and locked in args:
    print(os.environ.get("SHIP_TEST_LOCKED_MESSAGE",
                         "error: another bridge step is running on this host"), file=sys.stderr)
    sys.exit(1)
if "df" in args:
    print("Avail\\n100")
    if "/srv/usnm-boinc/project/quality" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
    if "/srv/usnm-boinc/project/usnmocr/download" in args:
        print(os.environ.get("SHIP_TEST_BULK_FREE", "100"))
elif "lvs" in args:
    print(json.dumps({"report": [{"lv": [{"lv_size": os.environ.get(
        "SHIP_TEST_POOL_FREE_BYTES", str(100 * 1024**3)), "data_percent": "0"}]}]}))
elif "/project/ops/qwen-feed/qwen_submit.py" in args:
    preflight = "--preflight" in args
    phase = "preflight" if preflight else "locked_recheck"
    code = int(os.environ.get("SHIP_TEST_PREFLIGHT_EXIT" if preflight
                              else "SHIP_TEST_PUBLISH_EXIT", "0"))
    print(json.dumps({"phase": phase, "status": "deferred" if code == 75 else "admitted",
                      "reason": "fresh_original_watermark" if code == 75 else None}))
    sys.exit(code)
elif "python3" in args and "-" in args:
    sys.stdin.read()
    if "--after-submit" in args:
        print("freed 0 bytes")
    else:
        campaign = args[args.index("--campaign") + 1]
        chunk = args[args.index("--chunk") + 1]
        if os.environ.get("SHIP_TEST_RECEIVE_FAIL") == chunk:
            sys.exit(1)
        print(f"/project/quality/{campaign}/incoming/{chunk}/sample.jsonl\\trus\\t1")
""")

    def stub(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)

    def run_ship(self, extra, native=True, langs="rus\n"):
        chunk = self.root / "outbox/test/c00001"
        chunk.mkdir(parents=True, exist_ok=True)
        (chunk / "READY").write_text("1\n")
        (chunk / "langs.txt").write_text(langs)
        return subprocess.run(
            ["bash", str(HERE / "quality-ship.sh"), "--campaign", "test",
             "--engine-image", "sha256:" + "a" * 64, "--outbox", str(self.root / "outbox"),
             *(["--qwen-engine-plan", "/project/plan.json", "--qwen-models", "/project/models"]
               if native else []),
             *extra], env=self.env, capture_output=True, text=True, timeout=30)

    def test_qwen_watermark_default_and_override_reach_submit(self):
        for extra, expected in (([], "1000"), (["--qwen-max-ready", "20000"], "20000")):
            with self.subTest(expected=expected):
                result = self.run_ship(extra)
                self.assertEqual(result.returncode, 0, result.stderr)
                calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
                submit = next(c for c in reversed(calls) if "/project/ops/qwen-feed/qwen_submit.py" in c)
                self.assertEqual(submit[submit.index("--max-ready") + 1], expected)
                self.assertEqual(submit[submit.index("--max-pages") + 1], "1")
                script = submit[submit.index("-c") + 1]
                probe = subprocess.run(
                    ["bash", "-c", script, "sh", "printf", "locked argv survives"],
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(probe.returncode, 0, probe.stderr)
                self.assertEqual(probe.stdout, "locked argv survives")

    def test_legacy_shipper_cannot_bypass_a_frozen_subset_parent(self):
        journal = self.root / "outbox/.qwen-transactions/test/c00001"
        journal.mkdir(parents=True)
        (journal / "parent.json").write_text("{}")
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("resume with --qwen-subsets", result.stderr)
        self.assertFalse((self.root / "calls.jsonl").exists())
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())

    GUARDED = ["--prompt-version", "3", "--qwen-root", "guard-v3", "--qwen-yiddish-root", "guard-v4"]

    def native_calls(self):
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        return [c for c in calls if "/project/ops/qwen-feed/qwen_submit.py" in c]

    def test_by_default_chunks_keep_their_campaign_and_request_version_1(self):
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        for call in self.native_calls():
            self.assertEqual(call[call.index("--campaign") + 1], "test")
            self.assertEqual(call[call.index("--prompt-version") + 1], "1")

    def test_a_chunk_goes_to_the_root_and_version_of_its_language(self):
        for langs, root, version in (("srp\n", "guard-v3", "3"), ("srp\nspa\n", "guard-v3", "3"),
                                     ("yid\n", "guard-v4", "4")):
            with self.subTest(langs=langs):
                (self.root / "calls.jsonl").unlink(missing_ok=True)
                result = self.run_ship(self.GUARDED, langs=langs)
                self.assertEqual(result.returncode, 0, result.stderr)
                native = self.native_calls()
                self.assertEqual(len(native), 2)
                for call in native:
                    self.assertEqual(call[call.index("--campaign") + 1], root)
                    self.assertEqual(call[call.index("--prompt-version") + 1], version)
                text = (self.root / "calls.jsonl").read_text()
                self.assertIn(f"QUALITY_CAMPAIGN={root}", text)
                self.assertIn(f"QUALITY_GROUP={root}", text)
                self.assertIn(f"/project/quality/{root}", text)
                self.assertNotIn("QUALITY_CAMPAIGN=test", text)
                # The chunk leaves the outbox it came from.
                self.assertFalse((self.root / "outbox/test/c00001").exists())

    def test_yiddish_never_ships_at_request_version_3(self):
        for extra, langs in ((["--prompt-version", "3", "--qwen-root", "guard-v3"], "yid\n"),
                             (self.GUARDED, "srp\nyid\n"), (self.GUARDED, "")):
            with self.subTest(langs=langs):
                (self.root / "calls.jsonl").unlink(missing_ok=True)
                result = self.run_ship(extra, langs=langs)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("READY retained", result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())
                self.assertTrue((self.root / "outbox/test/c00001/READY").exists())

    def test_request_versions_and_roots_are_native_only_and_checked(self):
        for extra, native in ((["--prompt-version", "3"], False), (["--qwen-root", "x"], False),
                              (["--prompt-version", "5"], True), (["--qwen-root", "Bad_Name"], True),
                              (["--qwen-submit-binary", "usnm-boinc"], False),
                              (["--qwen-submit-binary", "-x"], True), (["--qwen-submit-binary", "a b"], True)):
            with self.subTest(extra=extra):
                result = self.run_ship(extra, native=native)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())

    def test_a_submit_binary_reaches_only_the_native_submit(self):
        # #1449: once the image submits every request version, the service names it by config.
        for extra, expected in (([], None), (["--qwen-submit-binary", "usnm-boinc"], "usnm-boinc"),
                                (["--qwen-submit-binary", "/project/ops/x/usnm-boinc"], "/project/ops/x/usnm-boinc")):
            with self.subTest(extra=extra):
                (self.root / "calls.jsonl").unlink(missing_ok=True)
                result = self.run_ship([*self.GUARDED, *extra], langs="srp\n")
                self.assertEqual(result.returncode, 0, result.stderr)
                preflight, publish = self.native_calls()
                self.assertIn("--preflight", preflight)
                self.assertNotIn("--submit-binary", preflight)
                self.assertEqual(publish[publish.index("--submit-binary") + 1] if expected else None, expected)
                self.assertEqual(publish.count("--submit-binary"), 1 if expected else 0)

    def test_a_bundle_size_reaches_both_native_calls_for_the_main_root_only(self):
        # #1435: unset, neither call names a bundle size, so qwen_submit.py stages one page a unit.
        for extra, langs, expected in (([], "srp\n", None), (["--qwen-bundle-size", "8"], "srp\n", "8"),
                                       (["--qwen-bundle-size", "32"], "srp\nspa\n", "32"),
                                       (["--qwen-bundle-size", "8"], "yid\n", None)):
            with self.subTest(extra=extra, langs=langs):
                (self.root / "calls.jsonl").unlink(missing_ok=True)
                result = self.run_ship([*self.GUARDED, *extra], langs=langs)
                self.assertEqual(result.returncode, 0, result.stderr)
                native = self.native_calls()
                self.assertEqual(len(native), 2)
                for call in native:
                    self.assertEqual(call.count("--bundle-size"), 1 if expected else 0)
                    if expected:
                        self.assertEqual(call[call.index("--bundle-size") + 1], expected)
                        self.assertEqual(call[call.index("--campaign") + 1], "guard-v3")

    def test_a_bundle_size_is_native_only_and_bounded(self):
        for extra, native in ((["--qwen-bundle-size", "8"], False), (["--qwen-bundle-size", "1"], True),
                              (["--qwen-bundle-size", "33"], True), (["--qwen-bundle-size", "08"], True)):
            with self.subTest(extra=extra, native=native):
                result = self.run_ship(extra, native=native)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("--qwen-bundle-size", result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())

    def test_stage_and_submit_do_not_wait_for_the_step_window(self):
        # The stage has its own lock on the host, and only native publication reads the clock.
        self.stub("date", "#!/bin/bash\nexit 9\n")
        self.stub("sleep", "#!/bin/bash\necho \"$@\" >> " + str(self.root / "slept") + "\n")
        result = self.run_ship(["--step-window-minutes", "15"], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("waiting for the next step window", result.stderr)
        self.assertFalse((self.root / "slept").exists())
        self.assertIn("shipped 1 chunk(s)", result.stderr)

    def test_a_stage_that_stays_locked_defers_and_retains_ready(self):
        # Another stage holds the stage lock: 60 tries 30 s apart, then exit 75, no acknowledgement.
        self.env["SHIP_TEST_LOCKED_STEP"] = "quality-stage"
        self.stub("sleep", "#!/bin/bash\necho \"$@\" >> " + str(self.root / "slept") + "\n")
        result = self.run_ship(["--step-window-minutes", "15"], native=False)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("quality-stage: the lock stayed held for 30 minutes; READY retained", result.stderr)
        self.assertEqual(len((self.root / "slept").read_text().split()), 60)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("--after-submit" in c for c in calls))

    def run_chunks(self, names, extra=()):
        for name in names:
            chunk = self.root / "outbox/test" / name
            chunk.mkdir(parents=True, exist_ok=True)
            (chunk / "READY").write_text("1\n")
            (chunk / "langs.txt").write_text("eng\n")
        return subprocess.run(
            ["bash", str(HERE / "quality-ship.sh"), "--campaign", "test",
             "--engine-image", "sha256:" + "a" * 64, "--outbox", str(self.root / "outbox"), *extra],
            env=self.env, capture_output=True, text=True, timeout=30)

    def steps(self):
        """What each remote call did, in order: receive, stage, submit or free, with its chunk."""
        out = []
        for c in (json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()):
            if "--after-submit" in c:
                out.append(("free", c[c.index("--chunk") + 1]))
            elif "python3" in c and "--chunk" in c:
                out.append(("receive", c[c.index("--chunk") + 1]))
            elif "quality-stage" in c:
                sample = next(v for v in c if v.startswith("QUALITY_SAMPLE="))
                out.append(("stage", sample.split("/")[-2]))
            elif "--max-units" in c:
                out.append(("submit", None))
        return out

    def test_the_next_chunk_is_received_while_this_one_stages_and_before_it_submits(self):
        result = self.run_chunks(["c00001", "c00002", "c00003"], ["--max-chunks", "3"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("shipped 3 chunk(s)", result.stderr)
        steps = self.steps()
        # Each chunk is received exactly once, before its stage, and the receive ahead is done
        # before the current chunk's submit takes the step lock.
        for n in ("c00001", "c00002", "c00003"):
            self.assertEqual(steps.count(("receive", n)), 1, steps)
            self.assertLess(steps.index(("receive", n)), steps.index(("stage", n)))
        submits = [i for i, s in enumerate(steps) if s[0] == "submit"]
        self.assertEqual(len(submits), 3)
        self.assertLess(steps.index(("receive", "c00002")), submits[0])
        self.assertLess(steps.index(("receive", "c00003")), submits[1])
        self.assertLess(submits[0], steps.index(("free", "c00001")))
        self.assertEqual(list((self.root / "outbox/test").glob("c*")), [])
        self.assertEqual(list((self.root / "outbox").glob(".receive-*")), [])

    def test_max_chunks_bounds_the_receive_ahead(self):
        result = self.run_chunks(["c00001", "c00002"], ["--max-chunks", "1"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(("receive", "c00002"), self.steps())
        self.assertTrue((self.root / "outbox/test/c00002/READY").exists())

    def test_a_failed_receive_ahead_is_received_again_in_its_turn(self):
        self.env["SHIP_TEST_RECEIVE_FAIL"] = "c00002"
        result = self.run_chunks(["c00001", "c00002"], ["--max-chunks", "3"])
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("c00002: receiving ahead failed", result.stderr)
        steps = self.steps()
        self.assertIn(("free", "c00001"), steps)
        self.assertEqual(steps.count(("receive", "c00002")), 2)
        self.assertNotIn(("stage", "c00002"), steps)
        self.assertFalse((self.root / "outbox/test/c00001").exists())
        self.assertTrue((self.root / "outbox/test/c00002/READY").exists())

    def test_a_stage_that_fails_waits_for_the_receive_ahead(self):
        self.env["SHIP_TEST_LOCKED_STEP"] = "quality-stage"
        self.env["SHIP_TEST_LOCKED_MESSAGE"] = "error: the step's container ended with status 1"
        result = self.run_chunks(["c00001", "c00002"], ["--max-chunks", "3"])
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(("receive", "c00002"), self.steps())
        self.assertEqual(list((self.root / "outbox").glob(".receive-*")), [])
        for n in ("c00001", "c00002"):
            self.assertTrue((self.root / "outbox/test" / n / "READY").exists())

    def test_native_chunks_are_never_received_ahead(self):
        for name in ("c00001", "c00002"):
            chunk = self.root / "outbox/test" / name
            chunk.mkdir(parents=True, exist_ok=True)
            (chunk / "READY").write_text("1\n")
            (chunk / "langs.txt").write_text("rus\n")
        result = self.run_ship(["--max-chunks", "2"])
        self.assertEqual(result.returncode, 0, result.stderr)
        steps = self.steps()
        self.assertLess(steps.index(("free", "c00001")), steps.index(("receive", "c00002")))

    def test_submit_runs_in_the_server_under_the_step_lock_with_bounded_units(self):
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        submit = next(c for c in calls if "--max-units" in c)
        self.assertEqual(submit[submit.index("-w") + 1], "1800")
        self.assertIn("/run/usnm-boinc-step.lock", submit)
        self.assertEqual(submit[submit.index("--root") + 1], "/project/quality/test")
        self.assertEqual(submit[submit.index("--max-units") + 1], "6")
        self.assertFalse(any("quality-submit" in c for c in calls))
        script = submit[submit.index("-c") + 1]
        self.assertIn('usnm-boinc quality submit "$@"', script)

    def test_submit_that_stays_locked_or_fails_retains_ready(self):
        for code, message in ((75, "READY retained"), (1, "")):
            with self.subTest(code=code):
                self.env["SHIP_TEST_SUBMIT_EXIT"] = str(code)
                result = self.run_ship([], native=False)
                self.assertEqual(result.returncode, code, result.stderr)
                self.assertIn(message, result.stderr)
                self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
                calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
                self.assertFalse(any("--after-submit" in c for c in calls))

    def test_a_step_that_fails_for_another_reason_is_not_deferred(self):
        self.env["SHIP_TEST_LOCKED_STEP"] = "quality-stage"
        self.env["SHIP_TEST_LOCKED_MESSAGE"] = "error: the step's container ended with status 1"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("ended with status 1", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())

    def test_native_submit_waits_for_the_step_window_too(self):
        minutes = self.root / "minutes"
        minutes.write_text("40:00\n14:30\n")
        self.stub("date", f"""#!/bin/bash
head -1 {minutes}
sed -i.bak 1d {minutes}
""")
        self.stub("sleep", "#!/bin/bash\nexit 0\n")
        result = self.run_ship(["--step-window-minutes", "15"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr.count("waiting for the next step window"), 1)
        # At 14:30 of a 15-minute window, the lock wait ends with the window.
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        submit = next(c for c in reversed(calls) if "/project/ops/qwen-feed/qwen_submit.py" in c)
        self.assertEqual(submit[submit.index("-w") + 1], "30")

    def test_step_window_is_off_by_default_and_checked(self):
        self.stub("date", "#!/bin/bash\nexit 9\n")
        self.assertEqual(self.run_ship([], native=False).returncode, 0)
        result = self.run_ship(["--step-window-minutes", "60"], native=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("--step-window-minutes", result.stderr)

    def test_full_watermark_defers_repeated_ticks_before_any_transfer_or_stage(self):
        self.env["SHIP_TEST_PREFLIGHT_EXIT"] = "75"
        for _ in range(2):
            result = self.run_ship(["--qwen-max-ready", "200"])
            self.assertEqual(result.returncode, 75, result.stderr)
            self.assertEqual(json.loads(result.stdout)["status"], "deferred")
            self.assertIn("transferred 0 pages; READY retained", result.stderr)
            self.assertNotIn("submitted,", result.stderr)
            self.assertEqual((self.root / "outbox/test/c00001/READY").read_text(), "1\n")
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        preflights = [c for c in calls if "--preflight" in c]
        self.assertEqual(len(preflights), 2)
        self.assertTrue(all(c[c.index("--max-pages") + 1] == "1"
                            and c[c.index("--max-ready") + 1] == "200" for c in preflights))
        self.assertFalse(any("tar" in c or "--step" in c or "flock" in c
                             or "--after-submit" in c for c in calls))

    def test_capacity_change_under_project_lock_retains_ready_without_acknowledgement(self):
        self.env["SHIP_TEST_PUBLISH_EXIT"] = "75"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn("native publication deferred; READY retained", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("tar" in c for c in calls))
        self.assertTrue(any("--preflight" in c for c in calls))
        self.assertTrue(any("flock" in c and "/run/usnm-boinc-step.lock" in c for c in calls))
        self.assertFalse(any("--max-units" in c or "--after-submit" in c for c in calls))
        self.env["SHIP_TEST_PUBLISH_EXIT"] = "0"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((self.root / "outbox/test/c00001/READY").exists())

    def test_uncertainty_is_not_misclassified_as_normal_preflight_backpressure(self):
        self.env["SHIP_TEST_PREFLIGHT_EXIT"] = "1"
        result = self.run_ship([])
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertNotIn("preflight deferred", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in c or "--step" in c for c in calls))

    def test_qwen_watermark_rejects_nonpositive_noninteger_and_missing_values(self):
        for value in ("0", "-1", "1.5", "oops", "", None):
            with self.subTest(value=value):
                result = self.run_ship(["--qwen-max-ready"] + ([value] if value is not None else []))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("--qwen-max-ready: a positive integer", result.stderr)
                self.assertFalse((self.root / "calls.jsonl").exists())

    def test_bulk_mount_floor_blocks_even_when_project_root_has_space(self):
        self.env["SHIP_TEST_BULK_FREE"] = "39"
        result = self.run_ship(["--min-free-gb", "50"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("39 GB free (under 50)", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in call or "/project/ops/qwen-feed/qwen_submit.py" in call
                             for call in calls))

    def test_thin_pool_reserves_inputs_above_floor_before_copy(self):
        self.env["SHIP_TEST_POOL_FREE_BYTES"] = str(50 * 1024**3 + 1024**2)
        result = self.run_ship(["--min-free-gb", "50"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("including per-page reservation", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertFalse(any("tar" in call or "/project/ops/qwen-feed/qwen_submit.py" in call
                             for call in calls))

    def test_native_shipping_does_not_wait_for_unrelated_global_replicas(self):
        self.env["SHIP_TEST_READY"] = "5000"
        self.stub("curl", "#!/bin/sh\nexit 1\n")
        result = self.run_ship([])
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("/project/ops/qwen-feed/qwen_submit.py" in call for call in calls))

    def test_quality_shipping_does_not_wait_for_unrelated_gpu_work(self):
        self.env["SHIP_TEST_READY"] = "5000"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("shipped 1 chunk(s)", result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertTrue(any("--max-units" in call for call in calls))

    def test_quality_queue_retains_its_own_unsent_backpressure(self):
        self.env["SHIP_TEST_QUALITY_READY"] = "301"
        result = self.run_ship([], native=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("301 results unsent (over 300)", result.stderr)
        self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
        self.assertFalse((self.root / "calls.jsonl").exists())

    def test_quality_status_failure_never_admits_or_removes_ready_chunk(self):
        for body in (
            "<server_status/>",
            "<server_status><database_file_states><tasks_by_app>"
            "<app><name>usnm_qwen</name><unsent>0</unsent></app>"
            "</tasks_by_app></database_file_states></server_status>",
            "<server_status><database_file_states><tasks_by_app>"
            "<app><name>usnm_quality</name><unsent>bad</unsent></app>"
            "</tasks_by_app></database_file_states></server_status>",
            "not XML",
        ):
            with self.subTest(body=body):
                self.stub("curl", "#!/bin/sh\nprintf '%s' '" + body + "'\n")
                result = self.run_ship([], native=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue((self.root / "outbox/test/c00001/READY").exists())
                self.assertFalse((self.root / "calls.jsonl").exists())

    @unittest.skipUnless(shutil.which("flock"), "Linux shipper uses util-linux flock")
    def test_inline_and_independent_native_shippers_cannot_clean_same_chunk(self):
        import fcntl
        outbox = self.root / "outbox"
        outbox.mkdir()
        with (outbox / ".qwen-ship-test.lock").open("w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            result = self.run_ship([])
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertTrue((outbox / "test/c00001/READY").exists())
        self.assertFalse((self.root / "calls.jsonl").exists())
