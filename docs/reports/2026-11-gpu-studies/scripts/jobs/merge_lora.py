#!/usr/bin/env python3
"""Merge a LoRA adapter into its base and save plain weights for GGUF conversion. Output stays untrusted."""
import argparse, sys
sys.path.insert(0, "/opt/train")
import worker
ap = argparse.ArgumentParser()
ap.add_argument("--base", required=True)
ap.add_argument("--adapter", required=True)
ap.add_argument("--rank", type=int, default=8)
ap.add_argument("--out", required=True)
a = ap.parse_args()
import torch
from peft import get_peft_model_state_dict, set_peft_model_state_dict
from safetensors.torch import load_file
from transformers import AutoProcessor, Qwen3VLForConditionalGeneration
m = Qwen3VLForConditionalGeneration.from_pretrained(a.base, local_files_only=True, trust_remote_code=False, torch_dtype=torch.bfloat16, attn_implementation="sdpa")
m = worker.attach_lora(m, a.rank)
state = load_file(a.adapter)
expected = set(get_peft_model_state_dict(m))
missing, unexpected = expected - state.keys(), state.keys() - expected
if not expected or missing or unexpected:
    raise SystemExit(f"adapter keys do not fit: expected {len(expected)}, missing {len(missing)}, unexpected {len(unexpected)}")
r = set_peft_model_state_dict(m, state)
if getattr(r, "unexpected_keys", None):
    raise SystemExit("adapter keys do not fit after loading")
m = m.merge_and_unload()
m.save_pretrained(a.out, safe_serialization=True)
AutoProcessor.from_pretrained(a.base, local_files_only=True, trust_remote_code=False).save_pretrained(a.out)
