#!/usr/bin/env python3
"""Read a crop list with the pinned Qwen3-VL-2B base, with or without a LoRA adapter, on the same settings."""
import argparse, json, sys, time
from pathlib import Path
sys.path.insert(0, "/opt/train")
import worker

ap = argparse.ArgumentParser()
ap.add_argument("--list", type=Path, required=True)    # JSONL rows {"id","image"}
ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--adapter", type=Path)
ap.add_argument("--rank", type=int, default=8)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--min-pixels", type=int, required=True)
ap.add_argument("--max-pixels", type=int, required=True)
ap.add_argument("--max-new-tokens", type=int, default=1024)
a = ap.parse_args()

import torch
from PIL import Image
from peft import set_peft_model_state_dict
from safetensors.torch import load_file
from transformers import AutoProcessor, Qwen3VLForConditionalGeneration

proc = AutoProcessor.from_pretrained("/opt/base", local_files_only=True, trust_remote_code=False,
                                     size={"shortest_edge": a.min_pixels, "longest_edge": a.max_pixels})
model = Qwen3VLForConditionalGeneration.from_pretrained(
    "/opt/base", local_files_only=True, trust_remote_code=False,
    torch_dtype=torch.bfloat16, attn_implementation="sdpa").to("cuda")
if a.adapter:
    model = worker.attach_lora(model, a.rank)
    r = set_peft_model_state_dict(model, load_file(str(a.adapter)))
    if getattr(r, "unexpected_keys", None):
        raise SystemExit("adapter keys do not fit")
model.eval()
rows = [json.loads(l) for l in a.list.read_text(encoding="utf-8").splitlines() if l.strip()]
with a.out.open("w", buffering=1, encoding="utf-8") as out, open("/work/out/progress.txt", "a", buffering=1) as prog:
    t0 = time.time()
    for i, row in enumerate(rows):
        with Image.open(a.root / row["image"]) as src:
            im = src.convert("RGB")
        msg = [{"role": "user", "content": [{"type": "image", "image": im}, {"type": "text", "text": worker.PROMPT}]}]
        b = proc.apply_chat_template(msg, tokenize=True, add_generation_prompt=True, return_dict=True, return_tensors="pt").to("cuda")
        s = time.time()
        with torch.inference_mode():
            ids = model.generate(**b, max_new_tokens=a.max_new_tokens, do_sample=False, use_cache=True)
        gen = ids[0, b["input_ids"].shape[1]:]
        out.write(json.dumps({"id": row["id"], "text": proc.decode(gen, skip_special_tokens=True).strip(), "tokens": int(gen.shape[0]), "ms": int((time.time() - s) * 1000)}, ensure_ascii=False) + "\n")
        if i % 10 == 0:
            prog.write(f"{a.out.name} {i}/{len(rows)} {time.time() - t0:.0f}s\n")
