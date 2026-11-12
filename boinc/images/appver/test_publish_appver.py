"""Exercise the publish transaction on a temporary share, without a database."""
import json
import os
import importlib.util
from pathlib import Path
import subprocess
import shutil
import tempfile
import unittest
import unittest.mock


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / 'project'
        (self.project / 'bin').mkdir(parents=True)
        (self.project / 'html/inc').mkdir(parents=True)
        (self.project / 'config.xml').write_text('<config/>')
        (self.project / '.usnm-initialised').touch()
        for name in ('xadd', 'update_versions'):
            command = self.project / 'bin' / name
            command.write_text(f'#!/bin/sh\nprintf "%s\\n" "$0" >> "{self.project}/calls"\n')
            command.chmod(0o755)
        php = self.root / 'php'
        php.write_text('#!/bin/sh\ncat >/dev/null\nexit "${STUB_VERIFY_STATUS:-0}"\n')
        php.chmod(0o755)
        self.source = self.root / 'appver'
        (self.source / 'templates').mkdir(parents=True)
        (self.source / 'templates' / 'usnm_ocr_1.0.xml').write_text('template')
        (self.source / 'project.xml').write_text('<project/>')
        lib = self.root / 'lib.sh'
        lib.write_text('SECRETS="$PROJ/local-secrets"\nDB_RUNTIME_USER=fixture\nexport DB_RUNTIME_USER\nfail() { echo "$*" >&2; exit 1; }\nlog() { :; }\nfetch_secrets() { mkdir -p "$SECRETS/keys"; printf fixture > "$SECRETS/db-runtime"; }\nrender_config() { :; }\n')
        script = Path(__file__).with_name('publish-appver.sh').read_text()
        script = script.replace('. /usr/local/lib/usnm-boinc/lib.sh', f'. "{lib}"')
        script = script.replace('src=/usr/local/lib/usnm-boinc/appver', f'src="{self.source}"')
        # register-apps.php has its own tests (images/server/test_register_apps.py); here it's xadd.
        script = script.replace('php /usr/local/lib/usnm-boinc/register-apps.php', 'bin/xadd')
        self.script = self.root / 'publish.sh'
        self.script.write_text(script)

    def publish(self, platform, payload='wrapper', version='1.0'):
        src = self.source / f'apps/usnm_ocr/{version}' / f'{platform}__docker'
        src.mkdir(parents=True, exist_ok=True)
        (src / 'docker_wrapper').write_text(payload)
        return subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                              env={**os.environ, 'PROJ': str(self.project), 'APPVER_APP': 'usnm_ocr',
                                   'APPVER_VERSION': version, 'APPVER_PLATFORM': platform,
                                   'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                                   'PATH': str(self.root) + os.pathsep + os.environ['PATH']})

    def test_registration_sets_a_new_apps_declared_weight(self):
        # Plain xadd would register an app missing from the database, such as usnm_gold_native,
        # at weight 1. The release context carries the wrapper, so an older server image works.
        here = Path(__file__).resolve().parent
        script = (here / 'publish-appver.sh').read_text()
        self.assertIn('cp "$src/project.xml" project.xml && php /usr/local/lib/usnm-boinc/register-apps.php)', script)
        self.assertNotIn('&& bin/xadd', script)
        self.assertIn('COPY register-apps.php /usr/local/lib/usnm-boinc/register-apps.php',
                      (here / 'Containerfile').read_text())
        release = (here.parents[2] / 'boinc/scripts/boinc-app-release.sh').read_text()
        self.assertIn('git show "HEAD:boinc/images/server/register-apps.php" > "$ctx/register-apps.php"', release)

    def test_sibling_platform_publish_and_replay_preserve_linux(self):
        self.assertEqual(self.publish('x86_64-pc-linux-gnu', 'linux').returncode, 0)
        linux = self.project / 'apps/usnm_ocr/1.0/x86_64-pc-linux-gnu__docker/docker_wrapper'
        template = self.project / 'templates/usnm_ocr_1.0.xml'
        os.utime(template, (1_000_000_000, 1_000_000_000))
        before = linux.stat().st_mtime_ns
        template_before = (template.stat().st_ino, template.stat().st_mtime_ns)
        self.assertEqual(self.publish('arm64-apple-darwin', 'mac').returncode, 0)
        self.assertEqual(self.publish('arm64-apple-darwin', 'mac').returncode, 0)
        self.assertEqual(linux.read_text(), 'linux')
        self.assertEqual(linux.stat().st_mtime_ns, before)
        self.assertEqual((template.stat().st_ino, template.stat().st_mtime_ns), template_before)
        self.assertEqual(len((self.project / 'calls').read_text().splitlines()), 6)
        self.assertFalse((self.project / '.usnm-publish.lock').exists())

    def test_scoped_publication_preserves_live_configuration_and_app_registry(self):
        (self.project / 'html/inc/boinc_db.inc').write_text('existing database configuration')
        (self.project / 'config.xml').unlink()
        (self.project / 'config.xml').symlink_to('/dev/shm/absent-web-config.xml')
        (self.project / 'keys').symlink_to('/dev/shm/absent-web-keys')
        before = os.readlink(self.project / 'config.xml')
        with unittest.mock.patch.dict(os.environ, {'APPVER_REUSE_PROJECT': '1'}):
            result = self.publish('x86_64-pc-linux-gnu')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(os.readlink(self.project / 'config.xml'), before)
        self.assertEqual(os.readlink(self.project / 'keys'), '/dev/shm/absent-web-keys')
        self.assertTrue((self.project / 'local-secrets/appver-config.xml').is_file())
        calls = (self.project / 'calls').read_text().splitlines()
        self.assertEqual(len(calls), 1)
        self.assertTrue(calls[0].endswith('update_versions'))
        self.assertFalse((self.project / 'project.xml').exists())

    def test_registration_workspace_is_private_secrets_local_and_removed(self):
        update = self.project / 'bin/update_versions'
        update.write_text('#!/bin/sh\npwd > "$PROJ/registration-workspace"\n'
                          'python3 -c \'import os; assert os.stat(".").st_mode & 0o777 == 0o700\'\n')
        forbidden = self.root / 'mktemp'
        forbidden.write_text('#!/bin/sh\nexit 91\n')
        forbidden.chmod(0o755)
        with unittest.mock.patch.dict(os.environ, {'TMPDIR': str(self.root / 'absent')}):
            result = self.publish('x86_64-pc-linux-gnu')
        self.assertEqual(result.returncode, 0, result.stderr)
        workspace = Path((self.project / 'registration-workspace').read_text().strip())
        self.assertEqual(workspace.parent, self.project / 'local-secrets')
        self.assertTrue(workspace.name.startswith('appver-registration.'))
        self.assertFalse(workspace.exists())
        self.assertFalse(list((self.project / 'local-secrets').glob('appver-registration.*')))

    def test_registration_refuses_fixed_share_modes_before_executing_php(self):
        (self.project / 'html/inc/boinc_db.inc').write_text('existing configuration')
        script = self.script.read_text().replace(
            'path.mkdir(mode=0o700)',
            'path.mkdir(mode=0o700)\npath.chmod(0o775)',
        )
        self.script.write_text(script)
        with unittest.mock.patch.dict(os.environ, {'APPVER_REUSE_PROJECT': '1'}):
            result = self.publish('x86_64-pc-linux-gnu')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('must enforce private owner-only permissions', result.stderr)
        self.assertFalse((self.project / 'calls').exists())
        self.assertFalse(list((self.project / 'local-secrets').glob('appver-registration.*')))

    def test_scoped_ownership_does_not_walk_live_download_tree(self):
        (self.project / 'html/inc/boinc_db.inc').write_text('existing configuration')
        chown = self.root / 'chown'
        chown.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$PROJ/ownership-args"\n')
        chown.chmod(0o755)
        with unittest.mock.patch.dict(os.environ, {'APPVER_REUSE_PROJECT': '1'}):
            result = self.publish('x86_64-pc-linux-gnu')
        self.assertEqual(result.returncode, 0, result.stderr)
        args = (self.project / 'ownership-args').read_text().splitlines()
        self.assertIn(str(self.project / 'templates'), args)
        self.assertNotIn(str(self.project / 'download'), args)
        result = self.publish('x86_64-pc-linux-gnu')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(str(self.project / 'download'),
                      (self.project / 'ownership-args').read_text().splitlines())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_all_container_classes_publish_and_pass_actual_php_verification(self):
        # Execute the production PHP allowlist/lookup, not the usual no-op PHP stub.
        php = shutil.which('php')
        (self.root / 'php').write_text(f'#!/bin/sh\nexec "{php}" "$@"\n')
        (self.project / 'html/inc/boinc_db.inc').write_text('''<?php
class BoincApp {
    static function lookup($query) { return (object)["id" => 1]; }
}
class BoincPlatform {
    static function lookup($query) { return (object)["id" => 2]; }
}
class BoincAppVersion {
    static function enum($query) {
        $plan = getenv("APPVER_PLAN_CLASS");
        if (strpos($query, "plan_class='$plan'") === false) { exit(9); }
        return [(object)["id" => 3]];
    }
}
''')
        repo = Path(__file__).resolve().parents[3]
        spec = importlib.util.spec_from_file_location('boinc_install', repo / 'boinc/scripts/install.py')
        install = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(install)
        for app, plan in install.JOB_TOML:
            with self.subTest(app=app, plan=plan):
                subdir = f'x86_64-pc-linux-gnu__{plan}'
                src = self.source / f'apps/{app}/1.0/{subdir}'
                src.mkdir(parents=True)
                (src / 'docker_wrapper').write_text(f'{app}-{plan}')
                result = subprocess.run(
                    ['bash', str(self.script)], capture_output=True, text=True,
                    env={**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'container',
                         'APPVER_APP': app, 'APPVER_VERSION': '1.0',
                         'APPVER_PLATFORM': 'x86_64-pc-linux-gnu', 'APPVER_PLAN_CLASS': plan,
                         'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                                   'PATH': str(self.root) + os.pathsep + os.environ['PATH']},
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f'Verified {app} 1.0 x86_64-pc-linux-gnu {plan}', result.stdout)
                self.assertTrue((self.project / f'apps/{app}/1.0/{subdir}/docker_wrapper').is_file())

    def test_line_class_publishes_on_the_docker_platforms(self):
        # docker_lines (#1621) admits the docker hosts, Macs included.
        for platform in ('arm64-apple-darwin', 'x86_64-apple-darwin'):
            with self.subTest(platform=platform):
                src = self.source / f'apps/usnm_ocr/1.8/{platform}__docker_lines'
                src.mkdir(parents=True)
                (src / 'docker_wrapper').write_text(f'lines-{platform}')
                result = subprocess.run(
                    ['bash', str(self.script)], capture_output=True, text=True,
                    env={**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'container',
                         'APPVER_APP': 'usnm_ocr', 'APPVER_VERSION': '1.8', 'APPVER_PLATFORM': platform,
                         'APPVER_PLAN_CLASS': 'docker_lines',
                         'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                         'PATH': str(self.root) + os.pathsep + os.environ['PATH']},
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertTrue((self.project / f'apps/usnm_ocr/1.8/{platform}__docker_lines/docker_wrapper').is_file())

    INSERT_QUERY = '$query = "set create_time=$now, xml_doc=\'$xml\', plan_class=\'$plan_class\', beta=$b";'

    def held_fixture(self, inserts=1):
        """A real-PHP verification over a stub update_versions that, as the stock one, holds one
        app_version INSERT; running it records the query it would send."""
        php = shutil.which('php')
        # The php on PATH logs how it was called: a held copy must run through it (#1689 rerun,
        # 10 Nov), because the registration directory can be on a noexec mount.
        (self.root / 'php').write_text(f'#!/bin/sh\nprintf "%s\\n" "$*" >> "{self.root}/php-calls"\nexec "{php}" "$@"\n')
        (self.root / 'php').chmod(0o755)
        update = self.project / 'bin/update_versions'
        update.write_text('#! /usr/bin/env php\n<?php\n// ' + self.INSERT_QUERY + '\n'
                          + '// BoincAppVersion::insert($query);\n' * inserts
                          + 'preg_match("/beta=.*/", file_get_contents(__FILE__), $m);\n'
                          + 'file_put_contents(getenv("PROJ") . "/inserted", $m[0] . "\\n");\n')
        update.chmod(0o755)
        (self.project / 'html/inc/boinc_db.inc').write_text(
            '<?php\n'
            'class BoincApp { static function lookup($q) { return (object)["id" => 1]; } }\n'
            'class BoincPlatform { static function lookup($q) { return (object)["id" => 1]; } }\n'
            'class BoincAppVersion {\n'
            '    static function enum($q) {\n'
            '        $inserted = @file_get_contents(getenv("PROJ") . "/inserted");\n'
            '        return [(object)["id" => 86, "deprecated" => str_contains((string)$inserted, "deprecated=1") ? 1 : 0]];\n'
            '    }\n'
            '}\n')
        src = self.source / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker'
        src.mkdir(parents=True, exist_ok=True)
        (src / 'docker_wrapper').write_text('wrapper')
        return {**os.environ, 'PROJ': str(self.project), 'APPVER_APP': 'usnm_ocr', 'APPVER_VERSION': '1.7',
                'APPVER_PLATFORM': 'x86_64-pc-linux-gnu', 'APPVER_HOLD': '1',
                'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                'PATH': str(self.root) + os.pathsep + os.environ['PATH']}

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_docker_lines_version_publishes_held(self):
        # #1621: the line-job versions go out held too, released with app-version-hold.php.
        env = self.held_fixture()
        src = self.source / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker_lines'
        src.mkdir(parents=True)
        (src / 'docker_wrapper').write_text('wrapper')
        result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                                env={**env, 'APPVER_PLAN_CLASS': 'docker_lines'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.project / 'inserted').read_text().strip(), 'beta=$b, deprecated=1";')
        self.assertTrue((self.project / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker_lines/docker_wrapper').is_file())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_held_publication_works_when_its_copy_cannot_be_executed(self):
        # 10 Nov: the held copy's directory was on a noexec mount, so executing it failed with
        # status 126 and nothing registered. A copy without the execute bit stands in for noexec.
        env = self.held_fixture()
        script = self.script.read_text().replace(
            'fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o700)',
            'fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)')
        self.assertIn('0o600', script)
        noexec = self.root / 'publish-noexec.sh'
        noexec.write_text(script)
        result = subprocess.run(['bash', str(noexec)], capture_output=True, text=True, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.project / 'inserted').read_text().strip(), 'beta=$b, deprecated=1";')

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_held_publication_refuses_an_update_versions_that_isnt_php(self):
        env = self.held_fixture()
        update = self.project / 'bin/update_versions'
        update.write_text(update.read_text().replace('#! /usr/bin/env php', '#!/bin/sh', 1))
        result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("isn't a PHP script", result.stderr)
        self.assertFalse((self.project / 'inserted').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_held_publication_inserts_the_row_deprecated(self):
        # #1687: the row is born held, in update_versions' own INSERT, so no feeder reload between
        # registration and a later UPDATE can load it, and a failure after the insert leaves it held.
        env = self.held_fixture()
        result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.project / 'inserted').read_text().strip(), 'beta=$b, deprecated=1";')
        self.assertTrue((self.project / 'reread_db').exists())
        journal = list((self.project / 'ops-app-versions').glob('publish-held-usnm_ocr-1.7-x86_64-pc-linux-gnu-*.json'))
        self.assertEqual(len(journal), 1)
        self.assertEqual(json.loads(journal[0].read_text())['deprecated'], 1)
        # The stock script is untouched: the patched copy lived in the private registration area.
        self.assertNotIn('deprecated=1', (self.project / 'bin/update_versions').read_text())
        # It ran through php, not as an executable: /dev/shm in the server container is noexec.
        calls = (self.root / 'php-calls').read_text().splitlines()
        self.assertTrue(any(c.endswith('/update_versions.held --noconfirm') for c in calls), calls)
        # Without the hold the stock INSERT runs.
        (self.project / 'reread_db').unlink()
        live = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env={**env, 'APPVER_HOLD': '0'})
        self.assertEqual(live.returncode, 0, live.stderr)
        self.assertEqual((self.project / 'inserted').read_text().strip(), 'beta=$b";')
        # A held rerun that finds the version registered live refuses and doesn't notify the feeder.
        (self.project / 'reread_db').unlink()
        update = self.project / 'bin/update_versions'
        update.write_text(update.read_text().replace('file_put_contents(', '0 && file_put_contents('))
        rerun = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
        self.assertNotEqual(rerun.returncode, 0)
        self.assertIn('is live, not held', rerun.stderr)
        self.assertFalse((self.project / 'reread_db').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_hold_refuses_an_update_versions_it_cannot_patch(self):
        for inserts in (0, 2):
            with self.subTest(inserts=inserts):
                env = self.held_fixture(inserts=inserts)
                result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('nothing registered', result.stderr)
                self.assertFalse((self.project / 'inserted').exists())
                self.assertFalse((self.project / 'reread_db').exists())

    def test_only_ocr_and_gold_container_versions_can_be_held(self):
        src = self.source / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker'
        src.mkdir(parents=True)
        (src / 'docker_wrapper').write_text('wrapper')
        env = {**os.environ, 'PROJ': str(self.project), 'APPVER_VERSION': '1.7',
               'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
               'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
        for app, value in (('usnm_quality', '1'), ('usnm_ocr', '2')):
            bad = subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                                 env={**env, 'APPVER_APP': app, 'APPVER_HOLD': value})
            self.assertNotEqual(bad.returncode, 0)
            self.assertIn('APPVER_HOLD', bad.stderr)
        self.assertIn('APPVER_HOLD=${HOLD}', (Path(__file__).resolve().parent / 'Containerfile').read_text())
        # Native Qwen passes the hold gate (#1672); a native class-less version doesn't.
        native = {**env, 'APPVER_KIND': 'native', 'APPVER_APP': 'usnm_qwen', 'APPVER_HOLD': '1',
                  'APPVER_PLATFORM': 'windows_x86_64'}
        for plan in ('nvidia_vulkan_8b', 'nvidia_vulkan_6gb_8b'):
            passed = subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                                    env={**native, 'APPVER_PLAN_CLASS': plan})
            self.assertNotIn('APPVER_HOLD is only', passed.stderr)
            self.assertNotIn('APPVER_HOLD must', passed.stderr)
            self.assertNotIn('is not supported on', passed.stderr)

    def test_container_publication_rejects_wrong_or_legacy_model_classes(self):
        for app, plan, platform in (
            ('usnm_qwen', 'docker_mt', 'x86_64-pc-linux-gnu'),
            ('usnm_qwen', 'docker_nvidia_cuda', 'x86_64-pc-linux-gnu'),
            ('usnm_surya', 'docker_mt_2b', 'x86_64-pc-linux-gnu'),
            ('usnm_ocr', 'docker_nvidia_cuda_8b', 'x86_64-pc-linux-gnu'),
            ('usnm_qwen', 'docker_mt_2b', 'aarch64-unknown-linux-gnu'),
            ('usnm_qwen', 'docker_nvidia_cuda_8b', 'windows_x86_64'),
            ('usnm_quality', 'docker_lines', 'x86_64-pc-linux-gnu'),
            ('usnm_ocr', 'docker_mt', 'arm64-apple-darwin'),
        ):
            with self.subTest(app=app, plan=plan, platform=platform):
                result = subprocess.run(
                    ['bash', str(self.script)], capture_output=True, text=True,
                    env={**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'container',
                         'APPVER_APP': app, 'APPVER_VERSION': '1.0', 'APPVER_PLATFORM': platform,
                         'APPVER_PLAN_CLASS': plan,
                         'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                                   'PATH': str(self.root) + os.pathsep + os.environ['PATH']},
                )
                self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.project / 'calls').exists())

    def test_changed_platform_or_template_is_refused_before_registration(self):
        self.assertEqual(self.publish('x86_64-pc-linux-gnu').returncode, 0)
        calls = (self.project / 'calls').read_bytes()
        self.assertNotEqual(self.publish('x86_64-pc-linux-gnu', 'changed').returncode, 0)
        (self.source / 'templates' / 'usnm_ocr_1.0.xml').write_text('changed template')
        self.assertNotEqual(self.publish('arm64-apple-darwin').returncode, 0)
        self.assertFalse((self.project / 'apps/usnm_ocr/1.0/arm64-apple-darwin__docker').exists())
        self.assertEqual((self.project / 'calls').read_bytes(), calls)
        self.assertFalse((self.project / '.usnm-publish.lock').exists())

    def test_new_version_atomically_updates_changed_template(self):
        self.assertEqual(self.publish('x86_64-pc-linux-gnu', 'v1.0', version='1.0').returncode, 0)
        template = self.project / 'templates/usnm_ocr_1.0.xml'
        old_ino = template.stat().st_ino
        (self.source / 'templates' / 'usnm_ocr_1.0.xml').write_text('v1.1 template')
        self.assertEqual(self.publish('x86_64-pc-linux-gnu', 'v1.1', version='1.1').returncode, 0)
        self.assertEqual(template.read_text(), 'v1.1 template')
        self.assertNotEqual(template.stat().st_ino, old_ino)
        (self.source / 'templates' / 'usnm_ocr_1.0.xml').write_text('drifted sibling template')
        self.assertNotEqual(self.publish('arm64-apple-darwin', 'v1.1-mac', version='1.1').returncode, 0)

    def test_native_quality_and_qwen_publish_and_new_platforms(self):
        path_env = str(self.root) + os.pathsep + os.environ['PATH']
        for platform in ('aarch64-unknown-linux-gnu', 'windows_x86_64'):
            self.assertEqual(self.publish(platform, f'payload-{platform}').returncode, 0)
            self.assertTrue((self.project / f'apps/usnm_ocr/1.0/{platform}__docker/docker_wrapper').is_file())

        qual_src = self.source / 'apps/usnm_quality/1.0/windows_x86_64'
        qual_src.mkdir(parents=True, exist_ok=True)
        (qual_src / 'usnm_quality_1.0_windows_x86_64.exe').write_text('win-bin')
        res = subprocess.run(
            ['bash', str(self.script)],
            capture_output=True,
            text=True,
            env={
                **os.environ,
                'PROJ': str(self.project),
                'APPVER_KIND': 'native',
                'APPVER_APP': 'usnm_quality',
                'APPVER_VERSION': '1.0',
                'APPVER_PLATFORM': 'windows_x86_64',
                'APPVER_PLAN_CLASS': '',
                'PATH': path_env,
            },
        )
        self.assertEqual(res.returncode, 0, res.stderr)
        self.assertTrue((self.project / 'apps/usnm_quality/1.0/windows_x86_64/usnm_quality_1.0_windows_x86_64.exe').is_file())

        qwen_src = self.source / 'apps/usnm_qwen/1.0/arm64-apple-darwin__apple_gpu_8b'
        qwen_src.mkdir(parents=True, exist_ok=True)
        (qwen_src / 'usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b').write_text('mac-gpu')
        res_qwen = subprocess.run(
            ['bash', str(self.script)],
            capture_output=True,
            text=True,
            env={
                **os.environ,
                'PROJ': str(self.project),
                'APPVER_KIND': 'native',
                'APPVER_APP': 'usnm_qwen',
                'APPVER_VERSION': '1.0',
                'APPVER_PLATFORM': 'arm64-apple-darwin',
                'APPVER_PLAN_CLASS': 'apple_gpu_8b',
                'PATH': path_env,
            },
        )
        self.assertEqual(res_qwen.returncode, 0, res_qwen.stderr)
        self.assertTrue((self.project / 'apps/usnm_qwen/1.0/arm64-apple-darwin__apple_gpu_8b/usnm_qwen_1.0_arm64-apple-darwin_apple_gpu_8b').is_file())

        bad_res = subprocess.run(
            ['bash', str(self.script)],
            capture_output=True,
            text=True,
            env={
                **os.environ,
                'PROJ': str(self.project),
                'APPVER_KIND': 'native',
                'APPVER_APP': 'usnm_qwen',
                'APPVER_VERSION': '1.0',
                'APPVER_PLATFORM': 'x86_64-pc-linux-gnu',
                'APPVER_PLAN_CLASS': 'apple_gpu_8b',
                'PATH': path_env,
            },
        )
        self.assertNotEqual(bad_res.returncode, 0)

    def test_isolates_requested_platform_and_verifies_even_after_zero_exit(self):
        command = self.project / 'bin/update_versions'
        command.write_text(
            '#!/bin/sh\n'
            'test "$(find apps -type l | wc -l | tr -d " ")" = 1 || exit 9\n'
            'test -L "apps/usnm_ocr/$APPVER_VERSION/${APPVER_PLATFORM}__docker" || exit 9\n'
        )
        self.assertEqual(self.publish('x86_64-pc-linux-gnu').returncode, 0)
        self.assertTrue((self.project / 'reread_db').exists())
        (self.project / 'reread_db').unlink()
        php = self.root / 'php'
        php.write_text('#!/bin/sh\ncat >/dev/null\nexit 1\n')
        result = self.publish('x86_64-pc-linux-gnu')
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.project / 'reread_db').exists())
        self.assertFalse((self.project / '.usnm-publish.lock').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_native_amd_publication_platforms_and_verification_contract(self):
        for platform in ('x86_64-pc-linux-gnu', 'windows_x86_64',
                         'aarch64-unknown-linux-gnu', 'arm64-apple-darwin', 'x86_64-apple-darwin'):
            with self.subTest(platform=platform):
                subdir = f'{platform}__amd_vulkan_8b'
                src = self.source / f'apps/usnm_qwen/1.0/{subdir}'
                src.mkdir(parents=True)
                (src / 'usnm_qwen').write_text('native-fixture')
                # PHP checks the production verifier's allowlists without a real database.
                (self.project / 'html/inc/boinc_db.inc').write_text('''<?php
class BoincApp { static function lookup($query) { return (object)["id"=>1]; } }
class BoincPlatform { static function lookup($query) { return (object)["id"=>2]; } }
class BoincAppVersion { static function enum($query) {
    if (strpos($query, "plan_class='amd_vulkan_8b'") === false) exit(9);
    return [(object)["id"=>3]];
} }
''')
                env = {**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'native',
                       'APPVER_APP': 'usnm_qwen', 'APPVER_VERSION': '1.0',
                       'APPVER_PLATFORM': platform, 'APPVER_PLAN_CLASS': 'amd_vulkan_8b',
                       'DB_HOST': 'fixture', 'DB_NAME': 'fixture', 'MASTER_HOST': 'fixture',
                       'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
                # Use real PHP for the database readback allowlist, not the default stub.
                (self.root / 'php').unlink(missing_ok=True)
                result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                if platform in ('x86_64-pc-linux-gnu', 'windows_x86_64'):
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn(f'Verified usnm_qwen 1.0 {platform} amd_vulkan_8b', result.stdout)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('is not supported on', result.stderr)
                    self.assertFalse((self.project / f'apps/usnm_qwen/1.0/{subdir}').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_native_nvidia_vulkan_8b_classes_publish_on_linux_and_windows_x86_64(self):
        # nvidia_vulkan_8b and the 6 GB partial-offload class share the Windows Vulkan bundle (#1672).
        (self.root / 'php').unlink(missing_ok=True)
        for plan in ('nvidia_vulkan_8b', 'nvidia_vulkan_6gb_8b'):
            (self.project / 'html/inc/boinc_db.inc').write_text(f'''<?php
class BoincApp {{ static function lookup($query) {{ return (object)["id"=>1]; }} }}
class BoincPlatform {{ static function lookup($query) {{ return (object)["id"=>2]; }} }}
class BoincAppVersion {{ static function enum($query) {{
    if (strpos($query, "plan_class='{plan}'") === false) exit(9);
    return [(object)["id"=>3]];
}} }}
''')
            for platform in ('x86_64-pc-linux-gnu', 'windows_x86_64',
                             'aarch64-unknown-linux-gnu', 'arm64-apple-darwin', 'x86_64-apple-darwin'):
                with self.subTest(plan=plan, platform=platform):
                    subdir = f'{platform}__{plan}'
                    src = self.source / f'apps/usnm_qwen/1.0/{subdir}'
                    src.mkdir(parents=True)
                    (src / 'usnm_qwen').write_text('native-fixture')
                    env = {**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'native',
                           'APPVER_APP': 'usnm_qwen', 'APPVER_VERSION': '1.0',
                           'APPVER_PLATFORM': platform, 'APPVER_PLAN_CLASS': plan,
                           'DB_HOST': 'fixture', 'DB_NAME': 'fixture', 'MASTER_HOST': 'fixture',
                           'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
                    result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                    if platform in ('x86_64-pc-linux-gnu', 'windows_x86_64'):
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn(f'Verified usnm_qwen 1.0 {platform} {plan}', result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('is not supported on', result.stderr)
                        self.assertFalse((self.project / f'apps/usnm_qwen/1.0/{subdir}').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_native_xiaomi_classes_publish_where_their_qwen_classes_do(self):
        # #1776: mt_x08 on every platform, the GPU classes on their 8B classes' platforms.
        (self.root / 'php').unlink(missing_ok=True)
        platforms = ('x86_64-pc-linux-gnu', 'windows_x86_64', 'aarch64-unknown-linux-gnu',
                     'arm64-apple-darwin', 'x86_64-apple-darwin')
        allowed = {
            'mt_x08': set(platforms),
            'apple_gpu_x08': {'arm64-apple-darwin'},
            'nvidia_vulkan_x08': {'x86_64-pc-linux-gnu', 'windows_x86_64'},
            'intel_vulkan_x08': {'x86_64-pc-linux-gnu', 'windows_x86_64'},
            'amd_vulkan_x08': {'x86_64-pc-linux-gnu', 'windows_x86_64'},
        }
        for plan, ok in allowed.items():
            (self.project / 'html/inc/boinc_db.inc').write_text(f'''<?php
class BoincApp {{ static function lookup($query) {{ return (object)["id"=>1]; }} }}
class BoincPlatform {{ static function lookup($query) {{ return (object)["id"=>2]; }} }}
class BoincAppVersion {{ static function enum($query) {{
    if (strpos($query, "plan_class='{plan}'") === false) exit(9);
    return [(object)["id"=>3]];
}} }}
''')
            for platform in platforms:
                with self.subTest(plan=plan, platform=platform):
                    subdir = f'{platform}__{plan}'
                    src = self.source / f'apps/usnm_qwen/1.0/{subdir}'
                    src.mkdir(parents=True)
                    (src / 'usnm_qwen').write_text('native-fixture')
                    env = {**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'native',
                           'APPVER_APP': 'usnm_qwen', 'APPVER_VERSION': '1.0',
                           'APPVER_PLATFORM': platform, 'APPVER_PLAN_CLASS': plan,
                           'DB_HOST': 'fixture', 'DB_NAME': 'fixture', 'MASTER_HOST': 'fixture',
                           'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
                    result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                    if platform in ok:
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn(f'Verified usnm_qwen 1.0 {platform} {plan}', result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('is not supported on', result.stderr)
                        self.assertFalse((self.project / f'apps/usnm_qwen/1.0/{subdir}').exists())

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_native_ocr_apps_publish_only_on_their_built_platforms(self):
        (self.project / 'html/inc/boinc_db.inc').write_text('''<?php
class BoincApp { static function lookup($query) { return (object)["id"=>1]; } }
class BoincPlatform { static function lookup($query) { return (object)["id"=>2]; } }
class BoincAppVersion { static function enum($query) {
    if (strpos($query, "plan_class=''") === false) exit(9);
    return [(object)["id"=>3]];
} }
''')
        (self.root / 'php').unlink()
        for app in ('usnm_ocr_native', 'usnm_gold_native'):
            for platform in ('x86_64-pc-linux-gnu', 'arm64-apple-darwin', 'x86_64-apple-darwin',
                             'aarch64-unknown-linux-gnu', 'windows_x86_64'):
                with self.subTest(app=app, platform=platform):
                    src = self.source / f'apps/{app}/1.0/{platform}'
                    src.mkdir(parents=True)
                    (src / f'{app}_1.0_{platform}').write_text('native-fixture')
                    env = {**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'native',
                           'APPVER_APP': app, 'APPVER_VERSION': '1.0', 'APPVER_PLATFORM': platform,
                           'APPVER_PLAN_CLASS': '', 'DB_HOST': 'fixture', 'DB_NAME': 'fixture',
                           'MASTER_HOST': 'fixture', 'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
                    result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                    if platform in ('x86_64-pc-linux-gnu', 'arm64-apple-darwin', 'x86_64-apple-darwin'):
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn(f'Verified {app} 1.0 {platform} ', result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(f'native {app} is built only for x86_64-pc-linux-gnu, arm64-apple-darwin', result.stderr)
                        self.assertFalse((self.project / f'apps/{app}/1.0/{platform}').exists())


if __name__ == '__main__':
    unittest.main()
