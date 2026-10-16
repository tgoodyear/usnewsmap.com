"""Synthetic-only training contract tests; real tiny visual-model smoke is explicit."""
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import worker as w


class TrainingContract(unittest.TestCase):
    def job(self, root):
        pixels = b"synthetic pixels"
        example = {"crop_id": "synthetic-1", "image": "images/a.png",
                   "image_sha256": w.sha(pixels), "text": "SYNTHETIC TARGET",
                   "source_license": "CC-BY-4.0", "source_url": "https://example.org/synthetic"}
        manifest = {"v": 1, "split": "train", "split_sha256": "b" * 64,
                    "examples": [example], "notices": ["Synthetic test; no real source pixels"]}
        self.bundle(root, manifest, {"images/a.png": pixels})
        job = {"v": 1, "base_revision": w.BASE_REVISION,
               "campaign_sha256": "c" * 64,
               "bundle_sha256": w.sha((root / "training.zip").read_bytes()),
               "seed": 542, "steps": 2, "learning_rate": 1e-4, "rank": 4}
        (root / "train.json").write_text(json.dumps(job))
        return job, manifest

    def bundle(self, root, manifest, files):
        with zipfile.ZipFile(root / "training.zip", "w") as archive:
            archive.writestr("training.json", json.dumps(manifest))
            archive.writestr("NOTICE.txt", "Synthetic attribution\n")
            for name, data in files.items():
                archive.writestr(name, data)

    def test_loads_train_only_bounded_trial(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            job, _ = self.job(root)
            actual, examples = w.load_job(root)
            self.assertEqual(actual, job)
            self.assertEqual(examples[0]["pixels"], b"synthetic pixels")

    def test_rejects_invalid_job_fields_and_bounds(self):
        cases = {"steps": 65, "rank": 100, "learning_rate": float("nan"), "seed": True,
                 "base_revision": "main", "bundle_sha256": "0" * 64, "extra": "code.py"}
        for key, value in cases.items():
            with self.subTest(key=key), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                job, _ = self.job(root)
                job[key] = value
                (root / "train.json").write_text(json.dumps(job))
                with self.assertRaises(ValueError):
                    w.load_job(root)

    def test_rejects_heldout_rights_drift_and_archive_extras(self):
        for mode in ("heldout", "rights", "pixels", "text", "extra", "path", "duplicate", "notice"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                job, manifest = self.job(root)
                files = {"images/a.png": b"synthetic pixels"}
                if mode == "heldout":
                    manifest["split"] = "test"
                elif mode == "rights":
                    manifest["examples"][0]["source_license"] = "CC-BY-NC-ND-4.0"
                elif mode == "pixels":
                    files["images/a.png"] = b"changed"
                elif mode == "text":
                    manifest["examples"][0]["text"] = "x" * 801
                elif mode == "extra":
                    files["code.py"] = b"print('never execute')"
                elif mode == "path":
                    manifest["examples"][0]["image"] = "images/../other.png"
                elif mode == "duplicate":
                    manifest["examples"] *= 2
                else:
                    manifest["notices"] = []
                self.bundle(root, manifest, files)
                job["bundle_sha256"] = w.sha((root / "training.zip").read_bytes())
                (root / "train.json").write_text(json.dumps(job))
                with self.assertRaises(ValueError):
                    w.load_job(root)

    def test_duplicate_archive_and_decompressed_limit(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            job, _ = self.job(root)
            with zipfile.ZipFile(root / "training.zip", "a") as archive:
                with self.assertWarns(UserWarning):
                    archive.writestr("NOTICE.txt", "duplicate")
            job["bundle_sha256"] = w.sha((root / "training.zip").read_bytes())
            (root / "train.json").write_text(json.dumps(job))
            with self.assertRaisesRegex(ValueError, "duplicated"):
                w.load_job(root)
            with zipfile.ZipFile(root / "training.zip", "w", compression=zipfile.ZIP_DEFLATED) as archive:
                archive.writestr("bomb", b"x" * 8192)
            job["bundle_sha256"] = w.sha((root / "training.zip").read_bytes())
            (root / "train.json").write_text(json.dumps(job))
            with patch.object(w, "MAX_BUNDLE", 4096), self.assertRaisesRegex(ValueError, "exceeds limits"):
                w.load_job(root)

    def test_production_has_no_cpu_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.job(root)
            (root / "init_data.xml").write_text(
                "<app_init_data><gpu_type>NVIDIA</gpu_type><gpu_device_num>1</gpu_device_num></app_init_data>"
            )
            with patch("torch.cuda.is_available", return_value=False), \
                    self.assertRaisesRegex(ValueError, "CPU fallback is disabled"):
                w.run(root, root / "no-model-needed")

    def test_assigned_gpu_is_selected_before_cuda_and_ambiguous_devices_fail(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(w.os.environ):
            root = Path(tmp)
            path = root / "init_data.xml"
            path.write_text(
                "<app_init_data><gpu_type>NVIDIA</gpu_type><gpu_device_num>2</gpu_device_num></app_init_data>"
            )
            self.assertEqual(w.select_gpu(root), 2)
            self.assertEqual(w.os.environ["CUDA_VISIBLE_DEVICES"], "2")
            for content in (
                "<app_init_data/>",
                "<app_init_data><gpu_type>ATI</gpu_type><gpu_device_num>0</gpu_device_num></app_init_data>",
                "<app_init_data><gpu_type>NVIDIA</gpu_type><gpu_device_num>-1</gpu_device_num></app_init_data>",
                "<app_init_data><gpu_type>NVIDIA</gpu_type><gpu_device_num>0</gpu_device_num>"
                "<gpu_device_num>1</gpu_device_num></app_init_data>",
            ):
                path.write_text(content)
                with self.assertRaises(ValueError):
                    w.select_gpu(root)


def visual_smoke(processor_dir: Path) -> dict:
    """Actual Qwen3-VL forward/backward on synthetic pixels, not pretrained quality."""
    import torch
    from PIL import Image
    from transformers import AutoProcessor, Qwen3VLConfig, Qwen3VLForConditionalGeneration
    from peft import get_peft_model_state_dict
    from safetensors.torch import save_file, load_file
    torch.set_num_threads(2)
    torch.manual_seed(542)
    config = Qwen3VLConfig.from_pretrained(processor_dir, local_files_only=True)
    config.text_config.hidden_size = 64
    config.text_config.intermediate_size = 128
    config.text_config.num_hidden_layers = 1
    config.text_config.num_attention_heads = 4
    config.text_config.num_key_value_heads = 2
    config.text_config.head_dim = 16
    config.text_config.rope_scaling = {"rope_type": "default", "mrope_section": [2, 3, 3],
                                      "mrope_interleaved": True}
    config.vision_config.depth = 1
    config.vision_config.hidden_size = 32
    config.vision_config.intermediate_size = 64
    config.vision_config.num_heads = 4
    config.vision_config.out_hidden_size = 64
    config.vision_config.deepstack_visual_indexes = []
    config._attn_implementation = "eager"
    processor = AutoProcessor.from_pretrained(
        processor_dir, local_files_only=True, min_pixels=4096, max_pixels=4096,
    )
    buf = io.BytesIO()
    Image.new("RGB", (64, 64), "white").save(buf, format="PNG")
    batch = w.encode(processor, {"pixels": buf.getvalue(), "text": "SYNTHETIC NEWSPAPER"})
    assert (batch["labels"] == -100).any()
    assert (batch["labels"] != -100).any()
    model = w.attach_lora(Qwen3VLForConditionalGeneration(config), 4)
    model.peft_config["default"].base_model_name_or_path = str(processor_dir)
    model.config.use_cache = False
    model.gradient_checkpointing_enable(gradient_checkpointing_kwargs={"use_reentrant": False})
    frozen = {k: p.detach().clone() for k, p in model.named_parameters() if not p.requires_grad}
    adapter_before = {k: p.clone() for k, p in get_peft_model_state_dict(model).items()}
    losses = w.optimize(model, [batch], {"steps": 2, "learning_rate": 1e-4}, "cpu")
    state = get_peft_model_state_dict(model)
    assert any(not torch.equal(adapter_before[k], v) for k, v in state.items())
    assert all(torch.equal(frozen[k], p) for k, p in model.named_parameters() if k in frozen)
    assert all("visual" not in k for k in state)
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "adapter.safetensors"
        save_file({k: v.detach().contiguous() for k, v in state.items()}, path)
        assert all(torch.isfinite(v).all() for v in load_file(path).values())
    return {"visual_forward_backward": True, "losses": losses,
            "frozen_base_unchanged": True, "adapter_changed": True}


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--visual-smoke", type=Path)
    args, rest = parser.parse_known_args()
    if args.visual_smoke:
        print(json.dumps(visual_smoke(args.visual_smoke)))
    else:
        unittest.main(argv=[__file__, *rest])
