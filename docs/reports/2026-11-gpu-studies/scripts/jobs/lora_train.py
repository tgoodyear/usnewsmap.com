#!/usr/bin/env python3
"""Train one transcription LoRA on the pinned Qwen3-VL-2B base, inside the signed training image.

Reuses the image's worker.py for the prompt and the LoRA attachment (language q/v attention only, vision frozen).
Differs from the 16-step trial contract: more examples and steps, a working image resolution, gradient accumulation.
Saves an adapter at each checkpoint step. No checkpoint is trusted here; held-out scoring happens on our side.
"""
import argparse, hashlib, json, math, random, sys, time
from pathlib import Path
sys.path.insert(0, "/opt/train")
import worker  # the signed image's /opt/train/worker.py

ap = argparse.ArgumentParser()
ap.add_argument("--train", type=Path, required=True)   # JSONL rows {"id","image","text"}, image relative to --root
ap.add_argument("--root", type=Path, required=True)
ap.add_argument("--out", type=Path, required=True)
ap.add_argument("--rank", type=int, required=True)
ap.add_argument("--lr", type=float, required=True)
ap.add_argument("--steps", type=int, required=True)
ap.add_argument("--accum", type=int, required=True)
ap.add_argument("--checkpoints", required=True)        # comma separated optimizer steps
ap.add_argument("--seed", type=int, required=True)
ap.add_argument("--min-pixels", type=int, required=True)
ap.add_argument("--max-pixels", type=int, required=True)
ap.add_argument("--max-tokens", type=int, default=6000)
ap.add_argument("--base", default="/opt/base")
ap.add_argument("--base-revision", default=None)
a = ap.parse_args()

import torch
from PIL import Image
from peft import get_peft_model_state_dict
from safetensors.torch import save_file
from transformers import AutoProcessor, Qwen3VLForConditionalGeneration

random.seed(a.seed); torch.manual_seed(a.seed); torch.cuda.manual_seed_all(a.seed)
rows = [json.loads(l) for l in a.train.read_text(encoding="utf-8").splitlines() if l.strip()]
proc = AutoProcessor.from_pretrained(a.base, local_files_only=True, trust_remote_code=False,
                                     size={"shortest_edge": a.min_pixels, "longest_edge": a.max_pixels})
model = Qwen3VLForConditionalGeneration.from_pretrained(
    a.base, local_files_only=True, trust_remote_code=False,
    torch_dtype=torch.bfloat16, attn_implementation="sdpa").to("cuda")
model.config.use_cache = False
model = worker.attach_lora(model, a.rank)
model.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
params = [p for p in model.parameters() if p.requires_grad]
opt = torch.optim.AdamW(params, lr=a.lr, weight_decay=0.0)
sched = torch.optim.lr_scheduler.LambdaLR(opt, lambda s: min(1.0, (s + 1) / 100) * (0.1 + 0.9 * 0.5 * (1 + math.cos(math.pi * min(s, a.steps) / a.steps))))


def encode(row):
    with Image.open(a.root / row["image"]) as src:
        im = src.convert("RGB")
    msg = [{"role": "user", "content": [{"type": "image", "image": im}, {"type": "text", "text": worker.PROMPT}]}]
    p = proc.apply_chat_template(msg, tokenize=True, add_generation_prompt=True, return_dict=True, return_tensors="pt")
    f = proc.apply_chat_template(msg + [{"role": "assistant", "content": [{"type": "text", "text": row["text"]}]}],
                                 tokenize=True, add_generation_prompt=False, return_dict=True, return_tensors="pt")
    n = p["input_ids"].shape[1]
    if f["input_ids"].shape[1] > a.max_tokens or not torch.equal(p["input_ids"], f["input_ids"][:, :n]):
        return None
    labels = f["input_ids"].clone(); labels[:, :n] = -100
    f["labels"] = labels
    return f


keep = set(int(x) for x in a.checkpoints.split(","))
a.out.mkdir(parents=True, exist_ok=True)
order, pos, skipped, t0 = [], 0, 0, time.time()
log = (a.out / "train-log.jsonl").open("a", buffering=1)
prog = (a.out / "progress.txt").open("a", buffering=1)
model.train(); step = 0; running = 0.0; micro = 0
while step < a.steps:
    if pos >= len(order):
        order = list(range(len(rows))); random.shuffle(order); pos = 0
    b = encode(rows[order[pos]]); pos += 1
    if b is None:
        skipped += 1; continue
    b = {k: v.to("cuda") for k, v in b.items()}
    loss = model(**b).loss
    if not torch.isfinite(loss):
        raise SystemExit("non-finite loss")
    (loss / a.accum).backward(); running += float(loss.detach()); micro += 1
    if micro == a.accum:
        norm = torch.nn.utils.clip_grad_norm_(params, 1.0, error_if_nonfinite=True)
        opt.step(); sched.step(); opt.zero_grad(set_to_none=True); step += 1
        log.write(json.dumps({"step": step, "loss": running / a.accum, "grad_norm": float(norm), "seconds": round(time.time() - t0, 1), "lr": sched.get_last_lr()[0]}) + "\n")
        prog.write(f"step {step}/{a.steps} loss {running / a.accum:.3f} {time.time() - t0:.0f}s\n")
        running = 0.0; micro = 0
        if step in keep:
            state = {k: v.detach().cpu().contiguous() for k, v in get_peft_model_state_dict(model).items()}
            if not all(torch.isfinite(v).all() for v in state.values()):
                raise SystemExit("non-finite adapter")
            p = a.out / f"adapter-r{a.rank}-s{step}.safetensors"
            save_file(state, str(p))
            log.write(json.dumps({"checkpoint": p.name, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}) + "\n")
(a.out / "train-report.json").write_text(json.dumps({
    "status": "untrusted-requires-held-out-evaluation", "rank": a.rank, "lr": a.lr, "steps": a.steps, "accum": a.accum,
    "seed": a.seed, "examples": len(rows), "skipped": skipped, "seconds": round(time.time() - t0, 1),
    "peak_gpu_bytes": torch.cuda.max_memory_allocated(), "gpu": torch.cuda.get_device_name(0), "torch": torch.__version__,
    "base_revision": a.base_revision or worker.BASE_REVISION, "min_pixels": a.min_pixels, "max_pixels": a.max_pixels}, sort_keys=True))
