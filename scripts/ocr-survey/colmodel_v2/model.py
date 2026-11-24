"""The v2 column detector: torchvision Faster R-CNN, MobileNetV3-Large FPN backbone, one class (#2107).

torchvision is BSD-3-Clause; no Ultralytics code or weights. Two classes for torchvision (0 background,
1 column). Column regions are tall and thin (height/width often 5 to 20), so the anchors use
height/width ratios 1, 2, 4, 8 and 16 instead of torchvision's 0.5, 1, 2.

Images are fed as [3, H, W] floats in 0..1 (a grey scan repeated on three channels), pre-resized so the
longer side is MAX_SIDE; the model's own transform then keeps min side MIN_SIZE, max side MAX_SIDE.

Backbone start (--init): "none" (random; nothing downloaded, licence-clean), "imagenet" (torchvision's
ImageNet-1k MobileNetV3 weights; ImageNet's terms of access are research/non-commercial, see the
licence note before shipping weights started from it).
"""
import torch
from torchvision.models.detection import FasterRCNN
from torchvision.models.detection.anchor_utils import AnchorGenerator
from torchvision.models.detection.backbone_utils import mobilenet_backbone

MAX_SIDE = 1024
MIN_SIZE = 640
ANCHOR_SIZES = ((32, 64, 128, 256, 512),) * 3
ASPECTS = ((1.0, 2.0, 4.0, 8.0, 16.0),) * 3


def build(init="none", trainable_layers=6):
    weights = None
    if init == "imagenet":
        from torchvision.models import MobileNet_V3_Large_Weights
        weights = MobileNet_V3_Large_Weights.IMAGENET1K_V1
    elif init != "none":
        raise ValueError(init)
    backbone = mobilenet_backbone(backbone_name="mobilenet_v3_large", weights=weights, fpn=True,
                                  trainable_layers=trainable_layers if weights is not None else 6)
    return FasterRCNN(backbone, num_classes=2, rpn_anchor_generator=AnchorGenerator(ANCHOR_SIZES, ASPECTS),
                      min_size=MIN_SIZE, max_size=MAX_SIDE, box_detections_per_img=100,
                      rpn_score_thresh=0.05, image_mean=[0.5, 0.5, 0.5], image_std=[0.25, 0.25, 0.25])


class OnnxWrapper(torch.nn.Module):
    """[1, 3, H, W] -> (boxes [N, 4], scores [N]) for ONNX export (one image per call)."""

    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, x):
        out = self.model([x[0]])[0]
        return out["boxes"], out["scores"]


def to_tensor(gray_img):
    """PIL image (any mode) already resized -> [3, H, W] float tensor in 0..1."""
    import numpy as np
    a = np.asarray(gray_img.convert("L"), dtype=np.float32) / 255.0
    t = torch.from_numpy(a)[None]
    return t.expand(3, -1, -1).contiguous()


def resize_for_model(img, max_side=MAX_SIDE):
    """(resized PIL image, scale) with the longer side max_side (never enlarged)."""
    from PIL import Image
    w, h = img.size
    s = min(1.0, max_side / float(max(w, h)))
    if s < 1.0:
        img = img.resize((max(1, round(w * s)), max(1, round(h * s))), Image.BILINEAR)
    return img, s
