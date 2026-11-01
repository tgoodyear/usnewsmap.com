#!/usr/bin/env python3
"""Install operator-staged, commit-pinned data-flow code; never start services or migrate data."""
import argparse
import fcntl
import hashlib
import io
import os
from pathlib import Path
import pwd
import re
import shutil
import stat
import subprocess
import tarfile
import tempfile
import uuid
from urllib.parse import urlsplit


FILES = {
    "scripts/corpus/data_catalog.py": "usr/local/lib/usnm-data-flow/data_catalog.py",
    "scripts/corpus/data_flow_observer.py": "usr/local/lib/usnm-data-flow/data_flow_observer.py",
    "scripts/ocr-node/usnm-data-catalog.service": "etc/systemd/system/usnm-data-catalog.service",
    "scripts/ocr-node/usnm-data-flow-observer.service":
        "etc/systemd/system/usnm-data-flow-observer.service",
    "scripts/ocr-node/usnm-data-flow-observer.timer":
        "etc/systemd/system/usnm-data-flow-observer.timer",
    "scripts/ocr-node/usnm-data-catalog-refresh.service":
        "etc/systemd/system/usnm-data-catalog-refresh.service",
    "scripts/ocr-node/usnm-data-catalog-refresh.timer":
        "etc/systemd/system/usnm-data-catalog-refresh.timer",
    "scripts/ocr-node/install-data-flow.py": "usr/local/sbin/usnm-data-flow-install",
}
UNITS = ("usnm-data-catalog.service", "usnm-data-flow-observer.service",
         "usnm-data-flow-observer.timer", "usnm-data-catalog-refresh.service",
         "usnm-data-catalog-refresh.timer")
ENV = "etc/usnm/data-flow-observer.env"
STAMP = "usr/local/lib/usnm-data-flow/COMMIT"


def read_archive(path, commit, sha256):
    if not re.fullmatch(r"[a-f0-9]{40}", commit) or not re.fullmatch(r"[a-f0-9]{64}", sha256):
        raise ValueError("supply a full commit and independently recorded archive SHA-256")
    with path.open("rb") as stream:
        if os.fstat(stream.fileno()).st_size > 8 * 1024 * 1024:
            raise ValueError("archive exceeds 8 MiB")
        snapshot = stream.read(8 * 1024 * 1024 + 1)
        if len(snapshot) > 8 * 1024 * 1024:
            raise ValueError("archive exceeds 8 MiB")
        if hashlib.sha256(snapshot).hexdigest() != sha256:
            raise ValueError("archive SHA-256 differs")
        with tarfile.open(fileobj=io.BytesIO(snapshot), mode="r:") as archive:
            if archive.pax_headers.get("comment") != commit:
                raise ValueError("git archive commit differs")
            payload = {}
            for entry in archive:
                if entry.isdir() and entry.name in ("scripts", "scripts/corpus", "scripts/ocr-node"):
                    continue
                if (entry.name not in FILES or entry.name in payload
                        or not entry.isfile() or not 0 < entry.size <= 1024 * 1024):
                    raise ValueError("unexpected, duplicate or unsafe archive entry")
                data = archive.extractfile(entry).read()
                if entry.name.endswith(".py"):
                    compile(data, entry.name, "exec")
                payload[entry.name] = data
            if set(payload) != set(FILES):
                raise ValueError("archive is missing required data-flow files")
            return {FILES[name]: data for name, data in payload.items()}


def atomic_write(path, data, mode):
    fd, temporary = tempfile.mkstemp(prefix="." + path.name + ".", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def owned_parents(root, directory):
    current = root
    for component in directory.relative_to(root).parts:
        current = current / component
        try:
            current.mkdir(mode=0o755)
        except FileExistsError:
            pass
        else:
            current.chmod(0o755)
        metadata = current.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
                or metadata.st_mode & 0o022):
            raise ValueError("installation parent must be an owned, non-writable directory")


def validate_url(value):
    url = urlsplit(value)
    if (url.scheme not in ("http", "https") or not url.hostname or url.username
            or url.password or url.query or url.fragment
            or any(ord(c) < 33 or ord(c) > 126 or c in "\"'\\" for c in value)):
        raise ValueError("OCR URL must be a credential-free HTTP(S) URL without queries or quoting")
    _ = url.port
    if not url.path.endswith("/api/view.json"):
        raise ValueError("OCR URL must name the dashboard /api/view.json endpoint")


def verify_units(payload):
    with tempfile.TemporaryDirectory(prefix="usnm-data-flow-verify-") as temp:
        paths = []
        for unit in UNITS:
            path = Path(temp) / unit
            path.write_bytes(payload["etc/systemd/system/" + unit])
            paths.append(str(path))
        subprocess.run(["systemd-analyze", "verify", *paths], check=True)


def install(payload, commit, ocr_url=None, root=Path("/")):
    changes = {**payload, STAMP: (commit + "\n").encode()}
    if ocr_url is not None:
        validate_url(ocr_url)
        if (root / ENV).exists():
            raise ValueError("preserve the existing observer environment; omit --ocr-url")
        changes[ENV] = ("USNM_DATA_FLOW_OCR_URL=" + ocr_url + "\n").encode()
    elif not (root / ENV).is_file():
        raise ValueError("first install requires --ocr-url")
    verify_units(payload)
    for relative in changes:
        path = root / relative
        owned_parents(root, path.parent)
        if path.is_symlink() or (path.exists() and not path.is_file()):
            raise ValueError("installation target is not a regular file: " + relative)
        if path.exists() and path.stat().st_uid != os.geteuid():
            raise ValueError("installation target has unexpected ownership: " + relative)
    backup_root = root / "var/lib/usnm-data-flow-install"
    owned_parents(root, backup_root.parent)
    backup_root.mkdir(mode=0o700, exist_ok=True)
    metadata = backup_root.lstat()
    if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.geteuid()
            or stat.S_IMODE(metadata.st_mode) != 0o700):
        raise ValueError("installation rollback directory must be owned and private")
    backup = Path(tempfile.mkdtemp(prefix=commit[:12] + "-", dir=backup_root))
    originals = {}
    modified = []
    unrestored = set()
    try:
        for relative in changes:
            path = root / relative
            originals[relative] = None
            if path.exists():
                saved = backup / relative
                saved.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, saved)
                metadata = path.stat()
                os.chown(saved, metadata.st_uid, metadata.st_gid)
                link = path.with_name("." + path.name + "." + uuid.uuid4().hex + ".rollback")
                os.link(path, link, follow_symlinks=False)
                originals[relative] = link
        for relative, data in changes.items():
            path = root / relative
            modified.append(relative)
            atomic_write(path, data, 0o755 if relative == FILES["scripts/ocr-node/install-data-flow.py"]
                         else 0o644)
        subprocess.run(["systemd-analyze", "verify", *[
            str(root / "etc/systemd/system" / unit) for unit in UNITS]], check=True)
        subprocess.run(["systemctl", "daemon-reload"], check=True)
    except BaseException as original:
        failures = []
        for relative in reversed(modified):
            try:
                path, saved = root / relative, originals[relative]
                if saved is not None:
                    os.replace(saved, path)
                else:
                    path.unlink(missing_ok=True)
            except OSError as error:
                failures.append(error)
                unrestored.add(relative)
        try:
            subprocess.run(["systemctl", "daemon-reload"], check=True)
        except (OSError, subprocess.CalledProcessError) as error:
            failures.append(error)
        if failures:
            raise BaseExceptionGroup("installation failed and rollback requires operator recovery",
                                     [original, *failures])
        raise
    finally:
        for relative, saved in originals.items():
            if saved is not None and relative not in unrestored:
                saved.unlink(missing_ok=True)
    return backup


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--ocr-url")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    payload = read_archive(args.archive, args.commit, args.sha256)
    if args.ocr_url is not None:
        validate_url(args.ocr_url)
    if not args.apply:
        verify_units(payload)
        print("Validated commit " + args.commit + "; no installed files or services changed")
        return
    if os.geteuid() != 0:
        raise PermissionError("apply as root inside the dashboard host")
    pwd.getpwnam("ocrdash")
    fd = os.open("/run/usnm-data-flow-install.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        for unit in UNITS:
            state = subprocess.run(
                ["systemctl", "show", "--property=ActiveState", "--value", unit],
                check=True, capture_output=True, text=True).stdout.strip()
            if state not in ("inactive", "failed"):
                raise RuntimeError("stop " + unit + " in a maintenance window before installation")
        backup = install(payload, args.commit, args.ocr_url)
    print("Installed commit " + args.commit + "; rollback: " + str(backup))
    print("No services started or enabled; no catalog data or snapshots changed")


if __name__ == "__main__":
    main()
