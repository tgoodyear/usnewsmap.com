"""Partial publication: current inputs, explicit coverage, fail-closed loads and replay."""
import fcntl
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import types
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("quality_results_partial", HERE / "quality_results_partial.py")
partial = importlib.util.module_from_spec(spec)
spec.loader.exec_module(partial)


class PartialTest(unittest.TestCase):
    def setUp(self):
        env = mock.patch.dict(os.environ, {"CTID": "101"})
        env.start()
        self.addCleanup(env.stop)
        directory = tempfile.TemporaryDirectory(dir=HERE)
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.snapshot = "a" * 32
        self.campaign = "test"
        self.job = "b" * 64
        self.args = types.SimpleNamespace(
            campaign=self.campaign, snapshot=self.snapshot, after="", through="", qwen_campaign=None,
            collector=str(self.root / "collector"), server_ct=230, ctid=215,
            server_host="server.example", state_dir=self.root / "state",
            engine_image="sha256:" + "c" * 64, snapshot_dir=self.root / "snapshot",
        )
        Path(self.args.collector).write_text("stand-in collector\n")
        self.receipt = {
            "schema": 1, "kind": "verified-quality-unit-partial",
            "campaign": self.campaign, "snapshot": self.snapshot, "campaign_complete": False,
            "coverage": "selected-unit-only-at-collection",
            "submitted_units_observed": 2, "unvisited_units": 1,
            "selected_unit": "bundle-a", "selected_job": self.job,
            "cycle_through": "bundle-b",
            "selected_doc_ids": ["doc-a"], "collector_sha256": "d" * 64,
            "files": {name: "e" * 64 for name in partial.FILES},
            "snapshot_dir": "/project/ops/quality-results/scoped/test/"
                            + hashlib.sha256(b"bundle-a").hexdigest()
                            + "/snapshots/" + self.snapshot,
        }

    def cache_inventory(self, inventory):
        path = self.root / "ops/quality-results/scoped/test" / partial.INVENTORY_FILE
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"schema": 1, "generated_at": 0, "units": inventory}))
        return path

    def responses(self):
        loaded = {"snapshot": self.snapshot, "campaign": self.campaign,
                  "campaign_complete": False, "selected_unit": "bundle-a", "pages": 1, "reader_rows": 1}
        return [
            subprocess.CompletedProcess(
                ["ssh"], 0, stdout=json.dumps(self.receipt).encode(),
                stderr=(partial.LOCK_ACQUIRED + "\n").encode()),
            None, types.SimpleNamespace(stdout=b"archive"), None,
            types.SimpleNamespace(stdout=json.dumps(loaded).encode()),
        ]

    def test_inventory_needs_both_native_spools_readable(self):
        project = self.root / "project"
        (project / "qwen" / self.campaign / "units").mkdir(parents=True)
        (project / "usnmocr/results/8").mkdir(parents=True)
        args = types.SimpleNamespace(qwen_campaign=self.campaign, campaign="test", collector="collector")
        with mock.patch.object(partial.subprocess, "run") as run:
            with self.assertRaises(FileNotFoundError):
                partial.completion_inventory(args, project, self.root / "base")
            run.assert_not_called()
            (project / "usnmocr/results/108").mkdir()
            run.return_value = subprocess.CompletedProcess([], 0, stdout="{}", stderr="")
            partial.completion_inventory(args, project, self.root / "base")
            command = run.call_args.args[0]
            self.assertEqual(command[command.index("--qwen-xiaomi-spool") + 1],
                             str(project / "usnmocr/results/108"))

    def test_scoped_collection_rechecks_real_dependencies_and_never_uses_old_snapshot(self):
        root = self.root / "project"
        units = root / "quality/test/units"
        for name in ["bundle-a", "bundle-b"]:
            unit = units / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        canonical = root / "usnmocr/results/8/real-canonical"
        canonical.parent.mkdir(parents=True)
        self.args.qwen_campaign = self.campaign
        self.args.collector = str(self.root / "collector")
        Path(self.args.collector).write_text(
            "#!" + sys.executable + "\n"
            "from pathlib import Path\nimport sys\n"
            "out=Path(sys.argv[sys.argv.index('--out-dir')+1]);out.mkdir(exist_ok=True)\n"
            f"source=Path({str(canonical)!r})\n"
            "data=source.read_bytes() if source.exists() else b'pending'\n"
            "for name in ['pages.parquet','crops.parquet','summary.md']:\n"
            " (out/name).write_bytes(data)\n"
        )
        Path(self.args.collector).chmod(0o700)
        first = partial.scoped_collect(self.args, root)
        self.assertFalse(first["campaign_complete"])
        self.assertEqual((first["submitted_units_observed"], first["unvisited_units"]), (2, 1))
        self.assertFalse((root / "quality/test/out").exists())
        old_snapshot = Path(first["snapshot_dir"]) / "out/pages.parquet"
        self.assertEqual(old_snapshot.read_bytes(), b"pending")
        canonical.write_bytes(b"actual late canonical")
        self.args.snapshot = "f" * 32
        late = partial.scoped_collect(self.args, root)
        self.assertNotEqual(first["files"], late["files"])
        self.assertEqual(old_snapshot.read_bytes(), b"pending")
        canonical.write_bytes(b"changed canonical")
        self.args.snapshot = "e" * 32
        changed = partial.scoped_collect(self.args, root)
        self.assertNotEqual(late["files"], changed["files"])
        self.args.snapshot = "d" * 32
        repeated = partial.scoped_collect(self.args, root)
        self.assertEqual(changed["files"], repeated["files"])

    def test_failed_phase_or_replayed_commit_never_advances_cursor(self):
        fixed = mock.patch.object(partial.uuid, "uuid4",
                                  return_value=types.SimpleNamespace(hex=self.snapshot))
        with fixed:
            for phase in range(5):
                responses = self.responses()
                responses[phase] = subprocess.CalledProcessError(1, ["phase"])
                with self.subTest(phase=phase), mock.patch.object(
                        partial.subprocess, "run", side_effect=responses) as run:
                    with self.assertRaises(subprocess.CalledProcessError):
                        partial.host_pass(self.args)
                    self.assertEqual(run.call_count, phase + 1)
                    self.assertFalse((self.args.state_dir / "test/cursor.json").exists())
            actual_atomic = partial.atomic_json

            def lost_cursor(path, value):
                if path.name == "cursor.json":
                    raise OSError("crash after successful database upsert")
                actual_atomic(path, value)

            with mock.patch.object(partial.subprocess, "run", side_effect=self.responses()), \
                    mock.patch.object(partial, "atomic_json", side_effect=lost_cursor):
                with self.assertRaises(OSError):
                    partial.host_pass(self.args)
            self.assertFalse((self.args.state_dir / "test/cursor.json").exists())
            with mock.patch.object(partial.subprocess, "run", side_effect=self.responses()) as run:
                report = partial.host_pass(self.args)
            self.assertFalse(report["campaign_complete"])
            state = json.loads((self.args.state_dir / "test/cursor.json").read_bytes())
            self.assertEqual(state["after"], "bundle-a")
            self.assertEqual(state["through"], "bundle-b")
            self.assertIn("flock -n", run.call_args_list[0].args[0][-1])
            self.assertIn("-E 75", run.call_args_list[0].args[0][-1])
            self.assertIn("240s", run.call_args_list[0].args[0][-1])
            for call in run.call_args_list[2:4]:
                command = call.args[0]
                if command[0] == "ssh":
                    command = shlex.split(command[-1])
                inner = command.index("timeout")
                seconds = float(command[inner + 3].removesuffix("s"))
                grace = float(command[inner + 2].removeprefix("--kill-after=").removesuffix("s"))
                self.assertLess(seconds + grace, call.kwargs["timeout"])
                self.assertEqual(command[inner + 4], "tar")

    def warm_response(self, code=0, unit="bundle-a", late=False):
        lines = [json.dumps({"phase": "warm_selected", "unit": unit})]
        if late:
            lines.append(json.dumps({"phase": "collection_out_of_time", "step": "warm", "unit": unit,
                                     "budget_seconds": partial.WARM_SECONDS - 20}))
        return subprocess.CompletedProcess(["ssh"], code, b"", ("\n".join(lines) + "\n").encode())

    def test_warm_without_lock_warms_the_pass_unit_before_taking_the_lock(self):
        self.args.warm_without_lock = True
        with mock.patch.object(partial.uuid, "uuid4", return_value=types.SimpleNamespace(hex=self.snapshot)), \
                mock.patch.object(partial.subprocess, "run",
                                  side_effect=[self.warm_response(), *self.responses()]) as run, \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            partial.host_pass(self.args)
        warm, locked = (shlex.split(call.args[0][-1]) for call in run.call_args_list[:2])
        self.assertIn("--remote-warm", warm)
        self.assertNotIn("flock", warm)
        self.assertEqual(warm[warm.index("timeout") + 3], f"{partial.WARM_SECONDS}s")
        self.assertIn("flock", locked)
        self.assertIn("--warm-without-lock", locked)
        self.assertIn('"phase": "warm_done", "unit": "bundle-a"', log.getvalue())
        self.assertEqual(json.loads((self.args.state_dir / "test/cursor.json").read_bytes())["after"],
                         "bundle-a")

    def test_a_warm_out_of_time_defers_the_pass_without_the_lock(self):
        self.args.warm_without_lock = True
        self.args.out_of_time_passes = 2
        counts = self.args.state_dir / "test" / partial.OUT_OF_TIME_FILE
        for attempt, response in enumerate([self.warm_response(75, late=True), self.warm_response(124)], 1):
            with self.subTest(attempt=attempt), \
                    mock.patch.object(partial.subprocess, "run", return_value=response) as run, \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(SystemExit) as raised:
                    partial.host_pass(self.args)
                self.assertEqual(run.call_count, 1)  # the lock was never asked for
                self.assertIn('"phase": "warm_deferred"', log.getvalue())
                self.assertEqual(json.loads(counts.read_bytes())["units"], {"bundle-a": attempt})
        self.assertEqual(raised.exception.code, "bundle-a: out of time in 2 passes in a row")
        self.assertFalse((self.args.state_dir / "test/cursor.json").exists())

    def frontier_units(self):
        inventory = {
            "a-ready": {"status": "canonical-present", "fingerprint": "a" * 64},
            "b-ready": {"status": "canonical-present", "fingerprint": "b" * 64},
        }
        for name in inventory:
            unit = self.root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        self.cache_inventory(inventory)
        self.args.completion_frontier = True
        return inventory

    def test_a_warm_that_runs_out_of_time_advances_the_frontier_past_its_unit(self):
        self.frontier_units()
        frontier = self.root / "ops/quality-results/scoped/test/frontier.json"
        seen = []
        for _ in range(2):
            with mock.patch.object(partial.subprocess, "run",
                                   side_effect=subprocess.TimeoutExpired(["collector"], 580)), \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(partial.OutOfTime):
                    partial.scoped_warm(self.args, self.root)
            seen.append(json.loads(log.getvalue().splitlines()[0])["unit"])
            state = json.loads(frontier.read_bytes())
            self.assertEqual(state["loaded"], {})  # scheduling progress, not a load acknowledgement
        # The next warm (and so the locked pass) visits the other ready unit.
        self.assertEqual(seen, ["a-ready", "b-ready"])

    def test_a_warm_stopped_by_the_remote_limit_advances_the_frontier_too(self):
        self.frontier_units()
        frontier = self.root / "ops/quality-results/scoped/test/frontier.json"

        def hang(command, **_kwargs):
            signal_handler = partial.signal.getsignal(partial.signal.SIGTERM)
            signal_handler(partial.signal.SIGTERM, None)

        with mock.patch.object(partial.subprocess, "run", side_effect=hang), \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO):
            with self.assertRaises(partial.OutOfTime):
                partial.scoped_warm(self.args, self.root)
        self.assertEqual(json.loads(frontier.read_bytes())["priority_after"], "a-ready")
        self.assertEqual(partial.signal.getsignal(partial.signal.SIGTERM), partial.signal.SIG_DFL)

    def warm_run(self, deferred):
        progress = {"phase": "warm", "progress": {"dirty": 0, "deferred": deferred}}
        return subprocess.CompletedProcess(["collector"], 0, stdout="", stderr=json.dumps(progress) + "\n")

    def test_a_warm_that_stored_no_part_is_retried_then_defers_the_pass(self):
        unit = self.root / "quality/test/units/bundle-a"
        unit.mkdir(parents=True)
        (unit / "submitted").touch()
        (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        with mock.patch.object(partial.time, "sleep"), \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            with mock.patch.object(partial.subprocess, "run",
                                   side_effect=[self.warm_run(True), self.warm_run(False)]) as run:
                warmed = partial.scoped_warm(self.args, self.root)
            self.assertEqual((warmed["phase"], run.call_count), ("warmed", 2))
            self.assertIn('"phase": "warm_retry"', log.getvalue())
            # Still deferred when the budget is spent: the pass is deferred, not warmed.
            clock = iter([0.0, 1.0, 2.0, 1000.0, 1001.0])
            with mock.patch.object(partial.time, "monotonic", side_effect=lambda: next(clock)), \
                    mock.patch.object(partial.subprocess, "run", return_value=self.warm_run(True)):
                with self.assertRaises(partial.OutOfTime):
                    partial.scoped_warm(self.args, self.root)

    def test_a_failed_warm_leaves_the_error_to_the_locked_pass(self):
        self.args.warm_without_lock = True
        with mock.patch.object(partial.uuid, "uuid4", return_value=types.SimpleNamespace(hex=self.snapshot)), \
                mock.patch.object(partial.subprocess, "run",
                                  side_effect=[self.warm_response(1), *self.responses()]) as run, \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            partial.host_pass(self.args)
        self.assertEqual(run.call_count, 6)
        self.assertIn('"phase": "warm_failed", "exit": 1', log.getvalue())

    def test_scoped_warm_keys_the_pass_unit_alike_and_shares_one_digest_directory(self):
        root = self.root / "project"
        for name in ["bundle-a", "bundle-b"]:
            unit = root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        calls = self.root / "calls.jsonl"
        Path(self.args.collector).write_text(
            "#!" + sys.executable + "\n"
            "import json, sys\nfrom pathlib import Path\n"
            f"open({str(calls)!r}, 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n"
            "if '--out-dir' in sys.argv:\n"
            " out = Path(sys.argv[sys.argv.index('--out-dir') + 1])\n"
            " for name in ['pages.parquet', 'crops.parquet', 'summary.md']:\n"
            "  (out / name).write_bytes(b'rows')\n"
        )
        Path(self.args.collector).chmod(0o700)
        self.args.after = "bundle-a"
        self.args.warm_without_lock = True
        with mock.patch.object(sys, "stderr", new_callable=io.StringIO):
            warmed = partial.scoped_warm(self.args, root)
            receipt = partial.scoped_collect(self.args, root)
        self.assertEqual((warmed["unit"], receipt["selected_unit"]), ("bundle-b", "bundle-b"))
        warm, locked = (json.loads(line) for line in calls.read_text().splitlines())
        self.assertIn("--warm-only", warm)
        self.assertNotIn("--out-dir", warm)
        self.assertNotIn("--warm-only", locked)
        # The same call but for its output, so the locked collection finds the warmed part.
        self.assertEqual(warm[:-1], locked[:locked.index("--out-dir")])
        base = root / "ops/quality-results/scoped/test"
        self.assertEqual(warm[warm.index("--digest-cache") + 1], str(base / partial.DIGESTS_DIR))
        with mock.patch.object(partial.subprocess, "run") as run:
            run.return_value = subprocess.CompletedProcess([], 0, stdout="{}", stderr="")
            partial.completion_inventory(self.args, root, base)
            command = run.call_args.args[0]
        self.assertEqual(command[command.index("--digest-cache") + 1], str(base / partial.DIGESTS_DIR))
        # Without the flag, collectors that predate `--digest-cache` keep working.
        self.args.warm_without_lock = False
        self.args.snapshot = "f" * 32
        with mock.patch.object(sys, "stderr", new_callable=io.StringIO):
            partial.scoped_collect(self.args, root)
        self.assertNotIn("--digest-cache", json.loads(calls.read_text().splitlines()[-1]))

    def test_remote_failure_diagnostics_distinguish_lock_conflicts_without_advancing(self):
        for code, acquired, phase in [(75, False, "step_lock_busy"),
                                      (75, True, "collection_failed"),
                                      (255, False, "remote_admission_failed")]:
            stderr = (partial.LOCK_ACQUIRED + "\n" if acquired else "") + "remote diagnostic\n"
            response = subprocess.CompletedProcess(["ssh"], code, b"", stderr.encode())
            with self.subTest(phase=phase), mock.patch.object(
                    partial.subprocess, "run", return_value=response) as run, \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                expected_error = SystemExit if phase == "step_lock_busy" else subprocess.CalledProcessError
                with self.assertRaises(expected_error) as raised:
                    partial.host_pass(self.args)
                if phase == "step_lock_busy":
                    self.assertEqual(raised.exception.code, 75)
                self.assertEqual(run.call_count, 1)
                self.assertIn("remote diagnostic", log.getvalue())
                self.assertIn(phase, log.getvalue())
                self.assertFalse((self.args.state_dir / "test/cursor.json").exists())

    def test_step_lock_deferral_preserves_existing_cursor_and_acknowledgement(self):
        state = self.args.state_dir / "test/cursor.json"
        state.parent.mkdir(parents=True)
        original = json.dumps({
            "after": "bundle-a", "through": "bundle-b",
            "frontier_ack": {"unit": "bundle-a", "fingerprint": "a" * 64},
        }).encode()
        state.write_bytes(original)
        response = subprocess.CompletedProcess(["ssh"], 75, b"", b"")
        with mock.patch.object(partial.subprocess, "run", return_value=response) as run, \
                mock.patch.object(partial, "atomic_json") as write, \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            with self.assertRaises(SystemExit) as raised:
                partial.host_pass(self.args)
        self.assertEqual(raised.exception.code, 75)
        self.assertEqual(run.call_count, 1)
        self.assertIn("step_lock_busy", log.getvalue())
        write.assert_not_called()
        self.assertEqual(state.read_bytes(), original)

    def test_the_hold_is_journaled_from_the_acquire_and_release_times(self):
        stderr = (partial.LOCK_ACQUIRED + " 2026-11-12T08:00:00Z\ndiagnostic\n"
                  + partial.LOCK_RELEASED + " 2026-11-12T08:00:41Z\n").encode()
        with mock.patch.object(partial.sys, "stderr", new_callable=io.StringIO) as log:
            self.assertTrue(partial.replay_collection_diagnostics(stderr))
        held = [json.loads(line) for line in log.getvalue().splitlines() if "step_lock_held" in line]
        self.assertEqual(held, [{"phase": "step_lock_held", "seconds": 41.0}])
        # A receipt with no time, or one cut short by a kill, is still an acquisition.
        with mock.patch.object(partial.sys, "stderr", new_callable=io.StringIO) as log:
            self.assertTrue(partial.replay_collection_diagnostics((partial.LOCK_ACQUIRED + "\n").encode()))
            self.assertTrue(partial.replay_collection_diagnostics(
                (partial.LOCK_ACQUIRED + " 2026-11-12T08:00:00Z\n").encode()))
        self.assertNotIn("step_lock_held", log.getvalue())
        self.assertFalse(partial.replay_collection_diagnostics(b"remote diagnostic\n"))
        with mock.patch.object(partial.sys, "stderr", new_callable=io.StringIO):
            # A receipt cut off right after its space, as a transport timeout can leave it.
            self.assertTrue(partial.replay_collection_diagnostics((partial.LOCK_ACQUIRED + " \n").encode()))

    def test_host_lock_deferral_never_starts_remote_work(self):
        lock_path = self.root / "host.lock"
        with lock_path.open("a") as lock:
            partial.fcntl.flock(lock, partial.fcntl.LOCK_EX | partial.fcntl.LOCK_NB)
            with mock.patch.object(partial, "host_pass") as host, \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(SystemExit) as raised:
                    partial.main([
                        "--campaign", "test", "--engine-image", self.args.engine_image,
                        "--lock", str(lock_path), "--state-dir", str(self.args.state_dir),
                    ])
        self.assertEqual(raised.exception.code, 75)
        host.assert_not_called()
        self.assertIn("host_lock_busy", log.getvalue())
        self.assertFalse((self.args.state_dir / "test/cursor.json").exists())

    def test_success_without_lock_receipt_never_transfers_or_loads(self):
        response = subprocess.CompletedProcess(["ssh"], 0, b"{}", b"")
        with mock.patch.object(partial.subprocess, "run", return_value=response) as run:
            with self.assertRaisesRegex(RuntimeError, "Missing step-lock acquisition receipt"):
                partial.host_pass(self.args)
            self.assertEqual(run.call_count, 1)
        self.assertFalse((self.args.state_dir / "test/cursor.json").exists())

    def test_collection_timeout_preserves_received_diagnostics_without_loading(self):
        error = subprocess.TimeoutExpired(
            ["ssh"], 210, stderr=(partial.LOCK_ACQUIRED + "\nremote timeout\n").encode())
        with mock.patch.object(partial.subprocess, "run", side_effect=error) as run, \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            with self.assertRaises(subprocess.TimeoutExpired):
                partial.host_pass(self.args)
            self.assertEqual(run.call_count, 1)
            self.assertIn("remote timeout", log.getvalue())
            self.assertIn('"step_lock_acquired": true', log.getvalue())
        self.assertFalse((self.args.state_dir / "test/cursor.json").exists())

    def test_coverage_cannot_claim_completeness_or_cross_snapshot(self):
        for key, value in [("campaign_complete", True), ("snapshot", "f" * 32),
                           ("selected_unit", "../wrong"), ("selected_job", "old"),
                           ("snapshot_dir", "/project/quality/test/out"), ("unvisited_units", 0),
                           ("cycle_through", "../wrong"), ("cycle_through", "bundle-0")]:
            with self.subTest(key=key):
                receipt = {**self.receipt, key: value}
                with self.assertRaises(ValueError):
                    partial.validate_receipt(receipt, self.campaign, self.snapshot)
        self.assertEqual(partial.select_unit(["c", "a", "b"], "b"), ("c", "c"))
        self.assertEqual(partial.select_unit(["c", "a", "b"], "c"), ("a", "c"))

    def test_continuous_arrivals_cannot_starve_revisiting_prior_units(self):
        names = ["a", "b", "c"]
        after, through = "a", "c"
        selected = []
        for arriving in ["d", "e", "f"]:
            names.append(arriving)
            after, through = partial.select_unit(names, after, through)
            selected.append(after)
        self.assertEqual(selected, ["b", "c", "a"])
        self.assertEqual(through, "f")

    def test_completion_frontier_prioritizes_changes_and_fairly_revisits_pending(self):
        ready = lambda key: {"status": "canonical-present", "fingerprint": key * 64}
        inventory = {f"old-{i:03}": ready("a") for i in range(125)}
        inventory["pending"] = {"status": "pending", "fingerprint": None}
        state = {"schema": 1, "loaded": {n: v["fingerprint"] for n, v in inventory.items()
                                        if v["fingerprint"]}}
        inventory["new"] = ready("b")
        selected, _, lane, state = partial.select_frontier(inventory, state)
        self.assertEqual((selected, lane), ("new", "priority"))
        state["loaded"][selected] = inventory[selected]["fingerprint"]
        inventory["pending"] = ready("c")
        selected, _, lane, state = partial.select_frontier(inventory, state)
        self.assertEqual((selected, lane), ("pending", "priority"))
        state["loaded"][selected] = inventory[selected]["fingerprint"]
        inventory["old-010"] = ready("d")  # Late independent readings invalidate the same key.
        selected, _, lane, state = partial.select_frontier(inventory, state)
        self.assertEqual((selected, lane), ("old-010", "priority"))
        # An endless stream of dirty arrivals cannot displace the finite fair lane.
        seen = []
        for i in range(4 * len(inventory)):
            inventory[f"z-{i:04}"] = ready("e")
            selected, _, lane, state = partial.select_frontier(inventory, state)
            if lane == "fair":
                seen.append(selected)
            state = json.loads(json.dumps(state))  # Restart at every pass.
        self.assertIn("old-124", seen)
        self.assertIn("pending", seen)

    def test_completion_frontier_loads_new_units_before_reloads(self):
        ready = lambda key: {"status": "canonical-present", "fingerprint": key * 64}
        inventory = {f"a-{i:03}": ready("a") for i in range(50)}
        state = {"schema": 1, "loaded": {n: v["fingerprint"] for n, v in inventory.items()}}
        inventory = {n: ready("b") for n in inventory}  # A collector change dirties every unit.
        for i in range(10):
            inventory[f"z-new-{i}"] = ready("c")  # First collected after the change.
        picks = []
        for _ in range(8):
            selected, _, lane, state = partial.select_frontier(inventory, state)
            state = json.loads(json.dumps(state))  # Restart at every pass.
            picks.append(selected)
            if lane == "priority":
                state["loaded"][selected] = inventory[selected]["fingerprint"]
        # Two passes in four take new units, one a reload, one the fair lane.
        self.assertEqual(picks, ["z-new-0", "z-new-1", "a-000", "a-000",
                                 "z-new-2", "z-new-3", "a-001", "a-001"])

    def test_completion_frontier_failures_stay_in_their_cohort(self):
        ready = lambda key: {"status": "canonical-present", "fingerprint": key * 64}
        old = {f"a-{i:03}": ready("a") for i in range(6)}
        state = {"schema": 1, "loaded": {n: v["fingerprint"] for n, v in old.items()}}
        inventory = {n: ready("b") for n in old}
        inventory.update({f"z-new-{i}": ready("c") for i in range(4)})
        failing = {"a-000", "z-new-0", "z-new-1"}  # Never acknowledged.
        picks = []
        for _ in range(16):
            selected, _, lane, state = partial.select_frontier(inventory, state)
            state = json.loads(json.dumps(state))  # Restart at every pass.
            if lane == "priority":
                picks.append(selected)
                if selected not in failing:
                    state["loaded"][selected] = inventory[selected]["fingerprint"]
        # Failing first loads don't keep z-new-2 and z-new-3 waiting, and a failing reload
        # doesn't keep the other reloads waiting.
        self.assertTrue({"z-new-2", "z-new-3"} <= set(picks[:6]))
        self.assertTrue({"a-001", "a-002", "a-003"} <= set(picks))

    def test_a_loading_run_leaves_the_inventory_refresh_to_its_own_service(self):
        self.args.completion_frontier = True
        self.args.inventory_refresh_secs = 3600
        self.receipt["frontier"] = {
            "schema": 1, "lane": "priority", "status": "canonical-present", "fingerprint": "f" * 64,
        }
        for inline in (True, False):
            with self.subTest(no_inline_inventory=not inline):
                self.args.no_inline_inventory = not inline
                refresh = mock.patch.object(partial, "host_refresh_inventory")
                refresh_mock = refresh.start()
                self.addCleanup(refresh.stop)
                with mock.patch.object(partial.uuid, "uuid4",
                                       return_value=types.SimpleNamespace(hex=self.snapshot)):
                    with mock.patch.object(partial.subprocess, "run", side_effect=self.responses()):
                        partial.host_pass(self.args)
                self.assertEqual(refresh_mock.called, inline)
                refresh.stop()

    def test_the_inventory_service_takes_its_own_lock_not_the_loaders(self):
        lock = self.root / "inventory.lock"
        loader_lock = self.root / "loader.lock"
        argv = ["--refresh-inventory-only", "--campaign", self.campaign, "--state-dir", str(self.root / "state"),
                "--inventory-lock", lock.as_posix(), "--lock", loader_lock.as_posix()]
        with mock.patch.object(partial, "host_refresh_inventory", return_value={"phase": "inventory_refreshed"}) as refresh:
            with open(loader_lock, "a") as held:  # a loading run holds the host lock for minutes
                fcntl.flock(held, fcntl.LOCK_EX)
                partial.main(argv)
            refresh.assert_called_once()
            with open(lock, "a") as held:
                fcntl.flock(held, fcntl.LOCK_EX)
                with self.assertRaises(SystemExit) as busy:
                    partial.main(argv)
            self.assertEqual(busy.exception.code, 75)
            refresh.assert_called_once()

    def test_frontier_ack_is_only_persisted_after_verified_nonempty_load(self):
        self.args.completion_frontier = True
        self.args.inventory_refresh_secs = 3600
        refresh = mock.patch.object(partial, "host_refresh_inventory")
        self.addCleanup(refresh.stop)
        refresh.start()
        self.receipt["frontier"] = {
            "schema": 1, "lane": "priority", "status": "canonical-present", "fingerprint": "f" * 64,
        }
        cursor = self.args.state_dir / "test/cursor.json"
        partial.atomic_json(cursor, {"after": "old", "through": "old", "coverage": "legacy"})
        with mock.patch.object(partial.uuid, "uuid4",
                               return_value=types.SimpleNamespace(hex=self.snapshot)):
            responses = self.responses()
            responses[-1] = subprocess.CalledProcessError(1, ["load"])
            with mock.patch.object(partial.subprocess, "run", side_effect=responses):
                with self.assertRaises(subprocess.CalledProcessError):
                    partial.host_pass(self.args)
            self.assertNotIn("frontier_ack", json.loads(cursor.read_bytes()))
            with mock.patch.object(partial.subprocess, "run", side_effect=self.responses()) as run:
                partial.host_pass(self.args)
            self.assertIn("--completion-frontier", run.call_args_list[0].args[0][-1])
            self.assertEqual(json.loads(cursor.read_bytes())["frontier_ack"],
                             {"unit": "bundle-a", "fingerprint": "f" * 64})
            with mock.patch.object(partial.subprocess, "run", side_effect=self.responses()) as run:
                partial.host_pass(self.args)
            self.assertIn("--frontier-ack", run.call_args_list[0].args[0][-1])
        # Programmatic reconciliation callers without the explicit gate keep their frozen cursor.
        del self.args.completion_frontier
        del self.receipt["frontier"]
        with mock.patch.object(partial.uuid, "uuid4",
                               return_value=types.SimpleNamespace(hex=self.snapshot)), \
                mock.patch.object(partial.subprocess, "run", side_effect=self.responses()) as run:
            partial.host_pass(self.args)
        self.assertNotIn("--completion-frontier", run.call_args_list[0].args[0][-1])
        self.assertIn("--through bundle-b", run.call_args_list[0].args[0][-1])

    def test_frontier_inventory_failure_never_falls_back_to_cursor(self):
        self.args.completion_frontier = True
        units = self.root / "quality/test/units/bundle-a"
        units.mkdir(parents=True)
        (units / "submitted").touch()
        (units / "quality.json").write_text("{}")
        self.cache_inventory({"bundle-a": {"status": "pending", "fingerprint": None}})
        self.args.after = self.args.through = "legacy-cursor"
        with mock.patch.object(partial, "completion_inventory",
                               side_effect=subprocess.CalledProcessError(1, ["inventory"])), \
                mock.patch.object(partial, "select_unit", wraps=partial.select_unit) as select:
            with self.assertRaises(subprocess.CalledProcessError):
                partial.scoped_collect(self.args, self.root)
        # Only the frontier lanes choose; the legacy cursor never does.
        self.assertTrue(select.called)
        self.assertFalse(any("legacy-cursor" in call.args for call in select.call_args_list))
        self.assertFalse((self.root / "ops/quality-results/scoped/test/frontier.json").exists())

    def test_scoped_frontier_acknowledgement_restart_and_change_guard(self):
        self.args.completion_frontier = True
        self.args.after, self.args.through = "pending", "pending"  # Legacy cursor is not a load key.
        inventory = {
            "pending": {"status": "pending", "fingerprint": None},
            "ready": {"status": "canonical-present", "fingerprint": "a" * 64},
        }
        for name in inventory:
            unit = self.root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        Path(self.args.collector).write_bytes(b"test collector")
        self.cache_inventory(inventory)

        def collect(command, **_kwargs):
            out = Path(command[command.index("--out-dir") + 1])
            for name in partial.FILES:
                (out / name).write_bytes(b"verified test output")
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        frontier_path = self.root / "ops/quality-results/scoped/test/frontier.json"
        with mock.patch.object(partial, "completion_inventory", return_value=inventory), \
                mock.patch.object(partial.subprocess, "run", side_effect=collect) as run:
            first = partial.scoped_collect(self.args, self.root)
            self.assertEqual(first["selected_unit"], "ready")
            self.assertEqual(run.call_count, 1)
            self.assertEqual(json.loads(frontier_path.read_bytes())["loaded"], {})
            self.args.snapshot = "b" * 32
            self.args.frontier_ack = json.dumps({"unit": "ready", "fingerprint": "a" * 64})
            second = partial.scoped_collect(self.args, self.root)
            self.assertEqual(second["selected_unit"], "pending")
            self.assertEqual(json.loads(frontier_path.read_bytes())["loaded"], {"ready": "a" * 64})
            self.args.snapshot = "c" * 32
            third = partial.scoped_collect(self.args, self.root)  # Identical ack is idempotent.
            self.assertEqual(third["selected_unit"], "ready")
        previous = frontier_path.read_bytes()
        self.args.snapshot = "d" * 32
        changed = {**inventory, "pending": {"status": "canonical-present", "fingerprint": "e" * 64}}
        with mock.patch.object(partial, "completion_inventory", side_effect=[inventory, changed]), \
                mock.patch.object(partial.subprocess, "run", side_effect=collect):
            with self.assertRaisesRegex(ValueError, "changed during collection"):
                partial.scoped_collect(self.args, self.root)
        self.assertEqual(frontier_path.read_bytes(), previous)

    def test_a_refresh_written_during_a_pass_is_kept_by_the_pass_update(self):
        self.args.completion_frontier = True
        inventory = {"ready": {"status": "canonical-present", "fingerprint": "a" * 64}}
        for name in ("ready", "zz-new"):
            unit = self.root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        Path(self.args.collector).write_bytes(b"test collector")
        path = self.cache_inventory(inventory)

        def recheck(*_args, **_kwargs):
            # A full refresh, with no step lock, finishes while the pass rechecks its unit.
            path.write_text(json.dumps({"schema": 1, "generated_at": 1, "units": {
                **inventory, "zz-new": {"status": "pending", "fingerprint": None}}}))
            return inventory

        def collect(command, **_kwargs):
            out = Path(command[command.index("--out-dir") + 1])
            for name in partial.FILES:
                (out / name).write_bytes(b"verified test output")
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with mock.patch.object(partial, "completion_inventory", side_effect=recheck), \
                mock.patch.object(partial.subprocess, "run", side_effect=collect):
            partial.scoped_collect(self.args, self.root)
        self.assertEqual(set(json.loads(path.read_bytes())["units"]), {"ready", "zz-new"})

    def test_collector_failure_advances_only_scheduler_and_cannot_starve_other_units(self):
        self.args.completion_frontier = True
        inventory = {
            "a-poison": {"status": "canonical-present", "fingerprint": "a" * 64},
            "b-ready": {"status": "canonical-present", "fingerprint": "b" * 64},
            "c-pending": {"status": "pending", "fingerprint": None},
        }
        for name in inventory:
            unit = self.root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        self.cache_inventory(inventory)
        frontier_path = self.root / "ops/quality-results/scoped/test/frontier.json"
        failures = [
            subprocess.CompletedProcess(["collector"], 1, stdout="", stderr="invalid canonical"),
            subprocess.TimeoutExpired(["collector"], 120),
        ]
        with mock.patch.object(partial, "completion_inventory", return_value=inventory):
            for index in range(12):
                self.args.snapshot = f"{index:032x}"
                failure = failures[index % 2]
                with mock.patch.object(partial.subprocess, "run", side_effect=[failure]):
                    with self.assertRaises((subprocess.CalledProcessError, partial.OutOfTime)):
                        partial.scoped_collect(self.args, self.root)
                state = json.loads(frontier_path.read_bytes())
                self.assertEqual(state["loaded"], {})
                self.assertEqual(state["turns"], (index + 1) % 4)
                if index == 1:
                    self.assertEqual(state["priority_after"], "b-ready")
            self.assertEqual(state["fair_after"], "c-pending")
        # A failed inventory is still fail-closed, without advancing either lane.
        previous = frontier_path.read_bytes()
        with mock.patch.object(partial, "completion_inventory", side_effect=ValueError("unknown")):
            with self.assertRaisesRegex(ValueError, "unknown"):
                partial.scoped_collect(self.args, self.root)
        self.assertEqual(frontier_path.read_bytes(), previous)

    def test_slow_inventory_leaves_time_to_persist_collector_timeout_progress(self):
        self.args.completion_frontier = True
        unit = self.root / "quality/test/units/a-ready"
        unit.mkdir(parents=True)
        (unit / "submitted").touch()
        (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        inventory = {"a-ready": {"status": "canonical-present", "fingerprint": "a" * 64}}
        self.cache_inventory(inventory)
        # A 90-second inventory must not leave the collector its full per-call limit.
        with mock.patch.object(partial.time, "monotonic", side_effect=[0, 90, 90]), \
                mock.patch.object(partial, "completion_inventory", return_value=inventory), \
                mock.patch.object(partial.subprocess, "run",
                                  side_effect=subprocess.TimeoutExpired(["collector"], 70)) as run:
            with self.assertRaises(partial.OutOfTime) as raised:
                partial.scoped_collect(self.args, self.root)
        self.assertEqual((raised.exception.step, raised.exception.unit), ("collect", "a-ready"))
        self.assertEqual(run.call_args.kwargs["timeout"], partial.REMOTE_WORK_SECONDS - 90)
        state = json.loads((self.root / "ops/quality-results/scoped/test/frontier.json").read_bytes())
        self.assertEqual(state["priority_after"], "a-ready")
        self.assertEqual(state["turns"], 1)
        self.assertEqual(state["loaded"], {})

    def test_frontier_inventory_budget_is_shared_and_exhaustion_stays_unknown(self):
        with mock.patch.object(partial.time, "monotonic", return_value=150), \
                mock.patch.object(partial.subprocess, "run",
                                  side_effect=subprocess.TimeoutExpired(["inventory"], 10)) as run:
            with self.assertRaises(subprocess.TimeoutExpired):
                partial.completion_inventory(self.args, self.root, self.root, deadline=160)
        self.assertEqual(run.call_args.kwargs["timeout"], 10)
        with mock.patch.object(partial.time, "monotonic", return_value=160), \
                mock.patch.object(partial.subprocess, "run") as run:
            with self.assertRaises(subprocess.TimeoutExpired):
                partial.completion_inventory(self.args, self.root, self.root, deadline=160)
        run.assert_not_called()

    def test_pass_uses_cached_inventory_and_reinventories_only_the_selected_unit(self):
        self.args.completion_frontier = True
        inventory = {
            "a-ready": {"status": "canonical-present", "fingerprint": "a" * 64},
            "b-pending": {"status": "pending", "fingerprint": None},
        }
        for name in [*inventory, "c-new"]:
            unit = self.root / "quality/test/units" / name
            unit.mkdir(parents=True)
            (unit / "submitted").touch()
            (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        Path(self.args.collector).write_bytes(b"test collector")
        path = self.cache_inventory(inventory)
        fresh = {"a-ready": {"status": "canonical-present", "fingerprint": "f" * 64}}

        def collect(command, **_kwargs):
            out = Path(command[command.index("--out-dir") + 1])
            for name in partial.FILES:
                (out / name).write_bytes(b"verified test output")
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

        with mock.patch.object(partial, "completion_inventory", return_value=fresh) as inventory_call, \
                mock.patch.object(partial.subprocess, "run", side_effect=collect):
            receipt = partial.scoped_collect(self.args, self.root)
        # Never a full inventory under the step lock: both calls name the selected unit.
        self.assertEqual([call.args[3] for call in inventory_call.call_args_list], ["a-ready", "a-ready"])
        self.assertEqual(receipt["frontier"]["fingerprint"], "f" * 64)
        # The fresh key replaces the cached entry; the unit submitted since waits for a refresh.
        self.assertEqual(json.loads(path.read_bytes())["units"]["a-ready"], fresh["a-ready"])
        self.assertNotIn("c-new", json.loads(path.read_bytes())["units"])

    def test_pass_without_cached_inventory_or_with_vanished_unit_fails_closed(self):
        self.args.completion_frontier = True
        unit = self.root / "quality/test/units/a-ready"
        unit.mkdir(parents=True)
        (unit / "submitted").touch()
        (unit / "quality.json").write_text("{}")
        with mock.patch.object(partial, "completion_inventory") as inventory_call:
            with self.assertRaisesRegex(ValueError, "no scheduling inventory"):
                partial.scoped_collect(self.args, self.root)
            self.cache_inventory({"a-ready": {"status": "pending", "fingerprint": None},
                                  "gone": {"status": "pending", "fingerprint": None}})
            with self.assertRaisesRegex(ValueError, "submitted inventory changed"):
                partial.scoped_collect(self.args, self.root)
        inventory_call.assert_not_called()
        self.assertFalse((self.root / "ops/quality-results/scoped/test/frontier.json").exists())

    def test_refresh_writes_validated_inventory_with_its_own_budget(self):
        self.args.inventory_budget_secs = 1500
        inventory = {"a-ready": {"status": "canonical-present", "fingerprint": "a" * 64},
                     "b-pending": {"status": "pending", "fingerprint": None}}
        with mock.patch.object(partial, "completion_inventory", return_value=inventory) as call:
            report = partial.refresh_inventory(self.args, self.root)
        self.assertEqual(call.call_args.kwargs["limit"], 1500)
        self.assertIsNone(call.call_args.args[3] if len(call.call_args.args) > 3 else None)
        written = json.loads((self.root / "ops/quality-results/scoped/test" / partial.INVENTORY_FILE).read_bytes())
        self.assertEqual(written["units"], inventory)
        self.assertEqual(report["statuses"], {"canonical-present": 1, "pending": 1})
        bad = {"a-ready": {"status": "canonical-present", "fingerprint": None}}
        with mock.patch.object(partial, "completion_inventory", return_value=bad):
            with self.assertRaisesRegex(ValueError, "invalid canonical inventory entry"):
                partial.refresh_inventory(self.args, self.root)
        self.assertEqual(json.loads((self.root / "ops/quality-results/scoped/test"
                                     / partial.INVENTORY_FILE).read_bytes())["units"], inventory)

    def test_host_refresh_runs_without_step_lock_and_at_most_once_per_interval(self):
        self.args.inventory_budget_secs = 1500
        self.args.inventory_refresh_secs = 3600
        report = {"campaign": "test", "phase": "inventory_refreshed", "units": 2}
        done = subprocess.CompletedProcess(["ssh"], 0, stdout=json.dumps(report).encode(), stderr=b"")
        with mock.patch.object(partial.subprocess, "run", return_value=done) as run:
            self.assertEqual(partial.host_refresh_inventory(self.args), report)
            self.assertIsNone(partial.host_refresh_inventory(self.args))
        self.assertEqual(run.call_count, 1)
        command = run.call_args.args[0][-1]
        self.assertNotIn("flock", command)
        self.assertNotIn("usnm-boinc-step.lock", command)
        self.assertIn("--remote-inventory", command)
        self.assertEqual(run.call_args.kwargs["timeout"], 1620)
        marker = self.args.state_dir / "test/inventory-refresh.json"
        state = json.loads(marker.read_bytes())
        state["attempted_at"] -= 3601
        marker.write_text(json.dumps(state))
        failed = subprocess.CompletedProcess(["ssh"], 1, stdout=b"", stderr=b"boom")
        with mock.patch.object(partial.subprocess, "run", return_value=failed):
            self.assertIsNone(partial.host_refresh_inventory(self.args))
        # A failed refresh keeps the last good report and is retried only after the interval.
        self.assertEqual(json.loads(marker.read_bytes())["report"], report)
        # The interval counts from when an attempt ends, even one that used its whole budget.
        state = json.loads(marker.read_bytes())
        state["attempted_at"] -= 3601
        marker.write_text(json.dumps(state))
        started = time.time()
        with mock.patch.object(partial.subprocess, "run",
                               side_effect=subprocess.TimeoutExpired(["ssh"], 1620)):
            self.assertIsNone(partial.host_refresh_inventory(self.args))
        self.assertGreaterEqual(json.loads(marker.read_bytes())["attempted_at"], started)
        self.assertEqual(json.loads(marker.read_bytes())["report"], report)
        with mock.patch.object(partial.subprocess, "run") as run:
            self.assertIsNone(partial.host_refresh_inventory(self.args))
        run.assert_not_called()

    def test_run_loads_several_units_with_a_lock_release_between_each(self):
        self.args.units_per_run, self.args.run_budget_secs = 3, 600
        loaded = [{"pages": 200, "selected_unit": f"u{i}"} for i in range(3)]
        sleeps = []
        with mock.patch.object(partial, "host_pass", side_effect=loaded) as one, \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            report = partial.host_passes(self.args, sleep=sleeps.append)
        self.assertEqual(one.call_count, 3)
        self.assertEqual(sleeps, [partial.UNIT_GAP_SECONDS] * 2)
        self.assertEqual((report["units"], report["pages"]), (3, 600))

    def test_run_stops_quietly_on_a_later_busy_lock_but_not_on_the_first(self):
        self.args.units_per_run, self.args.run_budget_secs = 5, 600
        with mock.patch.object(partial, "host_pass", side_effect=[{"pages": 1}, SystemExit(75)]), \
                mock.patch("sys.stdout", new_callable=io.StringIO), mock.patch("sys.stderr", new_callable=io.StringIO):
            self.assertEqual(partial.host_passes(self.args, sleep=lambda _: None)["units"], 1)
        with mock.patch.object(partial, "host_pass", side_effect=SystemExit(75)):
            with self.assertRaises(SystemExit) as raised:
                partial.host_passes(self.args, sleep=lambda _: None)
        self.assertEqual(raised.exception.code, 75)
        # Any other failure, even after loaded units, still fails the run so it's visible.
        with mock.patch.object(partial, "host_pass",
                               side_effect=[{"pages": 1}, subprocess.CalledProcessError(1, ["ssh"])]), \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            with self.assertRaises(subprocess.CalledProcessError):
                partial.host_passes(self.args, sleep=lambda _: None)

    def test_run_starts_no_unit_past_its_budget(self):
        self.args.units_per_run, self.args.run_budget_secs = 5, 600
        with mock.patch.object(partial.time, "monotonic", side_effect=[0, 300, 601, 601]), \
                mock.patch.object(partial, "host_pass", side_effect=[{"pages": 1}, {"pages": 1}]) as one, \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            report = partial.host_passes(self.args, sleep=lambda _: None)
        self.assertEqual((one.call_count, report["units"]), (2, 2))
        # A pass ending at 599 s doesn't start another after the pause; reaching the budget ends it.
        clock = iter([0, 600, 600])
        with mock.patch.object(partial.time, "monotonic", side_effect=lambda: next(clock)), \
                mock.patch.object(partial, "host_pass", side_effect=[{"pages": 1}, {"pages": 1}]) as one, \
                mock.patch("sys.stdout", new_callable=io.StringIO):
            report = partial.host_passes(self.args, sleep=lambda _: None)
        self.assertEqual((one.call_count, report["units"]), (1, 1))

    def frontier_unit(self, name="a-ready"):
        self.args.completion_frontier = True
        unit = self.root / "quality/test/units" / name
        unit.mkdir(parents=True)
        (unit / "submitted").touch()
        (unit / "quality.json").write_text(json.dumps({"id": self.job, "doc_id": "doc-a"}))
        inventory = {name: {"status": "canonical-present", "fingerprint": "a" * 64}}
        self.cache_inventory(inventory)
        return inventory

    def test_a_collection_out_of_time_has_its_start_and_output_logged(self):
        self.frontier_unit()
        error = subprocess.TimeoutExpired(["collector"], 150, output=b"", stderr=b"keying bundle\n")
        with mock.patch.object(partial, "completion_inventory",
                               return_value={"a-ready": {"status": "canonical-present",
                                                         "fingerprint": "a" * 64}}), \
                mock.patch.object(partial.subprocess, "run", side_effect=error), \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as journal:
            with self.assertRaises(partial.OutOfTime):
                partial.scoped_collect(self.args, self.root)
        log = next((self.root / "ops/quality-results/scoped/test").glob("*/snapshots/*/collector.log"))
        lines = log.read_text().splitlines()
        start = json.loads(lines[0])
        self.assertEqual((start["phase"], start["step"], start["budget_seconds"]),
                         ("collector_call", "collect", 150))
        self.assertIn("--out-dir", start["argv"])
        self.assertEqual(lines[1], "keying bundle")
        self.assertEqual(json.loads(lines[2])["phase"], "collector_timeout")
        # The start is journaled too, through the remote's stderr.
        self.assertIn('"phase": "collector_call", "step": "collect"', journal.getvalue())

    def test_a_call_out_of_time_without_a_log_replays_its_output_on_stderr(self):
        error = subprocess.TimeoutExpired(["inventory"], 10, output=b"listing", stderr=b"units 40")
        with mock.patch.object(partial.subprocess, "run", side_effect=error), \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as journal:
            with self.assertRaises(subprocess.TimeoutExpired):
                partial.call_collector(["inventory"], 10, "inventory-unit")
            print("next event", file=sys.stderr)
        lines = journal.getvalue().splitlines()
        self.assertEqual(json.loads(lines[0])["phase"], "collector_call")
        # Ended with a newline, so what follows stays on its own line.
        self.assertEqual(lines[1:], ["listingunits 40", "next event"])

    def test_an_inventory_or_recheck_out_of_time_defers_without_advancing_either_lane(self):
        inventory = self.frontier_unit()
        frontier = self.root / "ops/quality-results/scoped/test/frontier.json"
        for step, effects in [("inventory-unit", [subprocess.TimeoutExpired(["inventory"], 3)]),
                              ("inventory-recheck", [inventory, subprocess.TimeoutExpired(["inventory"], 0)])]:
            with self.subTest(step=step), \
                    mock.patch.object(partial, "completion_inventory", side_effect=effects), \
                    mock.patch.object(partial.subprocess, "run",
                                      return_value=subprocess.CompletedProcess([], 0, "", "")):
                self.args.snapshot = ("1" if step == "inventory-unit" else "2") * 32
                with self.assertRaises(partial.OutOfTime) as raised:
                    partial.scoped_collect(self.args, self.root)
                self.assertEqual((raised.exception.step, raised.exception.unit), (step, "a-ready"))
                self.assertFalse(frontier.exists())

    def test_remote_out_of_time_exits_75_with_its_unit(self):
        error = partial.OutOfTime("collect", "a-ready", 150)
        with mock.patch.object(partial, "scoped_collect", side_effect=error), \
                mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
            with self.assertRaises(SystemExit) as raised:
                partial.main(["--remote", "--campaign", "test", "--snapshot", self.snapshot])
        self.assertEqual(raised.exception.code, 75)
        self.assertEqual(partial.out_of_time_report(log.getvalue()),
                         {"phase": "collection_out_of_time", "step": "collect", "unit": "a-ready",
                          "budget_seconds": 150})

    def out_of_time_response(self, unit="bundle-a"):
        line = json.dumps({"phase": "collection_out_of_time", "step": "collect", "unit": unit,
                           "budget_seconds": 150})
        return subprocess.CompletedProcess(
            ["ssh"], 75, b"", (partial.LOCK_ACQUIRED + "\n" + line + "\n").encode())

    def test_an_out_of_time_pass_is_deferred_until_its_unit_keeps_running_out(self):
        self.args.out_of_time_passes = 3
        counts = self.args.state_dir / "test" / partial.OUT_OF_TIME_FILE
        for attempt in (1, 2):
            with self.subTest(attempt=attempt), \
                    mock.patch.object(partial.subprocess, "run", return_value=self.out_of_time_response()) as run, \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(SystemExit) as raised:
                    partial.host_pass(self.args)
                self.assertEqual(raised.exception.code, 75)
                self.assertEqual(run.call_count, 1)  # nothing transferred or loaded
                self.assertIn('"phase": "collection_deferred"', log.getvalue())
                self.assertNotIn("collection_failed", log.getvalue())
                self.assertEqual(json.loads(counts.read_bytes())["units"], {"bundle-a": attempt})
        self.assertFalse((self.args.state_dir / "test/cursor.json").exists())
        # A verified load of the unit clears its count, so only consecutive timeouts add up.
        with mock.patch.object(partial.uuid, "uuid4", return_value=types.SimpleNamespace(hex=self.snapshot)), \
                mock.patch.object(partial.subprocess, "run", side_effect=self.responses()):
            partial.host_pass(self.args)
        self.assertEqual(json.loads(counts.read_bytes())["units"], {})
        for attempt in (1, 2, 3):
            with mock.patch.object(partial.subprocess, "run", return_value=self.out_of_time_response()), \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO):
                with self.assertRaises(SystemExit) as raised:
                    partial.host_pass(self.args)
        self.assertNotEqual(raised.exception.code, 75)  # the third in a row is a failure
        self.assertIn("out of time in 3 passes", str(raised.exception.code))

    def test_exit_75_after_the_lock_without_an_out_of_time_line_stays_a_failure(self):
        bogus = json.dumps({"phase": "collection_out_of_time", "unit": "../escape"})
        for stderr in ("", "remote diagnostic\n", bogus + "\n"):
            response = subprocess.CompletedProcess(
                ["ssh"], 75, b"", (partial.LOCK_ACQUIRED + "\n" + stderr).encode())
            with self.subTest(stderr=stderr), \
                    mock.patch.object(partial.subprocess, "run", return_value=response), \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(subprocess.CalledProcessError):
                    partial.host_pass(self.args)
                self.assertIn("collection_failed", log.getvalue())

    def load_fixture(self, rows):
        root = self.args.snapshot_dir
        (root / "out").mkdir(parents=True)
        for name in partial.FILES:
            (root / "out" / name).write_bytes(b"verified " + name.encode())
        self.receipt["files"] = {name: partial.digest(root / "out" / name) for name in partial.FILES}
        (root / "coverage.json").write_text(json.dumps(self.receipt))
        parquet = types.ModuleType("pyarrow.parquet")
        parquet.read_table = lambda _: types.SimpleNamespace(to_pylist=lambda: rows)
        arrow = types.ModuleType("pyarrow")
        arrow.parquet = parquet
        return mock.patch.dict(sys.modules, {"pyarrow": arrow, "pyarrow.parquet": parquet})

    def test_loader_rejects_stale_bytes_and_wrong_job_without_loading(self):
        rows = [{"doc_id": "doc-a", "job_id": self.job}]
        with self.load_fixture(rows), mock.patch.object(partial, "run_loader") as load:
            (self.args.snapshot_dir / "out/pages.parquet").write_bytes(b"stale snapshot")
            with self.assertRaises(ValueError):
                partial.verified_load(self.args)
            load.assert_not_called()
        (self.args.snapshot_dir / "out").rename(self.args.snapshot_dir / "old-out")
        with self.load_fixture([{"doc_id": "doc-a", "job_id": "f" * 64}]), \
                mock.patch.object(partial, "run_loader") as load:
            with self.assertRaises(ValueError):
                partial.verified_load(self.args)
            load.assert_not_called()

    def test_pending_scope_preserves_database_and_returns_zero_loaded_pages(self):
        with self.load_fixture([]), mock.patch.object(partial, "run_loader") as load:
            report = partial.verified_load(self.args)
        self.assertEqual(report["pages"], 0)
        self.assertFalse(report["campaign_complete"])
        load.assert_not_called()
        responses = self.responses()
        responses[-1].stdout = json.dumps(report).encode()
        with mock.patch.object(partial.uuid, "uuid4",
                               return_value=types.SimpleNamespace(hex=self.snapshot)), \
                mock.patch.object(partial.subprocess, "run", side_effect=responses):
            partial.host_pass(self.args)
        receipt = json.loads((self.args.state_dir / "test/receipts"
                              / (self.snapshot + ".json")).read_bytes())
        self.assertTrue(receipt["verified"])
        self.assertFalse(receipt["loaded"])
        state = json.loads((self.args.state_dir / "test/cursor.json").read_bytes())
        self.assertEqual(state["after"], "bundle-a")

    def test_verified_repeat_load_uses_only_existing_upsert_loader(self):
        rows = [{"doc_id": "doc-a", "job_id": self.job}]
        with self.load_fixture(rows), \
                mock.patch.object(partial.Path, "read_text", return_value="certificate-only-dsn"), \
                mock.patch.object(partial, "run_loader") as load:
            first = partial.verified_load(self.args)
            second = partial.verified_load(self.args)
        self.assertEqual(first, second)
        self.assertEqual(first["pages"], 1)
        self.assertEqual(load.call_count, 2)
        self.assertEqual(load.call_args_list[0], load.call_args_list[1])
        command = load.call_args.args[0]
        self.assertIn("/opt/usnm-quality-feed/load.py", command)
        self.assertIn(str(self.args.snapshot_dir / "out/pages.parquet"), command)
        self.assertNotIn("delete", command)

    def loader_orphan_case(self, parent_exit=None):
        late = self.root / "late-commit"
        ready = self.root / "child-ready"
        child = (
            "import signal,time;from pathlib import Path;"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
            f"Path({str(ready)!r}).touch();time.sleep(0.7);"
            f"Path({str(late)!r}).write_text('orphan commit')"
        )
        parent = (
            "import subprocess,sys,time;from pathlib import Path;"
            f"subprocess.Popen([sys.executable,'-c',{child!r}]);"
            f"p=Path({str(ready)!r});"
            "\nwhile not p.exists():time.sleep(0.01)\n"
            + ("time.sleep(30)" if parent_exit is None else f"sys.exit({parent_exit})")
        )
        error = subprocess.TimeoutExpired if parent_exit is None else subprocess.CalledProcessError
        with self.assertRaises(error):
            partial.run_loader([sys.executable, "-c", parent],
                               timeout=0.3 if parent_exit is None else 5)
        self.assertTrue(ready.exists())
        time.sleep(0.8)
        self.assertFalse(late.exists())

    def test_loader_timeout_kills_psql_like_child_after_parent_exits(self):
        self.loader_orphan_case()

    def test_failed_loader_kills_psql_like_child_after_parent_exits(self):
        self.loader_orphan_case(parent_exit=7)

    def test_term_ignoring_loader_is_reaped_before_preparation_kill_grace(self):
        late = self.root / "late-commit"
        ready = self.root / "child-ready"
        child = (
            "import signal,time;from pathlib import Path;"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
            f"Path({str(ready)!r}).touch();time.sleep(2);"
            f"Path({str(late)!r}).touch()"
        )
        parent = (
            "import signal,subprocess,sys,time;from pathlib import Path;"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
            f"subprocess.Popen([sys.executable,'-c',{child!r}]);"
            f"p=Path({str(ready)!r});"
            "\nwhile not p.exists():time.sleep(0.01)\n"
            "time.sleep(30)"
        )
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            partial.run_loader([sys.executable, "-c", parent], timeout=0.3)
        self.assertLess(time.monotonic() - started, 5)
        self.assertTrue(ready.exists())
        time.sleep(2)
        self.assertFalse(late.exists())


if __name__ == "__main__":
    unittest.main()
