#!/usr/bin/env python3
"""Install a signed-app candidate into an existing stock BOINC project.

Does not edit project config, publish/sign files, or install server packages.
"""
import argparse
from pathlib import Path
import re
import shutil
import xml.etree.ElementTree as ET


TEMPLATES = {
    "usnm_ocr": ["usnm_ocr_in", "usnm_ocr_out"],
    # The JP2 against IIIF JPEG assessment (#456): same worker image, same job.toml.
    "usnm_assess": ["usnm_assess_in", "usnm_assess_crash_in", "usnm_assess_out"],
    # The gold check (#462): same worker image. `usnm-boinc gold submit` writes the input
    # template, which depends on the bundle.
    "usnm_gold": ["usnm_gold_out"],
}


PLATFORM = "x86_64-pc-linux-gnu__docker"
ROOT = Path(__file__).resolve().parents[2]


def check(wrapper: Path, image: str, version: str, app: str) -> None:
    if app not in TEMPLATES:
        raise ValueError("--app must be one of " + ", ".join(sorted(TEMPLATES)))
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]*@sha256:[0-9a-f]{64}", image):
        raise ValueError("--image must be a pullable registry reference pinned by full digest")
    if not re.fullmatch(r"[1-9][0-9]*\.[0-9]+", version):
        raise ValueError("invalid app version")
    if not wrapper.is_file():
        raise ValueError("missing trusted Linux x86-64 docker_wrapper")


def install(project: Path, wrapper: Path, image: str, version: str, app: str = "usnm_ocr") -> Path:
    check(wrapper, image, version, app)
    if not (project / "config.xml").is_file():
        raise ValueError("create a stock BOINC project first")
    target = write_version(project / "apps" / app / version / PLATFORM, wrapper, image)
    (project / "templates").mkdir(exist_ok=True)
    for name in TEMPLATES[app]:
        shutil.copyfile(ROOT / "ocr/boinc" / name, project / "templates" / name)
    return target


def stage(out: Path, wrapper: Path, image: str, version: str, app: str = "usnm_ocr") -> Path:
    """The version's files for signing and upload, under OUT/<app>/<version>/<platform>; no
    project needed, no templates (the bridge image's install-versions step adds them)."""
    check(wrapper, image, version, app)
    return write_version(out / app / version / PLATFORM, wrapper, image)


def write_version(target: Path, wrapper: Path, image: str) -> Path:
    target.mkdir(parents=True, exist_ok=False)
    shutil.copyfile(wrapper, target / "docker_wrapper_1")
    (target / "docker_wrapper_1").chmod(0o755)
    (target / "Dockerfile_1").write_text(f"FROM {image}\n")
    shutil.copyfile(ROOT / "ocr/boinc/job.toml", target / "job_1.toml")
    spec = ET.Element("version")
    wrapper_file = ET.SubElement(spec, "file")
    ET.SubElement(wrapper_file, "physical_name").text = "docker_wrapper_1"
    ET.SubElement(wrapper_file, "main_program")
    for physical, logical in [("Dockerfile_1", "Dockerfile"), ("job_1.toml", "job.toml")]:
        item = ET.SubElement(spec, "file")
        ET.SubElement(item, "physical_name").text = physical
        ET.SubElement(item, "logical_name").text = logical
        ET.SubElement(item, "copy_file")
    ET.SubElement(spec, "is_wrapper")
    ET.indent(spec)
    ET.ElementTree(spec).write(target / "version.xml", encoding="unicode")
    return target


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    where = parser.add_mutually_exclusive_group(required=True)
    where.add_argument("--project", type=Path, help="install into this BOINC project")
    where.add_argument("--stage", type=Path,
                       help="write the version under STAGE/<app>/<version>/<platform> to sign and upload "
                            "(docs/boinc.md, \"Install a signed app version\")")
    parser.add_argument("--wrapper", required=True, type=Path)
    parser.add_argument("--image", required=True)
    parser.add_argument("--version", default="1.0")
    parser.add_argument("--app", default="usnm_ocr", choices=sorted(TEMPLATES))
    args = parser.parse_args()
    if args.stage:
        target = stage(args.stage.resolve(), args.wrapper.resolve(), args.image, args.version, args.app)
        print(target)
        files = sorted(p.name for p in target.iterdir() if p.name != "version.xml")
        print("Sign each of these offline (sign_executable FILE code_sign_private > FILE.sig): " + " ".join(files))
        return
    print(install(args.project.resolve(), args.wrapper.resolve(), args.image, args.version, args.app))
    print(f"Register {args.app}, sign/publish the app, and configure stock daemons; see docs/boinc.md.")


if __name__ == "__main__":
    main()
