"""Offline regressions for GPU-study input paths and rental endpoints."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
STUDY = ROOT / "docs/reports/2026-11-gpu-studies/scripts"
WORK = ROOT / f".gpu-study-endpoints-tests-{os.getpid()}"
IMAGE = b"synthetic image bytes\n"


def executable(path, source):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(textwrap.dedent(source), encoding="utf-8")
    path.chmod(0o755)


class GPUStudyEndpointTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        WORK.mkdir()
        cls.addClassCleanup(shutil.rmtree, WORK)

    def setUp(self):
        self.work = WORK / self._testMethodName
        self.work.mkdir()
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.env = {
            "HOME": str(self.work),
            "LC_ALL": "C",
            "PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "TMPDIR": str(self.work),
        }

    def run_script(self, script, env, *args):
        return subprocess.run(
            ["bash", str(script), *args],
            cwd=self.work,
            env=env,
            text=True,
            capture_output=True,
            timeout=30,
            check=False,
        )

    @staticmethod
    def events(path):
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]

    # Executable stand-ins consume existing child-command arguments and never access the network.
    def s7_fixture(self, name, rows):
        case = self.work / name
        inp, out, app, scratch = (
            case / "work/in", case / "work/out", case / "app", case / "scratch"
        )
        for directory in (inp / "images", out, app, scratch, case / "work/external-dir"):
            directory.mkdir(parents=True, exist_ok=True)
        (inp / "images/page one.png").write_bytes(IMAGE)
        (inp / "alias.jpg").symlink_to("images/page one.png")
        (case / "work/external.png").write_bytes(b"outside fixture\n")
        (case / "work/external-dir/external.png").write_bytes(b"outside directory fixture\n")
        (case / "work/external-dir/page one.png").write_bytes(b"outside race fixture\n")
        (case / "work/in-sibling").mkdir()
        (case / "work/in-sibling/external.png").write_bytes(b"outside prefix fixture\n")
        (inp / "file-link.png").symlink_to(case / "work/external.png")
        (inp / "dir-link").symlink_to(case / "work/external-dir", target_is_directory=True)
        (inp / "prefix-link").symlink_to(case / "work/in-sibling", target_is_directory=True)
        (inp / "loop.png").symlink_to("loop.png")
        (inp / "list.tsv").write_text(rows, encoding="utf-8")

        executable(
            self.bin / "realpath",
            """\
            #!/usr/bin/env python3
            import fcntl, os, pathlib, sys
            path = sys.argv[-1]
            root = pathlib.Path(os.environ["INPUT_ROOT"])
            if os.environ.get("SWAP_ROOT") == "1" and path == str(root):
                root.rename(root.with_name("saved-in"))
                root.symlink_to(os.environ["EXTERNAL_DIR"], target_is_directory=True)
            if path.startswith("/proc/self/fd/") and sys.platform == "darwin":
                fd = int(path.rsplit("/", 1)[1])
                data = fcntl.fcntl(fd, fcntl.F_GETPATH, bytes(1024))
                resolved = os.fsdecode(data.split(b"\\0", 1)[0])
            else:
                resolved = str(pathlib.Path(path).resolve(strict=True))
            if os.environ.get("SWAP_MANIFEST") == "1" and path == str(root / "list.tsv"):
                (root / "list.tsv").rename(root / "saved-list.tsv")
                (root / "list.tsv").symlink_to(os.environ["EXTERNAL_LIST"])
            if os.environ.get("SWAP_ON_RESOLVE") == "1" and path == str(root / "images/page one.png"):
                counter = pathlib.Path(os.environ["RESOLVE_COUNT"])
                count = int(counter.read_text()) + 1 if counter.exists() else 1
                counter.write_text(str(count))
                if count == 2:
                    (root / "images").rename(root / "saved-images")
                    (root / "images").symlink_to(os.environ["EXTERNAL_DIR"], target_is_directory=True)
            print(resolved)
            """,
        )
        executable(
            app / "llama-server",
            """\
            #!/usr/bin/env python3
            import json, os, signal, sys, time
            if sys.argv[1:] == ["--version"]:
                print("synthetic server")
                sys.exit(0)
            if sys.argv[1:] == ["--list-devices"]:
                print("CUDA0")
                sys.exit(0)
            signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
            with open(os.environ["SERVER_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(sys.argv[1:]) + "\\n")
            while True:
                time.sleep(3600)
            """,
        )
        executable(
            self.bin / "base64",
            """\
            #!/usr/bin/env python3
            import base64, json, os, pathlib, sys
            args = sys.argv[1:]
            if not args or args[0] != "-w0" or len(args) > 2:
                sys.exit(91)
            if len(args) == 2:
                with open(os.environ["ENCODE_TRACE"], "a", encoding="utf-8") as trace:
                    trace.write(json.dumps(args[1]) + "\\n")
                data = pathlib.Path(args[1]).read_bytes()
            else:
                with open(os.environ["ENCODE_TRACE"], "a", encoding="utf-8") as trace:
                    trace.write(json.dumps("<stdin>") + "\\n")
                if os.environ.get("SWAP_ON_ENCODE") == "1":
                    root = pathlib.Path(os.environ["INPUT_ROOT"])
                    (root / "images").rename(root / "saved-images")
                    (root / "images").symlink_to(os.environ["EXTERNAL_DIR"], target_is_directory=True)
                if os.environ.get("FAIL_IMAGE") == "1":
                    sys.exit(7)
                data = sys.stdin.buffer.read()
            sys.stdout.buffer.write(base64.b64encode(data))
            """,
        )
        executable(
            self.bin / "curl",
            """\
            #!/usr/bin/env python3
            import base64, hashlib, json, os, pathlib, sys
            args = sys.argv[1:]
            if args[-1] == "http://127.0.0.1:8090/health":
                sys.exit(0)
            if args[-1] != "http://127.0.0.1:8090/v1/chat/completions":
                sys.exit(92)
            arg = args[args.index("--data-binary") + 1]
            if not arg.startswith("@"):
                sys.exit(93)
            body = json.loads(pathlib.Path(arg[1:]).read_text(encoding="utf-8"))
            url = body["messages"][0]["content"][0]["image_url"]["url"]
            media, encoded = url.split(",", 1)
            event = {"digest": hashlib.sha256(base64.b64decode(encoded, validate=True)).hexdigest(),
                     "media": media, "max_tokens": body["max_tokens"]}
            with open(os.environ["REQUEST_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(event) + "\\n")
            print('{"choices":[{"message":{"content":"synthetic result"}}]}')
            """,
        )
        executable(self.bin / "nvidia-smi", "#!/bin/sh\necho 42\n")
        executable(self.bin / "sleep", "#!/bin/sh\nexit 0\n")
        executable(
            self.bin / "date",
            """\
            #!/bin/sh
            case "$1" in
              +%s) echo 1700000000;;
              +%s%N) echo 1700000000000000000;;
              *) exit 94;;
            esac
            """,
        )

        script = case / "s7.sh"
        source = (STUDY / "jobs/s7.sh").read_text(encoding="utf-8")
        for original, replacement in (
            ("/tmp/b.json", str(scratch / "body.json")),
            ("/tmp/r.json", str(scratch / "response.json")),
            ("/opt/qwen/models", str(case / "models")),
            ("/app", str(app)),
            ("/work", str(case / "work")),
        ):
            source = source.replace(original, replacement)
        self.assertNotIn("/tmp/", source)
        script.write_text(source, encoding="utf-8")
        env = self.env.copy()
        env.update(
            ENCODE_TRACE=str(case / "encodes"),
            EXTERNAL_DIR=str(case / "work/external-dir"),
            INPUT_ROOT=str(inp),
            REQUEST_TRACE=str(case / "requests"),
            RESOLVE_COUNT=str(case / "resolve-count"),
            SERVER_TRACE=str(case / "servers"),
        )
        return script, inp, out, case, env

    def test_s7_accepts_in_root_images_and_safe_symlinks(self):
        rows = "one\timages/page one.png\t2048\ntwo\talias.jpg\t64\nthree\timages/page one.png\t-1"
        script, _, out, case, env = self.s7_fixture("valid", rows)
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        requests = self.events(case / "requests")
        self.assertEqual(len(requests), 18)
        self.assertTrue(all(item["digest"] == hashlib.sha256(IMAGE).hexdigest() for item in requests))
        self.assertEqual([item["max_tokens"] for item in requests[:3]], [2048, 64, -1])
        self.assertEqual(
            [item["media"] for item in requests[:3]],
            ["data:image/png;base64", "data:image/jpeg;base64", "data:image/png;base64"],
        )
        for variant in ("default", "768", "1024", "1536", "2048", "3072"):
            lines = (out / f"s7-{variant}.b64").read_text(encoding="utf-8").splitlines()
            self.assertEqual([line.split("\t")[0] for line in lines], ["one", "two", "three"])
        servers = self.events(case / "servers")
        self.assertEqual(len(servers), 6)
        for args, variant in zip(servers, ("default", "768", "1024", "1536", "2048", "3072")):
            expected_ctx = 4096 if variant == "default" else max(4096, int(variant) + 2304)
            self.assertEqual(args[args.index("--ctx-size") + 1], str(expected_ctx))
            if variant == "default":
                self.assertNotIn("--image-min-tokens", args)
            else:
                self.assertEqual(args[args.index("--image-min-tokens") + 1], variant)

    def test_s7_rejects_escaping_missing_and_nonfile_paths_before_requests(self):
        paths = (
            "../external.png",
            "images/../../external.png",
            "/absolute/path.png",
            "file-link.png",
            "dir-link/external.png",
            "prefix-link/external.png",
            "loop.png",
            "missing.png",
            "images",
        )
        for index, path in enumerate(paths):
            with self.subTest(path=path):
                rows = f"valid\timages/page one.png\t1\nbad\t{path}\t1\n"
                script, _, _, case, env = self.s7_fixture(f"invalid-{index}", rows)
                result = self.run_script(script, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid", result.stderr.lower())
                self.assertEqual(self.events(case / "requests"), [])
                self.assertEqual(self.events(case / "encodes"), [])

    def test_s7_rejects_symlink_swaps_before_opening_inputs(self):
        script, inp, _, case, env = self.s7_fixture("swap-image", "ok\timages/page one.png\t1\n")
        env["SWAP_ON_RESOLVE"] = "1"
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertTrue((inp / "images").is_symlink())
        self.assertEqual((case / "resolve-count").read_text(encoding="utf-8"), "2")
        self.assertEqual(self.events(case / "encodes"), [])
        self.assertEqual(self.events(case / "requests"), [])

        script, inp, _, case, env = self.s7_fixture("swap-list", "ok\timages/page one.png\t1\n")
        external = case / "external-list.tsv"
        external.write_text((inp / "list.tsv").read_text(encoding="utf-8"), encoding="utf-8")
        env.update(SWAP_MANIFEST="1", EXTERNAL_LIST=str(external))
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertTrue((inp / "list.tsv").is_symlink())
        self.assertEqual(self.events(case / "requests"), [])

        script, inp, _, case, env = self.s7_fixture("swap-root", "ok\timages/page one.png\t1\n")
        external = Path(env["EXTERNAL_DIR"])
        (external / "list.tsv").write_text("ok\tpage one.png\t1\n", encoding="utf-8")
        env["SWAP_ROOT"] = "1"
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertTrue(inp.is_symlink())
        self.assertEqual(self.events(case / "requests"), [])

    def test_s7_reads_the_checked_descriptor_and_stops_on_read_failure(self):
        script, inp, _, case, env = self.s7_fixture("swap-after-open", "ok\timages/page one.png\t1\n")
        env["SWAP_ON_ENCODE"] = "1"
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertTrue((inp / "images").is_symlink())
        requests = self.events(case / "requests")
        self.assertEqual(len(requests), 1)
        self.assertEqual(requests[0]["digest"], hashlib.sha256(IMAGE).hexdigest())

        script, _, _, case, env = self.s7_fixture("failed-read", "ok\timages/page one.png\t1\n")
        env["FAIL_IMAGE"] = "1"
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("failed to read", result.stderr.lower())
        self.assertEqual(self.events(case / "requests"), [])

    def test_s7_rejects_external_manifest_and_malformed_rows(self):
        script, inp, _, case, env = self.s7_fixture("external-list", "ok\timages/page one.png\t1\n")
        external = case / "external-list.tsv"
        external.write_text((inp / "list.tsv").read_text(encoding="utf-8"), encoding="utf-8")
        (inp / "list.tsv").unlink()
        (inp / "list.tsv").symlink_to(external)
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertEqual(self.events(case / "requests"), [])

        script, inp, _, case, env = self.s7_fixture("nul-list", "ok\timages/page one.png\t1\n")
        (inp / "list.tsv").write_bytes(b"ok\timages/page one.png\x00\t1\n")
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertEqual(self.events(case / "requests"), [])

        script, inp, _, case, env = self.s7_fixture("symlink-root", "ok\timages/page one.png\t1\n")
        moved = inp.with_name("in-original")
        inp.rename(moved)
        inp.symlink_to(moved, target_is_directory=True)
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertEqual(self.events(case / "requests"), [])

    def test_s7_empty_list_remains_a_noop(self):
        script, _, _, case, env = self.s7_fixture("empty", "")
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.events(case / "requests"), [])

        for index, row in enumerate((
            "\timages/page one.png\t1\n",
            "bad\t\t1\n",
            "bad\timages/page one.png\t1\textra\n",
            "bad\timages/page one.png\t1},\"extra\":true\n",
        )):
            with self.subTest(row=row):
                script, _, _, case, env = self.s7_fixture(f"malformed-{index}", row)
                result = self.run_script(script, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid", result.stderr.lower())
                self.assertEqual(self.events(case / "requests"), [])

    def watcher_fixture(self, name, entries, limit=3):
        case = self.work / name
        root = case / "study root with spaces"
        (root / "scripts").mkdir(parents=True)
        listing = case / "listing.jsonl"
        listing.write_text("".join(json.dumps(item) + "\n" for item in entries), encoding="utf-8")
        executable(
            root / "scripts/vast.sh",
            """\
            #!/usr/bin/env python3
            import os, pathlib
            count = pathlib.Path(os.environ["VAST_CALLS"])
            n = int(count.read_text()) + 1 if count.exists() else 1
            count.write_text(str(n))
            if n <= int(os.environ["VAST_LIST_LIMIT"]):
                print(pathlib.Path(os.environ["VAST_LISTING"]).read_text(), end="")
            """,
        )
        executable(
            self.bin / "ssh",
            """\
            #!/usr/bin/env python3
            import json, os, sys
            args = sys.argv[1:]
            with open(os.environ["SSH_TRACE"], "a", encoding="utf-8") as trace:
                trace.write(json.dumps(args) + "\\n")
            if args[-1].startswith("wc -l"):
                print("20" if args[-2] == "root@203.0.113.8" else "19")
            elif args[-1] == "pkill -f surya_read.py":
                print("stopped")
            else:
                sys.exit(95)
            """,
        )
        executable(self.bin / "date", "#!/bin/sh\necho 2026-11-02T00:00:00Z\n")
        executable(self.bin / "sleep", "#!/bin/sh\nexit 0\n")
        source = (STUDY / "s4-stop-at-20.sh").read_text(encoding="utf-8")
        source = source.replace("/tmp/kh-w", str(case / "known hosts-"))
        self.assertNotIn("/tmp/", source)
        script = case / "s4-stop-at-20.sh"
        script.write_text(source, encoding="utf-8")
        env = self.env.copy()
        env.update(
            GPU_STUDY_ROOT=str(root),
            GPU_STUDY_SECRETS=str(case / "key directory with spaces"),
            SSH_TRACE=str(case / "ssh-trace"),
            VAST_CALLS=str(case / "vast-calls"),
            VAST_LISTING=str(listing),
            VAST_LIST_LIMIT=str(limit),
        )
        return script, root, case, env

    @staticmethod
    def rental(label, ip="203.0.113.8", port="2222"):
        return {"label": label, "public_ipaddr": ip, "ports": {"22/tcp": [{"HostPort": port}]}}

    def test_watcher_quotes_valid_endpoints_and_stops_at_twenty(self):
        entries = [
            self.rental("usnm-gpu-s4surya3090", port="00022"),
            self.rental("usnm-gpu-s4surya4090", ip="198.51.100.27", port=65535),
        ]
        script, root, case, env = self.watcher_fixture("valid-watch", entries)
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.events(case / "ssh-trace")
        self.assertEqual(len(calls), 3)
        self.assertEqual([args[-1] for args in calls], [
            "wc -l < /work/out/surya-pages.jsonl",
            "pkill -f surya_read.py",
            "wc -l < /work/out/surya-pages.jsonl",
        ])
        for args, host, port in zip(calls, ("203.0.113.8", "203.0.113.8", "198.51.100.27"),
                                    ("00022", "00022", "65535")):
            self.assertEqual(args[args.index("-p") + 1], port)
            self.assertEqual(args[args.index("-i") + 1], env["GPU_STUDY_SECRETS"] + "/rental_ed25519")
            self.assertIn("UserKnownHostsFile=" + str(case / "known hosts-s4surya3090")
                          if host == "203.0.113.8"
                          else "UserKnownHostsFile=" + str(case / "known hosts-s4surya4090"), args)
            self.assertEqual(args[-2], "root@" + host)
        self.assertIn("S4-WATCH-DONE", (root / "s4watch.log").read_text(encoding="utf-8"))

    def test_watcher_rejects_bad_endpoints_without_ssh(self):
        cases = (
            ("203.0.113.256", "22"),
            ("203.0.113.8 -oProxyCommand=bad", "22"),
            ("203.0.113.8\n", "22"),
            ("203.0.113.8\0", "22"),
            (None, "22"),
            ("203.0.113.8", None),
            ("203.0.113.8", "0"),
            ("203.0.113.8", "65536"),
            ("203.0.113.8", "-1"),
            ("203.0.113.8", "22 -oProxyCommand=bad"),
            ("203.0.113.8", "22\n"),
            ("203.0.113.8", "22\0"),
            ("203.0.113.8", " 22"),
        )
        for index, (ip, port) in enumerate(cases):
            with self.subTest(ip=ip, port=port):
                script, _, case, env = self.watcher_fixture(
                    f"bad-watch-{index}", [self.rental("usnm-gpu-s4surya3090", ip, port)]
                )
                result = self.run_script(script, env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("invalid", result.stderr.lower())
                self.assertEqual(self.events(case / "ssh-trace"), [])

        entries = [
            self.rental("usnm-gpu-s4surya3090-a", port=None),
            self.rental("usnm-gpu-s4surya3090-b", ip=None),
        ]
        script, _, case, env = self.watcher_fixture("ambiguous-watch", entries)
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("invalid", result.stderr.lower())
        self.assertEqual(self.events(case / "ssh-trace"), [])

    def test_watcher_handles_absent_jobs_and_port_lower_bound(self):
        script, root, case, env = self.watcher_fixture("no-rentals", [])
        env.pop("GPU_STUDY_SECRETS")
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.events(case / "ssh-trace"), [])
        log = (root / "s4watch.log").read_text(encoding="utf-8")
        self.assertEqual(log.count(" -1\n"), 2)
        self.assertIn("S4-WATCH-DONE", log)

        entries = [self.rental("usnm-gpu-s4surya3090", ip="255.255.255.255", port="1")]
        script, _, case, env = self.watcher_fixture("minimum-port", entries, limit=2)
        result = self.run_script(script, env)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.events(case / "ssh-trace")
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][calls[0].index("-p") + 1], "1")
        self.assertEqual(calls[0][-2], "root@255.255.255.255")

        script, _, case, env = self.watcher_fixture("missing-ssh-key", entries)
        env.pop("GPU_STUDY_SECRETS")
        result = self.run_script(script, env)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("GPU_STUDY_SECRETS", result.stderr)
        self.assertEqual(self.events(case / "ssh-trace"), [])

    def vast_fixture(self):
        executable(
            self.bin / "curl",
            """\
            #!/usr/bin/env python3
            import os, pathlib, sys
            args = sys.argv[1:]
            config = pathlib.Path(args[args.index("-K") + 1]).read_text(encoding="utf-8")
            if os.environ["EXPECTED_TEST_TOKEN"] not in config:
                sys.exit(96)
            pathlib.Path(os.environ["CURL_TRACE"]).write_text("called\\n", encoding="utf-8")
            print('{"credit": 1.25}')
            """,
        )
        env = self.env.copy()
        env["CURL_TRACE"] = str(self.work / "curl-trace")
        return env

    def assert_vast_success(self, result, tokens):
        if any(token in result.stdout or token in result.stderr for token in tokens):
            self.fail("API key material appeared in command output")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "1.25")

    def test_vast_cli_key_works_without_related_environment_and_is_not_printed(self):
        env = self.vast_fixture()
        token = "synthetic-cli-key-only"
        key = self.work / "synthetic key with spaces"
        key.write_text(token + "\n", encoding="utf-8")
        env["EXPECTED_TEST_TOKEN"] = token
        result = self.run_script(STUDY / "vast.sh", env, "--action", "credit", "--key-file", str(key))
        self.assert_vast_success(result, (token,))
        trace = Path(env["CURL_TRACE"]).read_text(encoding="utf-8")
        if token in trace:
            self.fail("API key material appeared in the trace")
        self.assertEqual(trace, "called\n")

    def test_vast_key_precedence_and_missing_key_fail_closed(self):
        env = self.vast_fixture()
        secrets_dir = self.work / "synthetic secrets"
        secrets_dir.mkdir()
        (secrets_dir / "vast.key").write_text("synthetic-study-key\n", encoding="utf-8")
        env_key = self.work / "env.key"
        env_key.write_text("synthetic-env-key\n", encoding="utf-8")
        cli_key = self.work / "cli.key"
        cli_key.write_text("synthetic-override-key\n", encoding="utf-8")
        tokens = ("synthetic-study-key", "synthetic-env-key", "synthetic-override-key")
        for source, variables, token, arguments in (
            ("study", {"GPU_STUDY_SECRETS": str(secrets_dir)}, tokens[0], ()),
            ("environment-only", {"VAST_KEY_FILE": str(env_key)}, tokens[1], ()),
            ("environment", {"GPU_STUDY_SECRETS": str(secrets_dir), "VAST_KEY_FILE": str(env_key)},
             tokens[1], ()),
            ("cli", {"GPU_STUDY_SECRETS": str(secrets_dir), "VAST_KEY_FILE": str(env_key)},
             tokens[2], ("--key-file", str(cli_key))),
        ):
            with self.subTest(source=source):
                test_env = {**env, **variables, "EXPECTED_TEST_TOKEN": token}
                result = self.run_script(STUDY / "vast.sh", test_env, "--action", "credit", *arguments)
                self.assert_vast_success(result, tokens)

        Path(env["CURL_TRACE"]).unlink()
        env["EXPECTED_TEST_TOKEN"] = "unused-synthetic-token"
        for args in ((), ("--key-file", ""), ("--key-file", str(self.work / "missing.key")),
                     ("--key-file", str(self.work))):
            with self.subTest(args=args):
                result = self.run_script(STUDY / "vast.sh", env, "--action", "credit", *args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("key", result.stderr.lower())
                self.assertFalse(Path(env["CURL_TRACE"]).exists())


if __name__ == "__main__":
    unittest.main()
