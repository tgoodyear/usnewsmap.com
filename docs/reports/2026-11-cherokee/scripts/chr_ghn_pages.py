"""The page manifest for the 699 GHN-only Cherokee Phoenix pages (#1622): one JSON line per page.

Reads the fetcher's inventory and the images it kept, and makes no network request. Each image is
hashed again and its size read from the JPEG header, and every field is checked against the
inventory (and against the verification receipts, when given). The 324 LoC pages
(`phoenix-pages.jsonl`) are checked to share no page and no issue with the GHN pages.

Usage: --inventory inventory.jsonl --images <dir holding images/> --loc-pages phoenix-pages.jsonl
       [--receipts verified-images.jsonl] --out ghn-pages.jsonl
"""
import argparse
import hashlib
import json
import os
import struct
import sys


def jpeg_size(path):
    """Width, height and component count from the first SOFn marker, without decoding."""
    with open(path, "rb") as f:
        if f.read(2) != b"\xff\xd8":
            raise ValueError("not a JPEG")
        while True:
            b = f.read(1)
            while b and b != b"\xff":
                b = f.read(1)
            while b == b"\xff":
                b = f.read(1)
            if not b:
                raise ValueError("no SOF marker")
            m = b[0]
            if m in (0xD8, 0x01) or 0xD0 <= m <= 0xD7:
                continue
            (n,) = struct.unpack(">H", f.read(2))
            if 0xC0 <= m <= 0xCF and m not in (0xC4, 0xC8, 0xCC):
                _, h, w, comps = struct.unpack(">BHHB", f.read(6))
                return w, h, comps
            f.seek(n - 2, 1)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--inventory", required=True)
    ap.add_argument("--images", required=True, help="directory the inventory's `file` paths are under")
    ap.add_argument("--loc-pages", required=True)
    ap.add_argument("--receipts")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    inv = [json.loads(line) for line in open(a.inventory)]
    ghn = [r for r in inv if r["source"] == "ghn"]
    loc = {json.loads(line)["page"] for line in open(a.loc_pages)}
    loc_issues = {p.rsplit("/", 1)[0] for p in loc}
    receipts = {}
    if a.receipts:
        receipts = {r["id"]: r for r in map(json.loads, open(a.receipts))}
    problems = []
    rows = []
    for r in sorted(ghn, key=lambda r: r["id"]):
        page = r["id"]
        if page in loc or page.rsplit("/", 1)[0] in loc_issues:
            problems.append(f"{page}: also a LoC page or issue")
        path = f"{a.images}/{r['file']}"
        sha = sha256(path)
        w, h, comps = jpeg_size(path)
        size = os.path.getsize(path)
        for name, got, want in (("sha256", sha, r["sha256"]), ("bytes", size, r["bytes"]),
                                ("width", w, r["width"]), ("height", h, r["height"])):
            if got != want:
                problems.append(f"{page}: {name} {got} != inventory {want}")
        rc = receipts.get(page)
        if a.receipts and (rc is None or rc["sha256"] != sha or (rc["width"], rc["height"]) != (w, h)
                           or not rc.get("fully_decoded")):
            problems.append(f"{page}: receipt disagrees")
        batch, _, ver = r["batch"].removeprefix("batch_").rpartition("_ver")
        rows.append({
            "batch": batch,
            "batch_version": int(ver),
            "components": comps,
            "doc_id": "ghn:" + page.replace("/", "_"),
            "file": r["file"],
            "height": h,
            "lines": None,
            "listing": r["listing"],
            "page": page,
            "scan": {"bytes": size, "sha256": sha,
                     "url": r["image_service"] + "/full/full/0/default.jpg"},
            "source": "ghn",
            "test_set": False,
            "width": w,
        })
    if len(rows) != len({r["page"] for r in rows}) or len({r["scan"]["sha256"] for r in rows}) != len(rows):
        problems.append("duplicate page or scan")
    if problems:
        sys.exit("\n".join(problems[:50]) + f"\n{len(problems)} problems; nothing written")
    with open(a.out, "w") as f:
        for r in rows:
            f.write(json.dumps(r, sort_keys=True, ensure_ascii=False, separators=(",", ":")) + "\n")
    print(json.dumps({"pages": len(rows), "bytes": sum(r["scan"]["bytes"] for r in rows),
                      "issues": len({r["page"].rsplit("/", 1)[0] for r in rows})}))


if __name__ == "__main__":
    main()
