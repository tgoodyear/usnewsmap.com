#!/usr/bin/env python3
"""Read a page image's bytes from a refs row's `image_path` (#2107).

Three forms (see layout_refs README): a plain file path; `<parquet>#row=N` (image embedded in that
parquet row, column page_image); `<images.zip>!images/<name>`. The bytes are returned as stored
(JPEG for every set used here), never re-encoded.
"""
import zipfile


class Reader:
    """Caches open zip files and the last parquet row group, so sequential reads stay cheap."""

    def __init__(self):
        self._zips = {}
        self._pq = {}
        self._group = (None, None, None)  # (path, group index, table)

    def read(self, image_path):
        if "!" in image_path:
            zp, name = image_path.split("!", 1)
            z = self._zips.get(zp) or self._zips.setdefault(zp, zipfile.ZipFile(zp))
            return z.read(name)
        if "#row=" in image_path:
            import pyarrow.parquet as pq
            path, n = image_path.split("#row=", 1)
            n = int(n)
            f = self._pq.get(path) or self._pq.setdefault(path, pq.ParquetFile(path))
            start = 0
            for g in range(f.metadata.num_row_groups):
                size = f.metadata.row_group(g).num_rows
                if n < start + size:
                    if self._group[:2] != (path, g):
                        self._group = (path, g, f.read_row_group(g, columns=["page_image"]).column(0))
                    return self._group[2][n - start].as_py()["bytes"]
                start += size
            raise IndexError(image_path)
        with open(image_path, "rb") as fh:
            return fh.read()


def extension(data):
    return ".png" if data[:4] == b"\x89PNG" else ".jpg"
