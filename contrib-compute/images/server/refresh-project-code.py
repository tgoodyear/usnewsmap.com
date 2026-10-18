#!/usr/bin/env python3
"""Inventory, back up and transactionally refresh copied BOINC runtime code only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat

TREES = ("bin", "cgi-bin", "py", "html/inc")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inventory(project, source, templates):
    rows = []
    for tree in TREES:
        root = project / tree
        if not root.is_dir() or root.is_symlink():
            raise ValueError(f"missing or linked runtime tree: {tree}")
        for path in sorted(root.rglob("*")):
            rel = path.relative_to(project)
            if any(parent.is_symlink() for parent in path.parents if parent != project and project in parent.parents):
                raise ValueError(f"linked parent: {rel}")
            if path.is_dir() and not path.is_symlink():
                continue
            row = {"path": str(rel)}
            if path.is_symlink():
                # Links resolve inside each image or to the existing start script; never rewrite them.
                target = os.readlink(path)
                allowed = (tree == "bin" and
                           ((path.name in ("stop", "status") and path.resolve() == (project / "bin/start").resolve()) or
                            (path.name.startswith("usnm_") and path.resolve().is_relative_to(templates.resolve()) and path.is_file())))
                if not allowed:
                    raise ValueError(f"unrecognized runtime link: {rel}")
                row.update(kind="symlink", target=target)
            elif path.is_file():
                row.update(kind="file", before=digest(path), mode=stat.S_IMODE(path.stat().st_mode),
                           uid=path.stat().st_uid, gid=path.stat().st_gid)
                if "__pycache__" in rel.parts or path.suffix == ".pyc" or str(rel) == "bin/boinc_path_config.py":
                    row["preserve"] = "generated project path or bytecode"
                else:
                    if tree == "bin":
                        candidates = [source / d / path.name for d in ("sched", "tools", "vda", "lib")]
                    elif tree == "cgi-bin":
                        candidates = [source / "sched" / path.name]
                    else:
                        candidates = [source / rel]
                    candidates = [p for p in candidates if p.is_file()]
                    if not candidates or len({digest(p) for p in candidates}) != 1:
                        raise ValueError(f"missing or ambiguous image source: {rel}")
                    row.update(source=str(candidates[0]), after=digest(candidates[0]))
            else:
                raise ValueError(f"unsupported runtime entry: {rel}")
            rows.append(row)
    if not rows:
        raise ValueError("empty runtime inventory")
    return rows


def replace(source, target, mode, uid=None, gid=None, expected=None):
    stage = target.with_name(target.name + ".usnm-tls-stage")
    if stage.exists() or stage.is_symlink():
        raise ValueError(f"staging file already exists: {target.name}")
    try:
        with stage.open("xb") as out, source.open("rb") as src:
            shutil.copyfileobj(src, out)
            out.flush()
            os.fsync(out.fileno())
        os.chmod(stage, mode)
        if uid is not None and gid is not None:
            os.chown(stage, uid, gid)
        if expected is not None and (target.is_symlink() or digest(target) != expected):
            raise ValueError(f"target drift before replacement: {target.name}")
        os.replace(stage, target)
    finally:
        if stage.exists():
            stage.unlink()


def changed(rows):
    return [row for row in rows if "after" in row and row["before"] != row["after"]]


def rollback(project, backup, only=None):
    manifest = json.loads((backup / "manifest.json").read_text())
    if str(project.resolve()) != manifest["project"]:
        raise ValueError("rollback project mismatch")
    for row in manifest["files"]:
        rel = Path(row["path"])
        if rel.is_absolute() or ".." in rel.parts or str(rel) == "." or not any(str(rel).startswith(tree + "/") for tree in TREES):
            raise ValueError("invalid manifest path")
        target = project / rel
        if any(parent.is_symlink() for parent in target.parents if parent != project and project in parent.parents):
            raise ValueError("linked rollback parent")
    rows = [row for row in changed(manifest["files"]) if only is None or row["path"] in only]
    failures = []
    restore = []
    for row in rows:
        target, saved = project / row["path"], backup / "files" / row["path"]
        try:
            if target.is_symlink() or digest(target) not in (row["before"], row["after"]):
                failures.append(f"rollback conflict: {row['path']}")
                continue
            if saved.is_symlink() or digest(saved) != row["before"]:
                failures.append(f"backup hash mismatch: {row['path']}")
                continue
            restore.append(row)
        except OSError:
            failures.append(f"rollback validation read failed: {row['path']}")
    for row in reversed(restore):
        try:
            target = project / row["path"]
            if not target.is_symlink() and digest(target) == row["before"]:
                continue
            replace(backup / "files" / row["path"], target, row["mode"], row["uid"], row["gid"], row["after"])
        except (OSError, ValueError):
            failures.append(f"rollback replacement failed: {row['path']}")
    if failures:
        raise ValueError("; ".join(failures))
    if any(digest(project / row["path"]) != row["before"] for row in rows):
        raise ValueError("rollback verification failed")
    return {"case": "project-code-rollback", "restored": len(rows)}


def apply(project, source, templates, backup):
    project, source, templates = project.resolve(), source.resolve(), templates.resolve()
    if project == Path("/") or project in backup.resolve().parents or backup.resolve() == project:
        raise ValueError("backup must be outside project")
    rows = inventory(project, source, templates)
    backup.mkdir(mode=0o700)
    for row in changed(rows):
        saved = backup / "files" / row["path"]
        saved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(project / row["path"], saved)
        if digest(saved) != row["before"]:
            raise ValueError("backup verification failed")
    (backup / "manifest.json").write_text(json.dumps({"project": str(project), "files": rows}, indent=2) + "\n")
    completed = []
    try:
        for row in changed(rows):
            target = project / row["path"]
            if target.is_symlink() or digest(target) != row["before"] or digest(Path(row["source"])) != row["after"]:
                raise ValueError("runtime code changed after inventory")
            replace(Path(row["source"]), target, row["mode"], row["uid"], row["gid"], row["before"])
            completed.append(row["path"])
        if any(digest(project / row["path"]) != row["after"] for row in changed(rows)):
            raise ValueError("migration verification failed")
    except Exception:
        rollback(project, backup, only=completed)
        raise
    return {"case": "project-code-refresh", "inventoried": len(rows), "replaced": len(changed(rows))}


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("mode", choices=("inventory", "apply", "rollback"))
    p.add_argument("--project", type=Path, required=True)
    p.add_argument("--source", type=Path, default=Path("/usr/local/boinc"))
    p.add_argument("--templates", type=Path, default=Path("/usr/local/lib/usnm-boinc"))
    p.add_argument("--backup", type=Path)
    p.add_argument("--ack-server-stopped", action="store_true")
    a = p.parse_args()
    if a.mode == "inventory":
        print(json.dumps({"case": "project-code-inventory", "files": inventory(a.project, a.source, a.templates)}))
        return
    if not a.ack_server_stopped or a.backup is None:
        p.error("mutation requires --ack-server-stopped and --backup; never change running job templates")
    result = rollback(a.project, a.backup) if a.mode == "rollback" else apply(a.project, a.source, a.templates, a.backup)
    print(json.dumps(result))


if __name__ == "__main__":
    main()
