"""ALTO XML for the Japanese pages NDLOCR-Lite reads (#151).

LoC ships these pages with an empty ALTO (iArchives ran ABBYY 8 with Lang:ja
and found no words), in inch1200 units at the scan's dpi (300 for every page
checked). Our images are the same scans at full size, so a pixel is
1200 / dpi inch1200 units, and the ALTO we write can stand in for LoC's.

NDLOCR-Lite's XML has text blocks and lines, each line with a pixel box, a
reading order (ORDER), a confidence and its text; no character boxes. So
each line becomes one TextLine with one String holding the line's text.
Blocks with no lines (running heads, page numbers, figures) are left out:
they have no text.
"""

from __future__ import annotations

import re
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from datetime import datetime
from xml.sax.saxutils import escape, quoteattr

NS = "http://www.loc.gov/standards/alto/ns-v3#"
SCHEMA = "http://www.loc.gov/standards/alto/v3/alto-3-1.xsd"
DEFAULT_DPI = 300


@dataclass(frozen=True)
class Geometry:
    """The page as LoC's ALTO describes it: the scan's dpi and the page size in inch1200."""
    dpi: int = DEFAULT_DPI
    width: int | None = None
    height: int | None = None


def loc_geometry(loc_alto: bytes | None) -> Geometry:
    """dpi and page size from LoC's ocr.xml; the defaults when it has none."""
    if not loc_alto:
        return Geometry()
    text = loc_alto.decode("utf-8", "replace")
    dpi = re.search(r"xdpi:\s*(\d+)", text)
    page = re.search(r"<Page\b[^>]*>", text)
    width = height = None
    if page:
        w = re.search(r'\bWIDTH="([\d.]+)"', page.group(0))
        h = re.search(r'\bHEIGHT="([\d.]+)"', page.group(0))
        unit = re.search(r"<MeasurementUnit>\s*(\w+)", text)
        if w and h and (unit is None or unit.group(1) == "inch1200"):
            width, height = round(float(w.group(1))), round(float(h.group(1)))
    d = int(dpi.group(1)) if dpi and int(dpi.group(1)) > 0 else DEFAULT_DPI
    return Geometry(dpi=d, width=width, height=height)


@dataclass
class Line:
    x: float
    y: float
    w: float
    h: float
    order: int
    conf: float
    text: str


def _lines(elem: ET.Element) -> list[Line]:
    out = []
    for ln in elem.iter("LINE"):
        text = (ln.get("STRING") or "").strip()
        try:
            box = [float(ln.get(k)) for k in ("X", "Y", "WIDTH", "HEIGHT")]
            order = int(float(ln.get("ORDER", "0")))
            conf = float(ln.get("CONF", "0"))
        except (TypeError, ValueError):
            continue
        if text and box[2] > 0 and box[3] > 0:
            out.append(Line(*box, order=order, conf=conf, text=text))
    return out


def blocks(ndl_xml: bytes) -> tuple[tuple[int, int], list[list[Line]]]:
    """The page's pixel size and its blocks of lines, both in NDLOCR-Lite's reading order.

    A TEXTBLOCK is a block; a LINE directly under PAGE, or the lines inside
    another BLOCK (a table), make a block of their own.
    """
    page = ET.fromstring(ndl_xml).find("PAGE")
    if page is None:
        raise ValueError("NDLOCR-Lite XML has no PAGE")
    size = (int(page.get("WIDTH")), int(page.get("HEIGHT")))
    out: list[list[Line]] = []
    for child in page:
        lines = _lines(child)
        if not lines:
            continue
        if child.tag == "LINE":
            out.append(lines)
        else:
            out.append(sorted(lines, key=lambda ln: ln.order))
    out.sort(key=lambda b: b[0].order)
    return size, out


def ndlocr_to_alto(ndl_xml: bytes, geometry: Geometry, *, image_url: str, engine: str,
                   seq: int, ocred_at: datetime) -> bytes:
    """ALTO v3 in inch1200 for one page of NDLOCR-Lite output."""
    (pw, ph), bl = blocks(ndl_xml)
    k = 1200 / geometry.dpi
    page_w = geometry.width or round(pw * k)
    page_h = geometry.height or round(ph * k)

    def box(x: float, y: float, w: float, h: float) -> str:
        # Clamp to the page: LoC's page can be a few pixels smaller than the image.
        x0, y0 = min(round(x * k), page_w), min(round(y * k), page_h)
        x1, y1 = min(round((x + w) * k), page_w), min(round((y + h) * k), page_h)
        return f'HPOS="{x0}" VPOS="{y0}" WIDTH="{x1 - x0}" HEIGHT="{y1 - y0}"'

    out = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        f'<alto xmlns="{NS}" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" '
        f'xsi:schemaLocation="{NS} {SCHEMA}">',
        "<Description>",
        "<MeasurementUnit>inch1200</MeasurementUnit>",
        f"<sourceImageInformation><fileName>{escape(image_url)}</fileName></sourceImageInformation>",
        '<OCRProcessing ID="OCR_0"><ocrProcessingStep>',
        f"<processingDateTime>{ocred_at.isoformat()}</processingDateTime>",
        "<processingStepSettings>"
        + escape(f"dpi:{geometry.dpi}\nlines:NDLOCR-Lite reading order\nstrings:one per line")
        + "</processingStepSettings>",
        "<processingSoftware><softwareCreator>National Diet Library</softwareCreator>"
        f"<softwareName>NDLOCR-Lite</softwareName><softwareVersion>{escape(engine)}</softwareVersion>"
        "</processingSoftware>",
        "</ocrProcessingStep></OCRProcessing>",
        "</Description>",
        "<Layout>",
        f'<Page ID="P{seq}" PHYSICAL_IMG_NR="{seq}" WIDTH="{page_w}" HEIGHT="{page_h}">',
        f'<PrintSpace HPOS="0" VPOS="0" WIDTH="{page_w}" HEIGHT="{page_h}">',
    ]
    n = 0
    for b, lines in enumerate(bl, 1):
        x0 = min(ln.x for ln in lines)
        y0 = min(ln.y for ln in lines)
        x1 = max(ln.x + ln.w for ln in lines)
        y1 = max(ln.y + ln.h for ln in lines)
        out.append(f'<TextBlock ID="P{seq}_TB{b:05d}" {box(x0, y0, x1 - x0, y1 - y0)} LANG="jpn">')
        for ln in lines:
            n += 1
            attrs = box(ln.x, ln.y, ln.w, ln.h)
            out.append(f'<TextLine ID="P{seq}_TL{n:05d}" {attrs}>'
                       f'<String ID="P{seq}_ST{n:05d}" {attrs} CONTENT={quoteattr(ln.text)} '
                       f'WC="{max(0.0, min(1.0, ln.conf)):.3f}"/></TextLine>')
        out.append("</TextBlock>")
    out += ["</PrintSpace>", "</Page>", "</Layout>", "</alto>", ""]
    return "\n".join(out).encode("utf-8")
