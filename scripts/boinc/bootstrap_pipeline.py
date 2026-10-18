#!/usr/bin/env python3
"""Reviewed BOINC bootstrap pipeline. No signing keys enter this process."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[2]
REPO = "tgoodyear/usnewsmap.com"
PHASES = ("base", "server", "worker", "review-runtime", "runtime", "sign",
          "review-app", "publish", "acceptance")
DIGEST = r"@sha256:[0-9a-f]{64}"


class PipelineError(Exception):
    pass


class Blocked(PipelineError):
    pass


def run(argv, cwd=ROOT, timeout=14400):
    process = subprocess.Popen([str(x) for x in argv], cwd=cwd, text=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    try:
        out, _ = process.communicate(timeout=timeout)
    except BaseException:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            for stream in (process.stdout, process.stderr):
                stream.close()
        raise
    if process.returncode:
        raise PipelineError(f"{Path(argv[0]).name} failed (exit {process.returncode}); output suppressed")
    return out.rstrip("\n")


def atomic(path, value):
    staging = path.with_suffix(".new")
    fd = os.open(staging, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(staging, path)


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def load(path):
    config = json.loads(path.read_text())
    required = {"env", "app", "version", "platform", "plan_class", "signing_host", "acceptance_config"}
    if set(config) != required:
        raise PipelineError("configuration must contain exactly the documented fields")
    if not all(isinstance(v, str) for v in config.values()):
        raise PipelineError("configuration fields must be strings")
    if config["env"] not in ("dev", "prod"):
        raise PipelineError("invalid environment")
    if config["platform"] != "x86_64-pc-linux-gnu":
        raise PipelineError("bootstrap currently supports Linux x86_64 container releases only")
    if (config["app"], config["plan_class"]) not in (
            ("usnm_ocr", "docker"), ("usnm_qwen", "docker_mt_2b"),
            ("usnm_qwen", "docker_nvidia_cuda_8b")):
        raise PipelineError("unsupported app/plan-class worker publication contract")
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]{1,2}", config["version"]):
        raise PipelineError("invalid version")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]*", config["signing_host"]):
        raise PipelineError("invalid signing host")
    acceptance = (path.resolve().parent / config["acceptance_config"]).resolve()
    if not acceptance.is_file():
        raise PipelineError("acceptance config is missing")
    # Configuration is delegated to #548; no authenticators or arbitrary argv are persisted here.
    json.loads(acceptance.read_text())
    config["acceptance_config"] = str(acceptance)
    return config, fingerprint({"config": config, "acceptance_sha256": hashlib.sha256(acceptance.read_bytes()).hexdigest()})


def check_images(images, keys):
    for key in keys:
        prefix = (r"ghcr\.io/tgoodyear/usnm-boinc-worker" if key == "worker" else
                  r"[a-z0-9.-]+\.azurecr\.io/usnm-boinc-" + key)
        if not re.fullmatch(prefix + DIGEST, images.get(key, "")):
            raise PipelineError("missing immutable " + key + " artifact")


class Pipeline:
    def __init__(self, args, config, identity, runner=run):
        self.args, self.config, self.identity, self.run = args, config, identity, runner
        self.directory = args.state_dir.resolve()
        self.path = self.directory / "state.json"
        self.state = json.loads(self.path.read_text()) if self.path.exists() else {
            "schema": 1, "identity": identity, "id": uuid.uuid4().hex,
            "done": [], "workspaces": {}, "reviews": {}}
        if self.state.get("schema") != 1 or self.state.get("identity") != identity:
            raise PipelineError("state/config identity mismatch; recovery never resets existing state")
        self.settings = f"contrib-compute/infra/boinc/env/{config['env']}.json"

    def save(self):
        atomic(self.path, self.state)

    def git(self, *args, cwd=ROOT):
        return self.run(["git", *args], cwd=cwd)

    def gh(self, *args):
        return self.run(["gh", *args, "-R", REPO], timeout=120)

    def main(self):
        self.git("fetch", "origin", "main", "--quiet")
        return self.git("rev-parse", "origin/main")

    def preflight(self):
        if self.git("status", "--porcelain", "--untracked-files=normal"):
            raise PipelineError("clean source tree required")
        if self.git("remote", "get-url", "origin") not in (
                "https://github.com/" + REPO + ".git", "git@github.com:" + REPO + ".git",
                "https://github.com/" + REPO):
            raise PipelineError("unexpected origin repository")
        sha = self.main()
        files = self.git("ls-tree", "-r", "--name-only", sha).splitlines()
        for name in (self.settings, "scripts/provision-boinc.sh", "scripts/build-boinc-images.sh",
                     "scripts/boinc-app-release.sh", "scripts/boinc/acceptance.py"):
            if name not in files:
                dependency = " (#807/#548 must merge first)" if name == "scripts/boinc/acceptance.py" else ""
                raise PipelineError("missing reviewed main entrypoint: " + name + dependency)
        if not self.args.dry_run and not re.fullmatch(
                r"https://github.com/tgoodyear/usnewsmap.com/issues/529#issuecomment-[0-9]+",
                self.args.coordination_claim or ""):
            raise PipelineError("execution requires an active #529 coordination claim (not deployment authorization)")
        settings = json.loads(self.git("show", sha + ":" + self.settings))
        if not settings.get("codeSignPublicKey"):
            raise PipelineError("reviewed public signing key is missing")
        if not (ROOT / "scripts/boinc/acceptance.py").is_file():
            raise PipelineError("local checkout lacks #548 entrypoint; reviewed main must be present locally too")
        # Exercise #548's offline parser before the first resource mutation.
        self.acceptance(ROOT, dry_run=True)

    def workspace(self, label):
        entry = self.state["workspaces"].get(label)
        if entry is None:
            entry = {"path": str(self.directory / label), "commit": self.main()}
            self.state["workspaces"][label] = entry
            self.save()
        path = Path(entry["path"])
        if not path.exists():
            self.git("worktree", "add", "--detach", str(path), entry["commit"])
        if self.git("rev-parse", "--show-toplevel", cwd=path) != str(path):
            raise PipelineError("workspace identity mismatch")
        self.git("merge-base", "--is-ancestor", entry["commit"], "HEAD", cwd=path)
        changed = self.git("diff", "--name-only", entry["commit"], "HEAD", cwd=path).splitlines()
        if any(name != self.settings for name in changed):
            raise PipelineError("workspace code differs from its reviewed main source")
        return path

    def read_settings(self, work):
        return json.loads((work / self.settings).read_text())

    def script(self, work, name, *args):
        if self.git("status", "--porcelain", "--untracked-files=normal", cwd=work):
            raise PipelineError("clean lifecycle workspace required; unreviewed local edits cannot deploy")
        self.run([str(work / "scripts" / name), "--env", self.config["env"], *args], cwd=work)

    def acceptance(self, work, dry_run=False):
        argv = [sys.executable, str(work / "scripts/boinc/acceptance.py"), "--env", self.config["env"],
                "--config", self.config["acceptance_config"], "--evidence", str(self.directory / "acceptance.json")]
        for key in ("app", "version", "platform", "plan_class"):
            argv.extend(["--" + key.replace("_", "-"), self.config[key]])
        if dry_run:
            argv.append("--dry-run")
        elif self.args.coordination_claim:
            argv.extend(["--coordination-claim", self.args.coordination_claim])
        self.run(argv, cwd=work)
        if not dry_run:
            self.state["acceptance_sha256"] = self.receipt()
            self.save()

    def receipt(self):
        try:
            raw = (self.directory / "acceptance.json").read_bytes()
            evidence = json.loads(raw)
        except (OSError, ValueError) as exc:
            raise PipelineError("durable acceptance evidence missing or unreadable") from exc
        major, minor = self.config["version"].split(".")
        release = {"app": self.config["app"], "version_num": int(major) * 100 + int(minor),
                   "platform": self.config["platform"], "plan_class": self.config["plan_class"]}
        if evidence.get("accepted") is not True or evidence.get("cleanup_complete") is not True or evidence.get("release") != release:
            raise PipelineError("canonical acceptance or cleanup evidence failed")
        digest = hashlib.sha256(raw).hexdigest()
        if self.state.get("acceptance_sha256", digest) != digest:
            raise PipelineError("durable acceptance evidence changed after completion")
        return digest

    def worker(self, work):
        workflow = "boinc-qwen.yml" if self.config["app"] == "usnm_qwen" else "boinc-worker.yml"
        correlation = "bootstrap-" + self.state["id"]
        fields = "databaseId,displayTitle,headSha,status,conclusion"
        known_run = self.state.get("worker_run")
        if known_run:
            runs = [json.loads(self.gh("run", "view", str(known_run), "--json", fields))]
        else:
            runs = json.loads(self.gh("run", "list", "--workflow", workflow, "--event", "workflow_dispatch",
                                     "--branch", "main", "--limit", "100", "--json", fields))
        matching = [r for r in runs if r["displayTitle"] == workflow[:-4] + " (" + correlation + ")"]
        if len(matching) > 1:
            raise PipelineError("ambiguous worker workflow dispatch")
        if not matching:
            if self.state.get("worker_dispatched"):
                raise Blocked("worker dispatch not visible; refusing duplicate publication")
            self.state["worker_dispatched"] = True
            self.save()
            self.gh("workflow", "run", workflow, "--ref", "main", "-f", "correlation=" + correlation)
            raise Blocked("worker build dispatched; awaiting terminal workflow result")
        build = matching[0]
        self.state["worker_run"] = build["databaseId"]
        self.save()
        if build["status"] != "completed":
            raise Blocked("worker build is still running")
        if build["conclusion"] != "success":
            raise PipelineError("worker publication workflow failed")
        prefix = ""
        if self.config["app"] == "usnm_qwen":
            prefix = "qwen-2b-" if self.config["plan_class"] == "docker_mt_2b" else "qwen-8b-"
        image = public_worker(prefix + build["headSha"])
        settings = self.read_settings(work)
        settings["images"]["worker"] = image
        check_images(settings["images"], ("server", "bridge", "worker"))
        atomic(work / self.settings, settings)
        self.state["worker_build"] = build["databaseId"]
        self.state["worker_commit"] = build["headSha"]
        self.save()

    def review(self, label, work, keys):
        settings = self.read_settings(work)
        check_images(settings["images"], keys)
        baseline = json.loads(self.git("show", "HEAD:" + self.settings, cwd=work))
        allowed = {"server", "bridge", "worker"} if label == "runtime" else {"appver"}
        expected = json.loads(json.dumps(baseline))
        for key in allowed:
            expected["images"][key] = settings["images"][key]
        if expected != settings:
            raise PipelineError("preparation modified configuration outside allowed artifact pins")
        reviews = self.state["reviews"]
        if label not in reviews:
            reviews[label] = {"branch": "boinc-bootstrap-" + self.state["id"] + "-" + label,
                              "settings": settings}
            self.save()
        review = reviews[label]
        if review["settings"] != settings:
            raise PipelineError("candidate changed after review intent was persisted")
        branch = review["branch"]
        dirty = self.git("status", "--porcelain", "--untracked-files=normal", cwd=work).splitlines()
        if any(line[3:] != self.settings for line in dirty):
            raise PipelineError("artifact preparation changed files outside the settings allowlist")
        if dirty:
            if self.git("branch", "--show-current", cwd=work) != branch:
                self.git("switch", "-c", branch, cwd=work)
            self.git("add", "--", self.settings, cwd=work)
            message = ("chore(boinc): review bootstrap " + label + " pins\n\nRefs #551.\n\n"
                       "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
            self.git("commit", "-m", message, cwd=work)
        elif self.git("branch", "--show-current", cwd=work) != branch:
            # Existing settings need no candidate PR, but still must match reviewed main.
            current = json.loads(self.git("show", self.main() + ":" + self.settings))
            if current != settings:
                raise PipelineError("candidate is missing its review commit")
            return
        prs = json.loads(self.gh("pr", "list", "--head", branch, "--state", "all", "--json", "number,state"))
        if not prs:
            self.git("push", "--set-upstream", "origin", branch, cwd=work)
            self.gh("pr", "create", "--base", "main", "--head", branch,
                    "--title", "chore(boinc): review bootstrap " + label + " pins",
                    "--body", "Refs #551. Immutable pipeline candidate; no auto-merge or deployment. "
                    "Maintainer review and required CI must pass before merging.")
            raise Blocked("artifact review PR created; awaiting reviewed merge")
        if len(prs) != 1 or prs[0]["state"] != "MERGED":
            raise Blocked("artifact review has not merged; no deployment permitted")
        approval = json.loads(self.gh("pr", "view", str(prs[0]["number"]), "--json",
                                      "baseRefName,reviewDecision,statusCheckRollup"))
        checks = approval.get("statusCheckRollup") or []
        if (approval.get("baseRefName") != "main" or approval.get("reviewDecision") != "APPROVED"
                or not checks or any(check.get("conclusion", check.get("state")) not in
                                     ("SUCCESS", "SKIPPED", "NEUTRAL") for check in checks)):
            raise Blocked("merged candidate lacks recorded approval and successful CI")
        current = json.loads(self.git("show", self.main() + ":" + self.settings))
        if current != review["settings"]:
            raise PipelineError("reviewed main pins/config differ from the candidate; refusing stale rollout")
        self.state["reviews"][label]["pr"] = prs[0]["number"]
        self.save()

    def phase(self, phase):
        if phase == "base":
            work = self.workspace("base")
            images = self.read_settings(work)["images"]
            if not images.get("server") and not images.get("bridge"):
                self.script(work, "provision-boinc.sh")
            else:
                check_images(images, ("server", "bridge"))
        elif phase in ("server", "worker", "review-runtime"):
            work = self.workspace("artifacts")
            if phase == "server":
                self.script(work, "build-boinc-images.sh")
            elif phase == "worker":
                self.worker(work)
            else:
                self.review("runtime", work, ("server", "bridge", "worker"))
        elif phase == "runtime":
            work = self.workspace("runtime")
            self.assert_reviewed(work, "runtime")
            self.script(work, "provision-boinc.sh")
        elif phase in ("sign", "review-app"):
            work = self.workspace("signing")
            if phase == "sign":
                self.assert_reviewed(work, "runtime")
                if self.git("status", "--porcelain", cwd=work):
                    raise PipelineError("signing requires a clean reviewed checkout")
                args = ["--prepare-only", "--signing-host", self.config["signing_host"],
                        "--worker-image", self.read_settings(work)["images"]["worker"]]
                for key in ("app", "version", "platform", "plan_class"):
                    args.extend(["--" + key.replace("_", "-"), self.config[key]])
                try:
                    self.script(work, "boinc-app-release.sh", *args)
                except PipelineError as exc:
                    raise PipelineError("isolated signing failed (including locked/unreachable signing service); never unlock or bypass") from exc
            else:
                self.review("app", work, ("server", "bridge", "worker", "appver"))
        elif phase in ("publish", "acceptance"):
            work = self.workspace("release")
            self.assert_reviewed(work, "app")
            if phase == "publish":
                self.script(work, "provision-boinc.sh")
            else:
                self.acceptance(work)

    def assert_reviewed(self, work, label):
        expected = self.state["reviews"][label]["settings"]
        current = json.loads(self.git("show", self.main() + ":" + self.settings))
        if self.read_settings(work) != expected or current != expected:
            raise PipelineError("reviewed settings changed; refusing stale or unreviewed deployment")

    def execute(self):
        self.preflight()
        if self.args.dry_run:
            return {"dry_run": True, "phases": list(PHASES), "mutations": False}
        active = self.state.get("active")
        if active and self.state.get("status") in ("running", "failed"):
            if self.args.approve_retry != active:
                raise PipelineError("ambiguous/failed phase " + active + "; explicit --approve-retry required after reconciliation")
        for phase in PHASES:
            if phase in self.state["done"]:
                continue
            self.state.update(active=phase, status="running")
            self.save()
            try:
                self.phase(phase)
            except Blocked:
                self.state["status"] = "blocked"
                self.save()
                raise
            except BaseException:
                self.state["status"] = "failed"
                self.save()
                raise
            self.state["done"].append(phase)
            self.state.update(active=None, status="ready")
            self.save()
        self.receipt()
        return {"complete": True, "evidence": str(self.directory / "acceptance.json")}


def public_worker(tag):
    repository = "tgoodyear/usnm-boinc-worker"
    with urllib.request.urlopen("https://ghcr.io/token?scope=repository:" + repository + ":pull", timeout=30) as response:
        token = json.load(response)["token"]
    request = urllib.request.Request("https://ghcr.io/v2/" + repository + "/manifests/" + tag,
                                     headers={"Authorization": "Bearer " + token, "Accept":
                                              "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = response.read()
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        if response.headers.get("Docker-Content-Digest") != digest:
            raise PipelineError("public worker registry checksum mismatch")
    return "ghcr.io/" + repository + "@" + digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--coordination-claim")
    parser.add_argument("--approve-retry", choices=PHASES)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    try:
        config, identity = load(args.config)
        directory = args.state_dir.resolve()
        if not directory.is_relative_to(ROOT / "target"):
            raise PipelineError("state-dir must be below this checkout's ignored target directory")
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(directory, 0o700)
        with (directory / "lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as exc:
                raise PipelineError("pipeline state is already locked") from exc
            for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
                signal.signal(sig, lambda *_: (_ for _ in ()).throw(PipelineError("interrupted")))
            result = Pipeline(args, config, identity).execute()
            print(json.dumps(result, sort_keys=True))
        return 0
    except Blocked as exc:
        print("pipeline blocked: " + str(exc), file=sys.stderr)
        return 3
    except Exception as exc:
        # No subprocess stderr, authenticators, host output, or arbitrary JSON is logged.
        print("pipeline failed: " + (str(exc) if isinstance(exc, PipelineError) else type(exc).__name__), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
