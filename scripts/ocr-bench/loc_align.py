#!/usr/bin/env python3
"""Produce scorer inputs only from explicitly registered, checksum-bound LoC ALTO."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import struct
import sys
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location(
    "altotext", Path(__file__).resolve().parents[1] / "ocr-survey" / "altotext.py")
altotext = importlib.util.module_from_spec(spec)
spec.loader.exec_module(altotext)
POLICY = "loc-word-centres-xml-order-v1"


def checked_file(path, digest, limit):
    if path.stat().st_size > limit:
        raise ValueError("input exceeds size bound")
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != digest:
        raise ValueError("input checksum mismatch")
    return data


def jpeg_dimensions(data):
    if data[:2] != b"\xff\xd8":
        raise ValueError("source image is not JPEG")
    position = 2
    while position < len(data):
        if data[position] != 0xFF:
            raise ValueError("invalid JPEG marker")
        while position < len(data) and data[position] == 0xFF:
            position += 1
        if position == len(data):
            break
        marker = data[position]
        position += 1
        if marker in (0xD9, 0xDA):
            break
        if marker in (0x01, *range(0xD0, 0xD9)):
            continue
        if position + 2 > len(data):
            break
        length = int.from_bytes(data[position:position + 2], "big")
        if length < 2 or position + length > len(data):
            raise ValueError("truncated JPEG segment")
        if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
            if length < 8:
                raise ValueError("truncated JPEG dimensions")
            height, width = struct.unpack(">HH", data[position + 3:position + 7])
            if not width or not height:
                raise ValueError("empty JPEG dimensions")
            return width, height
        position += length
    raise ValueError("JPEG has no supported dimensions")


def number(value):
    result = float(value)
    if not math.isfinite(result):
        raise ValueError("non-finite coordinate")
    return result


def rectangle(element):
    x, y, width, height = (number(element.attrib[key]) for key in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
    if min(x, y) < 0 or min(width, height) <= 0:
        raise ValueError("invalid ALTO rectangle")
    return x, y, x + width, y + height


def align(xml, width, height, box, scale):
    if altotext.namespace(xml) not in altotext.ALTO_NAMESPACES:
        raise ValueError("unsupported ALTO namespace")
    root = ET.fromstring(xml)
    units = root.findall(".//{*}MeasurementUnit")
    pages = root.findall(".//{*}Page")
    if len(units) != 1 or units[0].text not in ("pixel", "inch1200", "mm10") or len(pages) != 1:
        raise ValueError("ambiguous ALTO units or page count")
    scale = number(scale)
    if scale <= 0:
        raise ValueError("nonpositive explicit scale")
    page = pages[0]
    pw, ph = number(page.attrib["WIDTH"]), number(page.attrib["HEIGHT"])
    if min(pw, ph) <= 0 or abs(pw * scale - width) > 1 or abs(ph * scale - height) > 1:
        raise ValueError("ALTO/image dimensions do not match registered scale")
    if units[0].text == "pixel" and scale != 1:
        raise ValueError("pixel ALTO must match source dimensions without rescaling")
    x0, y0, x1, y1 = map(number, box)
    if not (0 <= x0 < x1 <= width and 0 <= y0 < y1 <= height):
        raise ValueError("crop outside source image")
    boundary_lines = 0
    for element in page.iter():
        tag = element.tag.rsplit("}", 1)[-1]
        if tag not in ("TextLine", "String"):
            continue
        left, top, right, bottom = rectangle(element)
        if right > pw + 1 or bottom > ph + 1:
            raise ValueError("ALTO rectangle outside page")
        if tag == "String" and "CONTENT" not in element.attrib:
            raise ValueError("String has no CONTENT")
        if tag == "TextLine":
            left, top, right, bottom = (coordinate * scale for coordinate in (left, top, right, bottom))
            intersects = left < x1 and right > x0 and top < y1 and bottom > y0
            contained = x0 <= left and right <= x1 and y0 <= top and bottom <= y1
            boundary_lines += int(intersects and not contained)
    _, words = altotext.parse(xml, scale=scale)
    return {"text": altotext.text_in_box(words, box), "boundary_lines": boundary_lines,
            "policy": POLICY, "measurement_unit": units[0].text, "scale": scale}


def adapt(sample, binding):
    provenance = sample["provenance"]
    if binding["source_page_url"] != provenance["page_url"]:
        raise ValueError("source page URL differs from benchmark")
    if binding["source_image_sha256"] != provenance["page_image_sha256"]:
        raise ValueError("source image identity differs from benchmark")
    for key in ("registration_evidence", "artifact_url", "retrieved_at", "version_evidence"):
        if not isinstance(binding[key], str) or not binding[key].strip():
            raise ValueError("nonempty string provenance required: " + key)
    image = checked_file(Path(binding["source_image"]), binding["source_image_sha256"], 128 << 20)
    width, height = jpeg_dimensions(image)
    xml = checked_file(Path(binding["alto"]), binding["alto_sha256"], 16 << 20)
    return align(xml, width, height, provenance["box"], binding["scale"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", type=Path, required=True)
    parser.add_argument("--subset", required=True)
    parser.add_argument("--baseline", choices=("loc-legacy-v1", "loc-refreshed-v1"), required=True)
    parser.add_argument("--bindings", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    manifest_bytes = (args.release / "manifest.json").read_bytes()
    manifest = json.loads(manifest_bytes)
    selected = set(manifest["subsets"][args.subset]["members"])
    bindings_bytes = args.bindings.read_bytes()
    bindings = {}
    for line in bindings_bytes.splitlines():
        row = json.loads(line)
        if row["id"] in bindings or row["id"] not in selected:
            raise ValueError("duplicate or out-of-subset binding")
        bindings[row["id"]] = row
    statuses, outputs = [], []
    for sample in manifest["samples"]:
        identity = sample["id"]
        if identity not in selected:
            continue
        binding = bindings.get(identity)
        if binding is None:
            statuses.append({"id": identity, "status": "missing"})
            continue
        try:
            for kind in ("image", "reference"):
                item = sample[kind]
                path = (args.release / item["path"]).resolve()
                if not path.is_relative_to(args.release.resolve()):
                    raise ValueError("benchmark path escapes release")
                data = checked_file(path, item["sha256"], 128 << 20)
                if kind == "image":
                    width, height = jpeg_dimensions(data)
                    box = sample["provenance"]["box"]
                    if (width, height) != (sample["image_width"], sample["image_height"]):
                        raise ValueError("benchmark crop dimensions differ from manifest")
                    if (width, height) != (box[2] - box[0], box[3] - box[1]):
                        raise ValueError("resized benchmark crop needs a different alignment policy")
            if sample["reference_status"] != "human-verified":
                raise ValueError("independent human reference required")
            result = adapt(sample, binding)
        except (ValueError, TypeError, KeyError, OSError, ET.ParseError) as error:
            statuses.append({"id": identity, "status": "error", "reason": str(error)})
            continue
        outputs.append({"id": identity, "text": result.pop("text")})
        statuses.append({"id": identity, "status": "aligned", **result,
                         "alto_sha256": binding["alto_sha256"],
                         "source_image_sha256": binding["source_image_sha256"],
                         "artifact_url": binding["artifact_url"],
                         "retrieved_at": binding["retrieved_at"],
                         "version_evidence": binding["version_evidence"],
                         "registration_evidence": binding["registration_evidence"]})
    counts = dict(Counter(row["status"] for row in statuses))
    report = {"format": "usnm-loc-alignment/1", "baseline": args.baseline, "subset": args.subset,
              "manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
              "bindings_sha256": hashlib.sha256(bindings_bytes).hexdigest(),
              "counts": counts, "samples": statuses, "inference_cost": "unknown",
              "limitations": "crop transcription only; XML order, not whole-page reading-order evaluation; "
                             "String centres in crop; HYP elements excluded; registration is operator-verified"}
    args.out_dir.mkdir()
    outputs_bytes = "".join(json.dumps(row) + "\n" for row in outputs).encode()
    (args.out_dir / "outputs.jsonl").write_bytes(outputs_bytes)
    report["outputs_sha256"] = hashlib.sha256(outputs_bytes).hexdigest()
    (args.out_dir / "alignment.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(json.dumps(counts))
    return 1 if counts.get("error") else 0


if __name__ == "__main__":
    sys.exit(main())
