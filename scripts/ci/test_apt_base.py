"""Tests for the pinned apt bases of the native Tesseract build and the usnm_iris image (#1683).

scripts/ci/apt-base.sh reads boinc/apps/<app>/base.lock; the Containerfiles install from the
Debian snapshot only in their apt-* stages, which the bases are built from.
"""

import os
import re
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci/apt-base.sh"
BASE_WORKFLOW = ROOT / ".github/workflows/tesseract-base.yml"
IMAGE = "ghcr.io/tgoodyear/usnm-tesseract-base"
DIGEST = {name: "sha256:" + name * 64 for name in "abcd"}
KEYS = {"native": ["NATIVE_BASE"], "iris": ["IRIS_TOOLS_BASE", "IRIS_BASE"]}


def containerfile(app):
    return ROOT / f"boinc/apps/{app}/Containerfile"


def stages(text):
    """Each stage's name (or its index) -> (FROM line, body), in order."""
    out = {}
    for index, part in enumerate(re.split(r"(?m)^(?=FROM )", text)[1:]):
        head, _, body = part.partition("\n")
        name = head.split(" AS ", 1)[1].strip() if " AS " in head else str(index)
        out[name] = (head, body)
    return out


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
    def lock(app, *values):
        return "# comment\n" + "".join(f"{key}={value}\n" for key, value in zip(KEYS[app], values))

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

    def test_the_repository_locks_parse(self):
        for app in KEYS:
            with self.subTest(app=app):
                done = subprocess.run(["bash", str(SCRIPT), "--app", app], capture_output=True, text=True, check=False)
                self.assertEqual(done.returncode, 0, done.stderr)
                lines = done.stdout.splitlines()
                self.assertIn(len(lines), (0, len(KEYS[app])))
                for line, key in zip(lines, KEYS[app]):
                    self.assertRegex(line, rf"^{key}={re.escape(IMAGE)}@sha256:[0-9a-f]{{64}}$")

    def test_a_lock_with_every_value_empty_prints_nothing(self):
        for app, keys in KEYS.items():
            with self.subTest(app=app):
                done = self.run_script(self.lock(app, *[""] * len(keys)), "--app", app)
                self.assertEqual((done.returncode, done.stdout), (0, ""), done.stderr)

    def test_a_pinned_app_prints_its_build_arguments(self):
        done = self.run_script(self.lock("native", f"{IMAGE}@{DIGEST['a']}"), "--app", "native")
        self.assertEqual((done.returncode, done.stdout), (0, f"NATIVE_BASE={IMAGE}@{DIGEST['a']}\n"), done.stderr)
        done = self.run_script(self.lock("iris", f"{IMAGE}@{DIGEST['a']}", f"{IMAGE}@{DIGEST['b']}"), "--app", "iris")
        self.assertEqual(done.stdout, f"IRIS_TOOLS_BASE={IMAGE}@{DIGEST['a']}\nIRIS_BASE={IMAGE}@{DIGEST['b']}\n", done.stderr)

    def test_a_malformed_lock_fails(self):
        good = f"{IMAGE}@{DIGEST['a']}"
        for why, app, text in [
            ("a tag, not a digest", "native", self.lock("native", f"{IMAGE}:latest")),
            ("another image", "native", self.lock("native", f"ghcr.io/someone/else@{DIGEST['a']}")),
            ("a short digest", "native", self.lock("native", f"{IMAGE}@sha256:abc")),
            ("one base of two", "iris", self.lock("iris", good, "")),
            ("a missing key", "iris", "IRIS_BASE=\n"),
            ("a repeated key", "native", self.lock("native", "") + "NATIVE_BASE=\n"),
            ("another app's key", "native", self.lock("native", "") + "IRIS_BASE=\n"),
            ("no equals sign", "native", "NATIVE_BASE\n"),
        ]:
            with self.subTest(why=why):
                done = self.run_script(text, "--app", app)
                self.assertEqual(done.returncode, 1, done.stdout)
                self.assertIn("apt-base.sh:", done.stderr)

    def test_flags_are_named_and_checked(self):
        for args in [[], ["--app", "qwen"], ["--app", "native", "--check-label", "buildah"], ["native"],
                     ["--print-recipe"], ["--app", "native", "--print-recipe", "--check-label", "docker"]]:
            with self.subTest(args=args):
                self.assertEqual(self.run_script(self.lock("native", ""), *args).returncode, 1)

    def test_the_labels_must_be_the_containerfiles_snapshot_and_recipe(self):
        for app in KEYS:
            snapshot = re.search(r"^ARG SNAPSHOT=(\S+)$", containerfile(app).read_text(), re.M).group(1)
            recipe = subprocess.run(["bash", str(SCRIPT), "--app", app, "--print-recipe"],
                                    capture_output=True, text=True, check=True).stdout.strip()
            self.assertRegex(recipe, r"^[0-9a-f]{64}$")
            text = self.lock(app, *[f"{IMAGE}@{DIGEST[d]}" for d in "abcd"[: len(KEYS[app])]])
            for tool in ("docker", "podman"):
                with self.subTest(app=app, tool=tool, labels="match"):
                    done = self.run_script(text, "--app", app, "--check-label", tool, env=self.fake_tool(tool, snapshot, recipe))
                    self.assertEqual(done.returncode, 0, done.stderr)
                    self.assertEqual(len(done.stdout.splitlines()), len(KEYS[app]))
                    calls = (self.dir / "calls").read_text()
                    for digest in "abcd"[: len(KEYS[app])]:
                        self.assertIn(f"pull --quiet --platform linux/amd64 {IMAGE}@{DIGEST[digest]}", calls)
                for why, labels, message in [
                    ("snapshot", ("20250101T000000Z", recipe), "not the Containerfile's SNAPSHOT"),
                    ("recipe", (snapshot, "0" * 64), "other apt stages"),
                    ("none", ("", ""), "not the Containerfile's SNAPSHOT"),
                ]:
                    with self.subTest(app=app, tool=tool, differs=why):
                        done = self.run_script(text, "--app", app, "--check-label", tool, env=self.fake_tool(tool, *labels))
                        self.assertEqual(done.returncode, 1, done.stdout)
                        self.assertIn(message, done.stderr)
                        self.assertEqual(done.stdout, "")

    def test_the_recipe_covers_the_apt_stages_and_iris_runtime_lock_only(self):
        def recipe(app, body, runtime=None):
            root = self.dir / "tree"
            (root / "scripts/ci").mkdir(parents=True, exist_ok=True)
            (root / f"boinc/apps/{app}").mkdir(parents=True, exist_ok=True)
            (root / "scripts/ci/apt-base.sh").write_text(SCRIPT.read_text())
            (root / f"boinc/apps/{app}/Containerfile").write_text(body)
            (root / f"boinc/apps/{app}/runtime.lock").write_text(runtime or (ROOT / "boinc/apps/iris/runtime.lock").read_text())
            return subprocess.run(["bash", str(root / "scripts/ci/apt-base.sh"), "--app", app, "--print-recipe"],
                                  capture_output=True, text=True, check=True).stdout

        native = containerfile("native").read_text()
        same = recipe("native", native)
        self.assertEqual(recipe("native", native.replace("COPY stubs.c", "COPY  stubs.c")), same)
        self.assertEqual(recipe("native", native.replace("# The apt base (#1683).", "# The base.")), same)
        self.assertNotEqual(recipe("native", native.replace("libtiff-dev", "libtiff-dev libtiff-tools")), same)
        iris = containerfile("iris").read_text()
        same = recipe("iris", iris)
        self.assertEqual(recipe("iris", iris.replace("model.lock boinc", "model.lock  boinc")), same)
        self.assertNotEqual(recipe("iris", iris.replace("python3 python3-venv", "python3 python3-venv python3-pip")), same)
        self.assertNotEqual(recipe("iris", iris, (ROOT / "boinc/apps/iris/runtime.lock").read_text() + "x\n"), same)


class Containerfiles(unittest.TestCase):
    def test_only_the_apt_stages_install_from_the_snapshot(self):
        for app in KEYS:
            for name, (head, body) in stages(containerfile(app).read_text()).items():
                with self.subTest(app=app, stage=name):
                    if name.startswith("apt-"):
                        continue
                    self.assertNotIn("apt-get", body)
                    self.assertNotRegex(body, r"> /etc/apt/sources")
                    self.assertNotIn("Acquire::Check-Valid-Until", body)

    def test_the_bases_default_to_the_apt_stages(self):
        native = containerfile("native").read_text()
        self.assertIn("ARG NATIVE_BASE=apt-native\n", native)
        self.assertEqual([h for h, _ in stages(native).values() if "${NATIVE_BASE}" in h], ["FROM ${NATIVE_BASE}"])
        iris = containerfile("iris").read_text()
        self.assertIn("ARG IRIS_TOOLS_BASE=apt-iris-tools\n", iris)
        self.assertIn("ARG IRIS_BASE=apt-iris\n", iris)
        uses = {name: head for name, (head, _) in stages(iris).items() if "_BASE}" in head}
        self.assertEqual(list(uses.values()), ["FROM ${IRIS_TOOLS_BASE} AS fetch", "FROM ${IRIS_BASE}"])
        self.assertEqual(list(uses)[0], "fetch")
        self.assertEqual(list(uses)[1], list(stages(iris))[-1])
        for text, names in [(native, ["apt-native"]), (iris, ["apt-sources", "apt-iris-tools", "apt-iris-debs", "apt-iris"])]:
            for name in names:
                self.assertIn(name, stages(text))

    def test_the_bases_record_and_check_the_snapshot(self):
        for app, published in [("native", ["apt-native"]), ("iris", ["apt-sources"])]:
            built = stages(containerfile(app).read_text())
            for name in published:
                self.assertIn("LABEL org.usnewsmap.debian-snapshot=${SNAPSHOT}", built[name][1])
            checked = [name for name, (head, body) in built.items() if "_BASE}" in head and "grep -qxF" in body]
            last = list(built)[-1]
            self.assertEqual(checked, [last] if app == "native" else ["fetch", last], checked)

    def test_iris_downloads_each_deb_once_and_the_licence_in_the_fetch_stage(self):
        built = stages(containerfile("iris").read_text())
        self.assertIn("*.deb) dest=", built["apt-iris-debs"][1])
        self.assertIn("LICENSE) continue", built["apt-iris-debs"][1])
        self.assertIn("*.deb) continue", built["fetch"][1])
        self.assertIn("LICENSE) dest=/out/LICENSE", built["fetch"][1])


class Workflows(unittest.TestCase):
    text = BASE_WORKFLOW.read_text()

    def test_the_app_bases_are_built_by_the_matrix_the_locks_name(self):
        include = re.findall(r"- \{app: (\w+), name: ([\w-]+), target: (apt-[\w-]+), context: ([\w./]+)\}", self.text)
        self.assertEqual(sorted(include), [
            ("iris", "iris", "apt-iris", "."),
            ("iris", "iris-tools", "apt-iris-tools", "."),
            ("native", "native", "apt-native", "boinc/apps/native"),
        ])
        for app, name, target, _ in include:
            self.assertIn(f" AS {target}\n", containerfile(app).read_text())
            key = name.upper().replace("-", "_") + "_BASE"
            self.assertIn(key, (ROOT / f"boinc/apps/{app}/base.lock").read_text())

    def test_the_lock_job_waits_for_the_app_bases(self):
        self.assertIn("needs: [publish, publish-apps]", self.text)
        for app in KEYS:
            self.assertIn(f"boinc/apps/{app}/base.lock", self.text)

    def test_the_consuming_workflows_pass_the_pinned_bases(self):
        for workflow, app in [("boinc-ocr-native.yml", "native"), ("boinc-iris.yml", "iris")]:
            text = (ROOT / ".github/workflows" / workflow).read_text()
            with self.subTest(workflow=workflow):
                self.assertIn(f"scripts/ci/apt-base.sh --app {app} --check-label docker", text)
                self.assertIn('"${bases[@]}" -f boinc/apps/', text)
                self.assertRegex(text, r"(?m)^\s+- \"?scripts/ci/apt-base\.sh\"?$")


if __name__ == "__main__":
    unittest.main()
