#!/usr/bin/env python3
"""Read-only anonymous Qwen image metadata and conservative cold-disk policy."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import urllib.request

REPOSITORY = "ghcr.io/tgoodyear/usnm-boinc-worker"


def policy():
    local = Path(__file__).with_name("qwen-disk-policy.json")
    source = local if local.is_file() else Path(__file__).resolve().parents[2] / "ocr/boinc/qwen/disk-policy.json"
    return json.loads(source.read_text())


def image_metadata(image, expected_model=None):
    """Fetch manifests/config only, never layers, credentials or model weights."""
    if not re.fullmatch(re.escape(REPOSITORY) + r"@sha256:[0-9a-f]{64}", image):
        raise ValueError("Qwen images must use the public repository and a full immutable digest")
    repo = REPOSITORY.removeprefix("ghcr.io/")
    with urllib.request.urlopen(f"https://ghcr.io/token?scope=repository:{repo}:pull", timeout=30) as response:
        token = json.load(response)["token"]

    def fetch(kind, digest):
        request = urllib.request.Request(
            f"https://ghcr.io/v2/{repo}/{kind}/{digest}",
            headers={
                "Authorization": f"Bearer {token}",
                "Accept": "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.docker.distribution.manifest.list.v2+json",
            },
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read(2 * 1024 * 1024 + 1)
        if len(raw) > 2 * 1024 * 1024 or "sha256:" + hashlib.sha256(raw).hexdigest() != digest:
            raise ValueError("Oversized or digest-mismatched registry metadata")
        return json.loads(raw)

    manifest = fetch("manifests", image.split("@")[1])
    if "manifests" in manifest:
        matches = [entry for entry in manifest["manifests"]
                   if entry.get("platform") == {"os": "linux", "architecture": "amd64"}]
        if len(matches) != 1:
            raise ValueError("Qwen requires exactly one linux/amd64 manifest")
        manifest = fetch("manifests", matches[0]["digest"])
    config = fetch("blobs", manifest["config"]["digest"])
    if config.get("os") != "linux" or config.get("architecture") != "amd64":
        raise ValueError("Qwen container must be linux/amd64")
    labels = config.get("config", {}).get("Labels") or {}
    model = labels.get("org.usnewsmap.qwen.model")
    env = dict(item.split("=", 1) for item in config["config"].get("Env", []) if "=" in item)
    if expected_model is None and model is None and "USNM_QWEN_WEIGHTS" not in env:
        return None
    p = policy()
    if model not in p["models"] or labels.get("org.usnewsmap.qwen.disk-policy") != str(p["version"]):
        raise ValueError("Image has no supported model-specific Qwen disk policy; legacy dual-model images cannot be released")
    if expected_model is not None and model != expected_model:
        raise ValueError(f"Plan class requires {expected_model}, image contains {model}")
    if (env.get("USNM_QWEN_MODEL") != model or env.get("USNM_QWEN_LLAMA_IMAGE") != p["base_index"]
            or env.get("USNM_QWEN_WEIGHTS") != p["weights_identity"]):
        raise ValueError("Image environment does not match model/base policy")
    compressed = sum(layer["size"] for layer in manifest["layers"])
    payload = sum(p["models"][model]["files"].values())
    if compressed > p["base_compressed_bytes"] + payload + p["image_overhead_bound_bytes"]:
        raise ValueError("Image compressed layers exceed the reviewed disk envelope")
    return {"model": model, "compressed_bytes": compressed, **p["models"][model]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--model", required=True, choices=("2b", "8b"))
    args = parser.parse_args()
    print(json.dumps(image_metadata(args.image, args.model), sort_keys=True))


if __name__ == "__main__":
    main()
