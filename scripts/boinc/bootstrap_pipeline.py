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


def run(argv, cwd=ROOT, timeout=14400, cleanup_timeout=30):
    process = subprocess.Popen([str(x) for x in argv], cwd=cwd, text=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True)
    try:
        out, _ = process.communicate(timeout=timeout)
    except BaseException:
        # Repeated operator signals must not interrupt the child's bounded teardown.
        handlers = {sig: signal.signal(sig, signal.SIG_IGN)
                    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        try:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.communicate(timeout=cleanup_timeout)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    process.communicate(timeout=1)
                except subprocess.TimeoutExpired:
                    for stream in (process.stdout, process.stderr):
                        stream.close()
        finally:
            for sig, handler in handlers.items():
                signal.signal(sig, handler)
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


def approved_review(value, authorized=lambda _: False):
    decision = value.get("reviewDecision") or ""
    if decision not in ("", "APPROVED"):
        return False
    latest = {}
    for review in value.get("reviews") or []:
        if review["state"] != "COMMENTED" or review["author"]["login"] == "copilot-pull-request-reviewer":
            latest[review["author"]["login"]] = review
    if any(r["state"] == "CHANGES_REQUESTED" for r in latest.values()):
        return False
    if value.get("reviewDecision") == "APPROVED":
        return True
    head = value.get("headRefOid")
    for author, review in latest.items():
        if not head or review.get("commit", {}).get("oid") != head:
            continue
        if review["state"] == "APPROVED" and authorized(author):
            return True
        if author == "copilot-pull-request-reviewer" and re.search(
                r"^### (?:🟢 Approval recommended|🔵 Needs a closer look)$", review.get("body", ""), re.MULTILINE):
            return True
    return False


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
        self.worker_pin = (f"contrib-compute/infra/boinc/releases/{config['env']}-{config['app']}-{config['plan_class']}.json"
                           if config["app"] == "usnm_qwen" else None)

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
        for name in (self.settings, "scripts/boinc/provision-boinc.sh", "scripts/boinc/build-boinc-images.sh",
                     "scripts/boinc/boinc-app-release.sh", "scripts/boinc/acceptance.py"):
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
        dirty = self.git("status", "--porcelain", "--untracked-files=all", cwd=path).splitlines()
        allowed = {self.settings, self.worker_pin}
        if any(name not in allowed for name in changed) or any(line[3:] not in allowed for line in dirty):
            raise PipelineError("workspace code differs from its reviewed main source")
        return path

    def read_settings(self, work):
        return json.loads((work / self.settings).read_text())

    def script(self, work, name, *args):
        if self.git("status", "--porcelain", "--untracked-files=normal", cwd=work):
            raise PipelineError("clean lifecycle workspace required; unreviewed local edits cannot deploy")
        self.run([str(work / "scripts" / "boinc" / name), "--env", self.config["env"], *args], cwd=work)

    def acceptance(self, work, dry_run=False):
        argv = [sys.executable, str(work / "scripts/boinc/acceptance.py"), "--env", self.config["env"],
                "--config", self.config["acceptance_config"], "--evidence", str(self.directory / "acceptance.json"),
                "--cleanup-timeout", "600"]
        for key in ("app", "version", "platform", "plan_class"):
            argv.extend(["--" + key.replace("_", "-"), self.config[key]])
        if dry_run:
            argv.append("--dry-run")
        elif self.args.coordination_claim:
            argv.extend(["--coordination-claim", self.args.coordination_claim])
        self.run(argv, cwd=work, cleanup_timeout=660 if not dry_run else 30)
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
        image = self.worker_image(build)
        settings = self.read_settings(work)
        if self.worker_pin:
            pin = dict(schema=1, env=self.config["env"], app=self.config["app"],
                       platform=self.config["platform"], plan_class=self.config["plan_class"],
                       image=image, source_commit=build["headSha"], workflow_run=build["databaseId"],
                       workflow_attempt=self.state["worker_attempt"])
            path = work / self.worker_pin
            path.parent.mkdir(parents=True, exist_ok=True)
            atomic(path, pin)
        else:
            settings["images"]["worker"] = image
            atomic(work / self.settings, settings)
        check_images(settings["images"], self.image_keys())
        self.state["worker_build"] = build["databaseId"]
        self.state["worker_commit"] = build["headSha"]
        self.save()

    def worker_image(self, build):
        detail = json.loads(self.gh("run", "view", str(build["databaseId"]), "--json",
                                    "databaseId,headSha,attempt,status,conclusion"))
        if detail["databaseId"] != build["databaseId"] or detail["headSha"] != build["headSha"]:
            raise PipelineError("worker run identity changed")
        if detail["status"] != "completed" or detail["conclusion"] != "success":
            raise Blocked("worker run attempt is not terminal successful")
        model = "worker" if self.config["app"] == "usnm_ocr" else (
            "qwen-2b" if self.config["plan_class"] == "docker_mt_2b" else "qwen-8b")
        attempt = self.successful_attempt(build, detail, model)
        directory = self.directory / f"worker-receipt-{build['databaseId']}-{attempt}"
        directory.mkdir(mode=0o700, exist_ok=True)
        receipt = directory / "final.json"
        if not receipt.exists():
            self.gh("run", "download", str(build["databaseId"]), "--name",
                    f"boinc-bootstrap-{model}-{attempt}", "--dir", str(directory))
        value = json.loads(receipt.read_text())
        expected = {"schema": 1, "run_id": str(build["databaseId"]), "run_attempt": str(attempt),
                    "commit": build["headSha"], "model": model, "image": value.get("image")}
        if value != expected:
            raise PipelineError("worker receipt does not match successful run/attempt/model/source")
        check_images({"worker": value["image"]}, ("worker",))
        image = public_worker(value["image"].split("@", 1)[1])
        if image != value["image"]:
            raise PipelineError("public worker digest differs from run-specific receipt")
        self.state["worker_receipt_sha256"] = fingerprint(value)
        self.state["worker_attempt"] = attempt
        self.save()
        return image

    def successful_attempt(self, build, detail, model):
        job_name = ("worker image (publish multi-architecture index)" if model == "worker" else
                    "qwen worker image (build, push) (" + model.removeprefix("qwen-") + ")")
        for attempt in range(detail["attempt"], 0, -1):
            jobs = json.loads(self.gh("run", "view", str(build["databaseId"]), "--attempt", str(attempt), "--json", "jobs"))["jobs"]
            selected = [job for job in jobs if job["name"] == job_name]
            if not selected:
                continue
            if len(selected) != 1 or selected[0]["status"] != "completed" or selected[0]["conclusion"] != "success":
                raise PipelineError("selected worker model's latest job attempt did not succeed")
            return attempt
        raise PipelineError("successful selected worker job attempt is missing")

    def image_keys(self, appver=False):
        return ("server", "bridge") + (() if self.worker_pin else ("worker",)) + (("appver",) if appver else ())

    def read_worker_pin(self, work):
        if not self.worker_pin:
            return None
        value = json.loads((work / self.worker_pin).read_text())
        expected = {"schema", "env", "app", "platform", "plan_class", "image", "source_commit", "workflow_run", "workflow_attempt"}
        if set(value) != expected or any(value[k] != self.config[k] for k in ("env", "app", "platform", "plan_class")) or value["schema"] != 1:
            raise PipelineError("app-specific worker pin identity mismatch")
        check_images({"worker": value["image"]}, ("worker",))
        if not re.fullmatch(r"[0-9a-f]{40}", value["source_commit"]) or any(
                not isinstance(value[k], int) or value[k] < 1 for k in ("workflow_run", "workflow_attempt")):
            raise PipelineError("app-specific worker provenance is invalid")
        return value

    def worker_for_signing(self, work):
        return self.read_worker_pin(work)["image"] if self.worker_pin else self.read_settings(work)["images"]["worker"]

    def review(self, label, work, keys):
        settings = self.read_settings(work)
        check_images(settings["images"], keys)
        source = self.state["workspaces"]["artifacts" if label == "runtime" else "signing"]["commit"]
        baseline = json.loads(self.git("show", source + ":" + self.settings, cwd=work))
        allowed = ({"server", "bridge"} | (set() if self.worker_pin else {"worker"})) if label == "runtime" else {"appver"}
        expected = json.loads(json.dumps(baseline))
        for key in allowed:
            expected["images"][key] = settings["images"][key]
        if expected != settings:
            raise PipelineError("preparation modified configuration outside allowed artifact pins")
        worker_pin = self.read_worker_pin(work)
        if label == "app" and self.worker_pin:
            original = json.loads(self.git("show", source + ":" + self.worker_pin, cwd=work))
            if original != worker_pin:
                raise PipelineError("signing changed the reviewed app-specific worker pin")
        reviews = self.state["reviews"]
        if label not in reviews:
            reviews[label] = {"branch": "boinc-bootstrap-" + self.state["id"] + "-" + label,
                              "settings": settings, "worker_pin": worker_pin}
            self.save()
        review = reviews[label]
        if review["settings"] != settings or review.get("worker_pin") != worker_pin:
            raise PipelineError("candidate changed after review intent was persisted")
        branch = review["branch"]
        dirty = self.git("status", "--porcelain", "--untracked-files=all", cwd=work).splitlines()
        allowed_paths = {self.settings} | ({self.worker_pin} if label == "runtime" and self.worker_pin else set())
        if any(line[3:] not in allowed_paths for line in dirty):
            raise PipelineError("artifact preparation changed files outside the settings allowlist")
        if dirty:
            if self.git("branch", "--show-current", cwd=work) != branch:
                self.git("switch", "-c", branch, cwd=work)
            self.git("add", "--", *sorted(allowed_paths), cwd=work)
            message = ("chore(boinc): review bootstrap " + label + " pins\n\nRefs #551.\n\n"
                       "Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>")
            self.git("commit", "-m", message, cwd=work)
        elif self.git("branch", "--show-current", cwd=work) != branch:
            # Existing settings need no candidate PR, but still must match reviewed main.
            main_sha = self.main()
            current = json.loads(self.git("show", main_sha + ":" + self.settings))
            current_pin = json.loads(self.git("show", main_sha + ":" + self.worker_pin)) if self.worker_pin else None
            if current != settings or current_pin != worker_pin:
                raise PipelineError("candidate is missing its review commit")
            return
        prs = json.loads(self.gh("pr", "list", "--head", branch, "--state", "all", "--json", "number,state"))
        if not prs:
            self.git("push", "--set-upstream", "origin", branch, cwd=work)
            self.gh("pr", "create", "--base", "main", "--head", branch,
                    "--title", "chore(boinc): review bootstrap " + label + " pins",
                    "--body", "Refs #551. Immutable pipeline candidate; no auto-merge or deployment. "
                    "Maintainer review and required CI must pass before merging.")
            self.request_review(review, branch)
            raise Blocked("artifact review PR created; awaiting reviewed merge")
        if len(prs) != 1 or prs[0]["state"] != "MERGED":
            if len(prs) == 1 and prs[0]["state"] == "OPEN":
                self.request_review(review, branch)
            raise Blocked("artifact review has not merged; no deployment permitted")
        approval = json.loads(self.gh("pr", "view", str(prs[0]["number"]), "--json",
                                      "baseRefName,headRefOid,reviewDecision,reviews,statusCheckRollup"))
        checks = approval.get("statusCheckRollup") or []
        if (approval.get("baseRefName") != "main" or not approved_review(approval, self.authorized_reviewer)
                or not checks or any(check.get("conclusion", check.get("state")) not in
                                     ("SUCCESS", "SKIPPED", "NEUTRAL") for check in checks)):
            raise Blocked("merged candidate lacks recorded approval and successful CI")
        if self.has_open_findings(prs[0]["number"]):
            raise Blocked("artifact review has unresolved findings")
        main_sha = self.main()
        current = json.loads(self.git("show", main_sha + ":" + self.settings))
        main_pin = json.loads(self.git("show", main_sha + ":" + self.worker_pin)) if self.worker_pin else None
        if current != review["settings"] or main_pin != review.get("worker_pin"):
            raise PipelineError("reviewed main pins/config differ from the candidate; refusing stale rollout")
        self.state["reviews"][label]["pr"] = prs[0]["number"]
        self.save()

    def authorized_reviewer(self, login):
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}", login):
            return False
        value = json.loads(self.run(["gh", "api", f"repos/{REPO}/collaborators/{login}/permission"], timeout=120))
        return value.get("permission") in ("admin", "maintain", "write")

    def request_review(self, review, branch):
        if not review.get("review_requested"):
            # Persist intent first; an ambiguous API failure must not silently request another round.
            review["review_requested"] = True
            self.save()
            self.gh("pr", "edit", branch, "--add-reviewer", "@copilot")

    def has_open_findings(self, number):
        query = ('query($number:Int!){repository(owner:"tgoodyear",name:"usnewsmap.com")'
                 '{pullRequest(number:$number){reviewThreads(first:100)'
                 '{pageInfo{hasNextPage} nodes{isResolved}}}}}')
        value = json.loads(self.run(["gh", "api", "graphql", "-f", "query=" + query,
                                     "-F", "number=" + str(int(number))], timeout=120))
        threads = value["data"]["repository"]["pullRequest"]["reviewThreads"]
        return threads["pageInfo"]["hasNextPage"] or any(not t["isResolved"] for t in threads["nodes"])

    def phase(self, phase):
        if phase == "base":
            work = self.workspace("base")
            self.assert_main_settings(work)
            images = self.read_settings(work)["images"]
            if not images.get("server") and not images.get("bridge"):
                self.script(work, "provision-boinc.sh")
            else:
                check_images(images, ("server", "bridge"))
        elif phase in ("server", "worker", "review-runtime"):
            work = self.workspace("artifacts")
            if phase == "server":
                self.assert_main_settings(work)
                self.script(work, "build-boinc-images.sh")
            elif phase == "worker":
                self.worker(work)
            else:
                self.review("runtime", work, self.image_keys())
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
                        "--worker-image", self.worker_for_signing(work)]
                for key in ("app", "version", "platform", "plan_class"):
                    args.extend(["--" + key.replace("_", "-"), self.config[key]])
                try:
                    self.script(work, "boinc-app-release.sh", *args)
                except PipelineError as exc:
                    raise PipelineError("isolated signing failed (including locked/unreachable signing service); never unlock or bypass") from exc
            else:
                self.review("app", work, self.image_keys(appver=True))
        elif phase in ("publish", "acceptance"):
            work = self.workspace("release")
            self.assert_reviewed(work, "app")
            if phase == "publish":
                self.script(work, "provision-boinc.sh")
            else:
                self.acceptance(work)

    def assert_main_settings(self, work):
        current = json.loads(self.git("show", self.main() + ":" + self.settings))
        if self.read_settings(work) != current:
            raise PipelineError("workspace settings differ from reviewed main; no base provisioning or build permitted")

    def assert_reviewed(self, work, label):
        expected = self.state["reviews"][label]["settings"]
        main_sha = self.main()
        current = json.loads(self.git("show", main_sha + ":" + self.settings))
        expected_pin = self.state["reviews"][label].get("worker_pin")
        main_pin = json.loads(self.git("show", main_sha + ":" + self.worker_pin)) if self.worker_pin else None
        if (self.read_settings(work) != expected or current != expected
                or self.read_worker_pin(work) != expected_pin or main_pin != expected_pin):
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


def public_worker(expected_digest):
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", expected_digest):
        raise PipelineError("public worker lookup requires an immutable digest")
    repository = "tgoodyear/usnm-boinc-worker"
    with urllib.request.urlopen("https://ghcr.io/token?scope=repository:" + repository + ":pull", timeout=30) as response:
        token = json.load(response)["token"]
    request = urllib.request.Request("https://ghcr.io/v2/" + repository + "/manifests/" + expected_digest,
                                     headers={"Authorization": "Bearer " + token, "Accept":
                                              "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = response.read()
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        if response.headers.get("Docker-Content-Digest") != digest or digest != expected_digest:
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
