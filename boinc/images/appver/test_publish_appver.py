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

    @unittest.skipUnless(shutil.which('php'), 'PHP unavailable')
    def test_a_held_publication_is_deprecated_and_journaled_before_the_feeder_hears(self):
        # #1687: boinc-app-release.sh --hold bakes APPVER_HOLD=1 into the image.
        php = shutil.which('php')
        (self.root / 'php').write_text(f'#!/bin/sh\nexec "{php}" "$@"\n')
        state = self.root / 'state.json'
        state.write_text('{"deprecated": 0, "touched_before": null}')
        (self.project / 'html/inc/boinc_db.inc').write_text('''<?php
function usnm_state() { return json_decode(file_get_contents(getenv("USNM_FAKE_STATE")), true); }
class BoincApp {
    public $id = 1, $name = "usnm_ocr", $min_version = 0;
    static function lookup($q) { return new self; }
    static function lookup_id($id) { return new self; }
}
class BoincPlatform {
    static function lookup($q) { return (object)["id" => 1]; }
    static function lookup_id($id) { return (object)["id" => 1, "name" => "x86_64-pc-linux-gnu"]; }
}
class BoincAppVersion {
    public $id = 86, $appid = 1, $platformid = 1, $version_num = 107, $plan_class = "docker", $deprecated, $xml_doc = "";
    function __construct() { $this->deprecated = usnm_state()["deprecated"]; }
    static function enum($q) {
        if (str_starts_with($q, "appid=1 and version_num=")) return [new self];
        $old = new self; $old->id = 82; $old->version_num = 106; $old->deprecated = 0;
        return [$old, new self];
    }
    static function lookup_id($id) { return new self; }
    function update($q) {
        $s = usnm_state();
        $s["deprecated"] = $q === "deprecated=1" ? 1 : 0;
        $s["touched_before"] = file_exists(getenv("PROJ") . "/reread_db");
        file_put_contents(getenv("USNM_FAKE_STATE"), json_encode($s));
        return true;
    }
}
class BoincDb {
    static function get() { return new self; }
    function do_query($q) { return new class { function fetch_assoc() { return null; } }; }
}
''')
        tool = Path(__file__).resolve().parents[2] / 'scripts/app-version-hold.php'
        self.script.write_text(self.script.read_text().replace(
            'php /usr/local/lib/usnm-boinc/app-version-hold.php', f'php "{tool}"'))
        src = self.source / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker'
        src.mkdir(parents=True)
        (src / 'docker_wrapper').write_text('wrapper')
        env = {**os.environ, 'PROJ': str(self.project), 'APPVER_APP': 'usnm_ocr', 'APPVER_VERSION': '1.7',
               'APPVER_PLATFORM': 'x86_64-pc-linux-gnu', 'APPVER_HOLD': '1', 'USNM_FAKE_STATE': str(state),
               'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
               'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
        result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(state.read_text()), {'deprecated': 1, 'touched_before': False})
        self.assertTrue((self.project / 'reread_db').exists())
        journal = list((self.project / 'ops-app-versions').glob('hold-app-version-86-*.json'))
        self.assertEqual(len(journal), 1)
        self.assertEqual(json.loads(journal[0].read_text())['deprecated_before'], 0)
        # Only usnm_ocr and usnm_gold container versions can be published held.
        for app, value in (('usnm_quality', '1'), ('usnm_ocr', '2')):
            bad = subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                                 env={**env, 'APPVER_APP': app, 'APPVER_HOLD': value})
            self.assertNotEqual(bad.returncode, 0)
            self.assertIn('APPVER_HOLD', bad.stderr)
        here = Path(__file__).resolve().parent
        containerfile = (here / 'Containerfile').read_text()
        self.assertIn('COPY app-version-hold.php /usr/local/lib/usnm-boinc/app-version-hold.php', containerfile)
        self.assertIn('APPVER_HOLD=${HOLD}', containerfile)

    def test_a_hold_without_a_registered_id_fails_loudly(self):
        src = self.source / 'apps/usnm_ocr/1.7/x86_64-pc-linux-gnu__docker'
        src.mkdir(parents=True)
        (src / 'docker_wrapper').write_text('wrapper')
        result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True,
                                env={**os.environ, 'PROJ': str(self.project), 'APPVER_APP': 'usnm_ocr',
                                     'APPVER_VERSION': '1.7', 'APPVER_HOLD': '1',
                                     'DB_HOST': 'db.example.org', 'DB_NAME': 'fixture', 'MASTER_HOST': 'boinc.example.org',
                                     'PATH': str(self.root) + os.pathsep + os.environ['PATH']})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('no app version id to hold', result.stderr)
        self.assertFalse((self.project / 'reread_db').exists())

    def test_container_publication_rejects_wrong_or_legacy_model_classes(self):
        for app, plan, platform in (
            ('usnm_qwen', 'docker_mt', 'x86_64-pc-linux-gnu'),
            ('usnm_qwen', 'docker_nvidia_cuda', 'x86_64-pc-linux-gnu'),
            ('usnm_surya', 'docker_mt_2b', 'x86_64-pc-linux-gnu'),
            ('usnm_ocr', 'docker_nvidia_cuda_8b', 'x86_64-pc-linux-gnu'),
            ('usnm_qwen', 'docker_mt_2b', 'aarch64-unknown-linux-gnu'),
            ('usnm_qwen', 'docker_nvidia_cuda_8b', 'windows_x86_64'),
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
    def test_native_ocr_apps_publish_only_on_linux_x86_64(self):
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
            for platform in ('x86_64-pc-linux-gnu', 'aarch64-unknown-linux-gnu'):
                with self.subTest(app=app, platform=platform):
                    src = self.source / f'apps/{app}/1.0/{platform}'
                    src.mkdir(parents=True)
                    (src / f'{app}_1.0_{platform}').write_text('native-fixture')
                    env = {**os.environ, 'PROJ': str(self.project), 'APPVER_KIND': 'native',
                           'APPVER_APP': app, 'APPVER_VERSION': '1.0', 'APPVER_PLATFORM': platform,
                           'APPVER_PLAN_CLASS': '', 'DB_HOST': 'fixture', 'DB_NAME': 'fixture',
                           'MASTER_HOST': 'fixture', 'PATH': str(self.root) + os.pathsep + os.environ['PATH']}
                    result = subprocess.run(['bash', str(self.script)], capture_output=True, text=True, env=env)
                    if platform == 'x86_64-pc-linux-gnu':
                        self.assertEqual(result.returncode, 0, result.stderr)
                        self.assertIn(f'Verified {app} 1.0 {platform} ', result.stdout)
                    else:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn(f'native {app} is built only for x86_64-pc-linux-gnu', result.stderr)
                        self.assertFalse((self.project / f'apps/{app}/1.0/{platform}').exists())


if __name__ == '__main__':
    unittest.main()
