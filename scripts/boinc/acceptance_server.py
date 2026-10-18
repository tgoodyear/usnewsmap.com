#!/usr/bin/env python3
"""Server-side acceptance adapter. Run as project owner, never on the signing host.

Reads one JSON request on stdin. Fixtures are operator-reviewed, pre-staged inputs
and templates, not production queue work. Keeps durable reservation/dispatch intent
so transport failures cannot cause duplicate submission. Uses existing validators.
"""
import argparse
import base64
import fcntl
import hashlib
import hmac
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import xml.etree.ElementTree as ET

SAFE = re.compile(r'[A-Za-z0-9_.-]{1,120}')


def durable(path, value):
    staged = path.with_suffix('.new')
    with staged.open('w') as stream:
        json.dump(value, stream)
        stream.flush()
        os.fsync(stream.fileno())
    staged.replace(path)
    fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def db_connect(project):
    import MySQLdb
    config = ET.parse(project / 'config.xml').find('config')
    options = '/usr/local/share/usnm/mysql-tls.cnf'
    if not Path(options).is_file():
        raise ValueError('required verified TLS option file is missing')
    db = MySQLdb.connect(host=config.findtext('db_host'), user=config.findtext('db_user'),
                         passwd=config.findtext('db_passwd'), db=config.findtext('db_name'),
                         ssl={'ca': '/usr/local/share/usnm/mysql-ca.pem'},
                         read_default_file=options, read_default_group='client')
    cursor = db.cursor()
    try:
        cursor.execute("SHOW SESSION STATUS LIKE 'Ssl_cipher'")
        cipher = cursor.fetchone()
        if not cipher or not cipher[1]:
            raise ValueError('database connection is not encrypted')
    except BaseException:
        db.close()
        raise
    finally:
        cursor.close()
    return db


def query(db, sql, values):
    cursor = db.cursor()
    try:
        cursor.execute(sql, values)
        names = [column[0] for column in cursor.description]
        return [dict(zip(names, row)) for row in cursor.fetchall()]
    finally:
        cursor.close()


def below(root, name):
    path = (root / name).resolve(strict=True)
    if not path.is_relative_to(root.resolve()) or not path.is_file():
        raise ValueError('fixture file outside allowed directory')
    return path


def file_hash(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def bounds(manifest):
    quorum, profile = manifest['min_quorum'], manifest['profile']
    return dict(min_quorum=quorum, target_nresults=quorum, max_error_results=1,
                max_total_results=quorum + 1, max_success_results=quorum + 1,
                rsc_memory_bound=profile['memory_bytes'], rsc_disk_bound=profile['disk_bytes'], delay_bound=14400)


def verify_bounds(values, manifest):
    for key, expected in bounds(manifest).items():
        if key not in values or float(values[key]) != expected:
            raise ValueError('effective workunit/template bound mismatch: ' + key)


def template_check(project, manifest):
    path = below(project / 'templates', manifest['input_template'])
    node = ET.parse(path).find('workunit')
    if node is None:
        raise ValueError('input template lacks workunit')
    for key, expected in bounds(manifest).items():
        text = node.findtext(key)
        if text is not None and float(text) != expected:
            raise ValueError('template overrides acceptance bound: ' + key)
    return file_hash(path)


def isolated_account(db, manifest, workunit=None, host_id=None, fingerprint=None):
    account = manifest['account_id']
    users = query(db, 'SELECT id, seti_id, teamid, authenticator FROM user WHERE id=%s', (account,))
    if len(users) != 1 or not users[0]['seti_id'] or users[0]['teamid']:
        raise ValueError('dedicated account must already have assignment-only scheduling and no team')
    if fingerprint is not None and not hmac.compare_digest(
            hashlib.sha256(users[0]['authenticator'].encode()).hexdigest(), fingerprint):
        raise ValueError('ephemeral authenticator does not match isolated account')
    for host in [*manifest.get('peer_host_ids', []), *([host_id] if host_id else [])]:
        hosts = query(db, 'SELECT id FROM host WHERE id=%s AND userid=%s', (host, account))
        if len(hosts) != 1:
            raise ValueError('client/peer is not on the isolated account')
    # seti_id is stock BOINC's only-run-own-jobs gate: targeted assignments are
    # served first, then ordinary and broadcast scheduling are skipped entirely.
    other = query(db, 'SELECT a.id FROM assignment a JOIN workunit w ON w.id=a.workunitid '
                  'LEFT JOIN host h ON a.target_type=1 AND h.id=a.target_id '
                  'WHERE ((a.target_type=2 AND a.target_id=%s) OR (a.target_type=1 AND h.userid=%s)) '
                  'AND w.canonical_resultid=0 AND w.error_mask=0 AND w.name!=%s LIMIT 1',
                  (account, account, workunit or ''))
    inflight = query(db, 'SELECT r.id FROM result r JOIN workunit w ON w.id=r.workunitid '
                     'WHERE r.userid=%s AND r.server_state=4 AND w.name!=%s LIMIT 1',
                     (account, workunit or ''))
    if other or inflight:
        raise ValueError('isolated account has unrelated assignments/in-progress work')


def fixture_load(root, name, release):
    manifest = json.loads(below(root, name + '.json').read_text())
    if manifest['release'] != release:
        raise ValueError('fixture release mismatch')
    if type(manifest['account_id']) is not int or manifest['account_id'] <= 0:
        raise ValueError('fixture requires a pre-isolated dedicated account')
    if not SAFE.fullmatch(manifest['workunit_name']):
        raise ValueError('invalid fixture workunit identity')
    profile = manifest['profile']
    if (type(manifest['batch']) is not int or not 0 <= manifest['batch'] < 2**31
            or any(type(profile[k]) is not int or profile[k] <= 0 for k in ('cpus', 'memory_bytes', 'disk_bytes'))
            or profile['gpu'] not in ('none', 'nvidia', 'intel', 'apple')
            or not manifest['outputs'] or not manifest['inputs']
            or type(manifest['min_quorum']) is not int or not 1 <= manifest['min_quorum'] <= 10):
        raise ValueError('invalid fixture requirements')
    peers = manifest.get('peer_host_ids', [])
    if (not isinstance(peers, list) or len(peers) != manifest['min_quorum'] - 1
            or any(type(host) is not int or host <= 0 for host in peers) or len(set(peers)) != len(peers)):
        raise ValueError('quorum requires explicit distinct existing peer_host_ids')
    for name in ('input_template', 'output_template'):
        if not SAFE.fullmatch(manifest[name]):
            raise ValueError('invalid template name')
    for entry in manifest['inputs']:
        if not SAFE.fullmatch(entry['name']) or not re.fullmatch(r'[0-9a-f]{64}', entry['sha256']):
            raise ValueError('invalid fixture input')
    for output in manifest['outputs']:
        if not SAFE.fullmatch(output['name']) or output['kind'] not in ('json', 'text', 'xml'):
            raise ValueError('invalid output contract')
        if output['kind'] == 'json' and not output.get('nonempty'):
            raise ValueError('JSON output requires nonempty inference fields')
    return manifest


def field(value, key):
    for part in key.split('.'):
        value = value[part]
    return value


def output_check(data, rule):
    if not data or len(data) > 4 * 1024 * 1024:
        raise ValueError('empty/oversized canonical output')
    if rule['kind'] == 'json':
        value = json.loads(data)
        for key, expected in rule.get('equals', {}).items():
            if field(value, key) != expected:
                raise ValueError('canonical JSON field mismatch')
        for key in rule['nonempty']:
            if not field(value, key):
                raise ValueError('canonical inference field empty')
    elif rule['kind'] == 'text':
        if not data.decode('utf-8').strip():
            raise ValueError('empty canonical transcription')
    else:
        ET.fromstring(data)


def scheduler_profile(project, manifest):
    profile = dict(manifest['profile'])
    plan = manifest['release']['plan_class']
    matches = [n for n in ET.parse(project / 'plan_class_spec.xml').getroot()
               if n.findtext('name') == plan]
    if not plan:
        return profile
    if len(matches) != 1:
        raise ValueError('missing/duplicate scheduler plan class')
    node = matches[0]
    batch = manifest['batch']
    if (type(batch) is not int or batch < int(node.findtext('min_batch', '0'))
            or batch > int(node.findtext('max_batch', str(2**31 - 1)))
            or int(node.findtext('min_wu_id', '0')) > int(node.findtext('max_wu_id', str(2**31 - 1)))):
        raise ValueError('fixture excluded by scheduler batch/workunit gate; never auto-open cohorts')
    profile['cpus'] = max(profile['cpus'], float(node.findtext('min_ncpus', '1')),
                          float(node.findtext('avg_ncpus', '1')))
    profile['memory_bytes'] = max(profile['memory_bytes'], int(float(node.findtext('mem_usage_base_mb', '0')) * 1e6))
    profile['gpu_memory_bytes'] = max(profile.get('gpu_memory_bytes', 0),
                                      int(float(node.findtext('min_gpu_ram_mb', '0')) * 1e6))
    profile['client_version'] = max(profile.get('client_version', 0),
                                    int(node.findtext('min_core_client_version', '0')))
    expected_gpu = node.findtext('gpu_type', 'none')
    if profile['gpu'] != expected_gpu:
        raise ValueError('fixture GPU differs from scheduler plan')
    for field, tag in [('cuda_version', 'min_cuda_version'), ('compute_capability', 'min_nvidia_compcap'),
                       ('opencl_version', 'min_opencl_version')]:
        profile[field] = max(profile.get(field, 0), int(node.findtext(tag, '0')))
    return profile


def create_args(manifest, workunit, host_id):
    release, profile = manifest['release'], manifest['profile']
    quorum = manifest['min_quorum']
    return ['--appname', release['app'], '--wu_name', workunit,
            '--app_version_num', str(release['version_num']),
            '--wu_template', 'templates/' + manifest['input_template'],
            '--result_template', 'templates/' + manifest['output_template'],
            '--target_host', str(host_id), '--batch', str(manifest['batch']), '--min_quorum', str(quorum),
            '--target_nresults', str(quorum), '--max_error_results', '1',
            '--max_total_results', str(quorum + 1), '--max_success_results', str(quorum + 1),
            '--rsc_fpops_est', '1e12', '--rsc_fpops_bound', '1e17',
            '--rsc_memory_bound', str(profile['memory_bytes']),
            '--rsc_disk_bound', str(profile['disk_bytes']), '--delay_bound', '14400',
            *[entry['name'] for entry in manifest['inputs']]]


def status(db, project, record, manifest):
    base = dict(run_id=record['run_id'], release=record['release'], state='pending')
    rows = query(db, 'SELECT id, canonical_resultid, error_mask, batch, assimilate_state, min_quorum, target_nresults, '
                 'max_error_results, max_total_results, max_success_results, rsc_memory_bound, rsc_disk_bound, '
                 'delay_bound FROM workunit WHERE name=%s',
                 (record['workunit'],))
    if not rows:
        return base
    wu = rows[0]
    verify_bounds(wu, manifest)
    if wu['error_mask'] or wu['batch'] != manifest['batch']:
        return dict(base, state='failed')
    clients = query(db, 'SELECT id, hostid, server_state, outcome, validate_state, app_version_id '
                    'FROM result WHERE workunitid=%s', (wu['id'],))
    # No cross-version or scheduler-only evidence can pass this gate.
    good = [r for r in clients if r['hostid'] == record['host_id'] and r['server_state'] == 5 and r['outcome'] == 1
            and r['validate_state'] == 1 and r['app_version_id'] == record['app_version_id']]
    if not good:
        if any(r['server_state'] == 5 and (r['outcome'] not in (0, 1) or r['validate_state'] == 2
                                         or r['app_version_id'] != record['app_version_id']) for r in clients):
            return dict(base, state='failed')
        return base
    approved = {record['host_id'], *manifest.get('peer_host_ids', [])}
    valid_hosts = {r['hostid'] for r in clients if r['hostid'] in approved and r['server_state'] == 5
                   and r['outcome'] == 1 and r['validate_state'] == 1
                   and r['app_version_id'] == record['app_version_id']}
    if len(valid_hosts) < manifest['min_quorum']:
        return base
    if not wu['canonical_resultid']:
        return base
    canonical = query(db, 'SELECT id, xml_doc_in, server_state, outcome, validate_state, app_version_id '
                      'FROM result WHERE id=%s AND workunitid=%s', (wu['canonical_resultid'], wu['id']))
    if (len(canonical) != 1 or canonical[0]['server_state'] != 5 or canonical[0]['outcome'] != 1
            or canonical[0]['validate_state'] != 1
            or canonical[0]['app_version_id'] != record['app_version_id']):
        return dict(base, state='failed')
    xml = canonical[0]['xml_doc_in']
    if isinstance(xml, bytes):
        xml = xml.decode()
    document = ET.fromstring('<result>' + xml + '</result>')
    refs = {}
    for ref in document.findall('.//file_ref'):
        name, physical = ref.findtext('open_name'), ref.findtext('file_name')
        if name in refs or not physical or not SAFE.fullmatch(physical):
            raise ValueError('invalid canonical output references')
        refs[name] = physical
    outputs = []
    for rule in manifest['outputs']:
        if rule['name'] not in refs:
            return dict(base, state='failed')
        physical = refs[rule['name']]
        location = subprocess.run([str(project / 'bin/dir_hier_path'), physical],
                                  cwd=project, capture_output=True, text=True, check=True, timeout=30).stdout.strip()
        path = Path(location)
        path = path if path.is_absolute() else project / path
        download = (project / 'download').resolve()
        if not path.resolve().is_relative_to(download):
            raise ValueError('BOINC hierarchy path outside download directory')
        path = project / 'upload' / path.resolve().relative_to(download)
        candidates = [(project / 'upload', path)]
        if wu['assimilate_state'] == 2:
            collected = project / 'results' / str(manifest['batch']) / (record['workunit'] + '__file_' + rule['name'])
            candidates.append((project / 'results', collected))
        data = None
        for root, candidate in candidates:
            try:
                candidate = below(root, str(candidate.resolve()))
                if candidate.stat().st_size > 4 * 1024 * 1024:
                    return dict(base, state='failed')
                data = candidate.read_bytes()
                break
            except FileNotFoundError:
                # The assimilator can move the file between lookup and open.
                continue
        if data is None:
            return base
        output_check(data, rule)
        outputs.append(dict(name=rule['name'], bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                            base64=base64.b64encode(data).decode()))
    return dict(base, state='validated', canonical_result_id=wu['canonical_resultid'],
                host_id=record['host_id'], client_validate_state=1, client_outcome=1,
                client_server_state=5, outputs=outputs)


def handle(request, project, fixtures, db):
    name, run_id, release = request['fixture'], request['run_id'], request['release']
    if not SAFE.fullmatch(name) or not re.fullmatch(r'[0-9a-f]{32}', run_id):
        raise ValueError('invalid reservation identity')
    journal = fixtures / (name + '.reservation.json')
    with (fixtures / (name + '.lock')).open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        record = json.loads(journal.read_text()) if journal.exists() else None
        action = request['action']
        if action == 'release':
            if record and record['run_id'] == run_id:
                record['released'] = True
                durable(journal, record)
            # Never release another run after an uncertain reserve.
            return {'released': record is None or record['run_id'] == run_id}
        manifest = fixture_load(fixtures, name, release)
        if action == 'reserve':
            if record and not record.get('released'):
                raise ValueError('fixture already reserved; reconcile its journal before recovery')
            if ET.parse(project / 'config.xml').findtext('./config/enable_assignment') != '1':
                raise ValueError('host-target assignment must already be enabled')
            isolated_account(db, manifest, fingerprint=request.get('authenticator_sha256'))
            input_hash = template_check(project, manifest)
            profile = scheduler_profile(project, manifest)
            versions = query(db, 'SELECT av.id FROM app_version av JOIN app a ON a.id=av.appid '
                             'JOIN platform p ON p.id=av.platformid WHERE a.name=%s AND av.version_num=%s '
                             'AND p.name=%s AND av.plan_class=%s AND av.deprecated=0',
                             (release['app'], release['version_num'], release['platform'], release['plan_class']))
            if len(versions) != 1:
                raise ValueError('expected exactly one active registered app version')
            alternatives = query(db, 'SELECT av.id FROM app_version av JOIN app a ON a.id=av.appid '
                                 'WHERE a.name=%s AND av.version_num=%s AND av.deprecated=0',
                                 (release['app'], release['version_num']))
            if len(alternatives) != 1 or alternatives[0]['id'] != versions[0]['id']:
                raise ValueError('ambiguous app/version routing; use distinct numeric versions per platform/plan')
            for peer in manifest.get('peer_host_ids', []):
                if len(query(db, 'SELECT id FROM host WHERE id=%s', (peer,))) != 1:
                    raise ValueError('unknown independent fixture peer')
            for key in ('input_template', 'output_template'):
                below(project / 'templates', manifest[key])
            for entry in manifest['inputs']:
                location = subprocess.run([str(project / 'bin/dir_hier_path'), entry['name']], cwd=project,
                                          check=True, capture_output=True, text=True, timeout=30).stdout.strip()
                path = Path(location)
                path = path if path.is_absolute() else project / path
                if file_hash(below(project / 'download', str(path.resolve()))) != entry['sha256']:
                    raise ValueError('fixture input changed')
            if query(db, 'SELECT id FROM workunit WHERE name=%s', (manifest['workunit_name'],)):
                raise ValueError('fixture workunit already exists; stage a fresh fixture identity')
            record = dict(run_id=run_id, release=release, app_version_id=versions[0]['id'],
                          workunit=manifest['workunit_name'], released=False, input_template_sha256=input_hash,
                          manifest_sha256=hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest())
            durable(journal, record)
            return dict(run_id=run_id, release=release, profile=profile, workunit=record['workunit'], isolation_verified=True)
        if not record or record['run_id'] != run_id or record.get('released') or record['release'] != release:
            raise ValueError('reservation not owned by this run')
        if record['manifest_sha256'] != hashlib.sha256(json.dumps(manifest, sort_keys=True).encode()).hexdigest():
            raise ValueError('fixture changed during reservation')
        isolated_account(db, manifest, workunit=record['workunit'], host_id=request.get('host_id') or record.get('host_id'))
        if action == 'dispatch':
            if template_check(project, manifest) != record['input_template_sha256']:
                raise ValueError('input template changed after reservation')
            host_id = request['host_id']
            if (type(host_id) is not int or host_id <= 0 or record.get('dispatch_intent')
                    or host_id in manifest.get('peer_host_ids', [])):
                raise ValueError('invalid host or uncertain/duplicate dispatch')
            if ET.parse(project / 'config.xml').findtext('./config/enable_assignment') != '1':
                raise ValueError('host-target assignment must already be enabled')
            if len(query(db, 'SELECT id FROM host WHERE id=%s', (host_id,))) != 1:
                raise ValueError('unknown client host')
            db.commit()  # End the host-lookup snapshot before external create_work commits.
            record.update(host_id=host_id, dispatch_intent=True)
            durable(journal, record)  # Persist before mutation: do not retry unknown outcomes.
            subprocess.run([str(project / 'bin/create_work'), *create_args(manifest, record['workunit'], host_id)],
                           cwd=project, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
            units = query(db, 'SELECT id, min_quorum, target_nresults, max_error_results, max_total_results, '
                          'max_success_results, rsc_memory_bound, rsc_disk_bound, delay_bound FROM workunit WHERE name=%s',
                          (record['workunit'],))
            if len(units) != 1:
                raise ValueError('uncertain fixture workunit creation')
            verify_bounds(units[0], manifest)
            if manifest.get('peer_host_ids'):
                cursor = db.cursor()
                try:
                    for peer in manifest['peer_host_ids']:
                        # BOINC ASSIGN_HOST=1; never assign to an entire account/team.
                        cursor.execute('INSERT INTO assignment (create_time, target_id, target_type, multi, workunitid) '
                                       'VALUES (UNIX_TIMESTAMP(), %s, 1, 0, %s)', (peer, units[0]['id']))
                    db.commit()
                finally:
                    cursor.close()
            record['dispatched'] = True
            durable(journal, record)
            return {'dispatched': True}
        if action == 'status' and record.get('dispatched'):
            return status(db, project, record, manifest)
        raise ValueError('invalid acceptance operation')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--project', required=True, type=Path)
    parser.add_argument('--fixtures', required=True, type=Path)
    args = parser.parse_args()
    db = None
    try:
        request = json.loads(sys.stdin.read(65536))
        db = db_connect(args.project)
        print(json.dumps(handle(request, args.project, args.fixtures, db)))
    except Exception:
        # MySQL connection exceptions can expose usernames, hosts, or credentials.
        print('acceptance server operation failed; inspect private reservation journal', file=sys.stderr)
        return 1
    finally:
        if db:
            db.close()
    return 0


if __name__ == '__main__':
    sys.exit(main())
