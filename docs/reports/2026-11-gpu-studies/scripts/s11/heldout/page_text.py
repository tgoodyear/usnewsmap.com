"""PAGE XML -> regions and lines in the file's reading order.

Library (parse) and CLI: --xml FILE [--show N]
"""
import argparse
import re
import xml.etree.ElementTree as ET


def local(tag):
    return tag.rsplit("}", 1)[-1]


def ro_index(el):
    m = re.search(r"readingOrder \{index:(\d+);", el.get("custom", ""))
    return int(m.group(1)) if m else None


def first_text(el):
    te = [e for e in el if local(e.tag) == "TextEquiv"]
    if not te:
        return None
    u = [e for e in te[0] if local(e.tag) == "Unicode"]
    return (u[0].text or "") if u else ""


def parse(path):
    root = ET.parse(path).getroot()
    page = next(e for e in root.iter() if local(e.tag) == "Page")
    meta = {"w": int(page.get("imageWidth")), "h": int(page.get("imageHeight")),
            "image": page.get("imageFilename"), "status": None}
    for e in root.iter():
        if local(e.tag) == "TranskribusMetadata":
            meta["status"] = e.get("status")
    order = {}
    for e in root.iter():
        if local(e.tag) == "RegionRefIndexed":
            order[e.get("regionRef")] = int(e.get("index"))
    regions = []
    text_regions = [e for e in page.iter() if local(e.tag) == "TextRegion"]
    for n, r in enumerate(text_regions):
        lines = []
        for ln in [e for e in r if local(e.tag) == "TextLine"]:
            idx = ro_index(ln)
            t = first_text(ln)
            lines.append((idx if idx is not None else len(lines), t if t is not None else ""))
        lines.sort(key=lambda x: x[0])
        coords = next((e.get("points") for e in r if local(e.tag) == "Coords"), "")
        m = re.search(r"structure \{type:([^;]+);", r.get("custom", ""))
        typ = r.get("type") or (m.group(1) if m else None)
        regions.append({"id": r.get("id"), "order": order.get(r.get("id"), 10**6 + n), "type": typ,
                        "coords": coords, "lines": [t for _, t in lines],
                        "region_text": first_text(r)})
    regions.sort(key=lambda r: r["order"])
    return meta, regions


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--xml", required=True)
    ap.add_argument("--show", type=int, default=20)
    a = ap.parse_args()
    meta, regs = parse(a.xml)
    nl = sum(len(r["lines"]) for r in regs)
    ne = sum(1 for r in regs for t in r["lines"] if not t.strip())
    print(meta, "regions", len(regs), "lines", nl, "empty", ne,
          "chars", sum(len(t) for r in regs for t in r["lines"]))
    shown = 0
    for r in regs:
        for t in r["lines"]:
            if shown < a.show:
                print(r["type"], "|", t)
                shown += 1


if __name__ == "__main__":
    main()
