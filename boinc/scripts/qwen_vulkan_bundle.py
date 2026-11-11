#!/usr/bin/env python3
"""Assemble the native usnm_qwen Vulkan release inputs from pinned sources (#1234, #1672).

`build` takes the CI-built usnm_qwen binary and the llama.lock-pinned ubuntu-vulkan-x64 archive,
and writes one directory a maintainer can hash, compare and sign from:

    usnm_qwen_x86_64-pc-linux-gnu   the binary, as built
    extra/<logical>                 llama-server, every lib*.so* (links copied as files),
                                    llama.LICENSE, llama.lock and weights.lock
    release.json                    the on-prem manifest's `release` and `worker` entries
    SHA256SUMS                      every file above

`build-windows` does the same for windows_x86_64 (#1672): the llama.lock-pinned win-vulkan-x64
zip's llama-server.exe and DLLs, unchanged, plus the Windows native CI artifact's usnm_qwen .exe
and its signed app-local Microsoft runtime DLLs, each checked against the artifact's SHA256SUMS:

    usnm_qwen_windows_x86_64.exe    the binary, as built
    extra/<logical>                 llama-server.exe, every ggml*.dll, llama.dll, mtmd.dll,
                                    libomp140.x86_64.dll, the Microsoft runtime DLLs and the locks

The same inputs give the same bytes. `verify` rehashes a directory; with --same-runtime-as it also
checks that the llama.cpp runtime (every extra file but the two locks) is byte for byte that of an
already-reviewed release manifest. `args` verifies, then prints the --binary/--binary-sha256/
--extra-file arguments for boinc/scripts/boinc-app-release.sh, one per line, with absolute paths.
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
LOCK = ROOT / "boinc/apps/qwen/llama.lock"
WEIGHTS = ROOT / "boinc/apps/qwen/weights.lock"
DISK_POLICY = ROOT / "boinc/apps/qwen/disk-policy.json"
ASSET = "ubuntu-vulkan-x64"
PLATFORM = "x86_64-pc-linux-gnu"
# The llama.cpp source commit of each pinned tag (docs/boinc/apps/intel-gpu.md); a new pin is reviewed here.
LLAMA_SOURCE = {"b8799": "e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd"}
PLAN_CLASSES = ("nvidia_vulkan_2b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_8b", "intel_vulkan_8b")
BINARY = f"usnm_qwen_{PLATFORM}"
# windows_x86_64 (#1672). NVIDIA (full and 6 GB partial-offload 8B) and Intel take the pinned
# runtime unchanged; AMD's Windows bundle replaces ggml-vulkan.dll and adds its PCI identity helper
# (docs/boinc/apps/amd-gpu.md), so it isn't built here.
WIN_ASSET = "win-vulkan-x64"
WIN_PLATFORM = "windows_x86_64"
WIN_PLAN_CLASSES = ("nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b", "intel_vulkan_8b")
WIN_BINARY = f"usnm_qwen_{WIN_PLATFORM}.exe"
WIN_RUNTIME = re.compile(r"llama-server\.exe|ggml[A-Za-z0-9_-]*\.dll|llama\.dll|mtmd\.dll|libomp140\.x86_64\.dll")
# The Microsoft runtime the Windows native CI job bundles and Authenticode-checks (VC_REDIST_SOURCE.json).
WIN_CRT = re.compile(r"(concrt140|msvcp140(_[a-z0-9_]+)?|vccorlib140|vcruntime140(_[a-z0-9_]+)?)\.dll")
# What install.py requires of a Windows Vulkan bundle.
WIN_REQUIRED = {"llama-server.exe", "llama.lock", "weights.lock", "ggml-vulkan.dll", "ggml-base.dll", "ggml.dll",
                "llama.dll", "mtmd.dll", "ggml-cpu-x64.dll", "libomp140.x86_64.dll", "msvcp140.dll",
                "vcruntime140.dll", "vcruntime140_1.dll"}
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


def read_sums(path):
    sums = {}
    for line in Path(path).read_text().splitlines():
        if not line.strip():
            continue
        digest, name = line.split(maxsplit=1)
        sums[name.lstrip("*")] = digest.lower()
    return sums


def build_windows(archive, artifact, source_commit, out, plan_class, lock_path=LOCK):
    """The windows_x86_64 bundle from the pinned zip and the Windows native CI artifact directory."""
    import zipfile
    if plan_class not in WIN_PLAN_CLASSES:
        raise ValueError(f"not a Windows Vulkan usnm_qwen plan class built here: {plan_class}")
    if not re.fullmatch(r"[0-9a-f]{40}", source_commit):
        raise ValueError("--source-commit must be the 40-hex commit the binary was built from")
    artifact = Path(artifact)
    if (artifact / "GIT_COMMIT").read_text().strip() != source_commit:
        raise ValueError("the artifact's GIT_COMMIT is not --source-commit")
    sums = read_sums(artifact / "SHA256SUMS")
    crt = sorted(p.name for p in artifact.iterdir() if WIN_CRT.fullmatch(p.name))
    for name in [WIN_BINARY, *crt]:
        if sums.get(name) != sha256(artifact / name):
            raise ValueError(f"{name} does not match the artifact's SHA256SUMS")
    lock = json.loads(Path(lock_path).read_text())
    asset = lock["assets"][WIN_ASSET]
    if lock["tag"] not in LLAMA_SOURCE or asset["file"] != f"llama-{lock['tag']}-bin-{WIN_ASSET}.zip":
        raise ValueError("llama.lock pins a runtime this script has not reviewed")
    if Path(archive).name != asset["file"] or sha256(archive) != asset["sha256"]:
        raise ValueError(f"{archive} is not the llama.lock-pinned {asset['file']}")
    out = Path(out)
    out.mkdir()
    extra = out / "extra"
    extra.mkdir()
    with zipfile.ZipFile(archive) as packed:
        seen = set()
        for info in packed.infolist():
            name = info.filename
            if name.startswith("/") or "\\" in name or ".." in PurePosixPath(name).parts:
                raise ValueError(f"unexpected archive member {name!r}")
            if info.is_dir() or not WIN_RUNTIME.fullmatch(name):
                continue
            if name in seen:
                raise ValueError(f"duplicate archive member {name!r}")
            seen.add(name)
            with packed.open(info) as source, open(extra / name, "wb") as target:
                shutil.copyfileobj(source, target)
    for name in crt:
        shutil.copyfile(artifact / name, extra / name)
    shutil.copyfile(lock_path, extra / "llama.lock")
    shutil.copyfile(WEIGHTS, extra / "weights.lock")
    shutil.copyfile(artifact / WIN_BINARY, out / WIN_BINARY)
    names = sorted(p.name for p in extra.iterdir())
    missing = sorted(WIN_REQUIRED - set(names))
    if missing:
        raise ValueError("bundle lacks " + ", ".join(missing))
    bound = json.loads(DISK_POLICY.read_text())["native_bundle_bound_bytes"]
    if sum(p.stat().st_size for p in [out / WIN_BINARY, *extra.iterdir()]) > bound:
        raise ValueError("bundle exceeds the reviewed native Qwen disk envelope")
    llama = LLAMA_SOURCE[lock["tag"]]
    release = {
        "release": {"app": "usnm_qwen", "kind": "native", "platform": WIN_PLATFORM, "plan_class": plan_class},
        "worker": {
            "sha256": sha256(out / WIN_BINARY),
            "source_commit": source_commit,
            # The locks and the Microsoft runtime come with the app's own commit; the rest is llama.cpp's.
            "extra_files": {name: {"sha256": sha256(extra / name),
                                   "source_commit": source_commit if name.endswith(".lock") or name in crt else llama}
                            for name in names},
        },
        "runtime": {"repository": lock["repository"], "tag": lock["tag"], "file": asset["file"],
                    "sha256": asset["sha256"], "source_commit": llama},
    }
    (out / "release.json").write_text(json.dumps(release, indent=2, sort_keys=True) + "\n")
    write_sums(out, [WIN_BINARY, "release.json", *(f"extra/{name}" for name in names)])
    return release


def layout(release):
    """(binary name, required extras) of a release.json's platform."""
    if release["release"]["platform"] == WIN_PLATFORM:
        return WIN_BINARY, WIN_REQUIRED
    return BINARY, REQUIRED


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
    binary, required = layout(release)
    if (listed.get(binary) != worker["sha256"] or set(extras) != {n[len("extra/"):] for n in listed if n.startswith("extra/")}
            or any(listed["extra/" + n] != e["sha256"] for n, e in extras.items()) or not required <= set(extras)):
        raise ValueError("release.json does not match the bundle")
    if same_runtime_as is not None:
        reviewed = json.loads(Path(same_runtime_as).read_text())["worker"]["extra_files"]
        if runtime_hashes(reviewed) != runtime_hashes(extras):
            raise ValueError(f"the runtime differs from {same_runtime_as}")
    return bundle, release


def release_args(bundle):
    bundle, release = verify(bundle)
    lines = ["--binary", str(bundle / layout(release)[0]), "--binary-sha256", release["worker"]["sha256"]]
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
    win = commands.add_parser("build-windows")
    win.add_argument("--archive", type=Path, required=True, help="the llama.lock-pinned win-vulkan-x64 zip")
    win.add_argument("--artifact", type=Path, required=True,
                     help="the unpacked boinc-native-windows_x86_64 CI artifact of --source-commit")
    win.add_argument("--source-commit", required=True)
    win.add_argument("--plan-class", required=True, choices=WIN_PLAN_CLASSES)
    win.add_argument("--out", type=Path, required=True)
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
        elif args.command == "build-windows":
            release = build_windows(args.archive, args.artifact, args.source_commit, args.out, args.plan_class)
            print(f"{release['worker']['sha256']}  {args.out / WIN_BINARY}")
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
