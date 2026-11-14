"""Tests for merged_lines_loaded.py, on small tar.bz2 archives built here: python3 -m unittest discover -s edge/check/tools."""
import collections
import contextlib
import http.server
import io
import json
import os
import struct
import sys
import tarfile
import tempfile
import threading
import unittest
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import merged_lines_loaded as mll  # noqa: E402
import yiddish_lines as yl  # noqa: E402

PAGE_CHR = "sn83020866/1828-05-28/ed-1/seq-3"
PAGE_YI = "sn83045837/1913-09-01/ed-1/seq-3"
PAGE_LOST = "sn99999999/1900-01-01/ed-1/seq-1"
PAGE_SHORT = "sn83020866/1828-06-04/ed-1/seq-1"


def alto(heights, ids=True, blocks=(0,)):
    """ALTO with one TextLine per height, ids P1_TL00001.., split into TextBlocks at the line numbers in `blocks`."""
    out = ['<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Layout><Page WIDTH="4000" HEIGHT="8000"><PrintSpace>']
    for i, h in enumerate(heights):
        if i in blocks:
            out.append("<TextBlock>" if i == blocks[0] else "</TextBlock><TextBlock>")
        out.append(f'<TextLine ID="P1_TL{i + 1:05d}" HPOS="10" VPOS="{100 * i}" WIDTH="900" HEIGHT="{h}"><String CONTENT="x" HPOS="10" VPOS="0" WIDTH="5" HEIGHT="5"/></TextLine>')
    out.append("</TextBlock></PrintSpace></Page></Layout></alto>")
    return "".join(out).encode()


def make_tar(path, members):
    with tarfile.open(path, "w:bz2") as tf:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))


def png(width, height):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    raw = b"".join(b"\x00" + b"\x00" * width for _ in range(height))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def dump(rows, answers=(), excluded=()):
    out = []
    for tid, page, line_key, extra in rows:
        lang, set_name, _ = tid.split("/", 2)
        out.append({"table": "tasks", "row": {"id": tid, "lang": lang, "set_name": set_name, "n": 0, "outcome": None, "settled": 0, "confusable_display": 0, "held": 0,
                                             **extra, "body": json.dumps({"page": page, "line": line_key})}})
    out += [{"table": "answers", "row": {"task_id": t, "contributor": c, "choice": "a", "at": 0}} for t, c in answers]
    out += [{"table": "answers_excluded", "row": {"task_id": t, "contributor": c, "choice": "a", "at": 0}} for t, c in excluded]
    return "\n".join(json.dumps(x) for x in out) + "\n"


def doc(page, line, k=0):
    return f"{page.replace('/', '_')}_{line}~{k}"


class Pieces(unittest.TestCase):
    def test_line_refs(self):
        self.assertEqual(mll.line_ref(PAGE_CHR, "sn83020866_1828-05-28_ed-1_seq-3_P3_TL00018"), ("id", "P3_TL00018"))
        self.assertEqual(mll.line_ref(PAGE_YI, "sn83045837_1913-09-01_ed-1_seq-3_l12"), ("index", 12))
        self.assertIsNone(mll.line_ref(PAGE_YI, "other_l1"))
        self.assertEqual(mll.split_task_id("chr/phoenix-nt-v1/sn83020866_1828-05-28_ed-1_seq-3_P3_TL00018~0"),
                         ("chr", "phoenix-nt-v1", "sn83020866_1828-05-28_ed-1_seq-3_P3_TL00018", "0"))

    def test_member_names(self):
        self.assertEqual(mll.archive_member(PAGE_CHR), "sn83020866/1828/05/28/ed-1/seq-3/ocr.xml")

    def test_image_sizes(self):
        self.assertEqual(mll.image_size(png(37, 11)), (37, 11))
        jpg = b"\xff\xd8" + b"\xff\xe0" + struct.pack(">H", 16) + b"JFIF\x00" + b"\x00" * 9 + b"\xff\xc0" + struct.pack(">HBHH", 17, 8, 55, 99) + b"\x00" * 10
        self.assertEqual(mll.image_size(jpg), (99, 55))
        self.assertIsNone(mll.image_size(b"nope"))

    def test_index_order_is_yiddish_lines_order(self):
        data = alto([60, 60, 150, 60, 60], blocks=(0, 2))
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "a.xml")
            open(p, "wb").write(data)
            _, _, lines = yl.alto_lines(p)
        self.assertEqual(mll.alto_heights(data)["indexed"], [ln["box"][3] for ln in lines])
        self.assertEqual(len(mll.alto_heights(data)["all"]), 5)


class Run(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.d = self.tmp.name
        self.arch = os.path.join(self.d, "arch")
        os.makedirs(self.arch)
        # Cherokee page: six lines, the third covers two printed lines. A second page in the same archive has only four lines.
        make_tar(os.path.join(self.arch, "dlc_misctopsn83020866_ver03.tar.bz2"), {
            "sn83020866/1828/05/28/ed-1/seq-3/ocr.txt": b"text",
            "sn83020866/1828/05/28/ed-1/seq-3/ocr.xml": alto([160, 160, 400, 160, 160, 160]),
            "sn83020866/1828/06/04/ed-1/seq-1/ocr.xml": alto([160, 160, 400, 160]),
        })
        # Yiddish page: eight lines in blocks; line 5 is tall. The member has a leading ./ as some archives do.
        make_tar(os.path.join(self.arch, "nyu_test_ver01.tar.bz2"), {
            "./sn83045837/1913/09/01/ed-1/seq-3/ocr.xml": alto([60, 60, 60, 60, 60, 150, 60, 60], blocks=(0, 4)),
        })
        with open(os.path.join(self.d, "pages.jsonl"), "w") as f:
            f.write(json.dumps({"page": PAGE_CHR, "batch": "dlc_misctopsn83020866", "batch_version": 3}) + "\n")
            f.write(json.dumps({"page": PAGE_SHORT, "batch": "dlc_misctopsn83020866", "batch_version": 3}) + "\n")
        with open(os.path.join(self.d, "map.tsv"), "w") as f:
            f.write("doc_id\tarchive\n" + PAGE_YI.replace("/", "_") + "\tnyu_test_ver01.tar.bz2\n")

    def tearDown(self):
        self.tmp.cleanup()

    def rows(self):
        return [
            (f"chr/phoenix-nt-v1/{doc(PAGE_CHR, 'P1_TL00003')}", PAGE_CHR, "chr/phoenix-nt-v1/lines/a.jpg", {"settled": 1, "outcome": "a", "n": 3}),
            (f"chr/phoenix-nt-v1/{doc(PAGE_CHR, 'P1_TL00001')}", PAGE_CHR, "chr/phoenix-nt-v1/lines/b.jpg", {}),
            (f"chr-lines/disputes-v1/{doc(PAGE_SHORT, 'P1_TL00003')}", PAGE_SHORT, "chr-lines/disputes-v1/lines/c.jpg", {}),  # tall, but the page has four lines
            (f"yi/loc-yidj2-v1/{doc(PAGE_YI, 'l5')}", PAGE_YI, "yi/loc-yidj2-v1/lines/d.png", {"held": 1, "confusable_display": 1}),
            (f"yi/loc-yidj2-v1/{doc(PAGE_YI, 'l5', 1)}", PAGE_YI, "yi/loc-yidj2-v1/lines/d.png", {}),
            (f"yi/loc-yidj2-v1/{doc(PAGE_YI, 'l2')}", PAGE_YI, "yi/loc-yidj2-v1/lines/e.png", {}),
            (f"sr/loc-boinc-tess-v1/{doc(PAGE_LOST, 'l0')}", PAGE_LOST, "sr/loc-boinc-tess-v1/lines/f.png", {}),  # no archive: clips
            (f"sr/loc-boinc-tess-v1/{doc(PAGE_LOST, 'l1')}", PAGE_LOST, "sr/loc-boinc-tess-v1/lines/g.png", {}),
            (f"sr/loc-boinc-tess-v1/{doc('sn1/1900-01-02/ed-1/seq-1', 'l1')}", "sn1/1900-01-02/ed-1/seq-1", "sr/loc-boinc-tess-v1/lines/zz.png", {}),  # no archive, no clip
        ]

    def run_tool(self, extra=()):
        text = dump(self.rows(), answers=[(self.rows()[0][0], "c1"), (self.rows()[0][0], "c2"), (self.rows()[0][0], "c3"), (self.rows()[1][0], "c1")],
                    excluded=[(self.rows()[3][0], "c1"), (self.rows()[3][0], "c2")])
        dump_path = os.path.join(self.d, "dump.jsonl")
        open(dump_path, "w").write(text)
        out = os.path.join(self.d, "out")
        clips = os.path.join(self.d, "clips")
        for key, h in (("f", 100), ("g", 330)):  # the fallback page has two clips: too few lines for a median
            os.makedirs(os.path.join(clips, "sr/loc-boinc-tess-v1/lines"), exist_ok=True)
            open(os.path.join(clips, f"sr/loc-boinc-tess-v1/lines/{key}.png"), "wb").write(png(20, h))
        args = ["--dump", dump_path, "--archives", self.arch, "--pages", os.path.join(self.d, "pages.jsonl"), "--archive-map", os.path.join(self.d, "map.tsv"),
                "--workers", "1", "--clips", clips, "--out-dir", out, *extra]
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(mll.main(args), 0)
        return out

    def test_alto_measure_flags_the_merged_lines_and_counts_what_is_on_them(self):
        out = self.run_tool()
        ids = [l for l in open(os.path.join(out, "affected-task-ids.txt"), encoding="utf-8") if not l.startswith("#")]
        self.assertEqual([i.strip() for i in ids], sorted([self.rows()[0][0], self.rows()[3][0], self.rows()[4][0]]))
        rep = json.load(open(os.path.join(out, "report.json")))
        chr_set = rep["sets"]["chr/phoenix-nt-v1"]
        self.assertEqual((chr_set["tasks"], chr_set["measured_alto"], chr_set["tasks_flagged"], chr_set["lines_flagged"], chr_set["answers"], chr_set["settled"], chr_set["settled_a"]),
                         (2, 2, 1, 1, 3, 1, 1))
        self.assertNotIn("tasks_flagged", rep["sets"]["chr-lines/disputes-v1"])  # four lines on the page: never flagged
        self.assertEqual(rep["sets"]["chr-lines/disputes-v1"]["page_too_few_lines"], 1)
        yi = rep["sets"]["yi/loc-yidj2-v1"]
        self.assertEqual((yi["tasks_flagged"], yi["lines_flagged"], yi["answers_set_aside"], yi["already_marked"], yi["measured_alto"]), (2, 1, 2, 1, 3))
        sr = rep["sets"]["sr/loc-boinc-tess-v1"]
        self.assertEqual((sr["measured_clip"], sr["measured_unmeasured"], sr.get("tasks_flagged", 0), sr["page_too_few_lines"]), (2, 1, 0, 2))
        un = open(os.path.join(out, "unmeasured-tasks.txt"), encoding="utf-8").read()
        self.assertEqual(len(un.splitlines()), 1)
        self.assertIn("no line clip", un)
        self.assertEqual(len(rep["pages_without_archive"]), 2)
        lines = [json.loads(x) for x in open(os.path.join(out, "flagged-lines.jsonl"), encoding="utf-8")]
        self.assertEqual(sorted((x["set"], x["measured"], x["height"], x["median"]) for x in lines), [("chr/phoenix-nt-v1", "alto", 400, 160.0), ("yi/loc-yidj2-v1", "alto", 150, 60.0)])
        self.assertEqual(len(lines[0]["tasks"]) if lines[0]["set"].startswith("chr") else len(lines[1]["tasks"]), 1)

    def test_a_looser_factor_flags_less_and_a_smaller_page_minimum_flags_the_short_page(self):
        out = self.run_tool(["--factor", "3"])
        self.assertEqual([l for l in open(os.path.join(out, "affected-task-ids.txt")) if not l.startswith("#")], [])
        out = self.run_tool(["--min-page-lines", "4"])
        flagged = [l.strip() for l in open(os.path.join(out, "affected-task-ids.txt")) if not l.startswith("#")]
        self.assertIn(self.rows()[2][0], flagged)

    def test_clip_fallback_flags_a_tall_clip_when_the_page_has_enough_lines(self):
        pages = {"p/1900-01-01/ed-1/seq-1": [100, 100, 100, 100, 100, 330]}
        clips_dir = os.path.join(self.d, "c2")
        os.makedirs(clips_dir)
        rows = []
        for i, h in enumerate(pages["p/1900-01-01/ed-1/seq-1"]):
            open(os.path.join(clips_dir, f"{i}.png"), "wb").write(png(20, h))
            rows.append((f"sr/s/p_1900-01-01_ed-1_seq-1_l{i}~0", "p/1900-01-01/ed-1/seq-1", f"{i}.png", {}))
        tasks, _, _ = mll.read_dump(dump(rows), ("sr",))
        res = mll.measure(tasks, {}, mll.Clips(clips_dir, None, 0), 1.6, 5)
        self.assertEqual([t for t, r in res.items() if r["flagged"]], ["sr/s/p_1900-01-01_ed-1_seq-1_l5~0"])
        self.assertEqual({r["how"] for r in res.values()}, {"clip"})

    def test_missing_archive_or_page_falls_back_and_nothing_is_fetched(self):
        jobs, why, prefs = mll.plan_archives({PAGE_CHR, PAGE_YI, PAGE_LOST}, self.arch, {PAGE_CHR: ["gone_ver01.tar.bz2"]}, {PAGE_YI.replace("/", "_"): ["nyu_test_ver01.tar.bz2"]})
        self.assertEqual(jobs, {"nyu_test_ver01.tar.bz2": {mll.archive_member(PAGE_YI)}})
        self.assertEqual(prefs, {mll.archive_member(PAGE_YI): ["nyu_test_ver01.tar.bz2"]})
        self.assertIn("archive not in --archives", why[PAGE_CHR])
        self.assertEqual(why[PAGE_LOST], "no archive listed for the page")

    def test_a_page_in_two_archives_takes_the_preferred_one(self):
        # ver03 is listed first; ver01 sorts first and also holds the page, with other heights
        make_tar(os.path.join(self.arch, "dlc_misctopsn83020866_ver01.tar.bz2"), {"sn83020866/1828/05/28/ed-1/seq-3/ocr.xml": alto([160] * 6)})
        jobs, why, prefs = mll.plan_archives({PAGE_CHR}, self.arch, {PAGE_CHR: ["dlc_misctopsn83020866_ver03.tar.bz2", "dlc_misctopsn83020866_ver01.tar.bz2"]}, {})
        got = mll.read_alto(jobs, prefs, self.arch, 1)
        self.assertEqual([h for _, h in got[mll.archive_member(PAGE_CHR)]["all"]], [160, 160, 400, 160, 160, 160])

    def test_one_source_line_loaded_in_two_sets_counts_once_for_the_median_and_once_per_set(self):
        clips = os.path.join(self.d, "c3")
        os.makedirs(clips)
        for name, h in (("a", 100), ("b", 100), ("c", 100), ("d", 100), ("e", 330)):
            with open(os.path.join(clips, name + ".png"), "wb") as f:
                f.write(png(20, h))
        page = "p/1900-01-01/ed-1/seq-1"
        stem = "p_1900-01-01_ed-1_seq-1_"
        rows = [(f"sr/{st}/{stem}l{i}~0", page, f"{st}/{n}.png", {}) for st in ("one", "two") for i, n in enumerate("abcde")]
        # keys differ by set, so the clip files are looked up under the set; give both sets the same files
        for st in ("one", "two"):
            os.makedirs(os.path.join(clips, st))
            for n in "abcde":
                os.link(os.path.join(clips, n + ".png"), os.path.join(clips, st, n + ".png"))
        tasks, _, _ = mll.read_dump(dump(rows), ("sr",))
        res = mll.measure(tasks, {}, mll.Clips(clips, None, 0), 1.6, 5)
        self.assertEqual([r["flagged"] for r in res.values()].count(True), 2)  # five distinct lines: checked, the tall one flagged in each set
        rep = mll.build_report(tasks, res, collections.Counter(), collections.Counter(), 1.6, 5, {}, 0)
        self.assertEqual([rep["sets"][f"sr/{s}"]["lines_flagged"] for s in ("one", "two")], [1, 1])
        short = [r for r in rows if r[0].endswith("l4~0") or r[0].endswith("l0~0")]
        tasks, _, _ = mll.read_dump(dump(short), ("sr",))
        res = mll.measure(tasks, {}, mll.Clips(clips, None, 0), 1.6, 5)
        self.assertFalse(any(r["flagged"] for r in res.values()), "two lines in two sets are not five lines")

    def test_fallback_median_is_per_set_and_counts_lines_the_alto_measured(self):
        clips = os.path.join(self.d, "c4")
        page = "p/1900-01-01/ed-1/seq-1"
        stem = "p_1900-01-01_ed-1_seq-1_"
        rows = []
        for st, scale in (("small", 100), ("big", 200)):  # the same five lines scanned at two scales
            os.makedirs(os.path.join(clips, st))
            for i in range(5):
                with open(os.path.join(clips, st, f"{i}.png"), "wb") as f:
                    f.write(png(20, scale))
                rows.append((f"sr/{st}/{stem}l{i}~0", page, f"{st}/{i}.png", {}))
        tasks, _, _ = mll.read_dump(dump(rows), ("sr",))
        res = mll.measure(tasks, {}, mll.Clips(clips, None, 0), 1.6, 5)
        self.assertFalse(any(r["flagged"] for r in res.values()), "a median over both scales would flag all of the big set")
        # one tall line the ALTO does not have, among five normal lines the ALTO does measure: the clips of all six make the median
        os.makedirs(os.path.join(clips, "one"))
        rows = []
        for i, h in enumerate([100, 100, 100, 100, 100, 300]):
            with open(os.path.join(clips, "one", f"{i}.png"), "wb") as f:
                f.write(png(20, h))
            rows.append((f"sr/one/{stem}P1_TL0000{i + 1}~0", page, f"one/{i}.png", {}))
        tasks, _, _ = mll.read_dump(dump(rows), ("sr",))
        alto = {mll.archive_member(page): {"all": [(f"P1_TL0000{i + 1}", 50) for i in range(5)], "indexed": []}}  # TL00006 is not in the ALTO
        res = mll.measure(tasks, alto, mll.Clips(clips, None, 0), 1.6, 5)
        self.assertEqual([t for t, r in res.items() if r["flagged"]], [rows[5][0]])
        self.assertEqual([r["how"] for r in res.values()], ["alto"] * 5 + ["clip"])

    def test_an_unreadable_page_does_not_stop_the_archive(self):
        path = os.path.join(self.d, "bad.tar.bz2")
        make_tar(path, {"sn1/1900/01/01/ed-1/seq-1/ocr.xml": b"<alto><unclosed>", "sn1/1900/01/01/ed-1/seq-2/ocr.xml": alto([60] * 5)})
        found = mll.scan_archive((path, ("sn1/1900/01/01/ed-1/seq-1/ocr.xml", "sn1/1900/01/01/ed-1/seq-2/ocr.xml")))
        self.assertEqual(list(found), ["sn1/1900/01/01/ed-1/seq-2/ocr.xml"])

    def test_clips_url_fetches_gently_and_keeps_the_clip(self):
        served = []

        class H(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                served.append(self.path)
                if self.path == "/clips/a/b.png":
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(png(5, 42))
                else:
                    self.send_response(404)
                    self.end_headers()

            def log_message(self, *a):
                pass

        srv = http.server.HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        try:
            cache = os.path.join(self.d, "cache")
            clips = mll.Clips(cache, f"http://127.0.0.1:{srv.server_port}", 0)
            self.assertEqual(clips.height("a/b.png"), 42)
            self.assertEqual(clips.height("a/b.png"), 42)  # from the saved copy
            self.assertIsNone(clips.height("a/missing.png"))
            self.assertIsNone(clips.height("../x"))
            self.assertEqual(served, ["/clips/a/b.png", "/clips/a/missing.png"])
        finally:
            srv.shutdown()


if __name__ == "__main__":
    unittest.main()
