"""Tests of image-security-check.sh (#969): python3 -m unittest -v scripts/boinc/onprem/test_image_security_check.py

Nothing here needs podman, apt or a network. podman is a stub that does what the real one is
asked to do for the image check: it runs the container's script (the -c argument) in the local
sh, with stub apt-get, apt-cache, dpkg-query and dpkg first on PATH. So the script that runs
inside the image is the real one; only the packages and their archive are made up. A second
stub answers podman pull and podman image inspect for the MySQL pin. Every call is logged, to
show the check only reads (it never stops, removes or restarts anything).
"""
import os
import re
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ONPREM = Path(__file__).resolve().parent
REPO = ONPREM.parents[2]
SCRIPT = ONPREM / "image-security-check.sh"
SERVICE = ONPREM / "usnm-image-security-check.service"
TIMER = ONPREM / "usnm-image-security-check.timer"
DOCS = REPO / "docs" / "boinc-onprem.md"
IMAGES = REPO / "contrib-compute" / "images"
CONTAINERFILES = sorted(IMAGES.glob("server/*Containerfile")) + sorted(IMAGES.glob("bridge/*Containerfile"))

PHP_FLOOR = "8.2.35-1~deb12u1"
APACHE_FLOOR = "2.4.67-1~deb12u3"
IMAGE = "localhost/usnm-boinc-bridge:4b7405f115cc"
MYSQL_TAG = "docker.io/library/mysql:8.4"
PINNED = "sha256:" + "a" * 64
MOVED = "sha256:" + "b" * 64

# apt-get -s -q upgrade, as apt 2.6 prints it on bookworm, now oldstable (Inst lines copied from a
# run against the archive on 24 October 2026). Only the Inst lines matter.
SIM_HEAD = "Reading package lists...\nBuilding dependency tree...\nReading state information...\nCalculating upgrade...\n"
SUMMARY_NONE = "0 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.\n"
SIM_NONE = SIM_HEAD + SUMMARY_NONE
SECURITY_INST = "Inst apache2 [2.4.67-1~deb12u2] (2.4.67-1~deb12u3 Debian-Security:12/oldstable-security [amd64])\n"
# The trailing [] is what apt prints after an Inst line it has no configure list for.
SECURITY_INST_BRACKETS = "Inst php8.2-xml [8.2.34-1~deb12u1] (8.2.35-1~deb12u1 Debian-Security:12/oldstable-security [amd64]) []\n"
UPDATE_INST = "Inst tzdata [2025b-0+deb12u1] (2026a-0+deb12u1 Debian:12.13/oldstable-updates [all])\n"
BOTH_INST = "Inst libssl3 [3.0.22-1~deb12u1] (3.0.23-1~deb12u1 Debian:12.13/oldstable, Debian-Security:12/oldstable-security [amd64])\n"
# An upgrade that needs a new package or a removal is kept back: apt prints the names, and no Inst line.
# apt wraps the list to the terminal width, so it can span lines.
KEPT_BACK = "The following packages have been kept back:\n  libcurl3-gnutls libcurl4\n  curl\n"
SIM_SUMMARY = "Conf apache2 (2.4.67-1~deb12u3 Debian-Security:12/oldstable-security [amd64])\n1 upgraded, 0 newly installed, 0 to remove and 0 not upgraded.\n"

POLICY = "Package files:\n 500 http://deb.debian.org/debian-security bookworm-security/main amd64 Packages\n     release v=12,o=Debian,a=oldstable-security,n=bookworm-security,l=Debian-Security,c=main,b=amd64\n"
POLICY_NO_SECURITY = "Package files:\n 500 http://deb.debian.org/debian bookworm/main amd64 Packages\n     release v=12.13,o=Debian,a=oldstable,n=bookworm,l=Debian,c=main,b=amd64\n"

STUBS = {
    "apt-get": r"""
echo "apt-get $*" >> "$STUB_DIR/calls.log"
case "$*" in
  update*) [ ! -e "$STUB_DIR/update-fails" ] || { echo "E: Failed to fetch" >&2; exit 100; }
           echo "Get:1 http://deb.debian.org/debian-security bookworm-security InRelease" ;;
  "-s -q upgrade") cat "$STUB_DIR/simulation" ;;
  *) echo "unexpected apt-get $*" >&2; exit 2 ;;
esac
""",
    "apt-cache": r"""
echo "apt-cache $*" >> "$STUB_DIR/calls.log"
[ "$1" = policy ] && cat "$STUB_DIR/policy"
""",
    "dpkg-query": r"""
echo "dpkg-query $*" >> "$STUB_DIR/calls.log"
cat "$STUB_DIR/installed-$3"
""",
    # dpkg --compare-versions A ge B; sort -V orders these Debian versions as dpkg does.
    "dpkg": r"""
[ "$1" = --compare-versions ] && [ "$3" = ge ] || exit 2
[ "$2" = "$4" ] && exit 0
[ "$(printf '%s\n%s\n' "$2" "$4" | sort -V | tail -n 1)" = "$2" ]
""",
    "podman": r"""
echo "podman $*" >> "$STUB_DIR/calls.log"
case "$1" in
  run)
    shift
    while :; do
      case "$1" in
        --rm | --pull=never) shift ;;
        --network | --entrypoint) shift 2 ;;
        -e) export "$2"; shift 2 ;;
        *) break ;;
      esac
    done
    echo "$1" > "$STUB_DIR/ran-image"
    [ "$2" = -c ] || exit 125
    exec sh -c "$3" ;;
  pull) [ ! -e "$STUB_DIR/pull-fails" ] || exit 125 ;;
  image) [ "$2" = inspect ] || exit 125
         [ ! -e "$STUB_DIR/inspect-fails" ] || { echo "Error: no such image" >&2; exit 125; }
         cat "$STUB_DIR/mysql-digest" ;;
  *) exit 125 ;;
esac
""",
}


class ImageSecurityCheck(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.bin = self.tmp / "bin"
        self.stub_dir = self.tmp / "state"
        self.bin.mkdir()
        self.stub_dir.mkdir()
        for name, body in STUBS.items():
            stub = self.bin / name
            stub.write_text("#!/bin/sh\n" + body.lstrip("\n"))
            stub.chmod(stub.stat().st_mode | stat.S_IXUSR)
        self.server_env = self.tmp / "server.env"
        self.server_env.write_text(f"SERVER_IMAGE={IMAGE}\n")
        self.mysql_unit = self.tmp / "usnm-boinc-mysql.service"
        self.set_mysql_unit(PINNED)
        self.state(simulation=SIM_NONE, policy=POLICY, mysql_digest=PINNED)
        self.installed(php=PHP_FLOOR, apache=APACHE_FLOOR)
        self.bash = shutil.which("bash")

    def tearDown(self):
        self._tmp.cleanup()

    def state(self, **files):
        """What the stubs answer: simulation (apt-get -s upgrade), policy (apt-cache policy), mysql_digest."""
        for name, text in files.items():
            (self.stub_dir / name.replace("_", "-")).write_text(text if text.endswith("\n") else text + "\n")

    def installed(self, *, php, apache):
        """The versions dpkg-query reports."""
        (self.stub_dir / "installed-php8.2-common").write_text(php + "\n")
        (self.stub_dir / "installed-apache2").write_text(apache + "\n")

    def set_mysql_unit(self, digest, repo="docker.io/library/mysql"):
        # The line mysql-setup.sh writes into the unit.
        self.mysql_unit.write_text(
            "[Service]\nExecStart=/usr/bin/podman run --rm --name usnm-boinc-mysql --network host \\\n"
            f"  --stop-timeout 110 {repo}@{digest}\nExecStop=/usr/bin/podman stop -t 110 usnm-boinc-mysql\n")

    def run_check(self, *extra, defaults=True, php_floor=PHP_FLOOR, apache_floor=APACHE_FLOOR):
        """The script with the host's usual flags (defaults), pointed at this test's files, then extra."""
        args = []
        if defaults:
            args = ["--php-floor", php_floor, "--apache-floor", apache_floor,
                    "--server-env", str(self.server_env), "--mysql-unit", str(self.mysql_unit)]
        env = {"PATH": f"{self.bin}{os.pathsep}/usr/bin:/bin", "STUB_DIR": str(self.stub_dir)}
        return subprocess.run([self.bash, str(SCRIPT), *args, *extra], env=env, text=True,
                              capture_output=True, timeout=60)

    def calls(self):
        path = self.stub_dir / "calls.log"
        return path.read_text().splitlines() if path.exists() else []

    def podman_subcommands(self):
        return {line.split()[1] for line in self.calls() if line.startswith("podman ")}

    # What a clean run looks like.
    def test_a_current_image_passes(self):
        done = self.run_check()
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn(f"checking {IMAGE}", done.stdout)
        self.assertIn(f"php8.2-common {PHP_FLOOR} meets {PHP_FLOOR}", done.stdout)
        self.assertIn(f"apache2 {APACHE_FLOOR} meets {APACHE_FLOOR}", done.stdout)
        self.assertIn(f"MySQL {MYSQL_TAG} is still {PINNED}", done.stdout)
        self.assertIn("OK:", done.stdout)
        self.assertNotIn("FAILED", done.stdout)
        self.assertNotIn("KEPT-BACK", done.stdout)

    def test_it_runs_the_local_image_without_pulling_it_and_only_reads(self):
        self.assertEqual(self.run_check().returncode, 0)
        self.assertEqual((self.stub_dir / "ran-image").read_text().strip(), IMAGE)
        run = next(line for line in self.calls() if line.startswith("podman run"))
        for part in ("--rm", "--pull=never", "--network host", "--entrypoint sh",
                     f"-e PHP_FLOOR={PHP_FLOOR}", f"-e APACHE_FLOOR={APACHE_FLOOR}"):
            self.assertIn(part, run)
        # Nothing stops, removes or restarts a container or pulls anything but the MySQL tag.
        self.assertEqual(self.podman_subcommands(), {"run", "pull", "image"})
        self.assertIn(f"podman pull -q {MYSQL_TAG}", self.calls())
        apt = [line for line in self.calls() if line.startswith("apt-get")]
        self.assertTrue(any("Error-Mode=any" in line for line in apt if " update" in line), apt)
        self.assertTrue(all(" -s " in line for line in apt if "upgrade" in line), apt)
        self.assertFalse(any(re.search(r"\b(install|dist-upgrade|full-upgrade)\b", line) for line in apt), apt)

    # Packages.
    def test_a_pending_security_upgrade_fails_and_is_named(self):
        self.state(simulation=SIM_HEAD + SECURITY_INST + SIM_SUMMARY)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("SECURITY apache2 2.4.67-1~deb12u2 -> 2.4.67-1~deb12u3 from Debian-Security:12/oldstable-security",
                      done.stdout)
        self.assertIn("FAILED: 1 finding(s)", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    def test_a_pending_upgrade_from_another_source_is_listed_and_fails_too(self):
        self.state(simulation=SIM_HEAD + UPDATE_INST + SIM_SUMMARY)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("UPDATE tzdata 2025b-0+deb12u1 -> 2026a-0+deb12u1 from Debian:12.13/oldstable-updates", done.stdout)
        self.assertNotIn("SECURITY", done.stdout)

    def test_every_pending_upgrade_is_listed(self):
        self.state(simulation=SIM_HEAD + SECURITY_INST + UPDATE_INST + BOTH_INST + SIM_SUMMARY)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("SECURITY apache2", done.stdout)
        self.assertIn("UPDATE tzdata", done.stdout)
        # A package offered by the point release and by the security archive counts as security.
        self.assertIn("SECURITY libssl3 3.0.22-1~deb12u1 -> 3.0.23-1~deb12u1", done.stdout)
        self.assertIn("FAILED: 3 finding(s)", done.stdout)

    def test_an_inst_line_with_a_trailing_bracket_pair_is_read(self):
        self.state(simulation=SIM_HEAD + SECURITY_INST_BRACKETS + SIM_SUMMARY)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("SECURITY php8.2-xml 8.2.34-1~deb12u1 -> 8.2.35-1~deb12u1 from Debian-Security:12/oldstable-security",
                      done.stdout)
        self.assertNotIn("not understood", done.stdout)

    def test_a_line_apt_prints_differently_is_still_a_finding(self):
        self.state(simulation=SIM_HEAD + "Inst somepkg (1.0 Debian-Security:12/oldstable-security)\n"
                   + "Inst otherpkg (1.0 Debian:12.13/oldstable)\n" + SUMMARY_NONE)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("SECURITY (apt line not understood) somepkg", done.stdout)
        self.assertIn("UPDATE (apt line not understood) otherpkg", done.stdout)
        self.assertIn("FAILED: 2 finding(s)", done.stdout)

    def test_a_package_apt_keeps_back_is_a_finding(self):
        # apt-get upgrade holds back a package whose upgrade needs a new package and prints no Inst
        # line for it, so only the kept-back list and the "not upgraded" count show it.
        self.state(simulation=SIM_HEAD + KEPT_BACK + "0 upgraded, 0 newly installed, 0 to remove and 3 not upgraded.\n")
        done = self.run_check()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("KEPT-BACK 3 package(s)", done.stdout)
        self.assertIn("libcurl3-gnutls libcurl4 curl", done.stdout)
        self.assertIn("FAILED: 1 finding(s)", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    def test_a_package_apt_does_not_upgrade_without_a_list_is_still_a_finding(self):
        # A held or pinned package is counted in the summary without being listed.
        self.state(simulation=SIM_HEAD + "0 upgraded, 0 newly installed, 0 to remove and 1 not upgraded.\n")
        done = self.run_check()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("KEPT-BACK 1 package(s)", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    def test_kept_back_packages_and_pending_upgrades_are_counted_together(self):
        # As apt prints it: the kept-back list, then the list of what it will upgrade, then the summary.
        self.state(simulation=SIM_HEAD + KEPT_BACK + "The following packages will be upgraded:\n  apache2\n"
                   + "1 upgraded, 0 newly installed, 0 to remove and 3 not upgraded.\n" + SECURITY_INST)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("SECURITY apache2 2.4.67-1~deb12u2 -> 2.4.67-1~deb12u3", done.stdout)
        self.assertIn("KEPT-BACK 3 package(s) apt-get upgrade will not upgrade, as that needs a new package or a removal: "
                      "libcurl3-gnutls libcurl4 curl\n", done.stdout)
        self.assertIn("FAILED: 2 finding(s)", done.stdout)

    def test_an_apt_simulation_without_a_summary_is_not_a_clean_run(self):
        # Without the "not upgraded" count the check cannot tell nothing was kept back.
        self.state(simulation=SIM_HEAD)
        done = self.run_check()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn("no 'not upgraded' count", done.stderr)
        self.assertIn("COULD-NOT-CHECK podman run", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    # The floors.
    def test_php_below_its_floor_fails(self):
        self.installed(php="8.2.34-1~deb12u1", apache=APACHE_FLOOR)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn(f"BELOW-FLOOR php8.2-common 8.2.34-1~deb12u1 is older than {PHP_FLOOR}", done.stdout)
        self.assertIn(f"apache2 {APACHE_FLOOR} meets", done.stdout)

    def test_apache_below_its_floor_fails(self):
        self.installed(php=PHP_FLOOR, apache="2.4.67-1~deb12u2")
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn(f"BELOW-FLOOR apache2 2.4.67-1~deb12u2 is older than {APACHE_FLOOR}", done.stdout)
        self.assertIn(f"php8.2-common {PHP_FLOOR} meets", done.stdout)

    def test_a_newer_package_than_the_floor_passes(self):
        # bookworm's own suite can be ahead of bookworm-security (apache2 2.4.68 against 2.4.67).
        self.installed(php="8.2.36-1~deb12u1", apache="2.4.68-1~deb12u1")
        done = self.run_check()
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    def test_the_floors_come_from_the_flags(self):
        done = self.run_check(php_floor="8.2.99-1~deb12u1", apache_floor="2.4.99-1~deb12u1")
        self.assertEqual(done.returncode, 1)
        self.assertIn("BELOW-FLOOR php8.2-common", done.stdout)
        self.assertIn("BELOW-FLOOR apache2", done.stdout)
        self.assertIn("FAILED: 2 finding(s)", done.stdout)

    # A check that can't answer must not pass.
    def test_a_failed_apt_update_is_not_a_clean_run(self):
        (self.stub_dir / "update-fails").write_text("")
        done = self.run_check()
        self.assertEqual(done.returncode, 3)
        self.assertIn("COULD-NOT-CHECK podman run", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    def test_an_image_without_a_security_archive_is_not_a_clean_run(self):
        self.state(policy=POLICY_NO_SECURITY)
        done = self.run_check()
        self.assertEqual(done.returncode, 3)
        self.assertIn("no security archive", done.stderr)
        self.assertNotIn("OK:", done.stdout)

    def test_no_server_image_is_not_a_clean_run(self):
        self.server_env.write_text("OTHER=1\n")
        done = self.run_check()
        self.assertEqual(done.returncode, 3)
        self.assertIn("no SERVER_IMAGE", done.stdout)
        self.assertNotIn("run", self.podman_subcommands())

    def test_a_missing_server_env_is_not_a_clean_run(self):
        self.server_env.unlink()
        self.assertEqual(self.run_check().returncode, 3)

    def test_the_last_server_image_in_the_file_wins(self):
        self.server_env.write_text("SERVER_IMAGE=localhost/old:1\nSERVER_IMAGE=localhost/new:2\n")
        self.assertEqual(self.run_check().returncode, 0)
        self.assertEqual((self.stub_dir / "ran-image").read_text().strip(), "localhost/new:2")

    def test_image_overrides_server_env(self):
        self.server_env.unlink()
        done = self.run_check("--image", "localhost/usnm-boinc-bridge:next")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual((self.stub_dir / "ran-image").read_text().strip(), "localhost/usnm-boinc-bridge:next")

    # MySQL.
    def test_a_moved_mysql_tag_is_reported_not_rolled(self):
        self.state(mysql_digest=MOVED)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn(f"MYSQL-DRIFT {MYSQL_TAG} is {MOVED} now; the unit runs {PINNED}", done.stdout)
        self.assertEqual(self.podman_subcommands(), {"run", "pull", "image"})

    def test_a_unit_without_a_digest_is_not_a_clean_run(self):
        self.mysql_unit.write_text("[Service]\nExecStart=/usr/bin/podman run docker.io/library/mysql:8.4\n")
        done = self.run_check()
        self.assertEqual(done.returncode, 3)
        self.assertIn("has no single docker.io/library/mysql@sha256 digest", done.stdout)

    def test_a_missing_unit_is_not_a_clean_run(self):
        self.mysql_unit.unlink()
        self.assertEqual(self.run_check().returncode, 3)

    def test_a_failed_mysql_pull_is_not_a_clean_run(self):
        (self.stub_dir / "pull-fails").write_text("")
        done = self.run_check()
        self.assertEqual(done.returncode, 3)
        self.assertIn(f"podman pull {MYSQL_TAG} failed", done.stdout)

    def test_a_failed_mysql_inspect_is_not_a_clean_run(self):
        # The pull worked, so the image check's own result is still reported, and the run ends in
        # exit 3 with the summary, not in podman's exit status.
        (self.stub_dir / "inspect-fails").write_text("")
        done = self.run_check()
        self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
        self.assertIn(f"COULD-NOT-CHECK podman image inspect {MYSQL_TAG} failed", done.stdout)
        self.assertIn("FAILED: 1 check(s) could not run", done.stdout)
        self.assertNotIn("OK:", done.stdout)

    def test_a_failed_mysql_inspect_does_not_hide_a_package_finding(self):
        (self.stub_dir / "inspect-fails").write_text("")
        self.state(simulation=SIM_HEAD + SECURITY_INST + SIM_SUMMARY)
        done = self.run_check()
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("SECURITY apache2", done.stdout)
        self.assertIn("COULD-NOT-CHECK podman image inspect", done.stdout)

    def test_every_tagged_form_of_a_mysql_image_reference_finds_its_pin(self):
        # The name a unit pins is the reference without its tag. A colon before the last slash is a
        # registry port, not a tag, so it stays in the name.
        cases = [
            ("registry.example:5000/library/mysql:8.4", "registry.example:5000/library/mysql"),
            ("registry.example:5000/mysql:8.4", "registry.example:5000/mysql"),
            ("docker.io/library/mysql:8.4", "docker.io/library/mysql"),
            ("localhost/mysql:8.4", "localhost/mysql"),
            ("mysql:8.4", "mysql"),
        ]
        for image, name in cases:
            with self.subTest(image=image):
                self.set_mysql_unit(PINNED, repo=name)
                self.state(mysql_digest=PINNED)
                (self.stub_dir / "calls.log").unlink(missing_ok=True)
                done = self.run_check("--mysql-image", image)
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
                self.assertIn(f"MySQL {image} is still {PINNED}", done.stdout)
                self.assertIn(f"podman pull -q {image}", self.calls())

                self.state(mysql_digest=MOVED)
                done = self.run_check("--mysql-image", image)
                self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
                self.assertIn(f"MYSQL-DRIFT {image} is {MOVED} now; the unit runs {PINNED}", done.stdout)

    def test_a_mysql_image_without_a_tag_or_with_a_digest_is_refused(self):
        # The drift check compares what podman pull gives for the tag with the unit's pin. With a
        # digest, podman returns that digest whatever the tag now is; with no tag it pulls :latest,
        # not the tag the pin was resolved from. Neither can show drift, so neither is run.
        digest_message = "--mysql-image: a reference with a digest can't show a moved tag; give an image name with a tag"
        tag_message = "--mysql-image: an image name with a tag, such as docker.io/library/mysql:8.4"
        cases = [
            ("docker.io/library/mysql", tag_message),
            ("mysql", tag_message),
            ("localhost/mysql", tag_message),
            ("registry.example:5000/mysql", tag_message),
            ("registry.example:5000/library/mysql", tag_message),
            ("mysql:", tag_message),
            ("registry.example:5000/mysql:", tag_message),
            ("mysql:8.4:9", tag_message),
            ("docker.io/library/mysql@" + PINNED, digest_message),
            ("docker.io/library/mysql:8.4@" + PINNED, digest_message),
            ("registry.example:5000/mysql@" + PINNED, digest_message),
            ("registry.example:5000/mysql:8.4@" + PINNED, digest_message),
            ("mysql@" + PINNED, digest_message),
        ]
        for image, message in cases:
            with self.subTest(image=image):
                done = self.run_check("--mysql-image", image)
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn(message, done.stderr)
                self.assertEqual(done.stdout, "")
                self.assertEqual(self.calls(), [])

    def test_a_mysql_image_with_other_characters_is_refused_for_them(self):
        # The message names the character rule: the reference may be fine as to its tag.
        message = "--mysql-image: an image name made only of letters, digits and . _ / : - (up to 200)"
        for image in ("docker.io/library/mysql:8.4;id", "mysql:8.4 --pull", "my$ql:8.4", "docker.io/library/mysql:8.4" + "a" * 200):
            with self.subTest(image=image):
                done = self.run_check("--mysql-image", image)
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn(message, done.stderr)
                self.assertEqual(done.stdout, "")
                self.assertEqual(self.calls(), [])

    def test_a_registry_port_is_part_of_the_name_the_unit_must_pin(self):
        image = "registry.example:5000/library/mysql:8.4"
        # Not the registry without its port, not another port, not the same name on docker.io.
        for repo in ("registry.example/library/mysql", "registry.example:5001/library/mysql", "docker.io/library/mysql"):
            with self.subTest(repo=repo):
                self.set_mysql_unit(PINNED, repo=repo)
                done = self.run_check("--mysql-image", image)
                self.assertEqual(done.returncode, 3, done.stdout + done.stderr)
                self.assertIn("has no single registry.example:5000/library/mysql@sha256 digest", done.stdout)
                self.assertNotIn("OK:", done.stdout)

    def test_a_pin_to_another_image_is_not_taken_for_mysql(self):
        self.mysql_unit.write_text(f"ExecStart=/usr/bin/podman run docker.io/library/mariadb@{PINNED}\n")
        self.assertEqual(self.run_check().returncode, 3)

    def test_skip_mysql_leaves_the_registry_alone(self):
        self.mysql_unit.unlink()
        done = self.run_check("--skip-mysql")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("MySQL skipped", done.stdout)
        self.assertEqual(self.podman_subcommands(), {"run"})

    def test_package_findings_and_mysql_drift_are_counted_together(self):
        self.state(simulation=SIM_HEAD + SECURITY_INST + SIM_SUMMARY, mysql_digest=MOVED)
        done = self.run_check()
        self.assertEqual(done.returncode, 1)
        self.assertIn("FAILED: 2 finding(s)", done.stdout)

    # Arguments: named only.
    def test_bad_arguments_are_refused_before_anything_runs(self):
        good = ["--php-floor", PHP_FLOOR, "--apache-floor", APACHE_FLOOR]
        cases = [
            ([], "--php-floor: a Debian version"),
            (["--php-floor", PHP_FLOOR], "--apache-floor: a Debian version"),
            (good + ["extra"], "unknown argument: extra"),
            (good + ["--unknown", "x"], "unknown argument: --unknown"),
            (good + ["--image"], "--image needs a value"),
            (["--php-floor"], "--php-floor needs a value"),
            (["--php-floor", "8.2;id", "--apache-floor", APACHE_FLOOR], "--php-floor: a Debian version"),
            (["--php-floor", PHP_FLOOR, "--apache-floor", "$(id)"], "--apache-floor: a Debian version"),
            (good + ["--image", "a b"], "--image: an image name"),
            (good + ["--mysql-image", "x;y"], "--mysql-image: an image name"),
        ]
        for args, message in cases:
            with self.subTest(args=args):
                done = self.run_check(*args, defaults=False)
                self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
                self.assertIn(message, done.stderr)
        self.assertEqual(self.calls(), [])

    def test_help_prints_the_usage(self):
        done = self.run_check("--help", defaults=False)
        self.assertEqual(done.returncode, 2)
        self.assertIn("--php-floor VERSION --apache-floor VERSION", done.stderr)
        self.assertIn("--mysql-image must be a name with a tag", done.stderr)


class Units(unittest.TestCase):
    def setUp(self):
        self.service = SERVICE.read_text()
        self.exec_start = re.search(r"^ExecStart=(.*)$", self.service, re.M).group(1).split()

    def test_the_script_is_executable_and_takes_flags_only(self):
        self.assertTrue(os.access(SCRIPT, os.X_OK))
        args = self.exec_start[1:]
        self.assertEqual(args[::2], ["--php-floor", "--apache-floor"], args)

    def test_the_unit_runs_the_installed_script_the_docs_install(self):
        self.assertEqual(self.exec_start[0], "/usr/local/bin/usnm-image-security-check")
        self.assertIn("install -m 0755 scripts/boinc/onprem/image-security-check.sh " + self.exec_start[0],
                      DOCS.read_text())

    def test_the_floors_in_the_unit_are_the_containerfiles_floors(self):
        flags = dict(zip(self.exec_start[1::2], self.exec_start[2::2]))
        self.assertTrue(CONTAINERFILES)
        for path in CONTAINERFILES:
            text = path.read_text()
            with self.subTest(path=path.name):
                self.assertEqual(re.findall(r"^ARG PHP_FLOOR=(\S+)$", text, re.M), [flags["--php-floor"]])
                self.assertEqual(re.findall(r"^ARG APACHE_FLOOR=(\S+)$", text, re.M), [flags["--apache-floor"]])

    def test_the_service_is_a_oneshot_that_changes_nothing(self):
        self.assertRegex(self.service, r"(?m)^Type=oneshot$")
        self.assertRegex(self.service, r"(?m)^After=network-online\.target$")
        self.assertNotRegex(self.service, r"(?m)^(ExecStartPost|ExecStop|Restart)")

    def test_the_timer_is_daily_and_catches_up(self):
        timer = TIMER.read_text()
        self.assertRegex(timer, r"(?m)^OnCalendar=\*-\*-\* \d\d:\d\d:\d\d$")
        self.assertRegex(timer, r"(?m)^Persistent=true$")
        self.assertRegex(timer, r"(?m)^RandomizedDelaySec=")
        self.assertRegex(timer, r"(?m)^WantedBy=timers\.target$")


if __name__ == "__main__":
    unittest.main()
