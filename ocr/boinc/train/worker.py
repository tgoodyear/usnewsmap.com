#!/usr/bin/env python3
"""One bounded, offline visual-transcription LoRA trial; no checkpoint is auto-promoted."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import random
import re
import signal
import time
import zipfile
import xml.etree.ElementTree as ET

BASE_REVISION = "89644892e4d85e24eaac8bacfd4f463576704203"
MAX_BUNDLE = 64 * 1024 * 1024
MAX_ADAPTER = 64 * 1024 * 1024
PROMPT = (
    "Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, "
    "line breaks and hyphenation. Do not correct or modernise anything, and do not add anything "
    "that is not printed. If a word is unreadable, write [?] for it. Output only the transcription."
)
PERMITTED_LICENSES = {"CC0-1.0", "CC-BY-4.0", "CC-BY-4.0 (Zenodo record)",
                      "CC0-1.0 (GitHub LICENSE file); CC-BY-4.0 (Zenodo record); images Public Domain Mark 1.0"}
CONFIG_KEYS = {"v", "base_revision", "bundle_sha256", "campaign_sha256", "seed", "steps", "learning_rate", "rank"}
HEX = re.compile(r"[0-9a-f]{64}\Z")


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_job(root: Path) -> tuple[dict, list[dict]]:
    raw = (root / "train.json").read_bytes()
    if len(raw) > 4096:
        raise ValueError("training config exceeds 4096 bytes")
    job = json.loads(raw)
    if not isinstance(job, dict) or set(job) != CONFIG_KEYS:
        raise ValueError("invalid training configuration fields")
    if type(job["v"]) is not int or job["v"] != 1 or job["base_revision"] != BASE_REVISION:
        raise ValueError("unsupported training contract or base revision")
    if not isinstance(job["campaign_sha256"], str) or not HEX.fullmatch(job["campaign_sha256"]):
        raise ValueError("invalid immutable campaign identity")
    for key, low, high in (("seed", 0, 2**31 - 1), ("steps", 1, 64), ("rank", 4, 8)):
        if type(job[key]) is not int or not low <= job[key] <= high:
            raise ValueError(f"{key} is outside the bounded trial range")
    lr = job["learning_rate"]
    if type(lr) not in (float, int) or not math.isfinite(lr) or not 1e-6 <= lr <= 2e-4:
        raise ValueError("learning_rate is outside the bounded trial range")
    bundle = root / "training.zip"
    if bundle.stat().st_size > MAX_BUNDLE or sha(bundle.read_bytes()) != job["bundle_sha256"]:
        raise ValueError("training bundle size or checksum mismatch")
    # Never extract volunteer input paths or execute code from the bundle.
    with zipfile.ZipFile(bundle) as archive:
        infos = archive.infolist()
        names = [i.filename for i in infos]
        if (len(names) != len(set(names)) or len(names) > 100
                or sum(i.file_size for i in infos) > MAX_BUNDLE):
            raise ValueError("training bundle is duplicated or exceeds limits")
        manifest = json.loads(archive.read("training.json"))
        if (not isinstance(manifest, dict)
                or set(manifest) != {"v", "split", "split_sha256", "examples", "notices"}
                or type(manifest["v"]) is not int or manifest["v"] != 1 or manifest["split"] != "train"
                or not isinstance(manifest["split_sha256"], str)
                or not HEX.fullmatch(manifest["split_sha256"])):
            raise ValueError("bundle must identify an immutable train-only split")
        examples = manifest["examples"]
        if not isinstance(examples, list) or not 1 <= len(examples) <= 64:
            raise ValueError("expected 1-64 training examples")
        expected = {"training.json", "NOTICE.txt"}
        if (not archive.read("NOTICE.txt").strip()
                or not isinstance(manifest["notices"], list) or not manifest["notices"]
                or any(not isinstance(n, str) or not n.strip() for n in manifest["notices"])):
            raise ValueError("training source notices are required")
        loaded = []
        ids = set()
        for row in examples:
            if (not isinstance(row, dict) or set(row) != {
                "crop_id", "image", "image_sha256", "text", "source_license", "source_url",
            } or not isinstance(row["source_license"], str)
                    or row["source_license"] not in PERMITTED_LICENSES
                    or not isinstance(row["source_url"], str) or not row["source_url"].startswith("https://")):
                raise ValueError("invalid training example or rights")
            name = row["image"]
            if (not isinstance(name, str) or not name.startswith("images/")
                    or not re.fullmatch(r"images/[A-Za-z0-9][A-Za-z0-9._-]{0,100}", name)
                    or name in expected or not isinstance(row["crop_id"], str)
                    or not 1 <= len(row["crop_id"]) <= 256 or row["crop_id"] in ids
                    or not isinstance(row["text"], str) or not 1 <= len(row["text"]) <= 800):
                raise ValueError("invalid or duplicated training image/target")
            ids.add(row["crop_id"])
            expected.add(name)
            image = archive.read(name)
            if len(image) > 2 * 1024 * 1024 or sha(image) != row["image_sha256"]:
                raise ValueError("training image checksum or size mismatch")
            loaded.append({**row, "pixels": image})
        if set(names) != expected:
            raise ValueError("training archive contains undeclared files")
    return job, loaded


def attach_lora(model, rank: int):
    from peft import LoraConfig, get_peft_model
    # Full module paths deliberately exclude visual attention and the output head.
    targets = [name for name, _ in model.named_modules()
               if ".language_model." in name and name.endswith((".q_proj", ".v_proj"))]
    if not targets:
        raise ValueError("base model has no expected language attention modules")
    model = get_peft_model(model, LoraConfig(
        r=rank, lora_alpha=2 * rank, lora_dropout=0.0, target_modules=targets,
        bias="none", task_type="CAUSAL_LM",
    ))
    if any("visual" in name for name, p in model.named_parameters() if p.requires_grad):
        raise ValueError("vision encoder must remain frozen")
    return model


def encode(processor, row: dict):
    import io
    from PIL import Image
    import torch
    with Image.open(io.BytesIO(row["pixels"])) as source:
        if source.width * source.height > 20_000_000:
            raise ValueError("training image has excessive dimensions")
        image = source.convert("RGB")
    messages = [{"role": "user", "content": [
        {"type": "image", "image": image}, {"type": "text", "text": PROMPT},
    ]}]
    prompt = processor.apply_chat_template(
        messages, tokenize=True, add_generation_prompt=True, return_dict=True, return_tensors="pt",
    )
    full = processor.apply_chat_template(
        messages + [{"role": "assistant", "content": [{"type": "text", "text": row["text"]}]}],
        tokenize=True, add_generation_prompt=False, return_dict=True, return_tensors="pt",
    )
    length = prompt["input_ids"].shape[1]
    if full["input_ids"].shape[1] > 2048:
        raise ValueError("training example exceeds 2048 tokens; targets must not be truncated")
    if not torch.equal(prompt["input_ids"], full["input_ids"][:, :length]):
        raise ValueError("assistant prefix does not match full transcription template")
    labels = full["input_ids"].clone()
    labels[:, :length] = -100
    if not (labels != -100).any():
        raise ValueError("training example has no assistant targets")
    full["labels"] = labels
    return full


def optimize(model, batches: list, job: dict, device: str) -> list[float]:
    import torch
    model.train()
    optimizer = torch.optim.AdamW([p for p in model.parameters() if p.requires_grad],
                                 lr=job["learning_rate"], weight_decay=0.0)
    losses = []
    for step in range(job["steps"]):
        batch = {k: v.to(device) for k, v in batches[step % len(batches)].items()}
        optimizer.zero_grad(set_to_none=True)
        loss = model(**batch).loss
        if not torch.isfinite(loss):
            raise ValueError("non-finite training loss")
        loss.backward()
        norm = torch.nn.utils.clip_grad_norm_(
            [p for p in model.parameters() if p.requires_grad], 1.0, error_if_nonfinite=True,
        )
        optimizer.step()
        losses.append(float(loss.detach().cpu()))
        print(json.dumps({"step": step + 1, "loss": losses[-1], "gradient_norm": float(norm)}), flush=True)
    return losses


def select_gpu(root: Path) -> int:
    with (root / "init_data.xml").open("rb") as source:
        raw = source.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise ValueError("BOINC init_data.xml exceeds size limit")
    init = ET.fromstring(raw)
    devices = init.findall("gpu_device_num")
    types = init.findall("gpu_type")
    if (len(devices) != 1 or not re.fullmatch(r"[0-9]{1,3}", devices[0].text or "")
            or len(types) != 1 or types[0].text not in ("NVIDIA", "CUDA")):
        raise ValueError("expected one BOINC-assigned NVIDIA CUDA device")
    device = int(devices[0].text)
    os.environ["CUDA_VISIBLE_DEVICES"] = str(device)
    return device


def run(root: Path, model_dir: Path) -> dict:
    assigned_gpu = select_gpu(root)
    import torch
    from transformers import AutoProcessor, Qwen3VLForConditionalGeneration
    from peft import get_peft_model_state_dict
    from safetensors.torch import save_file
    job, examples = load_job(root)
    if not torch.cuda.is_available() or not torch.cuda.is_bf16_supported(including_emulation=False):
        raise ValueError("this training app requires a BF16-capable NVIDIA GPU; CPU fallback is disabled")
    if torch.cuda.get_device_properties(0).total_memory < 15 * 1024**3:
        raise ValueError("training trial requires at least 15 GiB GPU memory")
    if json.loads((model_dir / "revision.json").read_text()) != {"revision": BASE_REVISION}:
        raise ValueError("training image base-model revision differs")
    started = time.monotonic()
    random.seed(job["seed"])
    torch.manual_seed(job["seed"])
    torch.cuda.manual_seed_all(job["seed"])
    processor = AutoProcessor.from_pretrained(
        model_dir, local_files_only=True, trust_remote_code=False,
        size={"shortest_edge": 4096, "longest_edge": 131072},
    )
    batches = [encode(processor, row) for row in examples]
    model = Qwen3VLForConditionalGeneration.from_pretrained(
        model_dir, local_files_only=True, trust_remote_code=False,
        torch_dtype=torch.bfloat16, attn_implementation="sdpa",
    ).to("cuda")
    model.config.use_cache = False
    model = attach_lora(model, job["rank"])
    model.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    before = {k: v.detach().cpu().clone() for k, v in get_peft_model_state_dict(model).items()}
    losses = optimize(model, batches, job, "cuda")
    state = {k: v.detach().cpu().contiguous() for k, v in get_peft_model_state_dict(model).items()}
    if not any(not torch.equal(before[k], v) for k, v in state.items()):
        raise ValueError("optimizer did not change the adapter")
    if not all(torch.isfinite(v).all() for v in state.values()):
        raise ValueError("non-finite adapter tensors")
    save_file(state, str(root / "adapter.safetensors"))
    adapter = (root / "adapter.safetensors").read_bytes()
    if len(adapter) > MAX_ADAPTER:
        raise ValueError("adapter exceeds output limit")
    report = {
        "v": 1, "job_sha256": sha((root / "train.json").read_bytes()),
        "base_revision": BASE_REVISION, "bundle_sha256": job["bundle_sha256"],
        "campaign_sha256": job["campaign_sha256"], "assigned_gpu": assigned_gpu,
        "steps": job["steps"], "rank": job["rank"], "seed": job["seed"],
        "learning_rate": job["learning_rate"], "losses": losses,
        "adapter_sha256": sha(adapter), "adapter_bytes": len(adapter),
        "elapsed_seconds": time.monotonic() - started,
        "peak_gpu_bytes": torch.cuda.max_memory_allocated(),
        "gpu": torch.cuda.get_device_name(0), "torch": torch.__version__,
        "candidate_status": "untrusted-requires-held-out-evaluation",
    }
    (root / "result.json").write_text(json.dumps(report, sort_keys=True) + "\n")
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dir", type=Path, default=Path("/work"))
    parser.add_argument("--model", type=Path, default=Path("/opt/base"))
    args = parser.parse_args()
    def timeout(_signum, _frame):
        raise TimeoutError("training trial exceeded its one-hour wall-clock limit")
    signal.signal(signal.SIGALRM, timeout)
    signal.alarm(3600)
    # Exceptions deliberately fail the task; no empty adapter or success-shaped fallback.
    print(json.dumps(run(args.dir, args.model), sort_keys=True))


if __name__ == "__main__":
    main()
