#!/usr/bin/env python3
"""Export a trained v2 column detector to ONNX and check it against PyTorch (#2107).

    export_onnx.py --weights OUT/model.pt --out OUT/colv2.onnx [--opset 17] [--check-image page.jpg]

The graph takes one image [1, 3, H, W] (float 0..1, H and W dynamic, longer side model.MAX_SIDE) and
returns boxes [N, 4] and scores [N] in the input's pixels. The TorchScript-based exporter is used
(dynamo=False), the path torchvision tests for Faster R-CNN. After export, onnxruntime runs the graph
on --check-image (or a synthetic page) and on a resized copy of it (another H and W). On each, the
sorted score lists must agree within 1e-3, and at least MIN_MATCH of the PyTorch boxes whose score is
unique (1e-4 from every other) must lie within 1 px of the ONNX box with the nearest score (near-tied
scores, as from an untrained net, can order and cut the top 100 differently). Writes the .onnx file
and <out>.json (sha256, opset, torch/onnxruntime versions, the match figures per check).
"""
import argparse
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import model as M  # noqa: E402

MIN_MATCH = 0.9


def synthetic_page(w=700, h=1000):
    from PIL import Image, ImageDraw
    img = Image.new("L", (w, h), 235)
    d = ImageDraw.Draw(img)
    for c in range(4):
        x0 = 20 + c * 170
        for y in range(80, h - 40, 14):
            d.rectangle([x0, y, x0 + 150, y + 7], fill=40)
    return img


def compare(wrap, sess, x):
    """Parity of PyTorch and ONNX on one input (see the module docstring)."""
    import numpy as np
    import torch
    with torch.no_grad():
        tb, ts = wrap(x)
    ob, os_ = sess.run(None, {"image": x.numpy()})
    tbn, tsn = tb.numpy(), ts.numpy()
    score_diff = float(np.abs(np.sort(tsn) - np.sort(os_)).max()) if len(tsn) == len(os_) and len(tsn) else 0.0
    gaps = np.abs(tsn[:, None] - tsn[None, :]) + np.eye(len(tsn)) * 9 if len(tsn) > 1 else np.ones((len(tsn), 1))
    uniq = [i for i in range(len(tsn)) if gaps[i].min() >= 1e-4]
    diffs = [float(np.abs(tbn[i] - ob[int(np.abs(os_ - tsn[i]).argmin())]).max()) for i in uniq] if len(ob) else []
    return {"boxes_onnx": int(len(ob)), "boxes_torch": int(len(tb)), "input": list(x.shape),
            "matched_within_1px": float(np.mean([d <= 1.0 for d in diffs])) if diffs else 1.0,
            "max_box_diff_px": max(diffs) if diffs else 0.0, "max_sorted_score_diff": score_diff,
            "unique_score_boxes": len(uniq)}


def export(weights, out, opset=17, check_image=None):
    import onnxruntime as ort
    import torch
    from PIL import Image
    net = M.build("none")
    net.load_state_dict(torch.load(weights, map_location="cpu"))
    net.eval()
    wrap = M.OnnxWrapper(net).eval()
    img = Image.open(check_image).convert("L") if check_image else synthetic_page()
    img, _ = M.resize_for_model(img)
    x = M.to_tensor(img)[None]
    torch.onnx.export(wrap, (x,), out, opset_version=opset, input_names=["image"],
                      output_names=["boxes", "scores"], dynamo=False,
                      dynamic_axes={"image": {2: "height", 3: "width"}, "boxes": {0: "n"}, "scores": {0: "n"}})
    sess = ort.InferenceSession(out, providers=["CPUExecutionProvider"])
    # the export size, and a second size, so a traced-in constant size would show
    img2 = img.resize((round(img.size[0] * 0.8), round(img.size[1] * 0.9)))
    checks = [compare(wrap, sess, x), compare(wrap, sess, M.to_tensor(img2)[None])]
    with open(out, "rb") as f:
        sha = hashlib.sha256(f.read()).hexdigest()
    meta = {"checks": checks, "opset": opset, "onnxruntime": ort.__version__, "sha256": sha,
            "size_bytes": os.path.getsize(out), "torch": torch.__version__}
    with open(out + ".json", "w") as f:
        json.dump(meta, f, indent=1, sort_keys=True)
    for c in checks:
        if c["boxes_torch"] != c["boxes_onnx"] or c["matched_within_1px"] < MIN_MATCH or c["max_sorted_score_diff"] > 1e-3:
            raise SystemExit("ONNX output differs from PyTorch: %s" % json.dumps(meta))
    return meta


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--weights", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--opset", type=int, default=17)
    ap.add_argument("--check-image")
    a = ap.parse_args(argv)
    print(json.dumps(export(a.weights, a.out, a.opset, a.check_image), sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
