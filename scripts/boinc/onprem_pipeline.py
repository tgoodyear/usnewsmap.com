"""On-prem adapter for the durable, two-review BOINC bootstrap lifecycle."""
import hashlib
import json
from pathlib import Path
import re
import shlex

from bootstrap_pipeline import Pipeline, PipelineError, atomic, fingerprint, load
from onprem_artifacts import validate


def load_onprem(path, args):
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]*", args.onprem_host or ""):
        raise PipelineError("on-prem execution requires a valid --onprem-host")
    if not re.fullmatch(r"[1-9][0-9]{2,8}", args.onprem_ct or ""):
        raise PipelineError("on-prem execution requires a valid --onprem-ct")
    original = json.loads(path.read_text())
    kind = original.get("kind", "container")
    if kind == "container":
        config, _ = load(path, {key: value for key, value in original.items() if key != "kind"})
        config["kind"] = kind
    elif kind == "native":
        required = {"env", "app", "version", "platform", "plan_class", "signing_host",
                    "acceptance_config", "kind", "binary", "binary_sha256", "binary_source"}
        if set(original) not in (required, required | {"extra_files"}) or not all(
                isinstance(original[k], str) for k in required):
            raise PipelineError("native configuration requires the documented binary provenance fields")
        # Reuse the common field validation without weakening Azure's container-only contract.
        common = {key: value for key, value in original.items()
                  if key not in {"kind", "binary", "binary_sha256", "binary_source", "extra_files"}}
        if (common["platform"] != "x86_64-pc-linux-gnu" or
                (common["app"], common["plan_class"]) not in
                (("usnm_quality", ""), ("usnm_qwen", "mt"), ("usnm_qwen", "nvidia_cuda_8b"),
                 ("usnm_qwen", "nvidia_cuda_2b"), ("usnm_qwen", "intel_vulkan_8b"),
                 ("usnm_qwen", "nvidia_vulkan_8b"), ("usnm_qwen", "nvidia_vulkan_6gb_8b"),
                 ("usnm_qwen", "amd_vulkan_8b"))):
            raise PipelineError("unsupported native on-prem release contract")
        if (common["env"] not in ("dev", "prod")
                or not re.fullmatch(r"[1-9][0-9]*\.[0-9]{1,2}", common["version"])
                or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]*", common["signing_host"])):
            raise PipelineError("invalid native release configuration")
        config = dict(original)
        for key in ("acceptance_config", "binary"):
            config[key] = str((path.resolve().parent / config[key]).resolve())
        config["extra_files"] = original.get("extra_files", {})
        if not isinstance(config["extra_files"], dict):
            raise PipelineError("native extra_files must be a mapping")
        for logical, entry in config["extra_files"].items():
            if (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", logical)
                    or not isinstance(entry, dict) or set(entry) != {"path", "sha256", "source_commit"}
                    or not all(isinstance(v, str) for v in entry.values())):
                raise PipelineError("invalid native extra-file provenance")
            entry["path"] = str((path.resolve().parent / entry["path"]).resolve())
        verify_binary(config)
    else:
        raise PipelineError("unsupported on-prem release kind")
    acceptance = Path(config["acceptance_config"])
    value = json.loads(acceptance.read_text())
    if value.get("client", {}).get("mode") != "existing":
        raise PipelineError("on-prem acceptance requires independent existing clients")
    config.update(onprem_host=args.onprem_host, onprem_ct=args.onprem_ct)
    return config, fingerprint({"config": config, "acceptance_sha256":
                               hashlib.sha256(acceptance.read_bytes()).hexdigest()})


def verify_binary(config):
    if (not re.fullmatch(r"[0-9a-f]{64}", config["binary_sha256"])
            or not re.fullmatch(r"[0-9a-f]{40}", config["binary_source"])
            or hashlib.sha256(Path(config["binary"]).read_bytes()).hexdigest() != config["binary_sha256"]):
        raise PipelineError("native binary provenance/checksum mismatch")
    for entry in config.get("extra_files", {}).values():
        if (not re.fullmatch(r"[0-9a-f]{64}", entry["sha256"])
                or not re.fullmatch(r"[0-9a-f]{40}", entry["source_commit"])
                or hashlib.sha256(Path(entry["path"]).read_bytes()).hexdigest() != entry["sha256"]):
            raise PipelineError("native extra-file provenance/checksum mismatch")
    if config["app"] == "usnm_qwen":
        required = {"llama-server", "llama.lock", "weights.lock"}
        if config["plan_class"] == "amd_vulkan_8b":
            required.add("usnm-amd-device-map")
        if config["plan_class"] in ("amd_vulkan_8b", "nvidia_vulkan_6gb_8b"):
            required |= {"libggml.so", "libggml-base.so", "libggml-vulkan.so"}
        if not required.issubset(config.get("extra_files", {})):
            raise PipelineError("native Qwen is missing required reviewed extra-file inputs")


class OnpremPipeline(Pipeline):
    topology = "onprem"

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.environment_settings = self.settings
        self.settings = f"contrib-compute/infra/boinc/releases/{self.config['env']}-onprem-{self.config['app']}-{self.config['plan_class'] or 'default'}.json"
        self.worker_pin = None
        self.remote_directory = "/root/.usnm-bootstrap-" + self.state["id"]

    def ssh(self, command):
        return self.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                         "root@" + self.config["onprem_host"], command])

    def ct(self, command):
        return self.ssh("pct exec " + self.config["onprem_ct"] +
                        " -- env -u XDG_RUNTIME_DIR PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin bash -c " +
                        shlex.quote(command))

    def remote(self, work, action, value):
        program = self.remote_directory + "/source/scripts/boinc/onprem_artifacts.py"
        return json.loads(self.ct("python3 " + shlex.quote(program) + " --operation " +
                                  shlex.quote(action) + " --payload " + shlex.quote(json.dumps(value))))

    def preflight(self):
        manifest_path = self.settings
        self.settings = self.environment_settings
        try:
            super().preflight()
        finally:
            self.settings = manifest_path
        if not self.args.dry_run and not self.args.authorize_runtime_install:
            raise PipelineError("on-prem execution requires --authorize-runtime-install; this permits a reviewed server restart")
        files = self.git("ls-tree", "-r", "--name-only", self.main()).splitlines()
        for name in ("scripts/boinc/onprem_pipeline.py", "scripts/boinc/onprem_artifacts.py",
                     "scripts/boinc/onprem_acceptance.py"):
            if name not in files:
                raise PipelineError("missing reviewed main on-prem entrypoint: " + name)
        if self.config["kind"] == "native":
            verify_binary(self.config)

    def release(self):
        return {key: self.config[key] for key in ("app", "version", "platform", "plan_class", "kind")}

    def check_images(self, images, keys):
        for key in keys:
            pattern = (r"ghcr\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}"
                       if key == "worker" else r"sha256:[0-9a-f]{64}")
            if not re.fullmatch(pattern, images.get(key) or ""):
                raise PipelineError("missing immutable on-prem " + key + " artifact")

    def image_keys(self, appver=False):
        return ("base", "bin", "server", "bridge") + (
            ("worker",) if self.config["kind"] == "container" else ()) + (("appver",) if appver else ())

    def candidate(self, work):
        value = self.read_settings(work)
        try:
            validate(value)
        except (ValueError, KeyError, TypeError) as exc:
            raise PipelineError("invalid on-prem artifact manifest") from exc
        if (value["env"] != self.config["env"] or value["host"] != self.config["onprem_host"]
                or value["ct"] != self.config["onprem_ct"] or value["release"] != self.release()):
            raise PipelineError("on-prem manifest target/release mismatch")
        return value

    def review(self, label, work, keys):
        value = self.candidate(work)
        self.check_images(value["images"], keys)
        if label == "runtime":
            if "appver" in value["images"] or "signing_source" in value:
                raise PipelineError("runtime candidate must precede signing")
            if value["source_commit"] != self.state["workspaces"]["artifacts"]["commit"]:
                raise PipelineError("runtime source differs from isolated reviewed source")
        else:
            expected = json.loads(json.dumps(self.state["reviews"]["runtime"]["settings"]))
            expected["images"]["appver"] = value["images"]["appver"]
            expected["signing_source"] = self.state["workspaces"]["signing"]["commit"]
            if value != expected:
                raise PipelineError("signing changed reviewed runtime/worker provenance")
        self.review_candidate(label, work, value, None, {self.settings})

    def assert_reviewed(self, work, label):
        super().assert_reviewed(work, label)
        self.candidate(work)
        # The unrelated Azure settings are not an on-prem image fallback, but the
        # signing key/topology must still be the current reviewed environment.
        current = self.git("show", self.main() + ":" + self.environment_settings)
        if json.loads((work / self.environment_settings).read_text()) != json.loads(current):
            raise PipelineError("reviewed environment changed; refusing stale signing or installation")

    def build(self, work):
        commit = self.state["workspaces"]["artifacts"]["commit"]
        archive = self.directory / "source.tar"
        self.git("archive", "--format=tar", "--output", str(archive), commit)
        checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
        upload = self.remote_directory + ".tar"
        self.run(["scp", "-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                  str(archive), "root@" + self.config["onprem_host"] + ":" + upload])
        self.ct("mkdir -p -m 700 " + self.remote_directory)
        self.ssh("pct push " + self.config["onprem_ct"] + " " + upload + " " + upload +
                 " && rm -- " + upload)
        source = self.remote_directory + "/source"
        self.ct("echo " + shlex.quote(checksum + "  " + upload) +
                " | sha256sum -c - >/dev/null && mkdir -p -m 700 " + source +
                " && tar -xf " + upload + " -C " + source + " && rm -- " + upload)
        images = self.remote(work, "build", {"source": source, "commit": commit})
        images["worker"] = None
        value = dict(schema=1, env=self.config["env"], host=self.config["onprem_host"],
                     ct=self.config["onprem_ct"], source_commit=commit,
                     release=self.release(), images=images, worker=None)
        path = work / self.settings
        path.parent.mkdir(parents=True, exist_ok=True)
        atomic(path, value)

    def worker(self, work):
        if self.config["kind"] == "native":
            verify_binary(self.config)
        else:
            super().worker(work)
        value = self.read_settings(work)
        value["worker"] = ({"sha256": self.config["binary_sha256"], "source_commit": self.config["binary_source"],
                            "extra_files": {logical: {key: entry[key] for key in ("sha256", "source_commit")}
                                            for logical, entry in self.config.get("extra_files", {}).items()}}
                           if self.config["kind"] == "native" else
                           {"source_commit": self.state["worker_commit"], "run": self.state["worker_build"],
                            "attempt": self.state["worker_attempt"], "receipt_sha256": self.state["worker_receipt_sha256"]})
        atomic(work / self.settings, value)

    def acceptance(self, work, dry_run=False):
        original = self.config["acceptance_config"]
        value = json.loads(Path(original).read_text())
        image = ("sha256:" + "0" * 64) if dry_run else self.candidate(work)["images"]["bridge"]
        command = ("pct exec " + self.config["onprem_ct"] +
                   " -- env -u XDG_RUNTIME_DIR PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin python3 " +
                   self.remote_directory + "/source/scripts/boinc/onprem_acceptance.py --expected-image " + image)
        value["server_command"] = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=15",
                                   "root@" + self.config["onprem_host"], command]
        # Existing-client adapter paths are left exactly as reviewed by the operator.
        generated = self.directory / "onprem-acceptance-config.json"
        atomic(generated, value)
        self.config["acceptance_config"] = str(generated)
        try:
            super().acceptance(work, dry_run)
        finally:
            self.config["acceptance_config"] = original

    def phase(self, phase):
        if phase == "base":
            self.ct("test -r /etc/usnm-boinc/server.env && test -r /etc/usnm-boinc/bridge.env"
                    " && command -v podman >/dev/null && command -v usnm-boinc-step >/dev/null"
                    " && systemctl is-active --quiet usnm-boinc-mysql.service"
                    " && systemctl is-active --quiet usnm-boinc-server.service")
        elif phase in ("server", "worker", "review-runtime"):
            work = self.workspace("artifacts")
            if phase == "server":
                if self.git("status", "--porcelain", "--untracked-files=normal", cwd=work):
                    raise PipelineError("clean reviewed source required before local build")
                self.build(work)
            elif phase == "worker":
                self.worker(work)
            else:
                self.review("runtime", work, self.image_keys())
        elif phase == "runtime":
            work = self.workspace("runtime")
            self.assert_reviewed(work, "runtime")
            self.remote(work, "install", self.candidate(work))
        elif phase in ("sign", "review-app"):
            work = self.workspace("signing")
            if phase == "review-app":
                self.review("app", work, self.image_keys(appver=True))
                return
            self.assert_reviewed(work, "runtime")
            self.remote(work, "verify", self.candidate(work))
            receipt = self.directory / "signed-artifact.json"
            args = ["--prepare-only", "--signing-host", self.config["signing_host"],
                    "--onprem-host", self.config["onprem_host"], "--onprem-ct", self.config["onprem_ct"],
                    "--onprem-manifest", self.settings, "--artifact-receipt", str(receipt),
                    "--kind", self.config["kind"]]
            for key in ("app", "version", "platform", "plan_class"):
                args += ["--" + key.replace("_", "-"), self.config[key]]
            if self.config["kind"] == "native":
                verify_binary(self.config)
                args += ["--binary", self.config["binary"], "--binary-sha256", self.config["binary_sha256"]]
                for logical, entry in self.config.get("extra_files", {}).items():
                    args += ["--extra-file", logical + "=" + entry["path"]]
            else:
                args += ["--worker-image", self.worker_for_signing(work)]
            self.script(work, "boinc-app-release.sh", *args)
            signed = json.loads(receipt.read_text())
            value = self.candidate(work)
            expected = {"schema": 1, "image": signed.get("image"),
                        "source_commit": self.state["workspaces"]["signing"]["commit"],
                        "server_image": value["images"]["bridge"]}
            if signed != expected:
                raise PipelineError("signed artifact receipt differs from reviewed source/runtime")
            self.check_images({"appver": signed["image"]}, ("appver",))
            value["images"]["appver"] = signed["image"]
            value["signing_source"] = signed["source_commit"]
            atomic(work / self.settings, value)
        elif phase in ("publish", "acceptance"):
            work = self.workspace("release")
            self.assert_reviewed(work, "app")
            value = self.candidate(work)
            self.remote(work, "verify", value)
            if phase == "publish":
                self.ct("usnm-boinc-step --env " + self.config["env"] +
                        " --target publish --step publish --image " + value["images"]["appver"] + " --wait-mins 30")
            else:
                self.acceptance(work)
