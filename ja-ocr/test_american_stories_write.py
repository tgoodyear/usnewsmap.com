"""american_stories_write.py: American Stories' pages written to the curated store, by title and year."""

import contextlib
import io
import json
import os
import tarfile
import tempfile
import unittest
from unittest import mock

import american_stories_write as asw
import jaocr

try:
    import pyarrow.parquet as pq
except ImportError:  # pragma: no cover
    pq = None


def scan(lccn, day, page, articles, legibility=None):
    """A scan file's name and body; `page` None gives a pNone name (image = 10 + its place)."""
    image = 10 + (page or 3)
    p = f"p{page}" if page else "pNone"
    name = f"faro_1865/{day}_{p}_{lccn}_0100_{day.replace('-', '')}01_{image:04d}.json"
    boxes, full = [], []
    for i, (headline, body) in enumerate(articles):
        boxes.append({"id": i, "class": "article", "legibility": (legibility or {}).get(i, "Legible"),
                      "x0": i, "y0": 0, "x1": i + 10, "y1": 20})
        full.append({"headline": headline, "byline": "", "article": body, "object_ids": [i],
                     "bbox_list": [{"x0": i, "y0": 0, "x1": i + 10, "y1": 20}]})
    boxes.append({"id": 99, "class": "ad", "legibility": "Illegible"})
    return name, {"scan": {"width": 4000, "height": 6000}, "bboxes": boxes, "full articles": full}


def tarball(scans) -> bytes:
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w:gz") as tar:
        for name, body in scans:
            data = json.dumps(body).encode()
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))
    return buf.getvalue()


@unittest.skipIf(pq is None, "pyarrow not installed")
class Write(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        d = self.tmp.name
        self.ref, self.cur = jaocr.LocalStore(os.path.join(d, "ref")), jaocr.LocalStore(os.path.join(d, "cur"))
        self.ref.write("current.json", json.dumps({"reference": "v1"}).encode())
        scans = [
            scan("sn1", "1865-01-02", 1, [("WAR NEWS", "The army moved."), ("", "Cotton is up.")], {1: "Illegible"}),
            scan("sn1", "1865-01-02", 2, [("LOCAL", "A fire on Main street.")]),
            scan("sn1", "1865-01-02", None, [("MORE", "Page three, named by its image.")]),
            # The same page again (another batch's copy): skipped.
            scan("sn1", "1865-01-02", 2, [("COPY", "Second copy.")]),
            scan("sn2", "1865-03-04", 1, []),
            ("faro_1865/notes.json", {}),
        ]
        scans[3] = (scans[3][0].replace("_0100_", "_0200_"), scans[3][1])
        self.tgz = tarball(scans)
        self.asked = []

    def tearDown(self):
        self.tmp.cleanup()

    def opener(self, req):
        self.asked.append(req.full_url)
        if req.full_url == asw.TREE:
            body = json.dumps([{"path": "faro_1865.tar.gz"}, {"path": "README.md"}]).encode()
        else:
            body = self.tgz
        return contextlib.closing(io.BytesIO(body))

    def rows(self):
        out = []
        for path in sorted(self.cur.list(asw.PAGES)):
            out += pq.read_table(io.BytesIO(self.cur.read(path))).to_pylist()
        return {r["doc_id"]: r for r in out}

    def test_writes_each_page_once_by_title_and_year(self):
        got = asw.write(self.ref, self.cur, run="exec-1", opener=self.opener)
        self.assertEqual((got["years_written"], got["pages"]), (1, 4))
        rows = self.rows()
        self.assertEqual(sorted(rows), ["sn1_1865-01-02_ed-1_seq-1", "sn1_1865-01-02_ed-1_seq-2",
                                        "sn1_1865-01-02_ed-1_seq-3", "sn2_1865-03-04_ed-1_seq-1"])
        self.assertEqual(sorted(p.rsplit("/", 2)[-2] + "/" + p.rsplit("/", 1)[-1] for p in self.cur.list(asw.PAGES)),
                         ["sn1/1865-000.parquet", "sn2/1865-000.parquet"])
        first = rows["sn1_1865-01-02_ed-1_seq-1"]
        self.assertEqual(first["text"], "WAR NEWS\nThe army moved.\n\nCotton is up.")
        arts = json.loads(first["articles"])
        self.assertEqual([first["text"][a["start"]:a["end"]] for a in arts], ["WAR NEWS\nThe army moved.", "Cotton is up."])
        self.assertEqual((arts[0]["headline"], arts[0]["bboxes"], arts[1]["legibility"]),
                         ("WAR NEWS", [[0, 0, 10, 20]], ["illegible"]))
        self.assertEqual(json.loads(first["legibility"]), {"illegible": 1, "legible": 1})  # the ad isn't text
        self.assertEqual((first["width"], first["height"], str(first["date"])), (4000, 6000, "1865-01-02"))
        self.assertEqual(rows["sn1_1865-01-02_ed-1_seq-2"]["text"], "LOCAL\nA fire on Main street.")  # not the copy
        self.assertEqual(rows["sn1_1865-01-02_ed-1_seq-3"]["text"], "MORE\nPage three, named by its image.")
        self.assertEqual(rows["sn2_1865-03-04_ed-1_seq-1"]["text"], "")
        marker = json.loads(self.cur.read(f"{asw.YEARS}/1865.json"))
        self.assertEqual({k: marker[k] for k in ("scans", "unparsed", "duplicates", "page_from_image", "pages")},
                         {"scans": 6, "unparsed": 1, "duplicates": 1, "page_from_image": 1, "pages": 4})

    def test_a_rerun_skips_written_years(self):
        asw.write(self.ref, self.cur, run="exec-1", opener=self.opener)
        self.asked.clear()
        got = asw.write(self.ref, self.cur, run="exec-2", opener=self.opener)
        self.assertEqual((got["years_written"], got["years_skipped"]), (0, 1))
        self.assertEqual(self.asked, [asw.TREE])  # the year's tarball isn't read again

    def test_a_year_hugging_face_lacks_is_refused(self):
        with self.assertRaisesRegex(ValueError, "no file for"):
            asw.write(self.ref, self.cur, years=[1700], run="exec-3", opener=self.opener)

    def test_many_pages_are_written_in_parts(self):
        with mock.patch.object(asw, "FLUSH_ROWS", 2):
            asw.write(self.ref, self.cur, run="exec-4", opener=self.opener)
        names = sorted(p.rsplit("/", 1)[-1] for p in self.cur.list(f"{asw.PAGES}/sn1"))
        self.assertEqual(names, ["1865-000.parquet", "1865-001.parquet"])
        self.assertEqual(len(self.rows()), 4)


if __name__ == "__main__":
    unittest.main()
