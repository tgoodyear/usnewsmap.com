#!/usr/bin/env python3
"""Study 9: read crops or pages with Xiaomi-OCR-0 through saturate, against a vLLM server in the same container.

Region mode follows upstream's region stage (evaluation_pipeline/twostage_img2md.py `infer_region`): the prompt
before the image. Page mode follows its whole-page mode and the uv-scripts saturate recipe: the image first,
fitted to 2,048 px. Both send PNG, temperature 0, top_p 0.8, no penalties, thinking off.
"""
import argparse
import base64
import io
import json
import sys
from pathlib import Path

REGION_PROMPT = "Extract the text in the image."
DOCUMENT_PROMPT = (
    "Extract all information from the main body of the document image and represent it "
    "in markdown format, ignoring headers and footers. Tables should be expressed in OTSL "
    "format, formulas in the document should be represented using LATEX format, and the "
    "parsing should be organized according to the reading order."
)
MAX_EDGE = 2048

ap = argparse.ArgumentParser()
ap.add_argument("--list", type=Path, required=True)  # JSONL rows {"id","image"}, image relative to --root
ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)  # saturate output directory
ap.add_argument("--mode", choices=["region", "page"], required=True)
ap.add_argument("--max-tokens", type=int, required=True)
ap.add_argument("--endpoint", default="http://127.0.0.1:8000/v1")
ap.add_argument("--model", default="xiaomi-research/Xiaomi-OCR-0")
ap.add_argument("--max-window", type=int, default=48)  # in-flight images live in RAM
ap.add_argument("--export", type=Path, required=True)  # JSONL {id,text,finish_reason,...} for the scorer
a = ap.parse_args()

from PIL import Image
from saturate import Auto, pump, read_output

root = a.root.resolve()


def input_path(rel):
    p = Path(rel)
    if not rel or p.is_absolute() or ".." in p.parts:
        raise ValueError(f"path outside --root: {rel!r}")
    r = (root / p).resolve()
    r.relative_to(root)
    return r


rows = []
for line in a.list.read_text(encoding="utf-8").splitlines():
    if line.strip():
        r = json.loads(line)
        rows.append((r["id"], {"image": r["image"]}))

fitted = []


def png_b64(rel):
    with Image.open(input_path(rel)) as src:
        im = src.convert("RGB")
    if a.mode == "page" or max(im.size) > MAX_EDGE:
        if max(im.size) > MAX_EDGE:
            fitted.append(rel)
        im.thumbnail((MAX_EDGE, MAX_EDGE), Image.Resampling.LANCZOS)
    buf = io.BytesIO()
    im.save(buf, format="PNG")
    return base64.b64encode(buf.getvalue()).decode()


def to_request(row):
    image = {"type": "image_url", "image_url": {"url": "data:image/png;base64," + png_b64(row["image"])}}
    if a.mode == "region":
        content = [{"type": "text", "text": REGION_PROMPT}, image]
    else:
        content = [image, {"type": "text", "text": DOCUMENT_PROMPT}]
    return {
        "model": a.model,
        "messages": [{"role": "user", "content": content}],
        "max_tokens": a.max_tokens,
        "temperature": 0.0,
        "top_p": 0.8,
        "presence_penalty": 0.0,
        "repetition_penalty": 1.0,
        "chat_template_kwargs": {"enable_thinking": False},
    }


def parse(row, body):
    choice = body["choices"][0]
    usage = body.get("usage") or {}
    return {
        "text": (choice["message"]["content"] or "").strip(),
        "finish_reason": choice.get("finish_reason"),
        "prompt_tokens": usage.get("prompt_tokens"),
        "completion_tokens": usage.get("completion_tokens"),
    }


stats = pump(rows, to_request, parse, a.endpoint, str(a.out), window=Auto(max_limit=a.max_window), prepare_workers=4)
print("S9_STATS " + stats.to_json(), flush=True)
with a.export.open("w", encoding="utf-8") as out:
    for row_id, row in read_output(str(a.out)):
        rec = {"id": row_id, "text": row["text"], "finish_reason": row["finish_reason"],
               "prompt_tokens": row["prompt_tokens"], "completion_tokens": row["completion_tokens"]}
        out.write(json.dumps(rec, ensure_ascii=False) + "\n")
print(f"S9_FITTED {len(fitted)}", flush=True)
sys.stdout.flush()
