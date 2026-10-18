#!/usr/bin/env python3
"""Bounded, opt-in BOINC release acceptance; never builds, signs, or publishes."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
CLIENT = Path(__file__).with_name('acceptance_client.py')


class AcceptanceError(Exception):
    pass


def run(argv, seconds, payload=None):
    # Kill descendants too: a timed-out ssh/az must not outlive its acceptance run.
    process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True, start_new_session=True)
    try:
        stdout, _ = process.communicate(payload, timeout=max(.01, seconds))
    except BaseException:
        try:
            # Descendants may retain pipes after the group leader has exited.
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            process.kill()
        try:
            process.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            # Even a descendant that escaped the group cannot block teardown.
            for stream in (process.stdin, process.stdout, process.stderr):
                if stream is not None:
                    stream.close()
        raise
    if process.returncode:
        # Command output can contain account keys, URLs with SAS, and RPC passwords.
        raise AcceptanceError(f'{Path(argv[0]).name} failed (exit {process.returncode}); output suppressed')
    return stdout


def command(value):
    if not isinstance(value, list) or not value or any(not isinstance(x, str) or not x for x in value):
        raise AcceptanceError('commands must be nonempty argv arrays (not shell strings)')
    return value


def load(path, app, version, platform, plan):
    config = json.loads(path.read_text())
    command(config['server_command'])
    client = config['client']
    if client['mode'] == 'existing':
        command(client['python_command'])
        if client.get('dedicated_test_client') is not True:
            raise AcceptanceError('existing clients require dedicated_test_client=true')
    elif client['mode'] == 'ephemeral':
        if platform != 'x86_64-pc-linux-gnu' or any(x in plan for x in ('apple', 'intel', 'vulkan')):
            raise AcceptanceError('Azure ephemeral clients support Linux x86 CPU/NVIDIA only')
        if not client.get('authenticator_file'):
            raise AcceptanceError('ephemeral clients require a private pre-isolated account authenticator_file')
        secret = Path(client['authenticator_file'])
        client['authenticator_file'] = str(secret if secret.is_absolute() else path.resolve().parent / secret)
        if not re.fullmatch(r'Standard_[A-Za-z0-9_]+', client['sku']):
            raise AcceptanceError('invalid Azure SKU')
    else:
        raise AcceptanceError('unknown client mode')
    if not re.fullmatch(r'[A-Za-z0-9_.-]{1,80}', config['fixture']):
        raise AcceptanceError('invalid fixture name')
    if not re.fullmatch(r'[1-9][0-9]*\.[0-9]{1,2}', version):
        raise AcceptanceError('version must have at most two fractional digits')
    major, minor = version.split('.')
    return config, dict(app=app, version_num=int(major) * 100 + int(minor),
                        platform=platform, plan_class=plan)


def eligible(probe, profile, release, url):
    if probe['platform'] != release['platform'] or probe['project_url'] != url or probe['host_id'] <= 0:
        raise AcceptanceError('client platform/project/host mismatch')
    for key in ('cpus', 'memory_bytes', 'disk_bytes'):
        if probe[key] < profile[key]:
            raise AcceptanceError(f'client has insufficient effective {key}')
    if probe['client_version'] < profile.get('client_version', 0):
        raise AcceptanceError('BOINC client version is below scheduler minimum')
    gpu = profile['gpu']
    plan = release['plan_class']
    required = ('nvidia' if any(x in plan for x in ('nvidia', 'cuda')) else
                'intel' if any(x in plan for x in ('intel', 'vulkan')) else
                'apple' if 'apple_gpu' in plan else 'none')
    if gpu != required or (gpu != 'none' and gpu not in probe['gpus']):
        raise AcceptanceError('fixture/client GPU does not match plan class')
    if gpu != 'none' and probe.get('gpu_memory_bytes', {}).get(gpu, 0) < profile.get('gpu_memory_bytes', 0):
        raise AcceptanceError('GPU memory is below scheduler minimum')
    # docker_mt needs four *usable* CPUs, not merely four physical cores.
    if ('docker_mt' in plan or plan == 'mt') and probe['cpus'] < 4:
        raise AcceptanceError('multi-thread plan requires four usable CPUs')
    for key in ('cuda_version', 'compute_capability', 'opencl_version'):
        if profile.get(key, 0) > probe.get('gpu_capabilities', {}).get(gpu, {}).get(key, 0):
            raise AcceptanceError('GPU lacks scheduler capability: ' + key)
    if plan.startswith('docker') and not probe.get('container_runtime'):
        raise AcceptanceError('container plan requires a usable container runtime')


def check_evidence(status, release, host_id, run_id):
    if status.get('run_id') != run_id or status.get('release') != release:
        raise AcceptanceError('evidence belongs to another reservation/release')
    if status.get('state') == 'failed':
        raise AcceptanceError('fixture execution or independent validation failed')
    if status.get('state') != 'validated':
        return None
    if (status.get('host_id') != host_id or not status.get('canonical_result_id')
            or status.get('client_validate_state') != 1 or status.get('client_outcome') != 1
            or status.get('client_server_state') != 5 or not status.get('outputs')):
        raise AcceptanceError('canonical validation lacks successful client execution/output')
    outputs = []
    for output in status['outputs']:
        data = base64.b64decode(output['base64'], validate=True)
        if not data or len(data) > 4 * 1024 * 1024 or hashlib.sha256(data).hexdigest() != output['sha256']:
            raise AcceptanceError('canonical output checksum/size mismatch')
        outputs.append({k: output[k] for k in ('name', 'sha256', 'bytes')})
        if output['bytes'] != len(data):
            raise AcceptanceError('canonical output length mismatch')
    # Never persist arbitrary output text, stderr, or RPC state (potential PII/secrets).
    return dict(canonical_result_id=status['canonical_result_id'], outputs=outputs,
                host_id=host_id, client_validate_state=1)


class Runner:
    def __init__(self, args, config, release):
        self.args, self.config, self.release = args, config, release
        self.run_id = uuid.uuid4().hex
        self.deadline = time.monotonic() + args.timeout
        self.cleanup = False
        self.vm_owned = False
        self.reserved = False
        self.auth_path = None
        self.provision_dir = None
        self.report = dict(schema_version=1, run_id=self.run_id, release=release,
                           fixture=config['fixture'], accepted=False, cleanup_complete=False,
                           coordination_claim=getattr(args, 'coordination_claim', None))
        settings = json.loads((ROOT / f'contrib-compute/infra/boinc/env/{args.env}.json').read_text())
        self.subscription = settings['subscriptionId']
        self.url = settings['projectUrl'] + 'usnmocr/'
        self.group = f'usnm-boinc-test-rg-{args.env}-{self.run_id}'
        self.vm = f'usnm-boinc-test-vm-{args.env}'

    def call(self, argv, payload=None):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise AcceptanceError('acceptance deadline exceeded')
        return run(argv, min(self.args.command_timeout, remaining), payload)

    def az(self, argv):
        return self.call(['az', *argv, '--subscription', self.subscription, '--only-show-errors'])

    def server(self, action, **extra):
        payload = dict(action=action, run_id=self.run_id, fixture=self.config['fixture'],
                       release=self.release, **extra)
        return json.loads(self.call(self.config['server_command'], json.dumps(payload)))

    def client(self, action):
        code = CLIENT.read_text()
        argv = ['-c', code, '--action', action, '--project-url', self.url]
        if self.config['client']['mode'] == 'existing':
            source = 'import sys; sys.argv = ' + repr(['acceptance-client', '--action', action, '--project-url', self.url]) + '\n' + code
            return json.loads(self.call(self.config['client']['python_command'], source))
        script = ('if id boinc >/dev/null 2>&1 && test -f /var/lib/boinc/client_state.xml; then '
                  'runuser -u boinc -- python3 ' + shlex.join(argv) +
                  "; else echo '{\"ready\":false}'; fi")
        raw = self.az(['vm', 'run-command', 'invoke', '-g', self.group, '-n', self.vm,
                       '--command-id', 'RunShellScript', '--scripts', script,
                       '--query', 'value[0].message', '-o', 'tsv'])
        # Azure's wrapper contains banners; no raw VM output is ever logged.
        for line in raw.splitlines():
            if line.startswith('{'):
                return json.loads(line)
        raise AcceptanceError('VM returned no client receipt')

    def execute(self):
        self.reserved = True  # Reservation may succeed remotely before the transport fails.
        private = {}
        if self.config['client']['mode'] == 'ephemeral':
            fd = os.open(self.config['client']['authenticator_file'], os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(fd) as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077:
                    raise AcceptanceError('authenticator file must be private and regular')
                key = stream.read(256).strip()
            if not re.fullmatch(r'[0-9a-f]{32}', key):
                raise AcceptanceError('expected dedicated test-account authenticator in private file')
            self.auth_path = ROOT / ('.boinc-acceptance-' + self.run_id)
            fd = os.open(self.auth_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, 'w') as stream:
                stream.write(key)
            private['authenticator_sha256'] = hashlib.sha256(key.encode()).hexdigest()
        reservation = self.server('reserve', **private)
        if reservation.get('isolation_verified') is not True:
            raise AcceptanceError('server did not verify assignment-only isolation before attachment')
        if reservation.get('run_id') != self.run_id or reservation.get('release') != self.release:
            raise AcceptanceError('invalid fixture reservation')
        self.report['profile'] = reservation['profile']
        self.report['workunit'] = reservation.get('workunit')
        if self.config['client']['mode'] == 'ephemeral':
            if self.az(['group', 'exists', '-n', self.group, '-o', 'tsv']).strip() != 'false':
                raise AcceptanceError('refusing to own an existing resource group')
            self.vm_owned = True  # Includes partial provisioning failures.
            provision_dir = ROOT / ('.boinc-provision-' + self.run_id)
            provision_dir.mkdir(mode=0o700)
            self.provision_dir = provision_dir
            self.call([str(ROOT / 'scripts/boinc-test-client.sh'), '--env', self.args.env,
                       '--create', '--sku', self.config['client']['sku'],
                       '--resource-suffix', self.run_id, '--authenticator-file', str(self.auth_path),
                       '--work-dir', str(self.provision_dir)])
        while True:
            probe = self.client('probe')
            if probe.get('ready'):
                break
            self.pause()
        eligible(probe, reservation['profile'], self.release, self.url)
        self.report['client'] = probe
        self.server('dispatch', host_id=probe['host_id'])
        while True:
            status = self.server('status')
            evidence = check_evidence(status, self.release, probe['host_id'], self.run_id)
            if evidence is not None:
                self.report['evidence'] = evidence
                self.report['accepted'] = True
                return
            self.client('update')
            self.pause()

    def pause(self):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise AcceptanceError('acceptance deadline exceeded')
        time.sleep(min(self.args.poll_interval, remaining))

    def finish(self):
        cleanup_end = time.monotonic() + self.args.cleanup_timeout
        # Azure may consume only its share; always leave time to contact the fixture server.
        self.deadline = time.monotonic() + self.args.cleanup_timeout * .75 if self.vm_owned else cleanup_end
        errors = []
        # Ignore a second interrupt during bounded teardown, not during inference.
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(sig, signal.SIG_IGN)
        if self.vm_owned:
            try:
                self.az(['group', 'delete', '-n', self.group, '--yes', '--no-wait', '-o', 'none'])
                while self.az(['group', 'exists', '-n', self.group, '-o', 'tsv']).strip() != 'false':
                    self.pause()
            except Exception:
                errors.append('ephemeral client deletion unconfirmed: ' + self.group)
        self.deadline = cleanup_end
        if self.reserved:
            try:
                if self.server('release').get('released') is not True:
                    raise AcceptanceError('fixture reservation not owned')
            except Exception:
                errors.append('fixture reservation release unconfirmed')
        if self.provision_dir is not None:
            try:
                if self.provision_dir.exists():
                    shutil.rmtree(self.provision_dir)
            except OSError:
                errors.append('private provisioning workspace cleanup failed')
        if self.auth_path is not None:
            try:
                self.auth_path.unlink(missing_ok=True)
            except OSError:
                errors.append('private acceptance authenticator cleanup failed')
        self.report['cleanup_complete'] = not errors
        self.report['cleanup_errors'] = errors
        if errors:
            self.report['accepted'] = False
        self.args.evidence.write_text(json.dumps(self.report, indent=2) + '\n')
        return not errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--env', choices=['dev', 'prod'], required=True)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--app', required=True)
    parser.add_argument('--version', required=True)
    parser.add_argument('--platform', required=True)
    parser.add_argument('--plan-class', default='')
    parser.add_argument('--evidence', type=Path, required=True)
    parser.add_argument('--timeout', type=int, default=1800)
    parser.add_argument('--command-timeout', type=int, default=120)
    parser.add_argument('--cleanup-timeout', type=int, default=600)
    parser.add_argument('--poll-interval', type=float, default=15)
    parser.add_argument('--coordination-claim', help='URL of active #529 claim; required for prod')
    parser.add_argument('--dry-run', action='store_true')
    args = parser.parse_args()
    try:
        if not (0 < args.timeout <= 14400 and 0 < args.command_timeout <= 600
                and 0 < args.cleanup_timeout <= 1800 and 0 < args.poll_interval <= 300):
            raise AcceptanceError('invalid bounds (maximum run 4h, cleanup 30m)')
        config, release = load(args.config, args.app, args.version, args.platform, args.plan_class)
        if args.dry_run:
            print(json.dumps(dict(dry_run=True, release=release, fixture=config['fixture'],
                                  client_mode=config['client']['mode'], timeout=args.timeout,
                                  actions=['reserve', 'client preflight', 'dispatch', 'update',
                                           'wait canonical validation', 'evidence', 'cleanup'])))
            return 0
        if args.env == 'prod' and not re.fullmatch(
                r'https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-[0-9]+',
                args.coordination_claim or ''):
            raise AcceptanceError('prod requires an active #529 --coordination-claim; this is not permission to deploy')
        # Never overwrite another run or create an evidence path after allocating resources.
        fd = os.open(args.evidence, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        runner = Runner(args, config, release)
        def interrupted(signum, _frame):
            raise AcceptanceError(f'interrupted by signal {signum}')
        for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            signal.signal(sig, interrupted)
        try:
            runner.execute()
        except Exception as exc:
            # Only our own fixed diagnostics are safe to persist.
            runner.report['error'] = str(exc) if isinstance(exc, AcceptanceError) else type(exc).__name__
        finally:
            runner.finish()
        print('acceptance passed' if runner.report['accepted'] else 'acceptance failed; inspect private evidence')
        return 0 if runner.report['accepted'] else 1
    except (AcceptanceError, ValueError, KeyError, OSError) as exc:
        print('acceptance configuration failed: ' + (str(exc) if isinstance(exc, AcceptanceError)
                                                     else type(exc).__name__), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
