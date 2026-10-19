#!/usr/bin/env python3
"""Refresh image plan classes without resetting explicit Qwen admission."""
import argparse
import fcntl
import os
from pathlib import Path
import uuid
import xml.etree.ElementTree as ET

COHORT_CLASSES = (
    "nvidia_cuda_2b", "nvidia_cuda_8b", "intel_vulkan_8b",
    "nvidia_vulkan_8b", "amd_vulkan_8b", "docker_nvidia_cuda_8b",
)
AMD_GENERAL_MARKER = "usnm-amd-general-admission-v1"


def parse_policy(payload: bytes) -> tuple[ET.Element, dict[str, ET.Element]]:
    root = ET.fromstring(payload, parser=ET.XMLParser(target=ET.TreeBuilder(insert_comments=True)))
    if root.tag != "plan_classes":
        raise ValueError("Expected plan_classes policy")
    result = {}
    for node in root.findall("plan_class"):
        name = node.findtext("name")
        if not name or name in result:
            raise ValueError("Missing or duplicate plan class name")
        result[name] = node
    return root, result


def merge_policy(source: bytes, current: bytes) -> bytes:
    root, reviewed = parse_policy(source)
    _, existing = parse_policy(current)
    for name in COHORT_CLASSES:
        if name not in existing:
            continue
        old = existing[name]
        fields = [old.findall(tag) for tag in ("min_wu_id", "max_wu_id")]
        if name == "amd_vulkan_8b":
            markers = [node for node in old if node.tag is ET.Comment
                       and (node.text or "").strip() == AMD_GENERAL_MARKER]
            if markers:
                if len(markers) != 1 or any(fields) or name not in reviewed:
                    raise ValueError("Invalid explicit AMD general admission")
                for node in list(reviewed[name]):
                    if node.tag in ("min_wu_id", "max_wu_id") or (
                            node.tag is ET.Comment and (node.text or "").strip() == AMD_GENERAL_MARKER):
                        reviewed[name].remove(node)
                reviewed[name].append(ET.Comment(AMD_GENERAL_MARKER))
                continue
        if not any(fields):
            continue
        if any(len(field) != 1 for field in fields) or name not in reviewed:
            raise ValueError(f"Cannot preserve cohort bounds for {name}")
        low, high = (int(field[0].text) for field in fields)
        if (low, high) != (2, 1) and not (2 <= low <= high and high - low < 10):
            raise ValueError(f"Invalid bounded cohort for {name}")
        if name == "amd_vulkan_8b" and (low, high) != (2, 1) and low != high:
            raise ValueError("AMD acceptance requires exactly one workunit")
        for tag, value in (("min_wu_id", low), ("max_wu_id", high)):
            targets = reviewed[name].findall(tag)
            if len(targets) > 1:
                raise ValueError(f"Duplicate reviewed cohort field for {name}")
            field = targets[0] if targets else ET.SubElement(reviewed[name], tag)
            field.text = str(value)
    ET.indent(root, space="    ")
    return ET.tostring(root, encoding="utf-8", xml_declaration=True) + b"\n"


def refresh(source: Path, target: Path) -> bool:
    lock_path = target.with_name(".native-qwen-cohort.lock")
    if target.is_symlink() or lock_path.is_symlink():
        raise ValueError("Refusing symlinked cohort policy or lock")
    with lock_path.open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        payload = source.read_bytes()
        current = target.read_bytes() if target.exists() else None
        payload = merge_policy(payload, current if current is not None else payload)
        if current == payload:
            return False
        staged = target.with_name(".usnm-plan-policy-" + uuid.uuid4().hex)
        created = False
        try:
            with staged.open("xb") as stream:
                created = True
                stream.write(payload)
                stream.flush()
                os.fsync(stream.fileno())
            staged.chmod(target.stat().st_mode & 0o777 if target.exists() else 0o644)
            os.replace(staged, target)
        finally:
            if created:
                staged.unlink(missing_ok=True)
    return True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--target", type=Path, required=True)
    args = parser.parse_args()
    changed = refresh(args.source, args.target)
    print("Plan policy refreshed; bounded admission preserved" if changed else "Plan policy matches image and admission")


if __name__ == "__main__":
    main()
