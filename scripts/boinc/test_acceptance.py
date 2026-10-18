"""Offline acceptance lifecycle tests; no Azure, SSH, keys, or paid resources."""
import argparse
import base64
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import MagicMock, patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, HERE / (name + '.py'))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


A = module('acceptance')
S = module('acceptance_server')
C = module('acceptance_client')
RELEASE = dict(app='usnm_qwen', version_num=100, platform='x86_64-pc-linux-gnu', plan_class='docker_mt_2b')
PROFILE = dict(cpus=4, memory_bytes=6 * 1024**3, disk_bytes=40 * 1024**3, gpu='none', client_version=80213)
URL = 'https://boinc.usnewsmap.com/usnmocr/'
PROBE = dict(ready=True, project_url=URL, host_id=2, platform=RELEASE['platform'], cpus=4,
             memory_bytes=8 * 1024**3, disk_bytes=60 * 1024**3, gpus=[], client_version=80213,
             container_runtime=True)
CONFIG = dict(fixture='qwen-cpu', server_command=['server'],
              client=dict(mode='existing', dedicated_test_client=True, python_command=['python3', '-']))
DATA = b'{"transcription":"actual inference", "unit_error":null}'


def evidence(run_id, **changes):
    output = dict(name='result.json', bytes=len(DATA), sha256=hashlib.sha256(DATA).hexdigest(),
                  base64=base64.b64encode(DATA).decode())
    return dict(run_id=run_id, release=RELEASE, state='validated', host_id=2, canonical_result_id=88,
                client_validate_state=1, client_outcome=1, client_server_state=5, outputs=[output], **changes)


class Workspace(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory(prefix='.acceptance-test-', dir=ROOT)
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        handlers = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        for sig, handler in handlers.items():
            self.addCleanup(signal.signal, sig, handler)

    def runner(self, config=None, **bounds):
        args = argparse.Namespace(env='prod', timeout=5, command_timeout=1, cleanup_timeout=2,
                                  poll_interval=.001, evidence=self.root / 'evidence.json', **bounds)
        return A.Runner(args, copy.deepcopy(config or CONFIG), dict(RELEASE))


class Eligibility(Workspace):
    def test_cpu_effective_resources_and_platform_fail_closed(self):
        A.eligible(PROBE, PROFILE, RELEASE, URL)
        for key, bad in [('cpus', 2), ('memory_bytes', 5 * 1024**3), ('disk_bytes', 2 * 1024**3),
                         ('platform', 'windows_x86_64'), ('host_id', 0), ('project_url', 'wrong'),
                         ('client_version', 80211), ('container_runtime', False)]:
            with self.subTest(key=key), self.assertRaises(A.AcceptanceError):
                A.eligible({**PROBE, key: bad}, PROFILE, RELEASE, URL)

    def test_gpu_vendor_and_vram_and_native_do_not_require_docker(self):
        for plan, gpu, platform in [('nvidia_cuda_8b', 'nvidia', RELEASE['platform']),
                                    ('apple_gpu_8b', 'apple', 'arm64-apple-darwin'),
                                    ('intel_vulkan_8b', 'intel', 'windows_x86_64')]:
            release = {**RELEASE, 'plan_class': plan, 'platform': platform}
            profile = {**PROFILE, 'gpu': gpu, 'gpu_memory_bytes': 7_000_000_000}
            probe = {**PROBE, 'platform': platform, 'gpus': [gpu],
                     'gpu_memory_bytes': {gpu: 8_000_000_000}, 'container_runtime': False}
            A.eligible(probe, profile, release, URL)
            for bad in ({'gpus': []}, {'gpu_memory_bytes': {gpu: 1}}, {'platform': 'wrong'}):
                with self.assertRaises(A.AcceptanceError):
                    A.eligible({**probe, **bad}, profile, release, URL)

    def test_config_does_not_silently_choose_gpu_or_paid_client(self):
        path = self.root / 'config.json'
        path.write_text(json.dumps(CONFIG))
        _, release = A.load(path, 'usnm_qwen', '1.23', RELEASE['platform'], 'mt')
        self.assertEqual(release['version_num'], 123)
        for client in [dict(mode='existing', python_command=['python3']),
                       dict(mode='ephemeral', sku='Standard_NC4as_T4_v3')]:
            path.write_text(json.dumps({**CONFIG, 'client': client}))
            with self.assertRaises(A.AcceptanceError):
                A.load(path, 'usnm_qwen', '1.0', 'arm64-apple-darwin', 'apple_gpu_8b')


class Evidence(Workspace):
    def test_registration_upload_or_other_host_version_is_not_acceptance(self):
        self.assertIsNone(A.check_evidence(dict(run_id='x', release=RELEASE, state='pending'), RELEASE, 2, 'x'))
        for field, value in [('run_id', 'other'), ('release', {}), ('state', 'failed'), ('host_id', 3),
                             ('client_validate_state', 0), ('client_outcome', 3), ('client_server_state', 4),
                             ('canonical_result_id', 0), ('outputs', [])]:
            with self.subTest(field=field), self.assertRaises(A.AcceptanceError):
                status = evidence('x')
                status[field] = value
                A.check_evidence(status, RELEASE, 2, 'x')

    def test_hash_lengths_and_canonical_bytes_checked_without_logging_content(self):
        accepted = A.check_evidence(evidence('x'), RELEASE, 2, 'x')
        self.assertNotIn('actual inference', json.dumps(accepted))
        for key, value in [('sha256', '0' * 64), ('bytes', 0), ('base64', '')]:
            row = evidence('x')
            row['outputs'][0][key] = value
            with self.assertRaises(A.AcceptanceError):
                A.check_evidence(row, RELEASE, 2, 'x')


class Lifecycle(Workspace):
    def simulate(self, fail=None, ephemeral=False, statuses=None):
        config = copy.deepcopy(CONFIG)
        if ephemeral:
            config['client'] = dict(mode='ephemeral', sku='Standard_D8s_v5')
        runner = self.runner(config)
        calls = []
        states = iter(statuses or [evidence(runner.run_id)])
        group = {'exists': False}

        def call(argv, payload=None):
            calls.append((argv, payload))
            if argv == ['server']:
                request = json.loads(payload)
                action = request['action']
                if fail == action:
                    raise A.AcceptanceError('injected ' + action)
                if action == 'reserve':
                    return json.dumps(dict(run_id=runner.run_id, release=RELEASE, profile=PROFILE))
                if action == 'status':
                    return json.dumps(next(states))
                return json.dumps({'released': True, 'dispatched': True})
            if argv[0].endswith('boinc-test-client.sh'):
                group['exists'] = True
                if fail == 'create':
                    raise A.AcceptanceError('partial provisioning')
                return ''
            if argv[0] == 'az':
                self.assertIn('--subscription', argv)
                if argv[1:3] == ['group', 'exists']:
                    return str(group['exists']).lower()
                if argv[1:3] == ['group', 'delete']:
                    if fail == 'delete':
                        raise A.AcceptanceError('delete failed')
                    group['exists'] = False
                    return ''
                if argv[1:4] == ['vm', 'run-command', 'invoke']:
                    if fail == 'probe':
                        raise A.AcceptanceError('probe failed')
                    return '[stdout]\n' + json.dumps(PROBE) + '\n[stderr]'
            if argv == CONFIG['client']['python_command']:
                if fail == 'probe':
                    raise A.AcceptanceError('probe failed')
                return json.dumps(PROBE)
            raise AssertionError(argv)
        with patch.object(runner, 'call', side_effect=call):
            try:
                runner.execute()
            except A.AcceptanceError:
                pass
            finally:
                runner.finish()
        return runner, calls

    def test_existing_client_is_updated_never_deleted_or_detached(self):
        runner, calls = self.simulate()
        self.assertTrue(runner.report['accepted'])
        self.assertTrue(runner.report['cleanup_complete'])
        self.assertFalse(any('group' in argv or 'detach' in argv for argv, _ in calls))
        self.assertTrue(any("'update'" in (payload or '') for _, payload in calls))
        self.assertEqual(json.loads(calls[-1][1])['action'], 'release')

    def test_ephemeral_success_and_every_partial_failure_delete_unique_group(self):
        for failure in [None, 'create', 'probe', 'dispatch', 'status']:
            with self.subTest(failure=failure):
                runner, calls = self.simulate(failure, ephemeral=True)
                deletes = [argv for argv, _ in calls if argv[:3] == ['az', 'group', 'delete']]
                self.assertEqual(len(deletes), 1)
                self.assertIn(runner.group, deletes[0])
                self.assertIn(runner.run_id, runner.group)
                self.assertEqual(runner.report['accepted'], failure is None)
                self.assertTrue(runner.report['cleanup_complete'])

    def test_uncertain_reservation_is_released_and_no_client_is_created(self):
        runner, calls = self.simulate('reserve', ephemeral=True)
        self.assertFalse(runner.report['accepted'])
        self.assertEqual([json.loads(payload)['action'] for _, payload in calls], ['reserve', 'release'])

    def test_cleanup_failures_never_report_success_and_leave_recovery_identity(self):
        for failure in ('delete', 'release'):
            runner, _ = self.simulate(failure, ephemeral=True)
            self.assertFalse(runner.report['accepted'])
            self.assertFalse(runner.report['cleanup_complete'])
            self.assertTrue(runner.report['cleanup_errors'])

    def test_deadlines_bound_pending_inference(self):
        runner = self.runner()
        runner.deadline = time.monotonic() - 1
        with self.assertRaises(A.AcceptanceError):
            runner.pause()
        with self.assertRaises(A.AcceptanceError):
            runner.call(['az'])

    def test_command_timeout_and_failure_never_echo_stderr_secrets(self):
        before = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            A.run([sys.executable, '-c', 'import time; time.sleep(10)'], .05)
        self.assertLess(time.monotonic() - before, 2)
        with self.assertRaises(A.AcceptanceError) as error:
            A.run([sys.executable, '-c', 'import sys; print("secret", file=sys.stderr); sys.exit(9)'], 1)
        self.assertNotIn('secret', str(error.exception))

    def test_real_az_subprocess_is_mocked_and_scoped(self):
        stub = self.root / 'az'
        log = self.root / 'az-calls.json'
        stub.write_text('#!' + sys.executable + '\nimport sys,json,os\nopen(os.environ["CALLS"],"w").write(json.dumps(sys.argv[1:])); print("false")\n')
        stub.chmod(0o700)
        runner = self.runner()
        with patch.dict(os.environ, PATH=str(self.root), CALLS=str(log)):
            self.assertEqual(runner.az(['group', 'exists', '-n', runner.group]).strip(), 'false')
        calls = json.loads(log.read_text())
        self.assertEqual(calls[-3:], ['--subscription', runner.subscription, '--only-show-errors'])
        self.assertIn(runner.run_id, calls[3])

    def test_sigterm_runs_bounded_fixture_cleanup_and_preserves_private_evidence(self):
        server = self.root / 'server.py'
        log = self.root / 'calls.jsonl'
        server.write_text("""import json,sys
request=json.load(sys.stdin)
with open(sys.argv[1], 'a') as out: out.write(json.dumps(request)+'\\n')
action=request['action']
if action=='reserve': print(json.dumps(dict(run_id=request['run_id'],release=request['release'],profile=""" + repr(PROFILE) + """)))
elif action=='status': print(json.dumps(dict(run_id=request['run_id'],release=request['release'],state='pending')))
else: print(json.dumps(dict(released=True,dispatched=True)))
""")
        client = self.root / 'client.py'
        client.write_text('import json,sys; sys.stdin.read(); print(' + repr(json.dumps(PROBE)) + ')')
        config = {**CONFIG, 'server_command': [sys.executable, str(server), str(log)],
                  'client': {**CONFIG['client'], 'python_command': [sys.executable, str(client)]}}
        cfg = self.root / 'config.json'
        cfg.write_text(json.dumps(config))
        receipt = self.root / 'signal-receipt.json'
        process = subprocess.Popen([sys.executable, str(HERE / 'acceptance.py'), '--env', 'prod',
                                    '--config', str(cfg), '--app', RELEASE['app'], '--version', '1.0',
                                    '--platform', RELEASE['platform'], '--plan-class', RELEASE['plan_class'],
                                    '--evidence', str(receipt), '--poll-interval', '.02',
                                    '--coordination-claim', 'https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-1'],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if log.exists() and 'status' in log.read_text():
                    break
                if process.poll() is not None:
                    self.fail(process.communicate())
                time.sleep(.02)
            else:
                self.fail('runner did not reach pending state')
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=5)
            self.assertEqual(process.returncode, 1, stdout + stderr)
            result = json.loads(receipt.read_text())
            self.assertFalse(result['accepted'])
            self.assertTrue(result['cleanup_complete'])
            self.assertIn('signal', result['error'])
            self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(log.read_text().splitlines()[-1])['action'], 'release')
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

    def test_cli_dry_run_is_offline_and_leaves_no_evidence(self):
        config = self.root / 'config.json'
        config.write_text(json.dumps(CONFIG))
        receipt = self.root / 'receipt.json'
        result = subprocess.run([sys.executable, str(HERE / 'acceptance.py'), '--env', 'prod', '--config', str(config),
                                 '--app', 'usnm_qwen', '--version', '1.0', '--platform', RELEASE['platform'],
                                 '--plan-class', RELEASE['plan_class'], '--evidence', str(receipt), '--dry-run'],
                                capture_output=True, text=True, env={**os.environ, 'PATH': '/nonexistent'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(json.loads(result.stdout)['dry_run'])
        self.assertFalse(receipt.exists())
        result = subprocess.run([sys.executable, str(HERE / 'acceptance.py'), '--env', 'prod', '--config', str(config),
                                 '--app', 'usnm_qwen', '--version', '1.0', '--platform', RELEASE['platform'],
                                 '--evidence', str(receipt)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('#529', result.stderr)
        self.assertFalse(receipt.exists())


class ReleaseIntegration(Workspace):
    def test_release_ready_is_gated_on_acceptance_exit_status(self):
        text = (ROOT / 'scripts/boinc-app-release.sh').read_text()
        tail = text.partition('echo "   $appver_image"\n')[2]
        scripts = self.root / 'scripts'
        scripts.mkdir()
        provision = scripts / 'provision-boinc.sh'
        provision.write_text('#!/bin/sh\nexit 0\n')
        provision.chmod(0o700)
        stub = self.root / 'python3'
        stub.write_text('#!/bin/sh\nprintf "%s\\n" "$*" > "$CALLS"\nexit "$ACCEPTANCE_RC"\n')
        stub.chmod(0o700)
        settings = self.root / 'settings.json'
        settings.write_text('{"images":{"appver":"old"}}')
        for code in ('0', '7'):
            env = {**os.environ, 'PATH': str(self.root) + os.pathsep + os.environ['PATH'],
                   'PREPARE_ONLY': 'false', 'ENV_NAME': 'prod', 'APP': 'usnm_qwen', 'VERSION': '1.0',
                   'PLATFORM': RELEASE['platform'], 'PLAN_CLASS': RELEASE['plan_class'],
                   'appver_image': 'example@sha256:abc', 'work': str(self.root),
                   'BOINC_SETTINGS': str(settings), 'ACCEPTANCE_CONFIG': 'config.json',
                   'COORDINATION_CLAIM': 'claim-url', 'ACCEPTANCE_RC': code, 'CALLS': str(self.root / 'calls')}
            shell = 'set -euo pipefail; die() { echo "$*" >&2; exit 1; }; acceptance_args=(--config config.json);\n' + tail
            result = subprocess.run(['bash', '-c', shell], cwd=self.root, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0 if code == '0' else 1, result.stderr)
            self.assertEqual('release-ready:' in result.stdout, code == '0')
            self.assertIn('--coordination-claim claim-url', (self.root / 'calls').read_text())
            if code != '0':
                self.assertIn('NOT release-ready', result.stderr)

    def test_prepare_only_rejects_acceptance_before_any_signing_or_azure(self):
        result = subprocess.run([str(ROOT / 'scripts/boinc-app-release.sh'), '--env', 'prod', '--app', 'usnm_ocr',
                                 '--version', '1.0', '--signing-host', 'unused', '--prepare-only',
                                 '--acceptance-config', 'unused.json', '--acceptance-evidence', 'unused-receipt.json'],
                                cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('unpublished candidate', result.stderr)


class ClientProbe(Workspace):
    def state(self, **changes):
        values = dict(hostid=2, platform=RELEASE['platform'], cpus=4)
        values.update(changes)
        (self.root / 'client_state.xml').write_text(f'''<client_state>
        <platform_name>{values['platform']}</platform_name><core_client_major_version>8</core_client_major_version>
        <core_client_minor_version>2</core_client_minor_version><core_client_release_version>13</core_client_release_version>
        <host_info><p_ncpus>{values['cpus']}</p_ncpus><m_nbytes>{16 * 1024**3}</m_nbytes></host_info>
        <project><master_url>{URL}</master_url><hostid>{values['hostid']}</hostid><authenticator>SECRET</authenticator></project>
        </client_state>''')

    def test_effective_cpu_and_ram_limits_not_physical_host_advertisement(self):
        self.state()
        (self.root / 'global_prefs_override.xml').write_text('''<global_preferences>
        <max_ncpus_pct>50</max_ncpus_pct><ram_max_used_busy_pct>25</ram_max_used_busy_pct>
        <ram_max_used_idle_pct>90</ram_max_used_idle_pct></global_preferences>''')
        with patch.object(C.shutil, 'which', return_value=None):
            probe = C.probe(self.root, URL)
        self.assertEqual(probe['cpus'], 2)
        self.assertEqual(probe['memory_bytes'], 4 * 1024**3)
        self.assertEqual(probe['client_version'], 80213)
        self.assertNotIn('SECRET', json.dumps(probe))

    def test_real_boinccmd_update_argv_is_mocked_and_has_no_authenticator(self):
        stub = self.root / 'boinccmd'
        log = self.root / 'calls.json'
        stub.write_text('#!' + sys.executable + '\nimport os,json,sys\nopen(os.environ["CALLS"],"w").write(json.dumps(sys.argv[1:]))\n')
        stub.chmod(0o700)
        result = subprocess.run([sys.executable, str(HERE / 'acceptance_client.py'), '--action', 'update', '--project-url', URL],
                                capture_output=True, text=True,
                                env={**os.environ, 'PATH': str(self.root), 'BOINC_DIR': str(self.root), 'CALLS': str(log)})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(log.read_text()), ['--project', URL, 'update'])
        self.assertEqual(result.stdout.strip(), '{}')


class Server(Workspace):
    def manifest(self):
        return dict(release=RELEASE, profile=PROFILE, batch=12, min_quorum=2,
                    workunit_name='usnm_qwen_' + 'a' * 64, peer_host_ids=[3],
                    input_template='qwen_in', output_template='qwen_out',
                    inputs=[dict(name='page.jpg', sha256='a' * 64)],
                    outputs=[dict(name='result.json', kind='json', equals={'unit_error': None}, nonempty=['transcription'])])

    def test_fixture_requires_exact_quorum_peers_and_app_native_identity(self):
        path = self.root / 'fixture.json'
        manifest = self.manifest()
        path.write_text(json.dumps(manifest))
        self.assertEqual(S.fixture_load(self.root, 'fixture', RELEASE), manifest)
        for changes in ({'peer_host_ids': []}, {'peer_host_ids': [0]}, {'peer_host_ids': [3, 3]},
                        {'workunit_name': '../escape'}, {'release': {}}):
            path.write_text(json.dumps({**manifest, **changes}))
            with self.assertRaises(ValueError):
                S.fixture_load(self.root, 'fixture', RELEASE)

    def test_dispatch_keeps_exact_version_host_batch_and_independent_quorum(self):
        args = S.create_args(self.manifest(), 'acceptance_abc', 2)
        for key, value in [('--app_version_num', '100'), ('--target_host', '2'), ('--batch', '12'),
                           ('--min_quorum', '2'), ('--target_nresults', '2')]:
            self.assertEqual(args[args.index(key) + 1], value)

    def test_output_contract_requires_real_inference_and_no_unit_error(self):
        rule = self.manifest()['outputs'][0]
        S.output_check(DATA, rule)
        for data in (b'', b'{}', b'{"transcription":"", "unit_error":null}',
                     b'{"transcription":"text", "unit_error":"failure"}'):
            with self.assertRaises((ValueError, KeyError)):
                S.output_check(data, rule)

    def test_live_plan_batch_and_closed_cohort_guards(self):
        (self.root / 'plan_class_spec.xml').write_bytes((ROOT / 'contrib-compute/boinc/plan_class_spec.xml').read_bytes())
        manifest = self.manifest()
        self.assertEqual(S.scheduler_profile(self.root, manifest)['client_version'], 80213)
        manifest['batch'] = 0
        with self.assertRaises(ValueError):
            S.scheduler_profile(self.root, manifest)
        manifest['release'] = {**RELEASE, 'plan_class': 'nvidia_cuda_8b'}
        manifest['profile'] = {**PROFILE, 'gpu': 'nvidia'}
        manifest['batch'] = 8
        with self.assertRaises(ValueError):
            S.scheduler_profile(self.root, manifest)

    def test_dispatch_adds_only_exact_host_peer_assignments(self):
        manifest = self.manifest()
        record = dict(run_id='a' * 32, release=RELEASE, workunit=manifest['workunit_name'],
                      app_version_id=7, released=False,
                      manifest_sha256=hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest())
        (self.root / 'fixture.reservation.json').write_text(json.dumps(record))
        (self.root / 'config.xml').write_text('<boinc><config><enable_assignment>1</enable_assignment></config></boinc>')
        request = dict(action='dispatch', fixture='fixture', run_id='a' * 32, release=RELEASE, host_id=2)
        db = MagicMock()
        with patch.object(S, 'fixture_load', return_value=manifest), \
                patch.object(S, 'query', return_value=[{'id': 11}]), patch.object(S.subprocess, 'run'):
            result = S.handle(request, self.root, self.root, db)
        self.assertTrue(result['dispatched'])
        sql, values = db.cursor.return_value.execute.call_args.args
        self.assertIn('assignment', sql)
        self.assertIn('1, 0', sql)
        self.assertEqual(values, (3, 11))
        db.commit.assert_called_once()
        self.assertTrue(json.loads((self.root / 'fixture.reservation.json').read_text())['dispatched'])

    def test_release_of_another_runs_uncertain_reservation_is_non_destructive(self):
        journal = self.root / 'fixture.reservation.json'
        original = dict(run_id='a' * 32, released=False)
        journal.write_text(json.dumps(original))
        result = S.handle(dict(fixture='fixture', run_id='b' * 32, release=RELEASE, action='release'),
                          self.root, self.root, None)
        self.assertFalse(result['released'])
        self.assertEqual(json.loads(journal.read_text()), original)

    def test_reservation_collision_uncertain_dispatch_and_owned_release(self):
        manifest = self.manifest()
        manifest['inputs'][0]['sha256'] = hashlib.sha256(b'fixture').hexdigest()
        templates = self.root / 'templates'
        templates.mkdir()
        for name in ('qwen_in', 'qwen_out'):
            (templates / name).write_text('<template/>')
        (self.root / 'download').mkdir()
        page = self.root / 'download/page.jpg'
        page.write_bytes(b'fixture')
        (self.root / 'config.xml').write_text('<boinc><config><enable_assignment>1</enable_assignment></config></boinc>')
        request = dict(fixture='fixture', run_id='a' * 32, release=RELEASE, action='reserve')
        with patch.object(S, 'fixture_load', return_value=manifest), patch.object(S, 'scheduler_profile', return_value=PROFILE), \
                patch.object(S, 'query', side_effect=lambda db, sql, args: [] if 'FROM workunit' in sql else [{'id': 7}]), patch.object(S.subprocess, 'run') as run:
            run.return_value.stdout = str(page)
            self.assertEqual(S.handle(request, self.root, self.root, None)['profile'], PROFILE)
            with self.assertRaises(ValueError):
                S.handle(request, self.root, self.root, None)
            run.side_effect = subprocess.TimeoutExpired('create_work', 60)
            request.update(action='dispatch', host_id=2)
            with self.assertRaises(subprocess.TimeoutExpired):
                S.handle(request, self.root, self.root, None)
            journal = self.root / 'fixture.reservation.json'
            self.assertTrue(json.loads(journal.read_text())['dispatch_intent'])
            with self.assertRaises(ValueError):
                S.handle(request, self.root, self.root, None)
            request['action'] = 'release'
            self.assertTrue(S.handle(request, self.root, self.root, None)['released'])
            self.assertTrue(json.loads(journal.read_text())['released'])

    def test_file_path_escape_is_rejected(self):
        outside = ROOT / 'README.md'
        with self.assertRaises(ValueError):
            S.below(self.root, str(outside))

    def test_validated_canonical_requires_success_from_the_requested_client_version(self):
        record = dict(run_id='a' * 32, release=RELEASE, host_id=2, workunit='acceptance_x', app_version_id=7)
        client = dict(id=88, hostid=2, server_state=5, outcome=1, validate_state=1, app_version_id=7)
        canonical = dict(client, xml_doc_in='<file_ref><file_name>output.json</file_name><open_name>result.json</open_name></file_ref>')
        upload = self.root / 'upload'
        upload.mkdir()
        output = upload / 'output.json'
        output.write_bytes(DATA)
        rows = [[dict(id=1, canonical_resultid=88, error_mask=0)], [client], [canonical]]
        with patch.object(S, 'query', side_effect=rows), patch.object(S.subprocess, 'run') as run:
            run.return_value.stdout = str(self.root / 'download' / output.name)
            receipt = S.status(None, self.root, record, self.manifest())
        self.assertEqual(receipt['state'], 'validated')
        self.assertEqual(A.check_evidence(receipt, RELEASE, 2, 'a' * 32)['canonical_result_id'], 88)
        for field, value in [('app_version_id', 9), ('validate_state', 2), ('outcome', 3)]:
            with patch.object(S, 'query', side_effect=[rows[0], [{**client, field: value}]]):
                self.assertEqual(S.status(None, self.root, record, self.manifest())['state'], 'failed')


if __name__ == '__main__':
    unittest.main()
