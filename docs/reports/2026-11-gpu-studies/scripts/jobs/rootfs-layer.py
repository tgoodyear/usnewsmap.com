#!/usr/bin/env python3
"""Verify and apply one OCI layer without following whiteout-path symlinks.

The root must be private to this extraction. Whiteouts and lower type replacements
are applied before extracting any member, so they cannot remove this layer's files.
Absolute symlinks may be stored for use in the chroot, but are never traversed here.
On filesystems that alias case or Unicode normalization, non-ASCII names and
case-ambiguous paths are refused rather than silently changing the image.
"""

import argparse
import hashlib
import math
import os
import re
import stat
import subprocess
import sys
import tarfile
import uuid


class LayerError(Exception):
    pass


PAX_FIELDS = {
    "path", "linkpath", "size", "mtime", "atime", "ctime", "uid", "gid",
    "uname", "gname", "charset", "hdrcharset", "comment",
}
PAX_FIELD_BYTES = {field.encode("ascii") for field in PAX_FIELDS}
REPO_COMPONENT = r"[a-z0-9]+(?:(?:[._]|__|-+)[a-z0-9]+)*"
PAX_TYPES = (tarfile.XHDTYPE, tarfile.XGLTYPE, tarfile.SOLARIS_XHDTYPE)
LONG_TYPES = (tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK)


class CheckedTarInfo(tarfile.TarInfo):
    def _proc_member(self, archive):
        if self.type not in PAX_TYPES + LONG_TYPES:
            return super()._proc_member(archive)
        parents = getattr(archive, "_rootfs_extension", ())
        if parents and (len(parents) != 1 or {parents[0], self.type} != set(LONG_TYPES)):
            raise LayerError("chained archive metadata is unsupported")
        if self.size < 0 or self.size > 1024 * 1024:
            raise LayerError("invalid archive metadata size")
        if not hasattr(archive, "_rootfs_extensions"):
            archive._rootfs_extensions = []
        archive._rootfs_extensions.append((archive.fileobj.tell(), self.size, self.type))
        archive._rootfs_extension = parents + (self.type,)
        try:
            return super()._proc_member(archive)
        finally:
            archive._rootfs_extension = parents


def validate_extension(data, kind):
    if kind in LONG_TYPES:
        if len(data) < 2 or not data.endswith(b"\0") or b"\0" in data[:-1]:
            raise LayerError("invalid GNU long name")
        return
    fields = set()
    position = 0
    while position < len(data):
        space = data.find(b" ", position)
        if space < 0:
            raise LayerError("invalid PAX record")
        digits = data[position:space]
        if (not digits or len(digits) > 10 or digits[0] == ord("0")
                or not digits.isdigit()):
            raise LayerError("invalid PAX record length")
        end = position + int(digits)
        if end > len(data) or end <= space + 2 or data[end - 1] != 10:
            raise LayerError("invalid PAX record length")
        equals = data.find(b"=", space + 1, end - 1)
        if equals <= space + 1:
            raise LayerError("invalid PAX record")
        key, value = data[space + 1:equals], data[equals + 1:end - 1]
        if key not in PAX_FIELD_BYTES:
            raise LayerError(f"unsupported layer metadata: {key!r}")
        if key in fields or not value or b"\0" in value or b"\n" in value:
            raise LayerError("invalid or duplicate PAX field")
        fields.add(key)
        position = end


def root_fd(path):
    if not os.path.isabs(path) or os.path.normpath(path) != path or path == "/":
        raise LayerError("ROOTFS must be an explicitly resolved, non-root directory")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    fd = os.open("/", flags)
    try:
        for component in path.split("/")[1:]:
            next_fd = os.open(component, flags, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def components(name):
    if not name or name.startswith("/") or "\0" in name:
        raise LayerError(f"unsafe archive path: {name!r}")
    parts = tuple(part for part in name.split("/") if part and part != ".")
    if ".." in parts:
        raise LayerError(f"unsafe archive path: {name!r}")
    return parts


def scan_layer(fd, case_aliases, unicode_aliases):
    os.lseek(fd, 0, os.SEEK_SET)
    with os.fdopen(os.dup(fd), "rb", buffering=0) as stream:
        with tarfile.open(fileobj=stream, mode="r:*", tarinfo=CheckedTarInfo) as archive:
            members = archive.getmembers()
            for offset, size, kind in getattr(archive, "_rootfs_extensions", ()):
                archive.fileobj.seek(offset)
                data = archive.fileobj.read(size)
                if len(data) != size:
                    raise LayerError("truncated archive metadata")
                validate_extension(data, kind)
            archive.fileobj.seek(archive.offset)
            trailer_size = 0
            while True:
                data = archive.fileobj.read(1024 * 1024)
                if not data:
                    break
                if data.strip(b"\0"):
                    raise LayerError("invalid or concatenated tar trailer")
                trailer_size += len(data)
            if trailer_size < 1024 or trailer_size % 512:
                raise LayerError("incomplete tar end marker")

    paths = {}
    whiteouts = []
    normal = {}
    hardlinks = []
    for member in members:
        unsupported = set(member.pax_headers) - PAX_FIELDS
        if unsupported or member.sparse is not None:
            raise LayerError(f"unsupported layer metadata: {member.name!r} {sorted(unsupported)!r}")
        for field in ("size", "uid", "gid"):
            value = member.pax_headers.get(field)
            if value is not None and re.fullmatch(r"[0-9]+", value) is None:
                raise LayerError(f"invalid PAX {field}: {member.name!r}")
        for field in ("mtime", "atime", "ctime"):
            value = member.pax_headers.get(field)
            if value is not None and re.fullmatch(r"-?[0-9]+(?:\.[0-9]+)?", value) is None:
                raise LayerError(f"invalid PAX {field}: {member.name!r}")
        if (member.size < 0 or member.uid < 0 or member.gid < 0
                or not math.isfinite(member.mtime)
                or (not member.isfile() and member.size != 0)):
            raise LayerError(f"invalid archive metadata: {member.name!r}")
        path = components(member.name)
        if path in paths:
            raise LayerError(f"duplicate archive path: {member.name!r}")
        paths[path] = member
        if not path:
            if not member.isdir():
                raise LayerError("archive root entry is not a directory")
            normal[path] = member
            continue
        if any(part.startswith(".wh.") for part in path[:-1]):
            raise LayerError(f"entry below a whiteout: {member.name!r}")
        name = path[-1]
        if name.startswith(".wh."):
            if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE) or member.size != 0:
                raise LayerError(f"whiteout is not an empty regular file: {member.name!r}")
            if name == ".wh..wh..opq":
                whiteouts.append((path[:-1], None))
            else:
                target = name[4:]
                if target in ("", ".", "..") or target.startswith(".wh."):
                    raise LayerError(f"invalid whiteout: {member.name!r}")
                whiteouts.append((path[:-1], target))
            continue
        if not (member.isfile() or member.isdir() or member.issym() or member.islnk()
                or member.isfifo() or member.ischr() or member.isblk()):
            raise LayerError(f"unsupported archive entry: {member.name!r}")
        if member.issym() and (not member.linkname or "\0" in member.linkname):
            raise LayerError(f"invalid symlink: {member.name!r}")
        if member.islnk():
            target = components(member.linkname)
            if not target or target == path or any(part.startswith(".wh.") for part in target):
                raise LayerError(f"invalid hardlink: {member.name!r}")
            hardlinks.append((path, target))
        normal[path] = member

    checked_paths = list(paths) + [target for _, target in hardlinks]
    if case_aliases or unicode_aliases:
        for path in checked_paths:
            if any(not part.isascii() for part in path):
                raise LayerError("filesystem aliases names; non-ASCII paths are unsupported")
    if case_aliases:
        seen = {}
        for path in checked_paths:
            for length in range(1, len(path) + 1):
                prefix = path[:length]
                previous = seen.setdefault(tuple(part.lower() for part in prefix), prefix)
                if previous != prefix:
                    raise LayerError(
                        f"filesystem aliases archive paths: {'/'.join(previous)!r} and "
                        f"{'/'.join(prefix)!r}"
                    )

    for path in normal:
        for length in range(1, len(path)):
            parent = normal.get(path[:length])
            if parent is not None and not parent.isdir():
                raise LayerError(f"non-directory archive ancestor: {'/'.join(path)!r}")
    for path, target in hardlinks:
        for length in range(1, len(target)):
            parent = normal.get(target[:length])
            if parent is not None and not parent.isdir():
                raise LayerError(f"hardlink target below a replacement: {'/'.join(path)!r}")
        target_member = normal.get(target)
        if target_member is not None and (target_member.isdir() or target_member.issym()):
            raise LayerError(f"unsafe hardlink target: {'/'.join(target)!r}")
    return normal, whiteouts, hardlinks


class Names:
    def __init__(self, aliases):
        self.aliases = aliases
        self.cache = {}

    def key(self, parent):
        entry = os.fstat(parent)
        return entry.st_dev, entry.st_ino

    def stat(self, parent, name):
        try:
            entry = os.stat(name, dir_fd=parent, follow_symlinks=False)
        except FileNotFoundError:
            return None
        if self.aliases:
            key = self.key(parent)
            if key not in self.cache:
                self.cache[key] = set(os.listdir(parent))
            if name not in self.cache[key]:
                raise LayerError(f"filesystem aliases an existing entry: {name!r}")
        return entry

    def removed(self, parent, name):
        if self.aliases:
            entries = self.cache.get(self.key(parent))
            if entries is not None:
                entries.discard(name)


def open_directory(root, path, names, replacements=()):
    """Open a lower-layer directory, or return None if it will not exist."""
    fd = os.dup(root)
    root_device = os.fstat(root).st_dev
    try:
        for index, name in enumerate(path, 1):
            replacement = replacements.get(path[:index]) if replacements else None
            if replacement is not None and not replacement.isdir():
                return None
            entry = names.stat(fd, name)
            if entry is None:
                return None
            if stat.S_ISLNK(entry.st_mode):
                if replacement is not None and replacement.isdir():
                    return None
                raise LayerError(f"symlink in layer path: {'/'.join(path[:index])!r}")
            if not stat.S_ISDIR(entry.st_mode):
                return None
            child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            if os.fstat(child).st_dev != root_device:
                os.close(child)
                raise LayerError(f"mount in layer path: {'/'.join(path[:index])!r}")
            os.close(fd)
            fd = child
        result, fd = fd, None
        return result
    finally:
        if fd is not None:
            os.close(fd)


def remove_entry(parent, name, root_device, names):
    entry = names.stat(parent, name)
    if entry is None:
        return
    if not stat.S_ISDIR(entry.st_mode):
        os.unlink(name, dir_fd=parent)
        names.removed(parent, name)
        return
    child = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
    try:
        if os.fstat(child).st_dev != root_device:
            raise LayerError(f"refusing to remove a mount: {name!r}")
        for item in os.listdir(child):
            remove_entry(child, item, root_device, names)
    finally:
        os.close(child)
    os.rmdir(name, dir_fd=parent)
    names.removed(parent, name)


def apply_whiteouts(root, normal, whiteouts, names):
    device = os.fstat(root).st_dev
    for parent_path, target in sorted(whiteouts, key=lambda item: len(item[0])):
        parent = open_directory(root, parent_path, names, normal)
        if parent is None:
            continue
        try:
            if target is None:
                for name in os.listdir(parent):
                    remove_entry(parent, name, device, names)
            else:
                remove_entry(parent, target, device, names)
        finally:
            os.close(parent)


def prepare_replacements(root, normal, names):
    device = os.fstat(root).st_dev
    for path, member in sorted(normal.items(), key=lambda item: len(item[0])):
        if not path:
            continue
        parent = open_directory(root, path[:-1], names, normal)
        if parent is None:
            continue
        try:
            entry = names.stat(parent, path[-1])
            if entry is None:
                continue
            if (stat.S_ISLNK(entry.st_mode)
                    or stat.S_ISDIR(entry.st_mode) != member.isdir()):
                remove_entry(parent, path[-1], device, names)
        finally:
            os.close(parent)


def check_paths(root, normal, hardlinks, names):
    for path in normal:
        if path:
            fd = open_directory(root, path[:-1], names)
            if fd is not None:
                os.close(fd)
    for _, target in hardlinks:
        parent = open_directory(root, target[:-1], names)
        if parent is None:
            continue
        try:
            entry = names.stat(parent, target[-1])
            if entry is None:
                continue
            if stat.S_ISLNK(entry.st_mode) or stat.S_ISDIR(entry.st_mode):
                raise LayerError(f"unsafe hardlink target: {'/'.join(target)!r}")
        finally:
            os.close(parent)


def verified_layer(root, digest, repo=None):
    if re.fullmatch(r"sha256:[0-9a-f]{64}", digest) is None:
        raise LayerError("invalid layer digest")
    if repo is not None and re.fullmatch(REPO_COMPONENT + "/" + REPO_COMPONENT, repo) is None:
        raise LayerError("invalid image repository")
    name = ".usnm-oci-layer-" + uuid.uuid4().hex + "-\u00e9"
    root_times = os.fstat(root)
    fd = os.open(name, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=root)
    try:
        try:
            identity = os.fstat(fd)

            def same_file(alias):
                try:
                    entry = os.stat(alias, dir_fd=root, follow_symlinks=False)
                except FileNotFoundError:
                    return False
                return (entry.st_dev, entry.st_ino) == (identity.st_dev, identity.st_ino)

            case_aliases = same_file(name.replace(".usnm-oci-layer-", ".USNM-OCI-LAYER-"))
            unicode_aliases = same_file(name.replace("\u00e9", "e\u0301"))
            if (case_aliases or unicode_aliases) and name not in os.listdir(root):
                raise LayerError("filesystem does not preserve path spelling")
        finally:
            try:
                os.unlink(name, dir_fd=root)
            finally:
                os.utime(root, ns=(root_times.st_atime_ns, root_times.st_mtime_ns))
        checksum = hashlib.sha256()
        if repo is None:
            with os.fdopen(os.dup(fd), "wb") as layer:
                while True:
                    data = sys.stdin.buffer.read(1024 * 1024)
                    if not data:
                        break
                    checksum.update(data)
                    layer.write(data)
        else:
            config = sys.stdin.buffer.read(65537)
            if not config or len(config) > 65536:
                raise LayerError("missing or oversized registry authentication config")
            result = subprocess.run(
                ["curl", "-K", "-", "-fsSL", f"https://ghcr.io/v2/{repo}/blobs/{digest}"],
                input=config, stdout=fd, check=False,
            )
            if result.returncode:
                raise LayerError(f"layer download failed: {digest}")
            os.lseek(fd, 0, os.SEEK_SET)
            with os.fdopen(os.dup(fd), "rb", buffering=0) as layer:
                while True:
                    data = layer.read(1024 * 1024)
                    if not data:
                        break
                    checksum.update(data)
        if checksum.hexdigest() != digest[7:]:
            raise LayerError(f"layer sha256 mismatch: {digest}")
        return fd, case_aliases, unicode_aliases
    except BaseException:
        os.close(fd)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--action", choices=("check", "apply", "fetch"), required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--digest")
    parser.add_argument("--repo")
    args = parser.parse_args()
    if (args.action == "check" and (args.digest is not None or args.repo is not None)
            or args.action == "apply" and (args.digest is None or args.repo is not None)
            or args.action == "fetch" and (args.digest is None or args.repo is None)):
        parser.error("check needs only --root; apply needs --digest; fetch also needs --repo")
    root = root_fd(args.root)
    try:
        if args.action == "check":
            if os.listdir(root):
                raise LayerError("ROOTFS must be empty before applying the image")
            return
        layer, case_aliases, unicode_aliases = verified_layer(root, args.digest, args.repo)
        try:
            normal, whiteouts, hardlinks = scan_layer(layer, case_aliases, unicode_aliases)
            names = Names(case_aliases or unicode_aliases)
            apply_whiteouts(root, normal, whiteouts, names)
            prepare_replacements(root, normal, names)
            check_paths(root, normal, hardlinks, names)
            os.lseek(layer, 0, os.SEEK_SET)
            os.set_inheritable(layer, True)
            os.fchdir(root)
            env = os.environ.copy()
            env.pop("TAR_OPTIONS", None)
            os.execvpe(
                "tar",
                ["tar", "-xpf", f"/dev/fd/{layer}", "--same-owner", "--numeric-owner",
                 "--exclude=.wh.*", "--exclude=*/.wh.*"],
                env,
            )
        finally:
            os.close(layer)
    finally:
        os.close(root)


if __name__ == "__main__":
    try:
        main()
    except (LayerError, OSError, EOFError, tarfile.TarError, ValueError, OverflowError) as error:
        print(f"rootfs layer: {error}", file=sys.stderr)
        sys.exit(1)
