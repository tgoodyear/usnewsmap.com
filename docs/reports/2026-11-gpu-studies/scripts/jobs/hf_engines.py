#!/usr/bin/env python3
"""Read a crop list with one Hugging Face engine: got, granite or churro. Weights are public, at pinned revisions."""
import argparse, json, re, sys, time
from pathlib import Path

ap = argparse.ArgumentParser()
ap.add_argument("--engine", choices=["got", "granite", "churro"], required=True)
ap.add_argument("--list", type=Path, required=True)   # JSONL rows {"id","image"}
ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--revision", required=True)
ap.add_argument("--max-new-tokens", type=int, default=4096)
a = ap.parse_args()

import torch
from PIL import Image
rows = [json.loads(l) for l in a.list.read_text(encoding="utf-8").splitlines() if l.strip()]
prog = open("/work/out/progress.txt", "a", buffering=1)

if a.engine == "churro":
    sys.path.insert(0, "/work/in/scripts")
    from churro_ocr.ocr import OCRClient, OCRBackendSpec, build_ocr_backend, HuggingFaceOptions  # noqa
    backend = build_ocr_backend(OCRBackendSpec(provider="hf", model="stanford-oval/churro-3B", options=HuggingFaceOptions(
        processor_kwargs={"revision": a.revision},
        model_kwargs={"revision": a.revision, "device_map": "auto", "dtype": torch.float16},
        generation_kwargs={"max_new_tokens": a.max_new_tokens, "do_sample": False})))
    client = OCRClient(backend)
    def read(i, path):
        return client.ocr_image(image_path=path, page_index=i).text or ""
else:
    from transformers import AutoModelForImageTextToText, AutoProcessor
    repo = {"got": "stepfun-ai/GOT-OCR-2.0-hf", "granite": "ibm-granite/granite-docling-258M"}[a.engine]
    proc = AutoProcessor.from_pretrained(repo, revision=a.revision, trust_remote_code=False)
    model = AutoModelForImageTextToText.from_pretrained(repo, revision=a.revision, trust_remote_code=False,
                                                        torch_dtype=torch.bfloat16).to("cuda").eval()
    def read(i, path):
        with Image.open(path) as src:
            im = src.convert("RGB")
        if a.engine == "got":
            b = proc(im, return_tensors="pt").to("cuda")
            b["pixel_values"] = b["pixel_values"].to(torch.bfloat16)
            ids = model.generate(**b, do_sample=False, tokenizer=proc.tokenizer, stop_strings="<|im_end|>", max_new_tokens=a.max_new_tokens)
            return proc.decode(ids[0, b["input_ids"].shape[1]:], skip_special_tokens=True).strip()
        msg = [{"role": "user", "content": [{"type": "image"}, {"type": "text", "text": "Convert this page to docling."}]}]
        p = proc.apply_chat_template(msg, add_generation_prompt=True)
        b = proc(text=p, images=[im], return_tensors="pt").to("cuda")
        ids = model.generate(**b, do_sample=False, max_new_tokens=a.max_new_tokens)
        raw = proc.decode(ids[0, b["input_ids"].shape[1]:], skip_special_tokens=False)
        return re.sub(r"<[^>]+>", " ", re.sub(r"<loc_\d+>", "", raw)).strip()

t0 = time.time()
with a.out.open("w", buffering=1, encoding="utf-8") as out:
    for i, row in enumerate(rows):
        s = time.time()
        try:
            text = read(i, a.root / row["image"]); status = "ok"
        except Exception as e:  # recorded and scored as missing; the run goes on
            text = None; status = f"error: {str(e)[:200]}"
        rec = {"id": row["id"], "status": status, "ms": int((time.time() - s) * 1000)}
        if text is not None:
            rec["text"] = text
        out.write(json.dumps(rec, ensure_ascii=False) + "\n")
        if i % 10 == 0:
            prog.write(f"{a.engine} {i}/{len(rows)} {time.time() - t0:.0f}s\n")
