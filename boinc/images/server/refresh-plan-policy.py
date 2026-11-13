#!/usr/bin/env python3
"""Refresh image plan classes without resetting explicit Qwen admission."""
import argparse
import fcntl
import os
from pathlib import Path
import re
import sys
import time
import uuid
import xml.etree.ElementTree as ET

COHORT_CLASSES = (
    "nvidia_cuda_2b", "nvidia_cuda_8b", "intel_vulkan_8b",
    "nvidia_vulkan_8b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_2b", "amd_vulkan_8b",
    "docker_nvidia_cuda_8b",
)
# Xiaomi-OCR-0 classes (#1776): shipped closed and opened one workunit at a time, never generally,
# so a restart keeps a bounded window and otherwise leaves them closed. Each is its own window,
# like AMD's: it rides on no other class's admission.
XIAOMI_CLASSES = ("mt_x08", "apple_gpu_x08", "nvidia_vulkan_x08", "intel_vulkan_x08", "amd_vulkan_x08")
SINGLE_WORKUNIT_CLASSES = ("amd_vulkan_8b", "nvidia_vulkan_6gb_8b", "nvidia_vulkan_2b")
AMD_GENERAL_MARKER = "usnm-amd-general-admission-v1"
NVIDIA_GENERAL_MARKER = "usnm-nvidia-general-admission-v1"
# Classes an operator can open to all of the app's work (native-qwen-cohort.php --general).
INTEL_GENERAL_MARKER = "usnm-intel-general-admission-v1"
# nvidia_vulkan_6gb_8b is independent like AMD: it rides on no other class's admission.
NVIDIA_6GB_GENERAL_MARKER = "usnm-nvidia-6gb-general-admission-v1"
GENERAL_MARKERS = {"amd_vulkan_8b": AMD_GENERAL_MARKER, "nvidia_vulkan_8b": NVIDIA_GENERAL_MARKER,
                   "intel_vulkan_8b": INTEL_GENERAL_MARKER,
                   "nvidia_vulkan_6gb_8b": NVIDIA_6GB_GENERAL_MARKER}
GENERAL_LABELS = {"amd_vulkan_8b": "AMD", "nvidia_vulkan_8b": "NVIDIA", "intel_vulkan_8b": "Intel",
                  "nvidia_vulkan_6gb_8b": "NVIDIA 6 GB"}
AMD_FIRST_MARKER = "usnm-amd-first-reservation-v1"
# Any admission marker an operator tool can leave in the config. A refresh that does not
# recognise one would drop it with the rest of the old plan class, so it stops instead.
ADMISSION_MARKER = re.compile(r"usnm-\S*-admission-\S*")
KNOWN_ADMISSION_MARKERS = frozenset(GENERAL_MARKERS.values())


def reject_unknown_admission_markers(current: bytes) -> None:
    # Scan the raw bytes: the parsed tree drops comments before and after the root element.
    for comment in re.findall(rb"<!--(.*?)-->", current, re.DOTALL):
        for marker in ADMISSION_MARKER.findall(comment.decode("utf-8", "replace")):
            if marker not in KNOWN_ADMISSION_MARKERS:
                raise ValueError(
                    f"Unknown admission marker {marker} in the current plan policy: this image "
                    "cannot preserve it (is it older than the tool that wrote it?); "
                    "refusing to refresh, config left untouched")


def admission_state(node: ET.Element) -> str:
    comments = {(child.text or "").strip() for child in node if child.tag is ET.Comment}
    marked = sorted(marker for marker in KNOWN_ADMISSION_MARKERS if marker in comments)
    if marked:
        return "general (" + ", ".join(marked) + ")"
    low, high = node.findtext("min_wu_id"), node.findtext("max_wu_id")
    if low is None and high is None:
        return "no workunit bounds"
    return "closed" if (low, high) == ("2", "1") else f"workunits {low}-{high}"


def admission_changes(current: bytes, merged: bytes) -> list[str]:
    _, before = parse_policy(current)
    _, after = parse_policy(merged)
    absent = "class absent"
    changes = []
    for name in sorted(before.keys() | after.keys()):
        old = admission_state(before[name]) if name in before else absent
        new = admission_state(after[name]) if name in after else absent
        if old != new:
            changes.append(f"{name}: admission {old} -> {new}")
    return changes


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
    old_root, existing = parse_policy(current)
    reject_unknown_admission_markers(current)
    for name in COHORT_CLASSES + XIAOMI_CLASSES:
        if name not in existing:
            continue
        old = existing[name]
        fields = [old.findall(tag) for tag in ("min_wu_id", "max_wu_id")]
        marker = GENERAL_MARKERS.get(name)
        if marker:
            markers = [node for node in old if node.tag is ET.Comment
                       and (node.text or "").strip() == marker]
            if markers:
                if len(markers) != 1 or any(fields) or name not in reviewed:
                    raise ValueError(f"Invalid explicit {GENERAL_LABELS[name]} general admission")
                for node in list(reviewed[name]):
                    if node.tag in ("min_wu_id", "max_wu_id") or (
                            node.tag is ET.Comment and (node.text or "").strip() == marker):
                        reviewed[name].remove(node)
                reviewed[name].append(ET.Comment(marker))
                continue
        if not any(fields):
            continue
        if any(len(field) != 1 for field in fields) or name not in reviewed:
            raise ValueError(f"Cannot preserve cohort bounds for {name}")
        low, high = (int(field[0].text) for field in fields)
        # The Xiaomi-OCR-0 classes take batch 108 only; native-qwen-cohort.php --plan-class x08
        # opens them over every workunit of named roots, which can be any length (#1867).
        if (low, high) != (2, 1) and not (2 <= low <= high and (high - low < 10 or name in XIAOMI_CLASSES)):
            raise ValueError(f"Invalid bounded cohort for {name}")
        if name in SINGLE_WORKUNIT_CLASSES and (low, high) != (2, 1) and low != high:
            raise ValueError(f"{name} acceptance requires exactly one workunit")
        for tag, value in (("min_wu_id", low), ("max_wu_id", high)):
            targets = reviewed[name].findall(tag)
            if len(targets) > 1:
                raise ValueError(f"Duplicate reviewed cohort field for {name}")
            field = targets[0] if targets else ET.SubElement(reviewed[name], tag)
            field.text = str(value)
    # Intel's general admission rides on NVIDIA's. Without NVIDIA's, Intel closes (2/1) rather
    # than keep a policy the refill controller rejects; a restart must not fail over it.
    def general(name):
        return name in reviewed and any(node.tag is ET.Comment and (node.text or "").strip() == GENERAL_MARKERS[name]
                                        for node in reviewed[name])
    if general("intel_vulkan_8b") and not general("nvidia_vulkan_8b"):
        intel = reviewed["intel_vulkan_8b"]
        for node in list(intel):
            if node.tag in ("min_wu_id", "max_wu_id") or (
                    node.tag is ET.Comment and (node.text or "").strip() == INTEL_GENERAL_MARKER):
                intel.remove(node)
        ET.SubElement(intel, "min_wu_id").text = "2"
        ET.SubElement(intel, "max_wu_id").text = "1"
    markers = [node for node in old_root.iter() if node.tag is ET.Comment
               and AMD_FIRST_MARKER in (node.text or "")]
    if markers:
        if len(markers) != 1 or markers[0] not in list(old_root):
            raise ValueError("Invalid AMD-first reservation marker")
        match = re.fullmatch(AMD_FIRST_MARKER + r":([a-f0-9]{32}):([1-9][0-9]*):([1-9][0-9]*)",
                             (markers[0].text or "").strip())
        if not match:
            raise ValueError("Malformed AMD-first reservation marker")
        workunit, expires = int(match[2]), int(match[3])
        apple = existing.get("apple_gpu_8b")
        fields = apple.findall("max_wu_id") if apple is not None else []
        if (len(fields) != 1 or fields[0].text != str(workunit - 1)
                or apple.findall("min_wu_id") or "apple_gpu_8b" not in reviewed):
            raise ValueError("Invalid AMD-first Apple ceiling")
        if expires > int(time.time()):
            target = reviewed["apple_gpu_8b"]
            if target.findall("min_wu_id") or target.findall("max_wu_id"):
                raise ValueError("Reviewed Apple policy conflicts with AMD-first lease")
            ET.SubElement(target, "max_wu_id").text = str(workunit - 1)
            root.append(ET.Comment((markers[0].text or "").strip()))
        else:
            amd = reviewed["amd_vulkan_8b"]
            if (amd.findtext("min_wu_id"), amd.findtext("max_wu_id")) == (str(workunit), str(workunit)):
                amd.find("min_wu_id").text, amd.find("max_wu_id").text = "2", "1"
    ET.indent(root, space="    ")
    return ET.tostring(root, encoding="utf-8", xml_declaration=True) + b"\n"


def refresh(source: Path, target: Path, log=None) -> bool:
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
        if current is not None and log is not None:
            for line in admission_changes(current, payload):
                log(line)
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
    try:
        changed = refresh(args.source, args.target,
                          log=lambda line: print("Admission changed by refresh: " + line, file=sys.stderr))
    except ValueError as error:
        sys.exit(f"refresh-plan-policy: {error}")
    print("Plan policy refreshed; bounded admission preserved" if changed else "Plan policy matches image and admission")


if __name__ == "__main__":
    main()
