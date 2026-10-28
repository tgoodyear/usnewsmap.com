"""Partial publication: current inputs, explicit coverage, fail-closed loads and replay."""
import hashlib
import importlib.util
import io
import json
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
            self.assertIn("180s", run.call_args_list[0].args[0][-1])
            for call in run.call_args_list[2:4]:
                command = call.args[0]
                if command[0] == "ssh":
                    command = shlex.split(command[-1])
                inner = command.index("timeout")
                seconds = float(command[inner + 3].removesuffix("s"))
                grace = float(command[inner + 2].removeprefix("--kill-after=").removesuffix("s"))
                self.assertLess(seconds + grace, call.kwargs["timeout"])
                self.assertEqual(command[inner + 4], "tar")

    def test_remote_failure_diagnostics_distinguish_lock_conflicts_without_advancing(self):
        for code, acquired, phase in [(75, False, "step_lock_busy"),
                                      (75, True, "collection_failed"),
                                      (255, False, "remote_admission_failed")]:
            stderr = (partial.LOCK_ACQUIRED + "\n" if acquired else "") + "remote diagnostic\n"
            response = subprocess.CompletedProcess(["ssh"], code, b"", stderr.encode())
            with self.subTest(phase=phase), mock.patch.object(
                    partial.subprocess, "run", return_value=response) as run, \
                    mock.patch.object(sys, "stderr", new_callable=io.StringIO) as log:
                with self.assertRaises(subprocess.CalledProcessError):
                    partial.host_pass(self.args)
                self.assertEqual(run.call_count, 1)
                self.assertIn("remote diagnostic", log.getvalue())
                self.assertIn(phase, log.getvalue())
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

    def test_frontier_ack_is_only_persisted_after_verified_nonempty_load(self):
        self.args.completion_frontier = True
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
        with mock.patch.object(partial, "completion_inventory",
                               side_effect=subprocess.CalledProcessError(1, ["inventory"])), \
                mock.patch.object(partial, "select_unit") as legacy:
            with self.assertRaises(subprocess.CalledProcessError):
                partial.scoped_collect(self.args, self.root)
        legacy.assert_not_called()
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
