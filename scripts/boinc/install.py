#!/usr/bin/env python3
"""Install a signed-app candidate into an existing stock BOINC project.

Does not edit project config, publish/sign files, or install server packages.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import xml.etree.ElementTree as ET


TEMPLATES = {
    "usnm_ocr": ["usnm_ocr_in", "usnm_ocr_out", "usnm_ocr_bundle_in", "usnm_ocr_bundle_out"],
    # The JP2 against IIIF JPEG assessment (#456): same worker image, same job.toml.
    "usnm_assess": ["usnm_assess_in", "usnm_assess_crash_in", "usnm_assess_out"],
    # The gold check (#462): same worker image. `usnm-boinc gold submit` writes the input
    # template, which depends on the bundle.
    "usnm_gold": ["usnm_gold_out"],
    # The Surya 2 test (#516): its own worker image, a CPU and a GPU version.
    "usnm_ndlocr": ["usnm_ndlocr_in", "usnm_ndlocr_out"],
    "usnm_surya": ["usnm_surya_in", "usnm_surya_out"],
    # The OCR quality estimate (#522): the CPU app runs in the worker image under the docker
    # class; the Qwen app has its own image, a CPU and a GPU version.
    "usnm_quality": ["usnm_quality_in", "usnm_quality_out"],
    "usnm_qwen": ["usnm_qwen_in", "usnm_qwen_2b_in", "usnm_qwen_8b_in", "usnm_qwen_out"],
    "usnm_qwen_train": ["usnm_qwen_train_in", "usnm_qwen_train_out"],
    "usnm_iris": ["usnm_iris_in", "usnm_iris_out", "usnm_iris_bundle_in", "usnm_iris_bundle_out"],
    # Tesseract with no container runtime (#1418): the same units as usnm_ocr and usnm_gold, in
    # their own apps so that neither kind of version can be sent the other's work.
    "usnm_ocr_native": ["usnm_ocr_in", "usnm_ocr_out", "usnm_ocr_bundle_in", "usnm_ocr_bundle_out"],
    "usnm_gold_native": ["usnm_gold_out"],
}
QWEN_NATIVE_MODEL_TEMPLATES = {
    "apple_gpu_8b": "usnm_qwen_8b_in",
    "nvidia_cuda_8b": "usnm_qwen_8b_in",
    "intel_vulkan_8b": "usnm_qwen_8b_in",
    "nvidia_vulkan_8b": "usnm_qwen_8b_in",
    "nvidia_vulkan_6gb_8b": "usnm_qwen_8b_in",
    "amd_vulkan_8b": "usnm_qwen_8b_in",
    "nvidia_cuda_2b": "usnm_qwen_2b_in",
    "mt": "usnm_qwen_2b_in",
}
DEFAULT_SERVER_WRAPPER = Path("/usr/local/lib/usnm-boinc/docker_wrapper")

# The plan classes an app version can be installed for (contrib-compute/boinc/plan_class_spec.xml),
# and the job.toml each app uses under each.
JOB_TOML = {
    ("usnm_ocr", "docker"): "ocr/boinc/job.toml",
    ("usnm_assess", "docker"): "ocr/boinc/job.toml",
    ("usnm_gold", "docker"): "ocr/boinc/job.toml",
    ("usnm_ndlocr", "docker"): "ocr/boinc/ndlocr-lite/job.toml",
    ("usnm_surya", "docker_mt"): "ocr/boinc/surya/job-cpu.toml",
    ("usnm_surya", "docker_nvidia_cuda"): "ocr/boinc/surya/job-gpu.toml",
    ("usnm_surya", "docker_intel_vulkan"): "ocr/boinc/surya/job-intel.toml",
    ("usnm_quality", "docker"): "ocr/boinc/job.toml",
    ("usnm_qwen", "docker_mt_2b"): "ocr/boinc/qwen/job-cpu.toml",
    ("usnm_qwen", "docker_nvidia_cuda_8b"): "ocr/boinc/qwen/job-gpu.toml",
    ("usnm_qwen_train", "docker_nvidia_train"): "ocr/boinc/train/job.toml",
    ("usnm_iris", "docker_intel_opencl_4b"): "ocr/boinc/iris/job.toml",
}

PLATFORMS = (
    "x86_64-pc-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "arm64-apple-darwin",
    "x86_64-apple-darwin",
    "windows_x86_64",
)

NATIVE_PLAN_CLASSES = {
    ("usnm_quality", ""): PLATFORMS,
    ("usnm_ocr_native", ""): ("x86_64-pc-linux-gnu",),
    ("usnm_gold_native", ""): ("x86_64-pc-linux-gnu",),
    ("usnm_qwen", "mt"): PLATFORMS,
    ("usnm_qwen", "apple_gpu_8b"): ("arm64-apple-darwin",),
    ("usnm_qwen", "nvidia_cuda_8b"): ("x86_64-pc-linux-gnu", "aarch64-unknown-linux-gnu", "windows_x86_64"),
    ("usnm_qwen", "nvidia_cuda_2b"): ("x86_64-pc-linux-gnu", "aarch64-unknown-linux-gnu", "windows_x86_64"),
    # Intel GPUs through llama.cpp Vulkan (#786); llama.lock pins only x86_64 Vulkan builds.
    ("usnm_qwen", "intel_vulkan_8b"): ("x86_64-pc-linux-gnu", "windows_x86_64"),
    ("usnm_qwen", "nvidia_vulkan_8b"): ("x86_64-pc-linux-gnu",),
    ("usnm_qwen", "nvidia_vulkan_6gb_8b"): ("x86_64-pc-linux-gnu",),
    ("usnm_qwen", "amd_vulkan_8b"): ("x86_64-pc-linux-gnu", "windows_x86_64"),
}
GPU_NATIVE_PLAN_CLASSES = frozenset({"apple_gpu_8b", "nvidia_cuda_8b", "nvidia_cuda_2b", "intel_vulkan_8b", "nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b", "amd_vulkan_8b"})
NATIVE_API_VERSION = "8.3.0"
LOGICAL_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,95}\Z")
NATIVE_OCR_APPS = ("usnm_ocr_native", "usnm_gold_native")
NATIVE_OCR_MANIFEST = "usnm_native_build.json"


def native_ocr_physical(parsed_extras, binary: Path) -> dict[str, str]:
    """The physical name of each file of a native OCR build, checked against its manifest.

    Names come from content, so the two apps share one download of the engine and the models.
    The manifest's name carries its digest: that digest is the job's engine.image, and the
    scheduler's Gold gate compares the two (scripts/boinc/gold-engine-routing.h).
    """
    extras = dict(parsed_extras)
    if NATIVE_OCR_MANIFEST not in extras:
        raise ValueError(f"a native OCR version requires {NATIVE_OCR_MANIFEST}")
    manifest = extras[NATIVE_OCR_MANIFEST].read_bytes()
    listed = json.loads(manifest)["files"]
    worker = {"sha256": hashlib.sha256(binary.read_bytes()).hexdigest(), "bytes": binary.stat().st_size}
    if listed.get("usnm_ocr") != worker:
        raise ValueError("--binary differs from the build manifest's usnm_ocr")
    physical = {NATIVE_OCR_MANIFEST: f"usnm_native_build_{hashlib.sha256(manifest).hexdigest()}.json"}
    if set(extras) != (set(listed) - {"usnm_ocr"}) | {NATIVE_OCR_MANIFEST}:
        raise ValueError("the staged files differ from the build manifest")
    for logical, src in extras.items():
        if logical == NATIVE_OCR_MANIFEST:
            continue
        sha256 = hashlib.sha256(src.read_bytes()).hexdigest()
        if sha256 != listed[logical]["sha256"]:
            raise ValueError(f"{logical} differs from the build manifest")
        physical[logical] = f"usnm_native_{sha256[:16]}_{logical}"
    return physical


def parse_extra_files(extra_files, bundle_dir: Path | None = None) -> list[tuple[str, Path]]:
    items: list[tuple[str, Path]] = []
    if extra_files is not None:
        if isinstance(extra_files, Path):
            bundle_dir = extra_files
        elif isinstance(extra_files, dict):
            items.extend((str(k), Path(v)) for k, v in extra_files.items())
        else:
            for entry in extra_files:
                if isinstance(entry, tuple):
                    logical, src = entry
                    items.append((str(logical), Path(src)))
                elif isinstance(entry, str) and "=" in entry:
                    logical, src = entry.split("=", 1)
                    items.append((logical, Path(src)))
                else:
                    raise ValueError("--extra-file must have the form LOGICAL=PATH")
    if bundle_dir is not None:
        bundle_path = Path(bundle_dir)
        if not bundle_path.is_dir():
            raise ValueError("--bundle-dir must be an existing directory")
        for child in sorted(bundle_path.iterdir()):
            if not child.is_file() or child.is_symlink():
                raise ValueError(f"bundle entry {child.name!r} is not a plain file")
            items.append((child.name, child))
    seen: set[str] = set()
    for logical, src in items:
        if not LOGICAL_NAME.fullmatch(logical) or logical == "version.xml" or logical.endswith(".sig"):
            raise ValueError(f"invalid extra file logical name {logical!r}")
        if logical in seen:
            raise ValueError(f"duplicate extra file logical name {logical!r}")
        if not src.is_file() or src.is_symlink():
            raise ValueError(f"missing extra file {src}")
        seen.add(logical)
    return items


def sign_file(path: Path, sign_key: Path, crypt_prog: str | Path) -> Path:
    if not Path(sign_key).is_file():
        raise ValueError(f"missing code-signing key {sign_key}")
    res = subprocess.run(
        [str(crypt_prog), "-sign", str(path), str(sign_key)],
        capture_output=True,
        text=True,
    )
    if res.returncode != 0:
        raise ValueError(f"crypt_prog -sign failed for {path.name}: {(res.stdout + res.stderr).strip()}")
    sig_path = path.with_name(path.name + ".sig")
    if res.stdout:
        sig_path.write_text(res.stdout)
    if not sig_path.is_file() or sig_path.stat().st_size == 0:
        raise ValueError(f"crypt_prog -sign produced an empty signature for {path.name}")
    return sig_path


def install(
    project: Path,
    wrapper: Path | None = None,
    image: str | None = None,
    version: str = "1.0",
    app: str = "usnm_ocr",
    plan_class: str | None = None,
    platform: str = "x86_64-pc-linux-gnu",
    kind: str = "container",
    binary: Path | None = None,
    extra_files=None,
    bundle_dir: Path | None = None,
    sign_key: Path | None = None,
    crypt_prog: str | Path = "/usr/local/boinc/lib/crypt_prog",
) -> Path:
    if kind not in ("container", "native"):
        raise ValueError("--kind must be container or native")
    if app not in TEMPLATES:
        raise ValueError("--app must be one of " + ", ".join(sorted(TEMPLATES)))
    if platform not in PLATFORMS:
        raise ValueError("unsupported --platform")
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]+", version):
        raise ValueError("invalid app version")
    if not (project / "config.xml").is_file():
        raise ValueError("create a stock BOINC project first")
    root = Path(__file__).resolve().parents[2]
    if kind == "container":
        if plan_class is None:
            plan_class = "docker"
        if binary is not None:
            raise ValueError("--binary is only valid with --kind native")
        if extra_files or bundle_dir is not None:
            raise ValueError("--extra-file and --bundle-dir are only valid with --kind native")
        if (app, plan_class) not in JOB_TOML:
            raise ValueError(
                f"{app} has no version for plan class {plan_class}: one of "
                + ", ".join(sorted(p for a, p in JOB_TOML if a == app))
            )
        if platform != "x86_64-pc-linux-gnu" and plan_class != "docker":
            raise ValueError(f"{platform} supports only plan class docker")
        if not image or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]*@sha256:[0-9a-f]{64}", image):
            raise ValueError("--image must be a pullable registry reference pinned by full digest")
        if platform != "x86_64-pc-linux-gnu" and (
            wrapper is None or Path(wrapper) == DEFAULT_SERVER_WRAPPER
        ):
            raise ValueError(
                f"{platform} container installs require a verified platform-specific --wrapper, not {DEFAULT_SERVER_WRAPPER}"
            )
        if wrapper is None or not Path(wrapper).is_file():
            raise ValueError("missing trusted docker_wrapper for the selected platform")
        target = project / "apps" / app / version / f"{platform}__{plan_class}"
        target.mkdir(parents=True, exist_ok=False)
        suffix = f"{app}_{version}_{platform}_{plan_class}"
        wrapper_ext = ".exe" if platform == "windows_x86_64" else ""
        wrapper_name = f"docker_wrapper_{suffix}{wrapper_ext}"
        dockerfile_name = f"Dockerfile_{suffix}"
        job_name = f"job_{suffix}.toml"
        shutil.copyfile(wrapper, target / wrapper_name)
        (target / wrapper_name).chmod(0o755)
        (target / dockerfile_name).write_text(f"FROM {image}\n")
        shutil.copyfile(root / JOB_TOML[(app, plan_class)], target / job_name)
        spec = ET.Element("version")
        wrapper_file = ET.SubElement(spec, "file")
        ET.SubElement(wrapper_file, "physical_name").text = wrapper_name
        ET.SubElement(wrapper_file, "main_program")
        for physical, logical in [(dockerfile_name, "Dockerfile"), (job_name, "job.toml")]:
            item = ET.SubElement(spec, "file")
            ET.SubElement(item, "physical_name").text = physical
            ET.SubElement(item, "logical_name").text = logical
            ET.SubElement(item, "copy_file")
        ET.SubElement(spec, "is_wrapper")
        staged_files = [target / wrapper_name, target / dockerfile_name, target / job_name]
    else:
        if plan_class is None:
            plan_class = ""
        if wrapper is not None:
            raise ValueError("--wrapper is only valid with --kind container")
        if image:
            raise ValueError("--image is only valid with --kind container")
        if binary is None or not Path(binary).is_file():
            raise ValueError("--binary must be an existing file when --kind native")
        if (app, plan_class) not in NATIVE_PLAN_CLASSES:
            allowed = sorted(p or '""' for a, p in NATIVE_PLAN_CLASSES if a == app)
            raise ValueError(f"{app} has no native version for plan class {plan_class!r}: one of " + ", ".join(allowed))
        if platform not in NATIVE_PLAN_CLASSES[(app, plan_class)]:
            raise ValueError(f"native plan class {plan_class} is not supported on {platform}")
        binary = Path(binary)
        if platform != "windows_x86_64" and binary.suffix.lower() == ".exe":
            raise ValueError(".exe binaries are only supported on windows_x86_64")
        parsed_extras = parse_extra_files(extra_files, bundle_dir)
        if app == "usnm_qwen":
            disk_policy = json.loads((root / "ocr/boinc/qwen/disk-policy.json").read_text())
            if binary.stat().st_size + sum(src.stat().st_size for _, src in parsed_extras) > disk_policy["native_bundle_bound_bytes"]:
                raise ValueError("native Qwen app bundle exceeds the reviewed 2 GiB disk envelope")
            present_extras = {logical for logical, _ in parsed_extras}
            required_server = "llama-server.exe" if platform == "windows_x86_64" else "llama-server"
            required_extras = (required_server, "llama.lock", "weights.lock")
            if plan_class == "amd_vulkan_8b":
                required_extras += ("usnm-amd-device-map.exe" if platform == "windows_x86_64" else "usnm-amd-device-map",)
            if plan_class in ("amd_vulkan_8b", "nvidia_vulkan_6gb_8b"):
                required_extras += (("ggml.dll", "ggml-base.dll", "ggml-vulkan.dll") if platform == "windows_x86_64"
                                    else ("libggml.so", "libggml-base.so", "libggml-vulkan.so"))
            if platform == "windows_x86_64":
                required_extras += ("msvcp140.dll", "vcruntime140.dll", "vcruntime140_1.dll")
                if plan_class in ("nvidia_cuda_2b", "nvidia_cuda_8b"):
                    required_extras += (
                        "ggml-cuda.dll", "ggml-base.dll", "ggml.dll", "llama.dll", "mtmd.dll",
                        "ggml-cpu-x64.dll", "libomp140.x86_64.dll",
                        "cudart64_12.dll", "cublas64_12.dll", "cublasLt64_12.dll",
                    )
                if plan_class in ("intel_vulkan_8b", "amd_vulkan_8b"):
                    # vulkan-1.dll, the Vulkan loader, comes with the GPU driver.
                    required_extras += (
                        "ggml-vulkan.dll", "ggml-base.dll", "ggml.dll", "llama.dll", "mtmd.dll",
                        "ggml-cpu-x64.dll", "libomp140.x86_64.dll",
                    )
            missing_extras = [name for name in required_extras if name not in present_extras]
            if missing_extras:
                raise ValueError(
                    f"native usnm_qwen on {platform} requires --extra-file for: {', '.join(missing_extras)}"
                )
        subdir = f"{platform}__{plan_class}" if plan_class else platform
        suffix = f"{app}_{version}_{platform}_{plan_class}" if plan_class else f"{app}_{version}_{platform}"
        ext = ".exe" if platform == "windows_x86_64" else ""
        main_name = f"{suffix}{ext}"
        target = project / "apps" / app / version / subdir
        target.mkdir(parents=True, exist_ok=False)
        shutil.copyfile(binary, target / main_name)
        (target / main_name).chmod(0o755)
        spec = ET.Element("version")
        ET.SubElement(spec, "api_version").text = NATIVE_API_VERSION
        main_file = ET.SubElement(spec, "file")
        ET.SubElement(main_file, "physical_name").text = main_name
        ET.SubElement(main_file, "main_program")
        ET.SubElement(main_file, "copy_file")
        staged_files = [target / main_name]
        executable_extras = {"llama-server", "llama-server.exe", "usnm-amd-device-map", "usnm-amd-device-map.exe"}
        native_ocr = native_ocr_physical(parsed_extras, binary) if app in NATIVE_OCR_APPS else None
        for logical, src in parsed_extras:
            phys = native_ocr[logical] if native_ocr else f"{suffix}_{logical}"
            shutil.copyfile(src, target / phys)
            is_exec = logical in executable_extras or os.access(src, os.X_OK)
            (target / phys).chmod(0o755 if is_exec else 0o644)
            item = ET.SubElement(spec, "file")
            ET.SubElement(item, "physical_name").text = phys
            ET.SubElement(item, "logical_name").text = logical
            # 200 MiB of models stay in the project directory: the worker links them, and
            # copying them into every slot would cost more than a page's download.
            if native_ocr is None:
                ET.SubElement(item, "copy_file")
            staged_files.append(target / phys)
        if plan_class in GPU_NATIVE_PLAN_CLASSES:
            ET.SubElement(spec, "dont_throttle")
    ET.indent(spec)
    ET.ElementTree(spec).write(target / "version.xml", encoding="unicode")
    if sign_key is not None:
        for staged in staged_files:
            sign_file(staged, Path(sign_key), crypt_prog)
    (project / "templates").mkdir(exist_ok=True)
    template_names = list(TEMPLATES[app])
    if kind == "native" and app == "usnm_qwen":
        model_template = QWEN_NATIVE_MODEL_TEMPLATES[plan_class]
        if model_template not in template_names:
            template_names.append(model_template)
    for name in template_names:
        shutil.copyfile(root / "ocr/boinc" / name, project / "templates" / name)
    return target


ALL_PLAN_CLASSES = sorted({p for _, p in JOB_TOML} | {p for _, p in NATIVE_PLAN_CLASSES})


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", required=True, type=Path)
    parser.add_argument("--kind", default="container", choices=("container", "native"))
    parser.add_argument("--wrapper", type=Path, default=None)
    parser.add_argument("--binary", type=Path, default=None)
    parser.add_argument("--image", default=None)
    parser.add_argument("--version", default="1.0")
    parser.add_argument("--app", default="usnm_ocr", choices=sorted(TEMPLATES))
    parser.add_argument("--plan-class", default=None, choices=ALL_PLAN_CLASSES)
    parser.add_argument("--platform", default="x86_64-pc-linux-gnu", choices=PLATFORMS)
    parser.add_argument("--extra-file", action="append", default=None)
    parser.add_argument("--bundle-dir", type=Path, default=None)
    parser.add_argument("--sign-key", type=Path, default=None)
    parser.add_argument("--crypt-prog", default="/usr/local/boinc/lib/crypt_prog")
    args = parser.parse_args()
    print(
        install(
            args.project.resolve(),
            args.wrapper.resolve() if args.wrapper is not None else None,
            args.image,
            args.version,
            args.app,
            args.plan_class,
            args.platform,
            kind=args.kind,
            binary=args.binary.resolve() if args.binary is not None else None,
            extra_files=args.extra_file,
            bundle_dir=args.bundle_dir.resolve() if args.bundle_dir is not None else None,
            sign_key=args.sign_key.resolve() if args.sign_key is not None else None,
            crypt_prog=args.crypt_prog,
        )
    )
    print(f"Register {args.app}, sign/publish the app, and configure stock daemons; see docs/boinc/server-and-apps.md.")


if __name__ == "__main__":
    main()
