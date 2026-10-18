#!/usr/bin/env python3
"""Export exact IndexFlatIP vectors in a FAISS-only process for the CPU benchmark adapter."""
from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import json
import platform
from pathlib import Path
from typing import Any


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--adapter-lock", type=Path, required=True)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--out-dir", type=Path, required=True)
    args = parser.parse_args()
    lock = json.loads(args.adapter_lock.read_text(encoding="utf-8"))
    runtime_lock = Path(__file__).resolve().parents[2] / lock["runtime"]["requirements_lock"]
    if sha256(runtime_lock) != lock["runtime"]["requirements_lock_sha256"]:
        raise SystemExit("EffOCR runtime requirements lock checksum mismatch")
    if platform.python_version() != lock["runtime"]["python"]:
        raise SystemExit("Python version differs from the EffOCR adapter lock")
    for package, expected in lock["runtime"]["packages"].items():
        if importlib.metadata.version(package) != expected:
            raise SystemExit(f"runtime package differs from the EffOCR adapter lock: {package}")
    model_dir = args.model_dir.resolve()
    out_dir = args.out_dir.resolve()
    if out_dir.exists() and any(out_dir.iterdir()):
        raise SystemExit(f"refusing nonempty vector output directory: {out_dir}")
    out_dir.mkdir(parents=True, exist_ok=True)

    import faiss
    import numpy as np

    faiss.omp_set_num_threads(1)
    arrays: list[dict[str, Any]] = []
    expected_files = {
        row["path"]: row
        for row in lock["model_repositories"]["effocr_en"]["files"]
    }
    for kind in ("word", "char"):
        relative = f"{kind}_recognizer/ref.index"
        index_path = model_dir / relative
        expected = expected_files[relative]
        if not index_path.is_file() or sha256(index_path) != expected["sha256"]:
            raise SystemExit(f"pinned {kind} index checksum mismatch")
        index = faiss.read_index(str(index_path))
        if index.metric_type != faiss.METRIC_INNER_PRODUCT or index.ntotal < 1 or index.d < 1:
            raise SystemExit(f"{kind} checkpoint is not a populated inner-product index")
        vectors = index.reconstruct_n(0, index.ntotal)
        if vectors.dtype != np.float32 or vectors.shape != (index.ntotal, index.d):
            raise SystemExit(f"unexpected reconstructed {kind} vector shape or dtype")
        queries = np.random.default_rng(0).random((4, index.d), dtype=np.float32)
        distances, indices = index.search(queries, 1)
        scores = queries @ vectors.T
        nearest = scores.argmax(axis=1)
        if not np.array_equal(indices[:, 0], nearest):
            raise SystemExit(f"NumPy and FAISS nearest-neighbor identities differ for {kind}")
        if not np.allclose(distances[:, 0], scores[np.arange(len(queries)), nearest], rtol=1e-5, atol=1e-5):
            raise SystemExit(f"NumPy and FAISS inner-product scores differ for {kind}")
        vector_path = out_dir / f"{kind}.npy"
        with vector_path.open("wb") as output:
            np.save(output, vectors, allow_pickle=False)
        arrays.append(
            {
                "kind": kind,
                "source_index": relative,
                "source_index_sha256": expected["sha256"],
                "index_type": type(index).__name__,
                "metric": "inner_product",
                "vectors": vector_path.name,
                "shape": list(vectors.shape),
                "dtype": str(vectors.dtype),
                "sha256": sha256(vector_path),
                "numpy_faiss_equivalence_queries": len(queries),
                "faiss_version": faiss.__version__,
            }
        )
    (out_dir / "manifest.json").write_text(json.dumps(arrays, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(arrays, indent=2))


if __name__ == "__main__":
    main()
