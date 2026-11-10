"""Tests for the pinned apt bases of the Tesseract and BOINC worker images (#1681).

scripts/ci/tesseract-base.sh reads ocr/engines/tesseract/base.lock; the Containerfiles install
from the Debian snapshot only in their apt stages, which the bases are built from.
"""

import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci/tesseract-base.sh"
LOCK = ROOT / "ocr/engines/tesseract/base.lock"
ENGINE = ROOT / "ocr/engines/tesseract/Containerfile"
WORKER = ROOT / "boinc/apps/Containerfile"
BASE_WORKFLOW = ROOT / ".github/workflows/tesseract-base.yml"
WORKER_WORKFLOW = ROOT / ".github/workflows/boinc-worker.yml"
IMAGE = "ghcr.io/tgoodyear/usnm-tesseract-base"
DIGEST = {name: "sha256:" + digit * 64 for name, digit in [("a", "a"), ("b", "b"), ("c", "c"), ("d", "d")]}


def stages(text):
    """Each stage's name (or its index) -> (FROM line, body), in order."""
    out = {}
    for index, part in enumerate(re.split(r"(?m)^(?=FROM )", text)[1:]):
        head, _, body = part.partition("\n")
        name = head.split(" AS ", 1)[1].strip() if " AS " in head else str(index)
        out[name] = (head, body)
    return out


def procps_download(body):
    start = body.index("RUN apt-get")
    return body[start:].split("\n\n", 1)[0].strip()


class Script(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(subprocess.run, ["rm", "-rf", str(self.dir)], check=False)

    def run_script(self, lock_text, *args, env=None):
        lock = self.dir / "base.lock"
        lock.write_text(lock_text)
        return subprocess.run(
            ["bash", str(SCRIPT), "--lock", str(lock), *args],
            capture_output=True, text=True, env=env, check=False,
        )

    @staticmethod
    def lock(amd64=("", ""), arm64=("", "")):
        return (
            "# comment\n"
            f"AMD64_ENGINE_BASE={amd64[0]}\nAMD64_TOOLS_BASE={amd64[1]}\n"
            f"ARM64_ENGINE_BASE={arm64[0]}\nARM64_TOOLS_BASE={arm64[1]}\n"
        )

    def fake_tool(self, name, snapshot, recipe):
        tool = self.dir / "bin" / name
        tool.parent.mkdir(exist_ok=True)
        tool.write_text(
            "#!/bin/sh\n"
            f'echo "$*" >> "{self.dir}/calls"\n'
            'case "$1" in pull) exit 0 ;; image) ;; *) exit 2 ;; esac\n'
            f'case "$*" in *debian-snapshot*) echo "{snapshot}" ;; *apt-recipe*) echo "{recipe}" ;; *) exit 2 ;; esac\n'
        )
        tool.chmod(tool.stat().st_mode | stat.S_IEXEC)
        return {**os.environ, "PATH": f"{tool.parent}:{os.environ['PATH']}"}

    def test_the_repository_lock_parses_for_both_architectures(self):
        for arch in ("amd64", "arm64"):
            with self.subTest(arch=arch):
                done = subprocess.run(["bash", str(SCRIPT), "--arch", arch], capture_output=True, text=True, check=False)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertRegex(done.stdout, r"^(|ENGINE_BASE=\S+\nTOOLS_BASE=\S+\n)$")

    def test_an_unpinned_architecture_prints_nothing(self):
        done = self.run_script(self.lock(arm64=(f"{IMAGE}@{DIGEST['a']}", f"{IMAGE}@{DIGEST['b']}")), "--arch", "amd64")
        self.assertEqual((done.returncode, done.stdout), (0, ""), done.stderr)

    def test_a_pinned_architecture_prints_its_build_arguments(self):
        text = self.lock(
            amd64=(f"{IMAGE}@{DIGEST['a']}", f"{IMAGE}@{DIGEST['b']}"),
            arm64=(f"{IMAGE}@{DIGEST['c']}", f"{IMAGE}@{DIGEST['d']}"),
        )
        done = self.run_script(text, "--arch", "arm64")
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout, f"ENGINE_BASE={IMAGE}@{DIGEST['c']}\nTOOLS_BASE={IMAGE}@{DIGEST['d']}\n")

    def test_a_malformed_lock_fails(self):
        good = f"{IMAGE}@{DIGEST['a']}"
        for why, text in [
            ("a tag, not a digest", self.lock(amd64=(f"{IMAGE}:latest", good))),
            ("another image", self.lock(amd64=(f"ghcr.io/someone/else@{DIGEST['a']}", good))),
            ("a short digest", self.lock(amd64=(f"{IMAGE}@sha256:abc", good))),
            ("one base of two", self.lock(amd64=(good, ""))),
            ("a missing key", "AMD64_ENGINE_BASE=\nAMD64_TOOLS_BASE=\nARM64_ENGINE_BASE=\n"),
            ("a repeated key", self.lock() + "AMD64_TOOLS_BASE=\n"),
            ("an unknown key", self.lock() + "AMD64_OTHER_BASE=\n"),
        ]:
            with self.subTest(why=why):
                done = self.run_script(text, "--arch", "amd64")
                self.assertEqual(done.returncode, 1, done.stdout)
                self.assertIn("tesseract-base.sh:", done.stderr)

    def test_flags_are_named_and_checked(self):
        for args in [[], ["--arch", "riscv64"], ["--arch", "amd64", "--check-label", "buildah"], ["amd64"],
                     ["--print-recipe", "--arch", "amd64"]]:
            with self.subTest(args=args):
                self.assertEqual(self.run_script(self.lock(), *args).returncode, 1)

    def test_the_labels_must_be_the_containerfiles_snapshot_and_recipe(self):
        snapshot = re.search(r"^ARG SNAPSHOT=(\S+)$", ENGINE.read_text(), re.M).group(1)
        recipe = subprocess.run(["bash", str(SCRIPT), "--print-recipe"], capture_output=True, text=True, check=True).stdout.strip()
        self.assertRegex(recipe, r"^[0-9a-f]{64}$")
        text = self.lock(amd64=(f"{IMAGE}@{DIGEST['a']}", f"{IMAGE}@{DIGEST['b']}"))
        for tool in ("docker", "podman"):
            with self.subTest(tool=tool, labels="match"):
                done = self.run_script(text, "--arch", "amd64", "--check-label", tool, env=self.fake_tool(tool, snapshot, recipe))
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertIn("TOOLS_BASE=", done.stdout)
                self.assertIn(f"pull --quiet --platform linux/amd64 {IMAGE}@{DIGEST['b']}", (self.dir / "calls").read_text())
            for why, labels, message in [
                ("snapshot", ("20250101T000000Z", recipe), "not the Containerfile's SNAPSHOT"),
                ("recipe", (snapshot, "0" * 64), "other apt stages"),
                ("none", ("", ""), "not the Containerfile's SNAPSHOT"),
            ]:
                with self.subTest(tool=tool, differs=why):
                    done = self.run_script(text, "--arch", "amd64", "--check-label", tool, env=self.fake_tool(tool, *labels))
                    self.assertEqual(done.returncode, 1, done.stdout)
                    self.assertIn(message, done.stderr)
                    self.assertEqual(done.stdout, "")

    def test_the_recipe_covers_the_apt_stages_only(self):
        text = ENGINE.read_text()
        def recipe(body):
            script = self.dir / "scripts/ci/tesseract-base.sh"
            script.parent.mkdir(parents=True, exist_ok=True)
            script.write_text(SCRIPT.read_text())
            (self.dir / "ocr/engines/tesseract").mkdir(parents=True, exist_ok=True)
            (self.dir / "ocr/engines/tesseract/Containerfile").write_text(body)
            return subprocess.run(["bash", str(script), "--print-recipe"], capture_output=True, text=True, check=True).stdout

        same = recipe(text)
        self.assertEqual(recipe(text.replace("COPY fpshim.c", "COPY  fpshim.c")), same)
        self.assertEqual(recipe(text.replace("# The published tools base", "# The tools base")), same)
        self.assertNotEqual(recipe(text.replace("tesseract-ocr=5.5.0-1+b1 libtesseract5", "tesseract-ocr=5.5.0-2 libtesseract5")), same)
        self.assertNotEqual(recipe(text.replace("ca-certificates curl gcc", "ca-certificates curl wget gcc")), same)


class Containerfiles(unittest.TestCase):
    def test_only_the_apt_stages_install_from_the_snapshot(self):
        engine = stages(ENGINE.read_text())
        for name, (head, body) in engine.items():
            with self.subTest(stage=name):
                if name.startswith("apt-"):
                    continue
                self.assertNotIn("apt-get", body)
                self.assertNotIn("snapshot.debian.org/archive/debian/%s", body)
        worker = stages(WORKER.read_text())
        for name, (head, body) in worker.items():
            with self.subTest(stage=name):
                if name == "procps-debs":
                    continue
                self.assertNotIn("apt-get update", body)
                self.assertNotIn("Acquire::Check-Valid-Until", body)

    def test_the_bases_default_to_the_apt_stages(self):
        text = ENGINE.read_text()
        engine = stages(text)
        self.assertIn("ARG ENGINE_BASE=apt-engine\n", text)
        self.assertIn("ARG TOOLS_BASE=apt-tools\n", text)
        for name in ("apt-engine", "apt-tools", "apt-tools-base", "apt-procps"):
            self.assertIn(name, engine)
        uses = [head for head, _ in engine.values() if "${ENGINE_BASE}" in head or "${TOOLS_BASE}" in head]
        self.assertEqual(len(uses), 3, uses)
        self.assertIn("ARG TOOLS_BASE=procps-debs\n", WORKER.read_text())
        self.assertIn("procps-debs", stages(WORKER.read_text()))

    def test_the_bases_record_and_check_the_snapshot(self):
        engine = stages(ENGINE.read_text())
        for name in ("apt-engine", "apt-tools"):
            self.assertIn("LABEL org.usnewsmap.debian-snapshot=${SNAPSHOT}", engine[name][1])
        check = 'grep -qxF "URIs: http://snapshot.debian.org/archive/debian/$SNAPSHOT"'
        checked = [name for name, (head, body) in engine.items() if "_BASE}" in head and check in body]
        self.assertEqual(checked, ["models", list(engine)[-1]], checked)
        self.assertIn("RUN cmp /tmp/usnm-debs/debian.sources /etc/apt/sources.list.d/debian.sources", WORKER.read_text())

    def test_the_worker_downloads_procps_as_the_tools_base_does(self):
        base = procps_download(stages(ENGINE.read_text())["apt-procps"][1])
        worker = procps_download(stages(WORKER.read_text())["procps-debs"][1])
        self.assertEqual(base, worker)
        self.assertIn("--download-only procps", base)


class Workflows(unittest.TestCase):
    def test_the_base_workflow_builds_on_the_worker_workflows_debian_bases(self):
        def env(path):
            return dict(re.findall(r"^  (A(?:MD|RM)64_BASE): (\S+)$", path.read_text(), re.M))
        bases = env(BASE_WORKFLOW)
        self.assertEqual(sorted(bases), ["AMD64_BASE", "ARM64_BASE"])
        self.assertEqual(bases, env(WORKER_WORKFLOW))
        default = re.search(r"^ARG BASE=\S+@(sha256:[0-9a-f]{64})$", ENGINE.read_text(), re.M).group(1)
        self.assertTrue(bases["AMD64_BASE"].endswith("@" + default))

    def test_the_base_workflow_publishes_from_main_by_hand_only(self):
        text = BASE_WORKFLOW.read_text()
        on = text.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertNotIn("pull_request", on)
        self.assertNotIn("push", on)
        self.assertIn("workflow_dispatch", on)
        self.assertIn("if: github.ref == 'refs/heads/main'", text)
        self.assertEqual(text.count("actions/attest-build-provenance@"), 2)

    def test_package_lists_name_packages_without_their_architecture(self):
        # dpkg-query -W's default names Multi-Arch libraries as libtesseract5:amd64.
        text = BASE_WORKFLOW.read_text()
        calls = re.findall(r"dpkg-query \S+ -W.*", text)
        self.assertTrue(calls)
        for call in calls:
            self.assertIn("-f '${Package}\\t${Version}\\n'", call)
        self.assertIn("grep -qx $'libtesseract5\\t5.5.0-1+b1'", text)

    def test_ghcr_login_lasts_through_the_attestations(self):
        steps = BASE_WORKFLOW.read_text().split("\n      - ")
        names = [step.splitlines()[0] for step in steps]
        self.assertNotIn("docker logout", "".join(s for s in steps if "trap" in s))
        logout = next(i for i, s in enumerate(steps) if "run: docker logout ghcr.io" in s)
        self.assertIn("if: always()", steps[logout])
        last_attest = max(i for i, s in enumerate(steps) if "attest-build-provenance@" in s)
        upload = max(i for i, s in enumerate(steps) if "upload-artifact@" in s)
        self.assertGreater(logout, max(last_attest, upload), names)


if __name__ == "__main__":
    unittest.main()
