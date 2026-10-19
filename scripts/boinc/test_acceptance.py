"""Offline acceptance lifecycle tests; no Azure, SSH, keys, or paid resources."""
import argparse
import base64
import copy
import hashlib
import importlib.util
import io
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
URL = 'https://boinc.usnewsmap.com/'
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


    def mock_az(self):
        stub, log = self.root / 'az', self.root / 'az-calls.jsonl'
        stub.write_text('#!' + sys.executable + '\n' + """import json,os,sys,time
args=sys.argv[1:]
with open(os.environ['AZ_CALLS'], 'a') as out: out.write(json.dumps(args)+'\\n')
if args[:2]==['account','show']: print(args[args.index('--subscription')+1])
elif args[:2]==['group','exists']: print('true')
elif args[:3]==['stack','group','show']: print(json.dumps({'keyVaultName':{'value':'test-vault'}}))
elif args[:3]==['deployment','group','create'] and os.environ.get('AZ_HANG'): time.sleep(10)
""")
        stub.chmod(0o700)
        return dict(PATH=str(self.root) + os.pathsep + os.environ['PATH'], AZ_CALLS=str(log))

    def runner(self, config=None, **bounds):
        args = argparse.Namespace(env='prod', timeout=5, command_timeout=1, cleanup_timeout=2,
                                  poll_interval=.001, evidence=self.root / 'evidence.json', **bounds)
        return A.Runner(args, copy.deepcopy(config or CONFIG), dict(RELEASE))


class Eligibility(Workspace):
    def test_runner_uses_configured_master_url_verbatim(self):
        self.assertEqual(self.runner().url, URL)

    def test_cpu_effective_resources_and_platform_fail_closed(self):
        A.eligible(PROBE, PROFILE, RELEASE, URL)
        for key, bad in [('cpus', 2), ('memory_bytes', 5 * 1024**3), ('disk_bytes', 2 * 1024**3),
                         ('platform', 'windows_x86_64'), ('host_id', 0), ('project_url', 'wrong'),
                         ('client_version', 80211), ('container_runtime', False)]:
            with self.subTest(key=key), self.assertRaises(A.AcceptanceError):
                A.eligible({**PROBE, key: bad}, PROFILE, RELEASE, URL)

    def test_gpu_cuda_compute_and_opencl_minima_are_enforced(self):
        release = {**RELEASE, 'plan_class': 'docker_nvidia_train'}
        profile = {**PROFILE, 'gpu': 'nvidia', 'cuda_version': 12080,
                   'compute_capability': 800, 'opencl_version': 300}
        caps = dict(cuda_version=12080, compute_capability=800, opencl_version=300)
        probe = {**PROBE, 'gpus': ['nvidia'], 'gpu_capabilities': {'nvidia': caps}}
        A.eligible(probe, profile, release, URL)
        for key in caps:
            with self.assertRaises(A.AcceptanceError):
                A.eligible({**probe, 'gpu_capabilities': {'nvidia': {**caps, key: caps[key] - 1}}},
                           profile, release, URL)

    def test_gpu_vendor_and_vram_and_native_do_not_require_docker(self):
        for plan, gpu, platform in [('nvidia_cuda_8b', 'nvidia', RELEASE['platform']),
                                    ('apple_gpu_8b', 'apple', 'arm64-apple-darwin'),
                                    ('intel_vulkan_8b', 'intel', 'windows_x86_64'),
                                    ('nvidia_vulkan_8b', 'nvidia', RELEASE['platform']),
                                    ('amd_vulkan_8b', 'amd', RELEASE['platform']),
                                    ('amd_vulkan_8b', 'amd', 'windows_x86_64')]:
            release = {**RELEASE, 'plan_class': plan, 'platform': platform}
            profile = {**PROFILE, 'gpu': gpu, 'gpu_memory_bytes': 7_000_000_000}
            probe = {**PROBE, 'platform': platform, 'gpus': [gpu],
                     'gpu_memory_bytes': {gpu: 8_000_000_000}, 'container_runtime': False}
            A.eligible(probe, profile, release, URL)
            for bad in ({'gpus': []}, {'gpus': ['intel' if gpu != 'intel' else 'amd']},
                        {'gpu_memory_bytes': {gpu: 1}}, {'platform': 'wrong'}):
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
            auth = self.root / 'authenticator'
            auth.write_text('a' * 32)
            auth.chmod(0o600)
            config['client'] = dict(mode='ephemeral', sku='Standard_D8s_v5', authenticator_file=str(auth))
        runner = self.runner(config)
        calls = []
        states = iter(statuses or [dict(state='pending', run_id=runner.run_id, release=RELEASE), evidence(runner.run_id)])
        group = {'exists': False}

        def call(argv, payload=None):
            calls.append((argv, payload))
            if argv == ['server']:
                request = json.loads(payload)
                action = request['action']
                if fail == action:
                    raise A.AcceptanceError('injected ' + action)
                if action == 'reserve':
                    return json.dumps(dict(run_id=runner.run_id, release=RELEASE, profile=PROFILE, isolation_verified=fail != 'isolation'))
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

    def test_forced_provisioning_termination_removes_parent_owned_secret_params(self):
        auth = self.root / 'authenticator'
        auth.write_text('a' * 32)
        auth.chmod(0o600)
        runner = self.runner({**CONFIG, 'client': dict(mode='ephemeral', sku='Standard_D8s_v5',
                                                     authenticator_file=str(auth))})
        runner.args.command_timeout = 1.5
        reserve = dict(run_id=runner.run_id, release=RELEASE, profile=PROFILE, isolation_verified=True)
        with patch.dict(os.environ, {**self.mock_az(), 'AZ_HANG': '1'}), \
                patch.object(runner, 'az', return_value='false'), \
                patch.object(runner, 'server', side_effect=lambda action, **kw: reserve if action == 'reserve' else {'released': True}):
            try:
                with self.assertRaises(subprocess.TimeoutExpired):
                    runner.execute()
                self.assertEqual(runner.provision_dir.stat().st_mode & 0o777, 0o700)
                params = runner.provision_dir / 'params.json'
                self.assertIn('a' * 32, params.read_text())
                self.assertEqual(params.stat().st_mode & 0o777, 0o600)
            finally:
                runner.finish()
        self.assertFalse(runner.provision_dir.exists())
        self.assertFalse(runner.auth_path.exists())
        self.assertTrue(runner.report['cleanup_complete'])

    def test_provisioning_workspace_removal_failure_cannot_claim_clean_acceptance(self):
        runner = self.runner()
        runner.provision_dir = self.root / 'private-provision'
        runner.provision_dir.mkdir(mode=0o700)
        runner.report['accepted'] = True
        with patch.object(A.shutil, 'rmtree', side_effect=OSError('injected')):
            self.assertFalse(runner.finish())
        self.assertFalse(runner.report['accepted'])
        self.assertIn('private provisioning workspace cleanup failed', runner.report['cleanup_errors'])

    def test_slow_azure_deletion_cannot_exhaust_fixture_release_budget(self):
        config = {**CONFIG, 'client': dict(mode='ephemeral', sku='Standard_D8s_v5')}
        runner = self.runner(config)
        runner.vm_owned = runner.reserved = True
        clock = [100.0]
        contacted = []
        def azure(argv):
            if argv[:2] == ['group', 'exists']:
                clock[0] += .8
                return 'true'
            return ''
        def server(action):
            self.assertGreater(runner.deadline - clock[0], 0)
            contacted.append(action)
            return {'released': True}
        with patch.object(A.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(A.time, 'sleep'), patch.object(runner, 'az', side_effect=azure), \
                patch.object(runner, 'server', side_effect=server):
            self.assertFalse(runner.finish())
        self.assertEqual(contacted, ['release'])
        self.assertFalse(runner.report['cleanup_complete'])

    def test_ephemeral_isolation_is_verified_before_attachment_and_keys_never_logged(self):
        runner, calls = self.simulate(ephemeral=True)
        reserve = json.loads(calls[0][1])
        self.assertEqual(reserve['authenticator_sha256'], hashlib.sha256(('a' * 32).encode()).hexdigest())
        create = next(argv for argv, _ in calls if argv[0].endswith('boinc-test-client.sh'))
        self.assertIn('--authenticator-file', create)
        self.assertFalse(runner.auth_path.exists())
        self.assertNotIn('a' * 32, json.dumps(runner.report))
        runner, calls = self.simulate('isolation', ephemeral=True)
        self.assertFalse(any(argv[0].endswith('boinc-test-client.sh') for argv, _ in calls))
        self.assertFalse(runner.auth_path.exists())

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
if action=='reserve': print(json.dumps(dict(run_id=request['run_id'],release=request['release'],profile=""" + repr(PROFILE) + """,isolation_verified=True)))
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

    def test_timeout_kills_descendants_even_after_group_leader_exits(self):
        source = ('import subprocess,sys; subprocess.Popen([sys.executable, "-c", '
                  '"import time; time.sleep(3)"])')
        started = time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):
            A.run([sys.executable, '-c', source], .1)
        self.assertLess(time.monotonic() - started, 1)

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
        (self.root / 'build').mkdir()
        settings = self.root / 'settings.json'
        settings.write_text('{"images":{"appver":"old"}}')
        for code in ('0', '7'):
            env = {**os.environ, 'PATH': str(self.root) + os.pathsep + os.environ['PATH'],
                   'PREPARE_ONLY': 'false', 'ENV_NAME': 'prod', 'APP': 'usnm_qwen', 'VERSION': '1.0',
                   'PLATFORM': RELEASE['platform'], 'PLAN_CLASS': RELEASE['plan_class'],
                   'appver_image': 'example@sha256:abc', 'work': str(self.root / 'build'),
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


class ProvisioningHelper(Workspace):
    def helper(self, *args):
        with patch.dict(os.environ, self.mock_az()):
            return subprocess.run([str(ROOT / 'scripts/boinc-test-client.sh'), '--env', 'prod',
                                   '--resource-suffix', 'b' * 32, *args], cwd=ROOT, capture_output=True, text=True)

    def test_recovery_deletion_does_not_require_authenticator(self):
        result = self.helper('--delete')
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / 'az-calls.jsonl').read_text().splitlines()]
        deletion = next(call for call in calls if call[:2] == ['group', 'delete'])
        self.assertIn('usnm-boinc-test-rg-prod-' + 'b' * 32, deletion)
        self.assertIn('--subscription', deletion)

    def test_caller_owned_workspace_is_private_empty_and_not_removed_by_child(self):
        auth = self.root / 'authenticator'
        auth.write_text('a' * 32)
        workspace = self.root / 'private-provision'
        workspace.mkdir(mode=0o700)
        args = ['--create', '--what-if', '--authenticator-file', str(auth), '--work-dir', str(workspace)]
        result = self.helper(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((workspace / 'params.json').exists())
        self.assertNotIn('a' * 32, result.stdout + result.stderr)
        for mode in (0o700, 0o755):
            if mode == 0o755:
                (workspace / 'params.json').unlink()
            workspace.chmod(mode)
            result = self.helper(*args)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('caller-owned private directory', result.stderr)


class ClientProbe(Workspace):
    def state(self, **changes):
        values = dict(hostid=2, platform=RELEASE['platform'], cpus=4)
        values.update(changes)
        (self.root / 'client_state.xml').write_text(f'''<client_state>
        <platform_name>{values['platform']}</platform_name><core_client_major_version>8</core_client_major_version>
        <core_client_minor_version>2</core_client_minor_version><core_client_release>13</core_client_release>
        <host_info><p_ncpus>{values['cpus']}</p_ncpus><m_nbytes>{16 * 1024**3}</m_nbytes></host_info>
        <project><master_url>{URL}</master_url><hostid>{values['hostid']}</hostid><authenticator>SECRET</authenticator></project>
        </client_state>''')

    def test_effective_cpu_and_ram_limits_not_physical_host_advertisement(self):
        self.state()
        (self.root / 'global_prefs_override.xml').write_text('''<global_preferences>
        <max_ncpus_pct>50</max_ncpus_pct><ram_max_used_busy_pct>25</ram_max_used_busy_pct>
        <ram_max_used_idle_pct>90</ram_max_used_idle_pct></global_preferences>''')
        with patch.object(C, 'container_runtime', return_value=False):
            probe = C.probe(self.root, URL)
        self.assertEqual(probe['cpus'], 2)
        self.assertEqual(probe['memory_bytes'], 4 * 1024**3)
        self.assertEqual(probe['client_version'], 80213)
        self.assertNotIn('SECRET', json.dumps(probe))

    def test_zero_cpu_percentage_means_all_processors(self):
        self.state()
        (self.root / 'global_prefs_override.xml').write_text('<global_preferences><max_ncpus_pct>0</max_ncpus_pct></global_preferences>')
        with patch.object(C, 'container_runtime', return_value=False):
            probe = C.probe(self.root, URL)
        self.assertEqual(probe['cpus'], 4)
        A.eligible(probe, PROFILE, {**RELEASE, 'plan_class': 'mt'}, URL)

    def test_optional_runtime_failures_preserve_native_clients_and_try_fallback(self):
        self.state()
        podman = json.dumps({'host': {'arch': 'amd64'}, 'store': {'graphRoot': '/test'}})
        for error in (subprocess.TimeoutExpired('docker', 15), FileNotFoundError(), PermissionError()):
            with self.subTest(error=type(error).__name__), patch.object(C.platform, 'system', return_value='Linux'), \
                    patch.object(C.shutil, 'which', side_effect=lambda name: '/test/' + name), \
                    patch.object(C.subprocess, 'run', side_effect=[error, subprocess.CompletedProcess([], 0, podman)]) as run:
                self.assertTrue(C.container_runtime())
                self.assertEqual(run.call_args_list[-1].args[0][0], '/test/podman')
            with patch.object(C.platform, 'system', return_value='Linux'), \
                    patch.object(C.shutil, 'which', return_value='/test/runtime'), \
                    patch.object(C.subprocess, 'run', side_effect=error):
                probe = C.probe(self.root, URL)
            self.assertFalse(probe['container_runtime'])
            A.eligible(probe, PROFILE, {**RELEASE, 'plan_class': 'mt'}, URL)

    def test_mac_runtime_probes_boinc_helper_context_and_validates_output(self):
        helper = '/Library/Application Support/BOINC Data/Run_Podman'
        podman = '/opt/podman/bin/podman'
        outputs = [('', False), ('error: unavailable', False), ('{}', False), ('[]', False),
                   ('{"host":{},"store":{}}', False),
                   ('{"host":{"arch":"arm64"},"store":{"graphRoot":"/Library/Application Support/BOINC podman"}}', True)]
        for output, expected in outputs:
            with self.subTest(output=output), patch.object(C.platform, 'system', return_value='Darwin'), \
                    patch.object(C.os, 'access', return_value=True), patch.object(C, 'mac_podman', return_value=podman), \
                    patch.object(C.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, output)) as run:
                self.assertEqual(C.container_runtime(), expected)
                self.assertEqual(run.call_args.args[0], [helper, podman, 'info', '--format=json'])

    def test_runtime_requires_structured_server_identity_not_only_exit_status(self):
        for output, code, expected in [('', 0, False), ('{}', 0, False), ('[]', 0, False),
                                      ('{"ServerVersion":""}', 0, False),
                                      ('{"ServerVersion":"27.0"}', 1, False),
                                      ('{"ServerVersion":"27.0"}', 0, True)]:
            with self.subTest(output=output, code=code), patch.object(C.platform, 'system', return_value='Linux'), \
                    patch.object(C.shutil, 'which', side_effect=lambda name: '/test/docker' if name == 'docker' else None), \
                    patch.object(C.subprocess, 'run', return_value=subprocess.CompletedProcess([], code, output)) as run:
                self.assertEqual(C.container_runtime(), expected)
                self.assertEqual(run.call_args.args[0], ['/test/docker', 'info', '--format', '{{json .}}'])
        for error in (OSError(), subprocess.TimeoutExpired('Run_Podman', 15)):
            with patch.object(C.platform, 'system', return_value='Darwin'), patch.object(C.os, 'access', return_value=True), \
                    patch.object(C, 'mac_podman', return_value='/opt/podman/bin/podman'), \
                    patch.object(C.subprocess, 'run', side_effect=error):
                self.assertFalse(C.container_runtime())

    def test_mac_discovery_uses_terminal_path_then_installer_and_homebrew(self):
        output = subprocess.CompletedProcess([], 0, 'PATH="/opt/local/bin:/usr/bin"; export PATH;')
        with patch.object(C.subprocess, 'run', return_value=output), \
                patch.object(C.shutil, 'which', return_value='/opt/local/bin/podman') as which:
            self.assertEqual(C.mac_podman(), '/opt/local/bin/podman')
            which.assert_called_once_with('podman', path='/opt/local/bin:/usr/bin')
        for arch, fallback in [('arm64', '/opt/homebrew/bin/podman'), ('x86_64', '/usr/local/bin/podman')]:
            with patch.object(C.subprocess, 'run', side_effect=OSError()), patch.object(C.shutil, 'which', return_value=None), \
                    patch.object(C.platform, 'machine', return_value=arch), \
                    patch.object(C.os, 'access', side_effect=lambda path, mode: path == fallback):
                self.assertEqual(C.mac_podman(), fallback)
        with patch.object(C.platform, 'system', return_value='Darwin'), \
                patch.object(C.os, 'access', return_value=False), patch.object(C.subprocess, 'run') as run:
            self.assertFalse(C.container_runtime())
            run.assert_not_called()

    def test_mac_boinccmd_uses_installed_app_when_path_is_restricted(self):
        with patch.object(C.platform, 'system', return_value='Darwin'), patch.object(C.shutil, 'which', return_value=None), \
                patch.object(C.os, 'access', return_value=True):
            self.assertEqual(C.boinccmd(), '/Applications/BOINCManager.app/Contents/Resources/boinccmd')

    def test_probe_reads_actual_gpu_capability_properties(self):
        self.state()
        state = C.ET.parse(self.root / 'client_state.xml')
        node = C.ET.SubElement(C.ET.SubElement(state.getroot(), 'coprocs'), 'coproc_cuda')
        for key, value in {'count': '1', 'cudaVersion': '12080', 'opencl_device_version': 'OpenCL 3.0 CUDA'}.items():
            C.ET.SubElement(node, key).text = value
        prop = C.ET.SubElement(node, 'prop')
        for key, value in {'major': '8', 'minor': '0', 'totalGlobalMem': str(16 * 1024**3)}.items():
            C.ET.SubElement(prop, key).text = value
        state.write(self.root / 'client_state.xml')
        with patch.object(C, 'container_runtime', return_value=False):
            probe = C.probe(self.root, URL)
        self.assertEqual(probe['gpu_capabilities']['nvidia'],
                         dict(cuda_version=12080, compute_capability=800, opencl_version=300))

    def test_probe_reports_boinc_ati_as_amd_for_native_vulkan(self):
        self.state()
        state = C.ET.parse(self.root / 'client_state.xml')
        node = C.ET.SubElement(C.ET.SubElement(state.getroot(), 'coprocs'), 'coproc_ati')
        for key, value in {'count': '1', 'available_ram': str(16 * 1024**3),
                           'opencl_device_version': 'OpenCL 3.0 AMD'}.items():
            C.ET.SubElement(node, key).text = value
        state.write(self.root / 'client_state.xml')
        with patch.object(C, 'container_runtime', return_value=False):
            probe = C.probe(self.root, URL)
        self.assertEqual(probe['gpus'], ['amd'])
        self.assertEqual(probe['gpu_memory_bytes']['amd'], 16 * 1024**3)
        self.assertEqual(probe['gpu_capabilities']['amd']['opencl_version'], 300)

    def test_disk_quota_subtracts_existing_usage_and_zero_absolute_is_unlimited(self):
        total, free, used = 100_000_000_000, 60_000_000_000, 20_000_000_000
        self.assertEqual(C.disk_allowance(total, free, used, {'disk_max_used_pct': 25,
                         'disk_max_used_gb': 50, 'disk_min_free_gb': 10}), 5_000_000_000)
        self.assertEqual(C.disk_allowance(total, free, used, {'disk_max_used_pct': 90,
                         'disk_max_used_gb': 25, 'disk_min_free_gb': 10}), 5_000_000_000)
        self.assertEqual(C.disk_allowance(total, free, used, {'disk_max_used_gb': 0}), 59_900_000_000)
        self.assertEqual(C.disk_allowance(total, free, used, {'disk_max_used_pct': 10}), 0)
        data = self.root / 'cached-model'
        data.write_bytes(b'fixture data')
        usage = C.directory_usage(self.root)
        self.assertGreaterEqual(usage, data.stat().st_size)
        (self.root / 'slot-link').symlink_to(data)
        self.assertEqual(C.directory_usage(self.root), usage)

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
    def test_amd_fixture_and_scheduler_profile_remain_closed_until_operator_admission(self):
        release = {**RELEASE, 'plan_class': 'amd_vulkan_8b'}
        manifest = {**self.manifest(), 'release': release, 'batch': 8,
                    'profile': {**PROFILE, 'gpu': 'amd'}}
        (self.root / 'fixture.json').write_text(json.dumps(manifest))
        self.assertEqual(S.fixture_load(self.root, 'fixture', release), manifest)
        source = ROOT / 'contrib-compute/boinc/plan_class_spec.xml'
        tree = C.ET.parse(source)
        target = self.root / 'plan_class_spec.xml'
        tree.write(target)
        with self.assertRaisesRegex(ValueError, 'never auto-open cohorts'):
            S.scheduler_profile(self.root, manifest)
        node = next(n for n in tree.getroot() if n.findtext('name') == 'amd_vulkan_8b')
        node.find('min_wu_id').text = node.find('max_wu_id').text = '300'
        tree.write(target)
        profile = S.scheduler_profile(self.root, manifest)
        self.assertEqual(profile['gpu'], 'amd')
        self.assertEqual(profile['gpu_memory_bytes'], 7_000_000_000)
        self.assertEqual(profile['client_version'], max(80211, PROFILE['client_version']))
        with self.assertRaisesRegex(ValueError, 'GPU differs'):
            S.scheduler_profile(self.root, {**manifest, 'profile': {**PROFILE, 'gpu': 'intel'}})

    def setUp(self):
        super().setUp()
        (self.root / 'config.xml').write_text('<boinc><config><enable_assignment>1</enable_assignment></config></boinc>')

    def manifest(self):
        return dict(release=RELEASE, profile=PROFILE, batch=12, min_quorum=2,
                    workunit_name='usnm_qwen_' + 'a' * 64, peer_host_ids=[3], account_id=42,
                    input_template='qwen_in', output_template='qwen_out',
                    inputs=[dict(name='page.jpg', sha256='a' * 64)],
                    outputs=[dict(name='result.json', kind='json', equals={'unit_error': None}, nonempty=['transcription'])])

    def test_account_isolation_rejects_ordinary_work_and_wrong_credentials(self):
        manifest = self.manifest()
        user = dict(id=42, seti_id=1, teamid=0, authenticator='a' * 32)
        def rows(_db, sql, args):
            if 'FROM user' in sql:
                return [user]
            if 'FROM host' in sql:
                self.assertEqual(args[1], 42)
                return [{'id': args[0]}]
            return []
        with patch.object(S, 'query', side_effect=rows):
            S.isolated_account(None, manifest, host_id=2,
                               fingerprint=hashlib.sha256(('a' * 32).encode()).hexdigest())
            user['seti_id'] = 0
            with self.assertRaises(ValueError):
                S.isolated_account(None, manifest)
            user['seti_id'] = 1
            with self.assertRaises(ValueError):
                S.isolated_account(None, manifest, fingerprint='b' * 64)
        with patch.object(S, 'query', side_effect=lambda db, sql, args: [{'id': 99}] if 'FROM assignment' in sql else rows(db, sql, args)):
            with self.assertRaises(ValueError):
                S.isolated_account(None, manifest)

    def test_input_hashing_streams_bounded_chunks(self):
        sizes = []
        class Reader(io.BytesIO):
            def read(self, size=-1):
                sizes.append(size)
                return super().read(size)
        data = b'x' * (3 * 1024 * 1024 + 3)
        path = MagicMock()
        path.open.return_value = Reader(data)
        self.assertEqual(S.file_hash(path), hashlib.sha256(data).hexdigest())
        self.assertTrue(all(size == 1024 * 1024 for size in sizes))

    def test_template_and_effective_bounds_cannot_lower_manifest_quorum(self):
        manifest = self.manifest()
        templates = self.root / 'templates'
        templates.mkdir()
        path = templates / manifest['input_template']
        values = S.bounds(manifest)
        def write(values):
            path.write_text('<input_template><workunit>' + ''.join('<'+k+'>'+str(v)+'</'+k+'>' for k,v in values.items()) + '</workunit></input_template>')
        write(values)
        self.assertEqual(S.template_check(self.root, manifest), hashlib.sha256(path.read_bytes()).hexdigest())
        for key in ('min_quorum', 'target_nresults', 'rsc_memory_bound', 'rsc_disk_bound'):
            bad = {**values, key: values[key] + 1}
            write(bad)
            with self.assertRaises(ValueError):
                S.template_check(self.root, manifest)
            with self.assertRaises(ValueError):
                S.verify_bounds(bad, manifest)

    def test_canonical_result_cannot_bypass_requested_independent_quorum(self):
        manifest = {**self.manifest(), 'min_quorum': 3, 'peer_host_ids': [3, 4]}
        record = dict(run_id='a'*32, release=RELEASE, host_id=2, workunit=manifest['workunit_name'], app_version_id=7)
        wu = dict(id=1, canonical_resultid=88, error_mask=0, batch=12, assimilate_state=2, **S.bounds(manifest))
        client = dict(id=88, hostid=2, server_state=5, outcome=1, validate_state=1, app_version_id=7)
        with patch.object(S, 'query', side_effect=[[wu], [client, {**client, 'hostid': 3, 'id': 89}]]):
            self.assertEqual(S.status(None, self.root, record, manifest)['state'], 'pending')

    def test_connector_uses_shipped_verified_tls_options_and_rejects_plaintext(self):
        (self.root / 'config.xml').write_text('<boinc><config><db_host>test.invalid</db_host>'
             '<db_user>fixture</db_user><db_passwd>fixture-only</db_passwd><db_name>fixture</db_name></config></boinc>')
        driver = MagicMock()
        db = driver.connect.return_value
        db.cursor.return_value.fetchone.return_value = ('Ssl_cipher', 'TLS_AES_256_GCM_SHA384')
        with patch.dict(sys.modules, MySQLdb=driver), patch.object(S.Path, 'is_file', return_value=True):
            self.assertIs(S.db_connect(self.root), db)
            options = driver.connect.call_args.kwargs
            self.assertEqual(options['read_default_file'], '/usr/local/share/usnm/mysql-tls.cnf')
            self.assertEqual(options['read_default_group'], 'client')
            self.assertNotIn('ssl_mode', options)
            db.cursor.return_value.fetchone.return_value = ('Ssl_cipher', '')
            with self.assertRaises(ValueError):
                S.db_connect(self.root)
            db.close.assert_called_once()
        driver.connect.reset_mock()
        with patch.dict(sys.modules, MySQLdb=driver), patch.object(S.Path, 'is_file', return_value=False):
            with self.assertRaises(ValueError):
                S.db_connect(self.root)
        driver.connect.assert_not_called()

    def test_reservation_rejects_ambiguous_plan_or_platform_before_provisioning(self):
        manifest = self.manifest()
        request = dict(fixture='fixture', run_id='a' * 32, release=RELEASE, action='reserve')
        with patch.object(S, 'fixture_load', return_value=manifest), patch.object(S, 'isolated_account'), patch.object(S, 'template_check', return_value='approved'), \
                patch.object(S, 'scheduler_profile', return_value=PROFILE), \
                patch.object(S, 'query', side_effect=[[{'id': 7}], [{'id': 7}, {'id': 8}]]):
            with self.assertRaisesRegex(ValueError, 'ambiguous'):
                S.handle(request, self.root, self.root, MagicMock())
        self.assertFalse((self.root / 'fixture.reservation.json').exists())

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

    def test_scheduler_minima_never_lower_stricter_fixture_requirements(self):
        (self.root / 'plan_class_spec.xml').write_text('<plan_classes><plan_class><name>docker_mt_2b</name>'
            '<min_core_client_version>80213</min_core_client_version><gpu_type>none</gpu_type>'
            '<min_gpu_ram_mb>7000</min_gpu_ram_mb></plan_class></plan_classes>')
        manifest = self.manifest()
        manifest['profile'] = {**PROFILE, 'gpu_memory_bytes': 12 * 1024**3, 'client_version': 80400}
        profile = S.scheduler_profile(self.root, manifest)
        self.assertEqual(profile['gpu_memory_bytes'], 12 * 1024**3)
        self.assertEqual(profile['client_version'], 80400)

    def test_live_plan_batch_and_closed_cohort_guards(self):
        (self.root / 'plan_class_spec.xml').write_bytes((ROOT / 'contrib-compute/boinc/plan_class_spec.xml').read_bytes())
        manifest = self.manifest()
        self.assertEqual(S.scheduler_profile(self.root, manifest)['client_version'], 80213)
        training = {**manifest, 'release': {**RELEASE, 'plan_class': 'docker_nvidia_train'},
                    'profile': {**PROFILE, 'gpu': 'nvidia'}}
        self.assertEqual(S.scheduler_profile(self.root, training)['cuda_version'], 12080)
        self.assertEqual(S.scheduler_profile(self.root, training)['compute_capability'], 800)
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
                      app_version_id=7, released=False, input_template_sha256='approved',
                      manifest_sha256=hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest())
        (self.root / 'fixture.reservation.json').write_text(json.dumps(record))
        (self.root / 'config.xml').write_text('<boinc><config><enable_assignment>1</enable_assignment></config></boinc>')
        request = dict(action='dispatch', fixture='fixture', run_id='a' * 32, release=RELEASE, host_id=2)
        db = MagicMock()
        visibility = {'snapshot': False, 'created': False}
        def commit():
            visibility['snapshot'] = False
        def query(_db, sql, _args):
            if 'FROM host' in sql:
                visibility['snapshot'] = True
                return [{'id': 2}]
            return [{'id': 11, **S.bounds(manifest)}] if visibility['created'] and not visibility['snapshot'] else []
        def create(*_args, **_kwargs):
            self.assertFalse(visibility['snapshot'], 'host lookup transaction must end before external create_work')
            visibility['created'] = True
        db.commit.side_effect = commit
        with patch.object(S, 'fixture_load', return_value=manifest), patch.object(S, 'isolated_account'), patch.object(S, 'template_check', return_value='approved'), \
                patch.object(S, 'query', side_effect=query), patch.object(S.subprocess, 'run', side_effect=create):
            result = S.handle(request, self.root, self.root, db)
        self.assertTrue(result['dispatched'])
        sql, values = db.cursor.return_value.execute.call_args.args
        self.assertIn('assignment', sql)
        self.assertIn('1, 0', sql)
        self.assertEqual(values, (3, 11))
        self.assertEqual(db.commit.call_count, 2)
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
        with patch.object(S, 'fixture_load', return_value=manifest), patch.object(S, 'isolated_account'), patch.object(S, 'template_check', return_value='approved'), patch.object(S, 'scheduler_profile', return_value=PROFILE), \
                patch.object(S, 'query', side_effect=lambda db, sql, args: [] if 'FROM workunit' in sql else [{'id': 7}]), patch.object(S.subprocess, 'run') as run:
            run.return_value.stdout = str(page)
            self.assertEqual(S.handle(request, self.root, self.root, None)['profile'], PROFILE)
            with self.assertRaises(ValueError):
                S.handle(request, self.root, self.root, None)
            run.side_effect = subprocess.TimeoutExpired('create_work', 60)
            request.update(action='dispatch', host_id=2)
            with self.assertRaises(subprocess.TimeoutExpired):
                S.handle(request, self.root, self.root, MagicMock())
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
        rows = [[dict(id=1, canonical_resultid=88, error_mask=0, batch=12, assimilate_state=0, **S.bounds(self.manifest()))], [client, {**client, 'hostid': 3, 'id': 89}], [canonical]]
        with patch.object(S, 'query', side_effect=rows), patch.object(S.subprocess, 'run') as run:
            run.return_value.stdout = str(self.root / 'download' / output.name)
            receipt = S.status(None, self.root, record, self.manifest())
        self.assertEqual(receipt['state'], 'validated')
        self.assertEqual(A.check_evidence(receipt, RELEASE, 2, 'a' * 32)['canonical_result_id'], 88)
        collected = self.root / 'results/12' / (record['workunit'] + '__file_result.json')
        collected.parent.mkdir(parents=True)
        for move_during_open in (False, True):
            output.write_bytes(DATA)
            if collected.exists():
                collected.unlink()
            if not move_during_open:
                output.rename(collected)
            original = Path.read_bytes
            def read(path):
                if path == output and move_during_open:
                    output.rename(collected)
                    raise FileNotFoundError(path)
                return original(path)
            settled = [[{**rows[0][0], 'assimilate_state': 2}], [client, {**client, 'hostid': 3, 'id': 89}], [canonical]]
            with patch.object(S, 'query', side_effect=settled), patch.object(S.subprocess, 'run') as run, \
                    patch.object(Path, 'read_bytes', read):
                run.return_value.stdout = str(self.root / 'download' / output.name)
                receipt = S.status(None, self.root, record, self.manifest())
                self.assertEqual(receipt['state'], 'validated')
                self.assertEqual(receipt['outputs'][0]['sha256'], hashlib.sha256(DATA).hexdigest())
        for field, value in [('app_version_id', 9), ('validate_state', 2), ('outcome', 3)]:
            with patch.object(S, 'query', side_effect=[rows[0], [{**client, field: value}]]):
                self.assertEqual(S.status(None, self.root, record, self.manifest())['state'], 'failed')


if __name__ == '__main__':
    unittest.main()
