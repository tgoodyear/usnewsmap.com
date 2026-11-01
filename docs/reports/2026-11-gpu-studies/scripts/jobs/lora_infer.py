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
ap.add_argument("--base", default=a.base)
ap.add_argument("--guard", choices=["none", "rp105", "rp110", "stop", "stop-retry"], default="none")
a = ap.parse_args()

import torch
from PIL import Image
from peft import set_peft_model_state_dict
from safetensors.torch import load_file
from transformers import AutoProcessor, Qwen3VLForConditionalGeneration, StoppingCriteria, StoppingCriteriaList


def tail_repetition(text):
    """usnm_ocr_contract::qwen::tail_repetition: period of at most 400 chars, three repeats, 30 chars."""
    s = text.rstrip()
    n = len(s)
    for p in range(1, min(400, n // 3) + 1):
        i = n - 1
        while i >= p and s[i] == s[i - p]:
            i -= 1
        span = n - 1 - i + p
        if span >= 3 * p and span >= 30:
            return p, span
    return None


class LoopStop(StoppingCriteria):
    """Checks the reading every 8 tokens and stops when it ends in a repeated run."""
    def __init__(self, start):
        self.start = start
    def __call__(self, ids, scores, **kw):
        n = ids.shape[1] - self.start
        if n < 12 or n % 8:
            return torch.zeros(ids.shape[0], dtype=torch.bool, device=ids.device)
        hit = tail_repetition(proc.decode(ids[0, self.start:], skip_special_tokens=True)) is not None
        return torch.full((ids.shape[0],), hit, dtype=torch.bool, device=ids.device)


def cut(text):
    """The text before the repeated run, plus one period of it."""
    t = text.rstrip()
    r = tail_repetition(t)
    return t if r is None else t[:len(t) - r[1] + r[0]]

proc = AutoProcessor.from_pretrained(a.base, local_files_only=True, trust_remote_code=False,
                                     size={"shortest_edge": a.min_pixels, "longest_edge": a.max_pixels})
model = Qwen3VLForConditionalGeneration.from_pretrained(
    a.base, local_files_only=True, trust_remote_code=False,
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
        start = b["input_ids"].shape[1]
        kw = dict(max_new_tokens=a.max_new_tokens, do_sample=False, use_cache=True)
        if a.guard == "rp105":
            kw["repetition_penalty"] = 1.05
        elif a.guard == "rp110":
            kw["repetition_penalty"] = 1.10
        elif a.guard in ("stop", "stop-retry"):
            kw["stopping_criteria"] = StoppingCriteriaList([LoopStop(start)])
        with torch.inference_mode():
            ids = model.generate(**b, **kw)
        gen = ids[0, start:]
        text = proc.decode(gen, skip_special_tokens=True).strip()
        tokens = int(gen.shape[0]); looped = a.guard in ("stop", "stop-retry") and tail_repetition(text) is not None; retried = False
        if looped:
            text = cut(text)
            if a.guard == "stop-retry":
                retried = True
                with torch.inference_mode():
                    ids = model.generate(**b, max_new_tokens=min(a.max_new_tokens, 2 * tokens), do_sample=False, use_cache=True,
                                         repetition_penalty=1.10, stopping_criteria=StoppingCriteriaList([LoopStop(start)]))
                gen = ids[0, start:]
                text = cut(proc.decode(gen, skip_special_tokens=True).strip())
        out.write(json.dumps({"id": row["id"], "text": text, "tokens": tokens, "looped": looped, "retried": retried, "ms": int((time.time() - s) * 1000)}, ensure_ascii=False) + "\n")
        if i % 10 == 0:
            prog.write(f"{a.out.name} {i}/{len(rows)} {time.time() - t0:.0f}s\n")
