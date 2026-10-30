#!/usr/bin/env python3
"""Reflow retained ALTO through reviewed regions, without another recognition pass."""
import argparse
import copy
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
from statistics import median
import xml.etree.ElementTree as ET


NAMESPACE = "http://www.loc.gov/standards/alto/ns-v3#"
NS = {"a": NAMESPACE}
MAX_ALTO = 32 * 1024 * 1024
MAX_LAYOUT = 1024 * 1024
VERSION = "reviewed-regions/1"


def tag(name):
    return f"{{{NAMESPACE}}}{name}"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def bounded(path, limit):
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"{path.name} exceeds {limit} bytes")
    return data


def image_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(data)
    return digest.hexdigest()


def rectangle(element):
    x, y, width, height = (float(element.attrib[key]) for key in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
    if not all(math.isfinite(v) for v in (x, y, width, height)) or width < 0 or height < 0:
        raise ValueError("invalid ALTO rectangle")
    return x, y, x + width, y + height


def bounds(elements):
    boxes = [rectangle(element) for element in elements]
    return (min(r[0] for r in boxes), min(r[1] for r in boxes),
            max(r[2] for r in boxes), max(r[3] for r in boxes))


def geometry(rect):
    x0, y0, x1, y1 = rect
    return dict(zip(("HPOS", "VPOS", "WIDTH", "HEIGHT"),
                    (format(v, ".12g") for v in (x0, y0, x1 - x0, y1 - y0))))


def regions(layout, width, height):
    if layout.get("method") != VERSION or layout.get("direction") != "ltr":
        raise ValueError("layout must explicitly select reviewed-regions/1 and ltr")
    result = layout["regions"]
    if not isinstance(result, list) or not 1 <= len(result) <= 256:
        raise ValueError("layout needs 1..256 ordered regions")
    names = set()
    for region in result:
        name, box = region["name"], region["box"]
        if not isinstance(name, str) or not name.strip() or name in names:
            raise ValueError("region names must be nonempty and unique")
        names.add(name)
        if (not isinstance(box, list) or len(box) != 4
                or any(type(v) not in (int, float) or not math.isfinite(v) for v in box)):
            raise ValueError(f"invalid region {name}")
        x0, y0, x1, y1 = box
        if not (0 <= x0 < x1 <= width and 0 <= y0 < y1 <= height):
            raise ValueError(f"region {name} is outside the page")
    for i, left in enumerate(result):
        a = left["box"]
        for right in result[i + 1:]:
            b = right["box"]
            if max(a[0], b[0]) < min(a[2], b[2]) and max(a[1], b[1]) < min(a[3], b[3]):
                raise ValueError(f"regions overlap: {left['name']}, {right['name']}")
    return result


def word_signature(root):
    return sorted(tuple(sorted(element.attrib.items())) for element in root.findall(".//a:String", NS))


def visual_order(fragments):
    """Use the survey crops' half-median-height visual-row rule, not exact top edges."""
    heights = [float(w.get("HEIGHT")) for line, *_ in fragments
               for w in line.findall("a:String", NS)]
    tolerance = median(heights) / 2 if heights else 0

    def centre(fragment):
        return median((r[1] + r[3]) / 2 for r in
                      map(rectangle, fragment[0].findall("a:String", NS)))

    bands = []
    for fragment in sorted(fragments, key=centre):
        y = centre(fragment)
        if not bands or y - bands[-1][0] > tolerance:
            bands.append((y, []))
        bands[-1][1].append(fragment)
    return [fragment for _, band in bands
            for fragment in sorted(band, key=lambda item: (float(item[0].get("HPOS")), item[1], item[2]))]


def order_checks(original, derived, checks):
    if not isinstance(checks, list) or not 1 <= len(checks) <= 256:
        raise ValueError("layout needs 1..256 scan-supported word-order checks")
    result = []
    for check in checks:
        expected = check["word_ids"]
        if not isinstance(expected, list) or not 2 <= len(expected) <= 256:
            raise ValueError("an order check needs 2..256 word IDs")
        if any(not isinstance(word, str) or word not in original for word in expected):
            raise ValueError("order check names an unknown word")

        def consecutive(words):
            start = words.index(expected[0])
            return words[start:start + len(expected)] == expected

        if not consecutive(derived):
            raise ValueError(f"reconstruction does not recover expected order: {check['name']}")
        result.append({"name": check["name"], "word_ids": expected,
                       "original_consecutive": consecutive(original), "derived_consecutive": True})
    return result


def reflow(data, layout, scan_sha256):
    if sha256(data) != layout["alto_sha256"] or scan_sha256 != layout["image_sha256"]:
        raise ValueError("layout does not match the exact ALTO and image bytes")
    xml = data.decode("utf-8")
    if "<!DOCTYPE" in xml or "<!ENTITY" in xml:
        raise ValueError("DTD/entity declarations are not allowed")
    root = ET.fromstring(xml)
    if root.tag != tag("alto"):
        raise ValueError("expected stored ALTO v3")
    for element in root.iter():
        if "ROTATION" in element.attrib:
            rotation = float(element.attrib["ROTATION"])
            if not math.isfinite(rotation) or rotation != 0:
                raise ValueError("rotated content needs separate layout review")
        if (element.tag == tag("ComposedBlock") and "STYLEREFS" in element.attrib
                or element.tag == tag("TextLine")
                and any(key in element.attrib for key in ("LANG", "STYLEREFS"))):
            raise ValueError("inherited line/composed-block language or styles need separate review")
    if root.findall(".//a:ReadingOrder", NS) or any(
            "IDNEXT" in element.attrib or "IDPREV" in element.attrib for element in root.iter()):
        raise ValueError("existing explicit reading-order references need separate review")
    if root.findtext("a:Description/a:MeasurementUnit", namespaces=NS) != "pixel":
        raise ValueError("reviewed regions require pixel ALTO")
    pages = root.findall("a:Layout/a:Page", NS)
    if len(pages) != 1:
        raise ValueError("expected exactly one page")
    identifiers = root.findall("a:Description/a:sourceImageInformation/a:fileIdentifier", NS)
    if [e.text for e in identifiers if e.get("fileIdentifierLocation") == "sha256"] != [scan_sha256]:
        raise ValueError("retained ALTO does not bind this scan SHA256")
    page = pages[0]
    width, height = float(page.attrib["WIDTH"]), float(page.attrib["HEIGHT"])
    if [width, height] != layout["page_size"]:
        raise ValueError("reviewed layout page dimensions do not match")
    ordered = regions(layout, width, height)
    spaces = page.findall("a:PrintSpace", NS)
    if len(spaces) != 1:
        raise ValueError("expected one PrintSpace")
    space = spaces[0]
    before = word_signature(root)
    original_word_ids = [e.get("ID") for e in root.findall(".//a:String", NS)]
    fragments = [[] for _ in ordered]
    lines_split = 0
    text_blocks = space.findall(".//a:TextBlock", NS)
    consumed = 0
    for block_index, block in enumerate(text_blocks):
        for line_index, line in enumerate(block.findall("a:TextLine", NS)):
            words = line.findall("a:String", NS)
            if not words:
                raise ValueError("empty text line cannot be reconstructed")
            grouped = {}
            for word in words:
                if "SUBS_TYPE" in word.attrib or "SUBS_CONTENT" in word.attrib:
                    raise ValueError("existing cross-line substitutions need separate review")
                x0, y0, x1, y1 = rectangle(word)
                cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
                matches = [i for i, region in enumerate(ordered)
                           if region["box"][0] <= cx < region["box"][2]
                           and region["box"][1] <= cy < region["box"][3]]
                if len(matches) != 1:
                    raise ValueError(f"word {word.get('ID')} has no unique reviewed region")
                grouped.setdefault(matches[0], []).append(word)
                consumed += 1
            lines_split += len(grouped) > 1
            for index, kept in grouped.items():
                # Preserve the engine's word sequence within each line fragment.
                new_line = ET.Element(tag("TextLine"))
                for word in kept:
                    new_line.append(copy.deepcopy(word))
                # HYP belongs only to the fragment containing the original last word.
                if kept[-1] is words[-1]:
                    for hyphen in line.findall("a:HYP", NS):
                        new_line.append(copy.deepcopy(hyphen))
                positioned_hyphens = [h for h in new_line.findall("a:HYP", NS)
                                      if all(key in h.attrib for key in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))]
                new_line.attrib.update(geometry(bounds(kept + positioned_hyphens)))
                fragments[index].append((new_line, block_index, line_index, block))
    if consumed != len(before) or not before:
        raise ValueError("every recognized word must occur exactly once inside a TextLine")
    # Keep illustration/separator geometry; their original interleaving does not carry text.
    graphics = [copy.deepcopy(e) for e in space.iter()
                if e.tag in (tag("Illustration"), tag("GraphicalElement"))]
    ids = [e.attrib["ID"] for e in root.iter() if "ID" in e.attrib]
    original_ids = set(ids)
    if len(original_ids) != len(ids):
        raise ValueError("duplicate original ALTO identifiers")
    counter = 0

    def identity():
        nonlocal counter
        while True:
            counter += 1
            value = f"reflow_{counter}"
            if value not in original_ids:
                original_ids.add(value)
                return value

    space[:] = []
    counts = []
    for region, group in zip(ordered, fragments):
        group = visual_order(group)
        output_blocks = []
        previous = None
        for line, source_index, _, source in group:
            line.set("ID", identity())
            if previous != source_index:
                attributes = {k: v for k, v in source.attrib.items()
                              if k not in ("ID", "HPOS", "VPOS", "WIDTH", "HEIGHT")}
                current = ET.Element(tag("TextBlock"), {**attributes, "ID": identity()})
                output_blocks.append(current)
            current.append(line)
            previous = source_index
        for block in output_blocks:
            block.attrib.update(geometry(bounds(list(block))))
            space.append(block)
        counts.append({"name": region["name"], "lines": len(group),
                       "words": sum(len(item[0].findall("a:String", NS)) for item in group)})
    space.extend(graphics)
    if word_signature(root) != before:
        raise ValueError("recognized word content/geometry/confidence changed")
    checks = order_checks(original_word_ids,
                          [e.get("ID") for e in root.findall(".//a:String", NS)], layout["order_checks"])
    processing = root.findall("a:Description/a:OCRProcessing", NS)
    if len(processing) != 1:
        raise ValueError("expected one original OCRProcessing record")
    step = ET.SubElement(processing[0], tag("postProcessingStep"))
    ET.SubElement(step, tag("processingDateTime")).text = datetime.now(timezone.utc).isoformat()
    ET.SubElement(step, tag("processingStepSettings")).text = (
        f"method={VERSION}\nparent_alto_sha256={sha256(data)}\n"
        f"layout_canonical_json_sha256={sha256(json.dumps(layout, sort_keys=True).encode())}\n"
        "word_membership=reviewed-region-containing-word-centre\n"
        "publication_acceptance=not-claimed")
    software = ET.SubElement(step, tag("processingSoftware"))
    ET.SubElement(software, tag("softwareName")).text = "usnm-reading-order-repair"
    ET.SubElement(software, tag("softwareVersion")).text = "1"
    ET.register_namespace("", NAMESPACE)
    output = ET.tostring(root, encoding="utf-8", xml_declaration=True)
    texts = []
    for block in space.findall("a:TextBlock", NS):
        rows = []
        for line in block.findall("a:TextLine", NS):
            text = " ".join(word.attrib["CONTENT"] for word in line.findall("a:String", NS))
            text += "".join(e.get("CONTENT", "") for e in line.findall("a:HYP", NS))
            rows.append(text)
        texts.append("\n".join(rows))
    return output, "\n\n".join(texts) + "\n", {
        "words_preserved": len(before), "original_lines_split": lines_split, "regions": counts,
        "order_checks": checks,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--alto", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--layout", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True, help="new private directory; never overwritten")
    args = parser.parse_args(argv)
    data = bounded(args.alto, MAX_ALTO)
    layout_bytes = bounded(args.layout, MAX_LAYOUT)
    layout = json.loads(layout_bytes)
    scan_sha256 = image_digest(args.image)
    output, text, counts = reflow(data, layout, scan_sha256)
    args.out.mkdir(mode=0o700)
    (args.out / "page.alto.xml").write_bytes(output)
    text_bytes = text.encode()
    (args.out / "page.txt").write_bytes(text_bytes)
    receipt = {
        "method": VERSION, "input_alto_sha256": sha256(data), "input_image_sha256": scan_sha256,
        "input_layout_sha256": sha256(layout_bytes), "output_alto_sha256": sha256(output),
        "output_text_sha256": sha256(text_bytes), **counts,
        "new_recognition": False, "publication_acceptance": False,
    }
    (args.out / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, sort_keys=True))


if __name__ == "__main__":
    main()
