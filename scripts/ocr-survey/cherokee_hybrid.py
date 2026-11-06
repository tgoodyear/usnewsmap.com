#!/usr/bin/env python3
"""Re-OCR Cherokee ALTO lines while preserving other language blocks (#1581).

This is an experimental transcript builder, not a production route. It uses
LoC ALTO language labels and line boxes; non-Cherokee text stays as LoC read it.
For example:

    python3 scripts/ocr-survey/cherokee_hybrid.py --image page.jpg --alto page.xml \
        --tessdata-dir /models/best --output hybrid.txt

An empty Cherokee recognition explicitly falls back to that line's LoC text;
the JSON sidecar records the fallback.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

from PIL import Image

PADDING_X, PADDING_Y = 12, 8
LANGUAGE = "chr"


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _line_text(line: ET.Element) -> str:
    parts = []
    for element in line:
        tag = _local(element.tag)
        if tag == "String":
            content = element.get("CONTENT", "").strip()
            if content:
                parts.append(content)
        elif tag == "HYP" and parts:
            parts[-1] += "-"
    return " ".join(parts)


def _page_size(root: ET.Element) -> tuple[float, float]:
    pages = [element for element in root.iter() if _local(element.tag) == "Page"]
    if len(pages) != 1:
        raise ValueError(f"ALTO must contain exactly one Page; found {len(pages)}")
    try:
        width, height = float(pages[0].get("WIDTH", "")), float(pages[0].get("HEIGHT", ""))
    except ValueError as exc:
        raise ValueError("ALTO Page has invalid WIDTH or HEIGHT") from exc
    if not math.isfinite(width) or not math.isfinite(height) or width <= 0 or height <= 0:
        raise ValueError("ALTO Page WIDTH and HEIGHT must be positive finite values")
    return width, height


def _pixel_box(line: ET.Element, page_size: tuple[float, float], image_size: tuple[int, int],
               padding_x: int, padding_y: int) -> tuple[int, int, int, int]:
    line_id = line.get("ID", "unknown")
    try:
        hpos, vpos, width, height = (float(line.get(key, "")) for key in ("HPOS", "VPOS", "WIDTH", "HEIGHT"))
    except ValueError as exc:
        raise ValueError(f"Cherokee TextLine {line_id} has invalid geometry") from exc
    if not all(math.isfinite(value) for value in (hpos, vpos, width, height)) or width <= 0 or height <= 0:
        raise ValueError(f"Cherokee TextLine {line_id} has invalid geometry")

    scale_x, scale_y = image_size[0] / page_size[0], image_size[1] / page_size[1]
    x0 = max(0, round(hpos * scale_x) - padding_x)
    y0 = max(0, round(vpos * scale_y) - padding_y)
    x1 = min(image_size[0], round((hpos + width) * scale_x) + padding_x)
    y1 = min(image_size[1], round((vpos + height) * scale_y) + padding_y)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"Cherokee TextLine {line_id} has an empty image crop")
    return x0, y0, x1, y1


def recognize_crop(crop: Image.Image, tessdata_dir: Path, *, psm: int = 7) -> str:
    with tempfile.TemporaryDirectory() as tmp:
        image_path = Path(tmp) / "line.png"
        crop.save(image_path, format="PNG")
        command = [
            "tesseract", str(image_path), "stdout", "-l", LANGUAGE, "--oem", "1",
            "--psm", str(psm), "--tessdata-dir", str(tessdata_dir),
        ]
        env = os.environ.copy()
        env["OMP_THREAD_LIMIT"] = "1"
        result = subprocess.run(command, capture_output=True, text=True, encoding="utf-8", check=False, env=env)
    if result.returncode:
        raise RuntimeError(f"Tesseract failed on a Cherokee line: {result.stderr[-2000:]}")
    return " ".join(result.stdout.split())


def process_page(image_path: Path, alto_xml: bytes, tessdata_dir: Path, *,
                 psm: int = 7, padding_x: int = PADDING_X, padding_y: int = PADDING_Y) -> dict:
    if padding_x < 0 or padding_y < 0:
        raise ValueError("padding must be non-negative")
    root = ET.fromstring(alto_xml)
    page_size = _page_size(root)
    with Image.open(image_path) as source:
        source.load()
        image = source.convert("RGB")

    blocks = []
    recognized_lines = 0
    empty_lines = 0
    fallback_lines = 0
    for block_number, block in enumerate((el for el in root.iter() if _local(el.tag) == "TextBlock"), 1):
        language = next((value.strip().casefold() for key, value in block.attrib.items()
                         if key.casefold() == "language"), "")
        is_cherokee = language == LANGUAGE
        lines = []
        for line_number, line in enumerate((el for el in block if _local(el.tag) == "TextLine"), 1):
            original_text = _line_text(line)
            box = None
            text = original_text
            tesseract_text = None
            source = "loc_alto"
            if is_cherokee:
                box = _pixel_box(line, page_size, image.size, padding_x, padding_y)
                tesseract_text = recognize_crop(image.crop(box), tessdata_dir, psm=psm)
                recognized_lines += 1
                if tesseract_text:
                    text = tesseract_text
                    source = "tesseract_chr"
                else:
                    empty_lines += 1
                    fallback_lines += 1
                    source = "loc_alto_fallback_empty_tesseract"
            lines.append({
                "line_id": line.get("ID", f"line-{line_number}"),
                "source_text": original_text,
                "tesseract_text": tesseract_text,
                "text": text,
                "source": source,
                "box_px": list(box) if box is not None else None,
            })
        if lines:
            blocks.append({
                "block_id": block.get("ID", f"block-{block_number}"),
                "language": language or None,
                "source": "hybrid_chr" if is_cherokee else "loc_alto",
                "text": "\n".join(line["text"] for line in lines),
                "lines": lines,
            })

    if not recognized_lines:
        raise ValueError("ALTO contains no language=chr TextLine to re-OCR")
    return {
        "schema": 1,
        "image": str(image_path),
        "image_sha256": hashlib.sha256(image_path.read_bytes()).hexdigest(),
        "alto_sha256": hashlib.sha256(alto_xml).hexdigest(),
        "alto_page_size": list(page_size),
        "image_size": list(image.size),
        "language": LANGUAGE,
        "psm": psm,
        "omp_thread_limit": 1,
        "padding_px": [padding_x, padding_y],
        "recognized_lines": recognized_lines,
        "empty_recognitions": empty_lines,
        "fallback_lines": fallback_lines,
        "text": "\n\n".join(block["text"] for block in blocks if block["text"].strip()),
        "blocks": blocks,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--alto", type=Path, required=True)
    parser.add_argument("--tessdata-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="hybrid text output; a JSON sidecar is also written")
    parser.add_argument("--psm", type=int, default=7)
    parser.add_argument("--padding-x", type=int, default=PADDING_X)
    parser.add_argument("--padding-y", type=int, default=PADDING_Y)
    args = parser.parse_args(argv)
    if args.output.suffix == ".json":
        parser.error("--output must not end in .json: the JSON sidecar is written next to it with that suffix")
    if args.padding_x < 0 or args.padding_y < 0:
        parser.error("padding must be non-negative")
    if not (args.tessdata_dir / f"{LANGUAGE}.traineddata").is_file():
        parser.error(f"{args.tessdata_dir} does not contain {LANGUAGE}.traineddata")

    result = process_page(args.image, args.alto.read_bytes(), args.tessdata_dir,
                          psm=args.psm, padding_x=args.padding_x, padding_y=args.padding_y)
    version = subprocess.run(["tesseract", "--version"], capture_output=True, text=True, check=True)
    result["tesseract_version"] = version.stdout.splitlines()[0]
    model = args.tessdata_dir / f"{LANGUAGE}.traineddata"
    result["model_sha256"] = hashlib.sha256(model.read_bytes()).hexdigest()
    text = result.pop("text")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(text + "\n", encoding="utf-8")
    args.output.with_suffix(".json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n",
                                                encoding="utf-8")
    print(f"{args.output}: re-OCRed {result['recognized_lines']} Cherokee lines "
          f"({result['empty_recognitions']} empty; {result['fallback_lines']} kept LoC text)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
