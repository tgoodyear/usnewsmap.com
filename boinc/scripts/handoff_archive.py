#!/usr/bin/env python3
"""The per-job hand-off objects on the BOINC host, for the held-page archive and the cross-build
check on another host (#1367). onprem-step.sh runs these for `--target handoff --step
archive-list|archive-read|archive-delete|archive-write`; the controller host's archive_history.py
and fetch_handoff.py reach them through handoff_ssh.py. They stand where the Azure container's
list, get, delete and create-only put stood, over the hand-off root (HANDOFF_URL=/handoff-state).

    handoff_archive.py --root DIR list --prefix P [--suffix S] [--max-age-secs N]
    handoff_archive.py --root DIR read --object NAME
    handoff_archive.py --root DIR read-many < {"names": [N, ...]}
    handoff_archive.py --root DIR delete < {"items": [{"name": N, "etag": E}, ...]}
    handoff_archive.py --root DIR write --object NAME [--md5 B64] < bytes

Only the three per-job prefixes are reachable: dispatch/jobs/<id>/, dispatch/quarantine/<id>/ and
spool/<id>/ (<id> is 64 hex digits). Every path is a regular file below the root with no link on
the way, and each name segment starts with a letter or digit.

list prints {"entries": [{"name", "etag", "size", "md5": null, "modified"}]} for the files whose
name starts with the prefix (and ends with --suffix, and changed at most --max-age-secs ago). The
ETag has the shape of usnm-store's LocalStore (device and inode, size, modification time), which
every write changes because each makes a new file; `modified` is an HTTP date as the container
gave. There is no Content-MD5: a file is not hashed to be listed.

read prints one JSON header line, {"etag", "size", "sha256"}, then the file's bytes (at most
512 MiB). The file is read from one open descriptor and refused if it changed while it was read.

read-many prints that frame (the header line, then the bytes) for each name, in order, for up to 1,000
names and 256 MiB in all: a held-page archive reads thousands of small journals and spool files, and
one ssh session for each is slow on a busy host. A name that is missing or changed while read
fails the call.

delete removes each item in order only while it still has the ETag that was archived, and stops
at the first that changed (nothing after it is touched); an item that is gone is counted as such.
It prints {"deleted": N, "gone": M}. Emptied job directories are removed.

write creates a file only where none is (a hard link from a private temporary file, so a reader
never sees a partial file), with the md5 checked when given. It prints {"written": true}, or
{"written": false} when the file is already there.
"""

import argparse
import base64
import email.utils
import hashlib
import json
import os
import re
import stat
import sys
from pathlib import Path

ROOTS = ("dispatch/jobs/", "dispatch/quarantine/", "spool/")
SEGMENT = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")
JOB_NAME = re.compile(r"(?:dispatch/jobs|dispatch/quarantine|spool)/[0-9a-f]{64}(/[A-Za-z0-9][A-Za-z0-9._-]*)+")
PREFIX = re.compile(r"(?:dispatch/jobs/|dispatch/quarantine/|spool/)[A-Za-z0-9._/-]{0,300}")
MAX_READ = 512 * 1024 * 1024
MAX_READ_MANY_NAMES = 1000
MAX_READ_MANY_BYTES = 256 * 1024 * 1024
# A dispatch page file is up to 128 MiB (usnm-ocr-contract MAX_SCAN): a restore writes it back.
MAX_WRITE = 128 * 1024 * 1024
MAX_ITEMS = 5000
MAX_STDIN_JSON = 8 * 1024 * 1024


class ArchiveError(Exception):
    pass


def check_root(root):
    """As handoff_read.py: canonical, owned by the runner's user and mode 0700."""
    if not root.is_dir() or root.is_symlink():
        raise ArchiveError("hand-off root is not a directory")
    info = root.stat()
    if (not root.is_absolute() or root.resolve() != root or info.st_uid != os.geteuid()
            or info.st_mode & 0o777 != 0o700):
        raise ArchiveError("hand-off root must be canonical, owned by the runner user and mode 0700")


def safe_name(name):
    if not isinstance(name, str) or not JOB_NAME.fullmatch(name) or len(name) > 400:
        raise ArchiveError("not a per-job hand-off object name")
    return name


def etag(info):
    """usnm-store's local_etag: device and inode, size, modification time."""
    return f"{info.st_dev:x}.{info.st_ino:x}-{info.st_size:x}-{info.st_mtime_ns:x}"


def modified(info):
    return email.utils.formatdate(info.st_mtime, usegmt=True)


def walk(directory, relative):
    """Every regular file below `directory`, as names below the root. A link, a special file or
    a name outside the segment rule is refused; a temporary file of a writer (a leading dot) is
    skipped, as LocalStore's listing skips it."""
    try:
        entries = sorted(os.scandir(directory), key=lambda entry: entry.name)
    except FileNotFoundError:
        return
    for entry in entries:
        if entry.name.startswith("."):
            continue
        if not SEGMENT.fullmatch(entry.name):
            raise ArchiveError(f"unsafe name under {relative}")
        info = entry.stat(follow_symlinks=False)
        name = f"{relative}{entry.name}"
        if stat.S_ISDIR(info.st_mode):
            yield from walk(entry.path, name + "/")
        elif stat.S_ISREG(info.st_mode):
            yield name, info
        else:
            raise ArchiveError(f"{name} is not a regular file")


def list_objects(root, prefix, suffix="", max_age_secs=None, now=None):
    if not PREFIX.fullmatch(prefix) or ".." in prefix or "//" in prefix:
        raise ArchiveError("the prefix must start with a per-job root")
    if suffix and not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", suffix):
        raise ArchiveError("invalid suffix")
    cutoff = None
    if max_age_secs is not None:
        import time
        cutoff = (time.time() if now is None else now) - max_age_secs
    directory, _, start = prefix.rpartition("/")
    base = root / directory if directory else root
    # The directory the prefix ends in; a partial last segment (a leading hex digit) filters its
    # children, so a listing of dispatch/jobs/0 reads only the job directories that start with 0.
    entries = []
    if base.exists() and base.resolve() != base:
        raise ArchiveError("unsafe directory")
    try:
        children = sorted(os.scandir(base), key=lambda entry: entry.name)
    except FileNotFoundError:
        return entries
    for child in children:
        if child.name.startswith(".") or not child.name.startswith(start):
            continue
        if not SEGMENT.fullmatch(child.name):
            raise ArchiveError(f"unsafe name under {directory}/")
        info = child.stat(follow_symlinks=False)
        name = f"{directory}/{child.name}"
        if stat.S_ISDIR(info.st_mode):
            found = walk(child.path, name + "/")
        elif stat.S_ISREG(info.st_mode):
            found = [(name, info)]
        else:
            raise ArchiveError(f"{name} is not a regular file")
        for found_name, found_info in found:
            if not JOB_NAME.fullmatch(found_name):
                continue
            if suffix and not found_name.endswith(suffix):
                continue
            if cutoff is not None and found_info.st_mtime < cutoff:
                continue
            entries.append({"name": found_name, "etag": etag(found_info), "size": found_info.st_size,
                            "md5": None, "modified": modified(found_info)})
    return sorted(entries, key=lambda entry: entry["name"])


def open_regular(root, name):
    """The file as one descriptor, opened without following a link, and its stat."""
    safe_name(name)
    path = root / name
    try:
        parent = path.parent.resolve()
        if parent != path.parent:
            raise ArchiveError("unsafe object path")
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except (FileNotFoundError, NotADirectoryError):
        raise ArchiveError("missing object") from None
    except OSError:
        raise ArchiveError("missing or unsafe object") from None
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode):
        os.close(fd)
        raise ArchiveError("missing or unsafe object")
    return fd, info


def read_object(root, name, out):
    fd, before = open_regular(root, name)
    with os.fdopen(fd, "rb") as handle:
        if before.st_size > MAX_READ:
            raise ArchiveError("object exceeds limit")
        data = handle.read(MAX_READ + 1)
        after = os.fstat(handle.fileno())
    if len(data) > MAX_READ or etag(before) != etag(after) or len(data) != before.st_size:
        raise ArchiveError("object changed while it was read")
    header = {"etag": etag(before), "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
    out.write(json.dumps(header, sort_keys=True).encode() + b"\n")
    out.write(data)
    out.flush()


def read_many(root, names, out):
    if not isinstance(names, list) or not 1 <= len(names) <= MAX_READ_MANY_NAMES \
            or not all(isinstance(name, str) for name in names):
        raise ArchiveError(f"read-many takes 1 to {MAX_READ_MANY_NAMES} names")
    total = 0
    for name in names:
        fd, before = open_regular(root, name)
        total += before.st_size
        if total > MAX_READ_MANY_BYTES:
            os.close(fd)
            raise ArchiveError("read-many is over its byte limit")
        with os.fdopen(fd, "rb") as handle:
            data = handle.read(MAX_READ + 1)
            after = os.fstat(handle.fileno())
        if etag(before) != etag(after) or len(data) != before.st_size:
            raise ArchiveError("object changed while it was read")
        header = {"name": name, "etag": etag(before), "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        out.write(json.dumps(header, sort_keys=True).encode() + b"\n")
        out.write(data)
    out.flush()


def delete_objects(root, items):
    if not isinstance(items, list) or not items or len(items) > MAX_ITEMS:
        raise ArchiveError("delete takes 1 to 5000 items")
    for item in items:
        if not (isinstance(item, dict) and set(item) == {"name", "etag"} and isinstance(item["etag"], str)):
            raise ArchiveError("each item is {name, etag}")
        safe_name(item["name"])
    deleted = gone = 0
    for item in items:
        path = root / item["name"]
        try:
            if path.parent.resolve() != path.parent:
                raise ArchiveError("unsafe object path")
            info = path.lstat()
        except FileNotFoundError:
            gone += 1
            continue
        if not stat.S_ISREG(info.st_mode):
            raise ArchiveError(f"{item['name']} is not a regular file")
        if etag(info) != item["etag"]:
            raise ArchiveError(f"{item['name']} changed after it was archived; nothing more was deleted")
        path.unlink()
        deleted += 1
    fsync_parents(root, items)
    for item in items:
        remove_empty(root, item["name"])
    return {"deleted": deleted, "gone": gone}


def fsync_parents(root, items):
    for directory in sorted({(root / item["name"]).parent for item in items}):
        try:
            fd = os.open(directory, os.O_RDONLY)
        except FileNotFoundError:
            continue
        try:
            os.fsync(fd)
        finally:
            os.close(fd)


def remove_empty(root, name):
    """The job directory (and a nested one such as accepted/) a delete left empty, never the
    per-job root itself."""
    floor = 3 if name.startswith("dispatch/") else 2
    parent = Path(name).parent
    while len(parent.parts) >= floor:
        try:
            (root / parent).rmdir()
        except OSError:
            return
        parent = parent.parent


def write_object(root, name, data, md5=None):
    safe_name(name)
    if len(data) > MAX_WRITE:
        raise ArchiveError("object exceeds limit")
    if md5 and base64.b64encode(hashlib.md5(data).digest()).decode() != md5:
        raise ArchiveError("Md5Mismatch: the data is not the md5 given")
    path = root / name
    for ancestor in reversed(path.relative_to(root).parents):
        directory = root / ancestor
        if directory != root and directory.is_symlink():
            raise ArchiveError("unsafe object path")
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if path.parent.resolve() != path.parent:
        raise ArchiveError("unsafe object path")
    temporary = path.parent / f".tmp-archive-{os.getpid()}-{os.urandom(6).hex()}"
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(temporary, path)
        except FileExistsError:
            return False
    finally:
        temporary.unlink(missing_ok=True)
    dirfd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)
    return True


def main(argv=None, stdin=None, stdout=None):
    stdin = stdin if stdin is not None else sys.stdin.buffer
    stdout = stdout if stdout is not None else sys.stdout.buffer
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    one = sub.add_parser("list")
    one.add_argument("--prefix", required=True)
    one.add_argument("--suffix", default="")
    one.add_argument("--max-age-secs", type=int)
    two = sub.add_parser("read")
    two.add_argument("--object", required=True)
    sub.add_parser("read-many")
    sub.add_parser("delete")
    three = sub.add_parser("write")
    three.add_argument("--object", required=True)
    three.add_argument("--md5")
    args = parser.parse_args(argv)
    try:
        check_root(args.root)
        if args.command == "list":
            if args.max_age_secs is not None and args.max_age_secs < 0:
                raise ArchiveError("--max-age-secs is not negative")
            entries = list_objects(args.root, args.prefix, args.suffix, args.max_age_secs)
            stdout.write(json.dumps({"entries": entries}, sort_keys=True, separators=(",", ":")).encode() + b"\n")
        elif args.command == "read":
            read_object(args.root, args.object, stdout)
        elif args.command == "read-many":
            body = stdin.read(MAX_STDIN_JSON + 1)
            try:
                if len(body) > MAX_STDIN_JSON:
                    raise ValueError
                names = json.loads(body)["names"]
            except (ValueError, KeyError, TypeError):
                raise ArchiveError("read-many takes {names: [...]} on stdin") from None
            read_many(args.root, names, stdout)
        elif args.command == "delete":
            body = stdin.read(MAX_STDIN_JSON + 1)
            if len(body) > MAX_STDIN_JSON:
                raise ArchiveError("delete input is over its limit")
            try:
                items = json.loads(body)["items"]
            except (ValueError, KeyError, TypeError):
                raise ArchiveError("delete takes {items: [...]} on stdin") from None
            stdout.write(json.dumps(delete_objects(args.root, items), sort_keys=True).encode() + b"\n")
        else:
            data = stdin.read(MAX_WRITE + 1)
            written = write_object(args.root, args.object, data, args.md5)
            stdout.write(json.dumps({"written": written}).encode() + b"\n")
        stdout.flush()
    except (ArchiveError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
