#!/usr/bin/env python3
"""Assemble the native usnm_qwen Linux Vulkan release inputs from pinned sources (#1234).

`build` takes the CI-built usnm_qwen binary and the llama.lock-pinned ubuntu-vulkan-x64 archive,
and writes one directory a maintainer can hash, compare and sign from:

    usnm_qwen_x86_64-pc-linux-gnu   the binary, as built
    extra/<logical>                 llama-server, every lib*.so* (links copied as files),
                                    llama.LICENSE, llama.lock and weights.lock
    release.json                    the on-prem manifest's `release` and `worker` entries
    SHA256SUMS                      every file above

The same inputs give the same bytes. `verify` rehashes a directory; with --same-runtime-as it also
checks that the llama.cpp runtime (every extra file but the two locks) is byte for byte that of an
already-reviewed release manifest. `args` verifies, then prints the --binary/--binary-sha256/
--extra-file arguments for scripts/boinc/boinc-app-release.sh, one per line, with absolute paths.
Nothing here signs, uploads or publishes.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[2]
LOCK = ROOT / "ocr/boinc/qwen/llama.lock"
WEIGHTS = ROOT / "ocr/boinc/qwen/weights.lock"
DISK_POLICY = ROOT / "ocr/boinc/qwen/disk-policy.json"
ASSET = "ubuntu-vulkan-x64"
PLATFORM = "x86_64-pc-linux-gnu"
# The llama.cpp source commit of each pinned tag (docs/boinc-intel-gpu.md); a new pin is reviewed here.
LLAMA_SOURCE = {"b8799": "e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd"}
PLAN_CLASSES = ("nvidia_vulkan_2b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_8b", "intel_vulkan_8b")
BINARY = f"usnm_qwen_{PLATFORM}"
LIBRARY = re.compile(r"lib[A-Za-z0-9_-]+\.so(\.[0-9]+)*")
# What install.py requires of a Linux Vulkan bundle, and what llama-server links.
REQUIRED = {"llama-server", "llama.LICENSE", "llama.lock", "weights.lock", "libggml.so", "libggml-base.so",
            "libggml-vulkan.so", "libllama.so", "libmtmd.so"}


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def logical_name(name):
    if name == "LICENSE":
        return "llama.LICENSE"
    if name == "llama-server" or LIBRARY.fullmatch(name):
        return name
    return None


def runtime_members(packed, prefix):
    """{logical name: regular-file member} for the archive's runtime, links resolved inside it."""
    members = {}
    for member in packed.getmembers():
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError(f"unexpected archive member {member.name!r}")
        if member.isdir():
            continue
        if not member.name.startswith(prefix):
            raise ValueError(f"unexpected archive member {member.name!r}")
        name = member.name[len(prefix):]
        if not name:
            continue
        if "/" in name:
            raise ValueError(f"nested archive member {member.name!r}")
        if name in members:
            raise ValueError(f"duplicate archive member {member.name!r}")
        members[name] = member
    chosen = {}
    for name, member in members.items():
        logical = logical_name(name)
        if logical is None:
            continue
        for _ in range(8):
            if member.isfile():
                break
            if not (member.issym() or member.islnk()):
                raise ValueError(f"{name} is not a file or link")
            target = member.linkname
            if member.islnk():
                if not target.startswith(prefix):
                    raise ValueError(f"{name} links outside the release")
                target = target[len(prefix):]
            if "/" in target or target not in members:
                raise ValueError(f"{name} links outside the release")
            member = members[target]
        else:
            raise ValueError(f"{name} has too many links")
        chosen[logical] = member
    missing = sorted(REQUIRED - {"llama.lock", "weights.lock"} - set(chosen))
    if missing:
        raise ValueError("archive lacks " + ", ".join(missing))
    return chosen


def write_sums(out, names):
    lines = "".join(f"{sha256(out / name)}  {name}\n" for name in sorted(names))
    (out / "SHA256SUMS").write_text(lines)


def build(archive, binary, source_commit, out, plan_class, lock_path=LOCK):
    if plan_class not in PLAN_CLASSES:
        raise ValueError(f"not a Linux Vulkan usnm_qwen plan class: {plan_class}")
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise ValueError("--source-commit must be the 40-hex commit the binary was built from")
    lock = json.loads(Path(lock_path).read_text())
    asset = lock["assets"][ASSET]
    if lock["tag"] not in LLAMA_SOURCE or asset["file"] != f"llama-{lock['tag']}-bin-{ASSET}.tar.gz":
        raise ValueError("llama.lock pins a runtime this script has not reviewed")
    if Path(archive).name != asset["file"] or sha256(archive) != asset["sha256"]:
        raise ValueError(f"{archive} is not the llama.lock-pinned {asset['file']}")
    out = Path(out)
    out.mkdir()
    extra = out / "extra"
    extra.mkdir()
    with tarfile.open(archive, "r:gz") as packed:
        for logical, member in sorted(runtime_members(packed, f"llama-{lock['tag']}/").items()):
            with packed.extractfile(member) as source, open(extra / logical, "wb") as target:
                shutil.copyfileobj(source, target)
            (extra / logical).chmod(0o755 if logical == "llama-server" else 0o644)
    shutil.copyfile(lock_path, extra / "llama.lock")
    shutil.copyfile(WEIGHTS, extra / "weights.lock")
    shutil.copyfile(binary, out / BINARY)
    (out / BINARY).chmod(0o755)
    names = sorted(p.name for p in extra.iterdir())
    bound = json.loads(DISK_POLICY.read_text())["native_bundle_bound_bytes"]
    if sum(p.stat().st_size for p in [out / BINARY, *extra.iterdir()]) > bound:
        raise ValueError("bundle exceeds the reviewed native Qwen disk envelope")
    llama = LLAMA_SOURCE[lock["tag"]]
    release = {
        "release": {"app": "usnm_qwen", "kind": "native", "platform": PLATFORM, "plan_class": plan_class},
        "worker": {
            "sha256": sha256(out / BINARY),
            "source_commit": source_commit,
            "extra_files": {name: {"sha256": sha256(extra / name),
                                   "source_commit": source_commit if name.endswith(".lock") else llama}
                            for name in names},
        },
        "runtime": {"repository": lock["repository"], "tag": lock["tag"], "file": asset["file"],
                    "sha256": asset["sha256"], "source_commit": llama},
    }
    (out / "release.json").write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
    write_sums(out, [BINARY, "release.json", *(f"extra/{name}" for name in names)])
    return release


def runtime_hashes(extra_files):
    return {name: entry["sha256"] for name, entry in extra_files.items() if not name.endswith(".lock")}


def verify(bundle, same_runtime_as=None):
    bundle = Path(bundle).resolve()
    listed = {}
    for line in (bundle / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        listed[name] = digest
    present = {p.relative_to(bundle).as_posix() for p in bundle.rglob("*") if p.is_file()} - {"SHA256SUMS"}
    if set(listed) != present:
        raise ValueError("SHA256SUMS does not list exactly the bundle's files")
    for name, digest in listed.items():
        if (bundle / name).is_symlink() or sha256(bundle / name) != digest:
            raise ValueError(f"{name} does not match SHA256SUMS")
    release = json.loads((bundle / "release.json").read_text())
    worker = release["worker"]
    extras = worker["extra_files"]
    if (listed[BINARY] != worker["sha256"] or set(extras) != {n[len("extra/"):] for n in listed if n.startswith("extra/")}
            or any(listed["extra/" + n] != e["sha256"] for n, e in extras.items()) or not REQUIRED <= set(extras)):
        raise ValueError("release.json does not match the bundle")
    if same_runtime_as is not None:
        reviewed = json.loads(Path(same_runtime_as).read_text())["worker"]["extra_files"]
        if runtime_hashes(reviewed) != runtime_hashes(extras):
            raise ValueError(f"the runtime differs from {same_runtime_as}")
    return bundle, release


def release_args(bundle):
    bundle, release = verify(bundle)
    lines = ["--binary", str(bundle / BINARY), "--binary-sha256", release["worker"]["sha256"]]
    for name in sorted(release["worker"]["extra_files"]):
        lines += ["--extra-file", f"{name}={bundle / 'extra' / name}"]
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    make = commands.add_parser("build")
    make.add_argument("--archive", type=Path, required=True)
    make.add_argument("--binary", type=Path, required=True)
    make.add_argument("--source-commit", required=True)
    make.add_argument("--plan-class", required=True, choices=PLAN_CLASSES)
    make.add_argument("--out", type=Path, required=True)
    check = commands.add_parser("verify")
    check.add_argument("--bundle", type=Path, required=True)
    check.add_argument("--same-runtime-as", type=Path)
    args_ = commands.add_parser("args")
    args_.add_argument("--bundle", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        if args.command == "build":
            release = build(args.archive, args.binary, args.source_commit, args.out, args.plan_class)
            print(f"{release['worker']['sha256']}  {args.out / BINARY}")
            print(f"{len(release['worker']['extra_files'])} extra files; see {args.out / 'SHA256SUMS'}")
        elif args.command == "verify":
            verify(args.bundle, args.same_runtime_as)
            print(f"{args.bundle}: verified" + (f"; runtime identical to {args.same_runtime_as}"
                                                if args.same_runtime_as else ""))
        else:
            print("\n".join(release_args(args.bundle)))
    except (OSError, ValueError, KeyError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
