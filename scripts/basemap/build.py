#!/usr/bin/env python3
"""Build a bounded, same-schema OpenFreeMap overview archive and its static assets."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import gzip
import hashlib
import json
import math
from pathlib import Path
import sqlite3
import subprocess
import tarfile
import threading
from urllib.parse import quote
from urllib.error import HTTPError
from urllib.request import Request, urlopen


LIMIT = 50 * 1024 * 1024
UA = "USNewsMap-BasemapBundle/1.0 (+https://github.com/tgoodyear/usnewsmap.com)"


def fields(data):
    def varint(pos):
        value = shift = 0
        while True:
            byte = data[pos]
            pos += 1
            value |= (byte & 127) << shift
            if byte < 128:
                return value, pos
            shift += 7
            if shift > 63:
                raise ValueError("Invalid protobuf varint")
    pos = 0
    while pos < len(data):
        tag, pos = varint(pos)
        wire = tag & 7
        if wire == 2:
            size, pos = varint(pos)
            end = pos + size
            if end > len(data):
                raise ValueError("Truncated protobuf field")
            yield tag >> 3, data[pos:end]
            pos = end
        elif wire == 0:
            _, pos = varint(pos)
        elif wire in (1, 5):
            pos += 8 if wire == 1 else 4
        else:
            raise ValueError("Unsupported protobuf wire type")


def varints(data):
    value = shift = 0
    for byte in data:
        value |= (byte & 127) << shift
        if byte < 128:
            yield value
            value = shift = 0
        else:
            shift += 7
            if shift > 63:
                raise ValueError("Invalid packed protobuf varint")
    if shift:
        raise ValueError("Truncated packed protobuf varint")


def glyph_ranges(tile):
    ranges = {0}
    for tag, layer in fields(tile):
        if tag != 3:
            continue
        entries = list(fields(layer))
        keys = [v.decode("utf-8") for field, v in entries if field == 3]
        values = [v for field, v in entries if field == 4]
        for field, feature in entries:
            if field != 2:
                continue
            for kind, tags in fields(feature):
                if kind != 2:
                    continue
                pairs = list(varints(tags))
                if len(pairs) % 2:
                    raise ValueError("Unpaired feature tags")
                for key, value in zip(pairs[::2], pairs[1::2]):
                    if keys[key] in {"name", "name_en", "name:latin", "name:nonlatin", "ref"}:
                        for value_kind, text in fields(values[value]):
                            if value_kind == 1:
                                # OpenFreeMap's glyph endpoint serves BMP ranges only.
                                ranges.update(ord(char) // 256 * 256 for char in text.decode("utf-8")
                                              if ord(char) <= 65535)
    return ranges


def build(a):
    if not 0 <= a.maxzoom <= 5:
        raise ValueError("Overview bundles support zooms 0 through 5 only")
    west, south, east, north = a.bbox
    if not (-180 <= west < east <= 180 and -85 <= south < north <= 85):
        raise ValueError("Invalid overview bounding box")
    a.out.mkdir(parents=True, exist_ok=True)
    root = a.out / a.name
    if root.exists():
        raise ValueError("Bundle output already exists; use a new version or output directory")
    cache = a.out / "downloads"
    cache.mkdir(exist_ok=True)
    lock = threading.Lock()
    total = 0

    def download(url):
        nonlocal total
        path = cache / hashlib.sha256((url + ":gzip").encode()).hexdigest()
        if path.exists():
            data = path.read_bytes()
            encoding = path.with_suffix(".encoding").read_text()
        else:
            request = Request(url, headers={"User-Agent": UA, "Accept-Encoding": "gzip"})
            try:
                with urlopen(request, timeout=60) as response:
                    data = response.read(LIMIT + 1)
                    encoding = response.headers.get("Content-Encoding", "")
            except HTTPError as error:
                raise RuntimeError(f"Download failed: {url} ({error.code})") from error
            if len(data) > LIMIT:
                raise ValueError("Download exceeds offline bundle limit")
            path.write_bytes(data)
            path.with_suffix(".encoding").write_text(encoding)
        with lock:
            total += len(data)
            if total > LIMIT:
                raise ValueError("Bundle exceeds OpenFreeMap's 50 MiB offline limit")
        return gzip.decompress(data) if encoding == "gzip" else data

    upstream_style = download(a.style)
    style = json.loads(upstream_style)
    metadata = json.loads(download(a.tilejson))
    template = metadata["tiles"][0]
    root.mkdir()
    db_path = a.out / "overview.mbtiles"
    if db_path.exists():
        db_path.unlink()
    db = sqlite3.connect(db_path)
    db.executescript(
        "CREATE TABLE metadata(name TEXT, value TEXT);"
        "CREATE TABLE tiles(zoom_level INTEGER,tile_column INTEGER,tile_row INTEGER,tile_data BLOB);"
        "CREATE UNIQUE INDEX tile_index ON tiles(zoom_level,tile_column,tile_row);"
    )
    info = {"name": a.name, "format": "pbf", "minzoom": "0", "maxzoom": str(a.maxzoom),
            "bounds": "-180,-85.0511287798066,180,85.0511287798066",
            "attribution": metadata["attribution"],
            "json": json.dumps({"vector_layers": metadata["vector_layers"]})}
    db.executemany("INSERT INTO metadata VALUES (?,?)", info.items())
    tiles = []
    for z in range(a.maxzoom + 1):
        size = 2 ** z
        west, south, east, north = a.bbox
        xmin = max(0, math.floor((west + 180) / 360 * size))
        xmax = min(size - 1, math.floor((east + 180) / 360 * size))
        def tile_y(lat):
            return (1 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2 * size
        ymin, ymax = max(0, math.floor(tile_y(north))), min(size - 1, math.floor(tile_y(south)))
        tiles.extend((z, x, y) for x in range(size) for y in range(size)
                     if z <= 2 or (xmin <= x <= xmax and ymin <= y <= ymax))
    ranges = {0}

    def fetch_tile(tile):
        z, x, y = tile
        url = template.replace("{z}", str(z)).replace("{x}", str(x)).replace("{y}", str(y))
        data = download(url)
        raw = gzip.decompress(data) if data.startswith(b"\x1f\x8b") else data
        return tile, gzip.compress(raw, mtime=0), glyph_ranges(raw)

    with ThreadPoolExecutor(max_workers=4) as pool:
        for i, ((z, x, y), data, blocks) in enumerate(pool.map(fetch_tile, tiles), 1):
            db.execute("INSERT INTO tiles VALUES (?,?,?,?)", (z, x, 2 ** z - 1 - y, data))
            ranges.update(blocks)
            if i % 100 == 0:
                print(json.dumps({"tiles": i, "download_bytes": total}), flush=True)
    db.commit()
    db.close()
    archive = root / "overview.pmtiles.bin"
    if archive.exists():
        archive.unlink()
    subprocess.run([str(a.pmtiles), "convert", str(db_path), str(archive)], check=True)
    subprocess.run([str(a.pmtiles), "verify", str(archive)], check=True)

    fonts = sorted({font for layer in style["layers"] for font in layer.get("layout", {}).get("text-font", [])})
    assets = []
    assets.extend([
        ("LICENSE-OpenFreeMap.md", "https://raw.githubusercontent.com/hyperknot/openfreemap/main/LICENSE.md"),
        ("LICENSE-fonts.txt", "https://raw.githubusercontent.com/openmaptiles/fonts/master/noto-sans/LICENSE"),
    ])
    for font in fonts:
        for start in sorted(ranges):
            span = f"{start}-{start + 255}"
            url = style["glyphs"].replace("{fontstack}", quote(font)).replace("{range}", span)
            assets.append((f"fonts/{font}/{span}.pbf", url))
    for suffix in ("", "@2x"):
        for ext in ("json", "png"):
            assets.append((f"sprites/ofm{suffix}.{ext}", f"{style['sprite']}{suffix}.{ext}"))

    def fetch_asset(item):
        name, url = item
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(download(url))

    with ThreadPoolExecutor(max_workers=4) as pool:
        list(pool.map(fetch_asset, assets))
    # Keep one style/schema across both sources; only the transport changes at zoom 6.
    prefix = f"/basemap/{a.name}"
    style["sprite"] = prefix + "/sprites/ofm"
    style["glyphs"] = "basemap://glyphs/{fontstack}/{range}.pbf"
    style["sources"] = {
        "openmaptiles": {"type": "vector", "tiles": ["basemap://tiles/{z}/{x}/{y}.pbf"],
                         "minzoom": 0, "maxzoom": metadata["maxzoom"],
                         "attribution": metadata["attribution"]},
    }
    for layer in style["layers"]:
        if layer.get("source") not in (None, "openmaptiles"):
            raise ValueError("Unexpected style source")
    (root / "style.json").write_text(json.dumps(style, ensure_ascii=False, separators=(",", ":")) + "\n")
    (root / "upstream-style.json").write_bytes(upstream_style)
    (root / "tilejson.json").write_text(json.dumps(metadata))
    # The glyph protocol falls back upstream only for ranges not in this overview bundle.
    (root / "glyphs.json").write_text(json.dumps({"fonts": fonts, "ranges": sorted(ranges),
                                                "upstream": json.loads((root / "upstream-style.json").read_text())["glyphs"]}))
    provenance = {"source_style": a.style, "source_tilejson": a.tilejson, "source_tiles": template,
                  "maxzoom": a.maxzoom, "bbox": a.bbox, "global_maxzoom": 2,
                  "tile_count": len(tiles), "download_bytes": total,
                  "pmtiles_version": subprocess.check_output([str(a.pmtiles), "version"], text=True).strip(),
                  "attribution": metadata["attribution"], "license": "https://opendatacommons.org/licenses/odbl/",
                  "font_license": "https://github.com/openmaptiles/fonts/blob/master/noto-sans/LICENSE"}
    (root / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
    hashes = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
              for p in sorted(root.rglob("*")) if p.is_file() and p.name != "checksums.json"}
    (root / "checksums.json").write_text(json.dumps(hashes, indent=2) + "\n")
    bundle = a.out / (a.name + ".tar.gz")
    with tarfile.open(bundle, "w:gz") as tar:
        tar.add(root, arcname=a.name)
    print(json.dumps({"bundle": str(bundle), "sha256": hashlib.sha256(bundle.read_bytes()).hexdigest(),
                      "archive_bytes": archive.stat().st_size, "bundle_bytes": bundle.stat().st_size,
                      "glyph_ranges": len(ranges), "download_bytes": total}, indent=2))


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--style", required=True)
    ap.add_argument("--tilejson", required=True)
    ap.add_argument("--maxzoom", type=int, default=5)
    ap.add_argument("--bbox", type=float, nargs=4, default=[-155, 0, -35, 70])
    ap.add_argument("--pmtiles", type=Path, required=True)
    args = ap.parse_args()
    import re
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,80}", args.name):
        ap.error("invalid bundle name")
    build(args)
