"""Tests for recompute_marks.py: python3 -m unittest discover -s edge/check/tools. They need Pillow with Raqm."""
import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(__file__))

try:
    from PIL import features
    import lookalike as la
    import recompute_marks as rm
    HAVE_RAQM = bool(features.check("raqm"))
except ImportError:
    HAVE_RAQM = False

HERE = os.path.dirname(os.path.abspath(__file__))
AL, PATAH, QAMATS = "א", "ַ", "ָ"


def body(tid, a, b, **extra):
    """A task as /admin/tasks takes it, keys in the order build_tasks.py writes them."""
    return {"id": tid, "lang": "yi", "set": "s", "page": "p", "source": "u", "label": "l", "line": "x.jpg", "word": "w.jpg",
            "box": [0.1, 0.2, 0.3, 0.4], "a": a, "b": b, "a_diff": [0], "b_diff": [0], "a_is": "loc", "b_is": "yidj2", **extra}


def mark_task(tid="yi/s/1~0", **extra):
    return body(tid, AL + PATAH, AL + QAMATS, **extra)


def plain_task(tid="yi/s/2~0"):
    return body(tid, "ר", "ד")


def context(**kw):
    return rm.Context(la.Fonts(), os.path.join(HERE, "..", "languages"), la.THRESHOLD, [], kw.get("allow_unmeasured", True))  # a CI machine may have no Hebrew reference font


def body_text(r):
    return json.dumps({k: v for k, v in r.items() if k not in ("id", "lang", "set")}, ensure_ascii=False)


def dump_text(records, table="tasks"):
    return "\n".join(json.dumps({"table": table, "row": {"id": r["id"], "lang": r["lang"], "set_name": r["set"], "body": body_text(r)}}, ensure_ascii=False)
                     for r in records)


@unittest.skipUnless(HAVE_RAQM, "Pillow with Raqm not installed")
class Recompute(unittest.TestCase):
    def test_a_body_with_no_pair_to_flag_is_unchanged_and_writes_nothing(self):
        t = plain_task()
        changed, rep = rm.recompute_language("yi", [t], context(), True)
        self.assertEqual(changed, [])
        self.assertEqual((rep["totals"]["tasks_read"], rep["totals"]["unchanged"], rep["totals"]["changed"]), (1, 1, 0))
        # the recomputed body is the old one, byte for byte
        self.assertEqual(rm.canonical(rm.new_body(t, [], [])), rm.canonical(t))

    def test_a_mark_pair_gains_mark_only_after_the_other_keys_and_nothing_else_changes(self):
        t = mark_task()
        changed, rep = rm.recompute_language("yi", [t], context(), True)
        (new,) = changed
        self.assertEqual(new["id"], t["id"])
        self.assertEqual(list(new)[:-1], list(t))
        self.assertEqual(list(new)[-1], "mark_only")
        self.assertEqual(new["mark_only"], [{"a": 0, "a_marks": PATAH, "b": 0, "b_marks": QAMATS, "base": AL, "differ": PATAH + QAMATS}])
        self.assertEqual(rm.check_only_fields_differ(t, new), "")
        self.assertEqual({k: v for k, v in new.items() if k != "mark_only"}, t)
        self.assertEqual(rep["totals"]["changed_by_field"], {"lookalikes": 0, "mark_only": 1})
        self.assertEqual(rep["totals"]["changed_by_kind"], {"mark_only added": 1})
        self.assertEqual(rep["totals"]["changed_by_mark"], {"U+05B7": 1, "U+05B8": 1})
        self.assertEqual(rep["tasks"], [{"id": t["id"], "changed": ["mark_only"]}])
        # running it again on the new body changes nothing
        again, rep2 = rm.recompute_language("yi", changed, context(), True)
        self.assertEqual((again, rep2["totals"]["unchanged"]), ([], 1))

    def test_a_field_already_there_is_replaced_in_place_and_a_stale_one_is_removed(self):
        stale = [{"a": 0, "a_label": "x", "b": 0, "b_label": "y", "iou": 0.9}]
        t = mark_task(lookalikes=stale, a_note="after")  # lookalikes sits before another key
        (new,), rep = rm.recompute_language("yi", [t], context(), True)
        self.assertEqual(list(new), [k for k in t if k != "lookalikes"] + ["mark_only"])
        self.assertNotIn("lookalikes", new)
        self.assertEqual(rep["totals"]["changed_in_both_fields"], 1)
        self.assertEqual(rep["totals"]["changed_by_kind"], {"lookalikes removed": 1, "mark_only added": 1})
        t2 = mark_task(mark_only=[{"a": 0}], z="last")
        (new2,), _ = rm.recompute_language("yi", [t2], context(), True)
        self.assertEqual(list(new2), list(t2))  # replaced where it was
        self.assertEqual(new2["mark_only"][0]["base"], AL)

    def test_tampering_with_another_field_fails_the_check(self):
        t = mark_task()
        (new,), _ = rm.recompute_language("yi", [t], context(), True)
        for key, value in (("a", "x"), ("box", [0, 0, 0, 0]), ("page", "q"), ("id", "yi/s/other")):
            self.assertNotEqual(rm.check_only_fields_differ(t, {**new, key: value}), "", key)
        reordered = {k: new[k] for k in reversed(list(new))}
        self.assertNotEqual(rm.check_only_fields_differ(t, reordered), "")
        self.assertNotEqual(rm.check_only_fields_differ(t, {k: v for k, v in new.items() if k != "page"}), "")
        with mock.patch.object(rm, "new_body", lambda old, alike, marks: {**old, "page": "tampered"}):
            with self.assertRaises(rm.Refused):
                rm.recompute_language("yi", [mark_task()], context(), True)

    def test_ids_are_kept_and_unchanged_tasks_are_not_written(self):
        recs = [mark_task("yi/s/1~0"), plain_task("yi/s/2~0"), mark_task("yi/s/3~1")]
        changed, rep = rm.recompute_language("yi", recs, context(), True)
        self.assertEqual([c["id"] for c in changed], ["yi/s/1~0", "yi/s/3~1"])
        self.assertEqual([(c["lang"], c["set"]) for c in changed], [("yi", "s")] * 2)
        self.assertEqual((rep["totals"]["tasks_read"], rep["totals"]["unchanged"], rep["totals"]["changed"]), (3, 1, 2))

    def test_without_raqm_a_language_whose_readings_have_marks_is_refused_not_skipped(self):
        ctx = context()
        ctx.fonts.raqm = False
        with self.assertRaises(rm.Refused) as e:
            rm.recompute_language("yi", [mark_task()], ctx, True)
        self.assertIn("Raqm", str(e.exception))
        # readings with no combining mark still work
        self.assertEqual(rm.recompute_language("yi", [plain_task()], ctx, True)[0], [])

    def test_unmeasured_pairs_fail_unless_allowed(self):
        t = body("yi/s/9~0", "ר", "ד")
        ctx = context(allow_unmeasured=False)
        ctx.fonts.overrides["HEBREW"] = os.path.join(HERE, "no-such-font.ttf")
        ctx.fonts.paths["HEBREW"] = None  # no font for the script
        with self.assertRaises(rm.Refused):
            rm.recompute_language("yi", [t], ctx, True)
        ctx2 = context(allow_unmeasured=True)
        ctx2.fonts.paths["HEBREW"] = None
        _, rep = rm.recompute_language("yi", [t], ctx2, True)
        self.assertEqual(rep["unmeasured_pairs"], {"HEBREW": 1})

    def test_cherokee_uses_the_bundled_font(self):
        t = {"id": "chr/s/1~0", "lang": "chr", "set": "s", "a": "ᏒᎯ", "b": "ᎡᎯ", "a_is": "x", "b_is": "y"}
        ctx = rm.Context(la.Fonts(), os.path.join(HERE, "..", "languages"), la.THRESHOLD, [], False)
        try:
            rm.recompute_language("chr", [t], ctx, True)
        except OSError as e:  # a FreeType that can't read woff2 and no fonttools
            self.skipTest(str(e))
        self.assertEqual(ctx.fonts.paths["CHEROKEE"], la.BUNDLED["CHEROKEE"])

    def test_a_language_with_no_configuration_is_refused(self):
        with self.assertRaises(rm.Refused):
            rm.recompute_language("xx", [dict(plain_task(), lang="xx")], context(), True)

    def test_an_export_row_is_previewed_against_the_exported_fields(self):
        row = {"id": "yi/s/1~0", "lang": "yi", "set": "s", "a": AL + PATAH, "b": AL + QAMATS}
        changed, rep = rm.recompute_language("yi", [row], context(), False)
        self.assertEqual((rep["preview"], rep["input_sha256"], rep["tasks_sha256"], rep["totals"]["changed"]), (True, None, None, 1))
        self.assertEqual(rep["totals"]["changed_by_field"]["mark_only"], 1)


@unittest.skipUnless(HAVE_RAQM, "Pillow with Raqm not installed")
class Commands(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.recs = [mark_task("yi/s/1~0"), plain_task("yi/s/2~0")]
        self.bodies = os.path.join(self.tmp.name, "bodies.jsonl")
        with open(self.bodies, "w", encoding="utf-8") as f:
            f.writelines(rm.canonical(r) + "\n" for r in self.recs)
        self.out = os.path.join(self.tmp.name, "out")

    def run_tool(self, *args):
        return subprocess.run([sys.executable, os.path.join(HERE, "recompute_marks.py"), "--allow-unmeasured", *args], capture_output=True, text=True)

    def test_dry_run_writes_bodies_and_a_passing_report(self):
        r = self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out)
        self.assertEqual(r.returncode, 0, r.stderr)
        rep = json.load(open(os.path.join(self.out, "yi", "report.json"), encoding="utf-8"))
        self.assertTrue(rep["passed"])
        self.assertEqual(rep["totals"]["changed"], 1)
        lines = open(os.path.join(self.out, "yi", "tasks.jsonl"), encoding="utf-8").read().splitlines()
        self.assertEqual([json.loads(l)["id"] for l in lines], ["yi/s/1~0"])
        self.assertTrue(os.path.exists(os.path.join(self.out, "yi", "report.txt")))
        # the same bodies from a dump give the same input hash
        dump = os.path.join(self.tmp.name, "dump.jsonl")
        open(dump, "w", encoding="utf-8").write(dump_text(self.recs) + "\n" + json.dumps({"table": "answers", "row": {"task_id": "x"}}) + "\n")
        out2 = os.path.join(self.tmp.name, "out2")
        self.assertEqual(self.run_tool("--lang", "yi", "--dump", dump, "--out-dir", out2).returncode, 0)
        self.assertEqual(json.load(open(os.path.join(out2, "yi", "report.json")))["input_sha256"], rep["input_sha256"])

    def test_whole_number_floats_in_offline_bodies_match_what_the_site_stores(self):
        # build_tasks.py writes 1.0 and 0.0 in `box`; the site stores 1 and 0
        t = mark_task()
        t["box"] = [0.0, 0.2, 1.0, 0.4]
        site_form = dict(t, box=[0, 0.2, 1, 0.4])
        self.assertEqual(rm.canonical(t), rm.canonical(site_form))
        self.assertEqual(rm.input_hash([t]), rm.input_hash([site_form]))
        (new,), _ = rm.recompute_language("yi", [t], context(), True)
        self.assertEqual(rm.check_only_fields_differ(site_form, new), "")
        self.assertIn('"box":[0,0.2,1,0.4]', rm.canonical(new))

    def test_a_refusal_before_the_dry_run_also_removes_an_older_passing_report(self):
        self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 0)
        open(self.bodies, "w", encoding="utf-8").write(json.dumps({"id": "yi/s/1~0", "lang": "yi"}) + "\n")  # no `set`
        self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 1)
        self.assertFalse(os.path.exists(os.path.join(self.out, "yi", "tasks.jsonl")))
        self.assertFalse(json.load(open(os.path.join(self.out, "yi", "report.json")))["passed"])

    def test_cherokee_font_override_and_missing_source_are_refused(self):
        self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out, "--font", "Cherokee=x.ttf").returncode, 1)
        self.assertEqual(self.run_tool("--lang", "yi", "--out-dir", self.out).returncode, 1)
        self.assertEqual(self.run_tool("--lang", "sr", "--bodies", self.bodies, "--out-dir", self.out).returncode, 1)  # no sr tasks

    def test_a_refused_run_removes_an_older_passing_report(self):
        self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 0)
        bad = dict(self.recs[0])
        bad.pop("a")
        open(self.bodies, "w", encoding="utf-8").write(rm.canonical(bad) + "\n")
        self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 1)
        self.assertFalse(os.path.exists(os.path.join(self.out, "yi", "tasks.jsonl")))
        self.assertFalse(json.load(open(os.path.join(self.out, "yi", "report.json")))["passed"])

    def assert_stale_output_is_gone(self, r):
        self.assertEqual(r.returncode, 1, r.stderr)
        self.assertFalse(os.path.exists(os.path.join(self.out, "yi", "tasks.jsonl")))
        self.assertFalse(os.path.exists(os.path.join(self.out, "yi", "report.txt")))
        self.assertFalse(json.load(open(os.path.join(self.out, "yi", "report.json")))["passed"])

    def test_an_input_that_cannot_be_read_removes_an_older_passing_report_and_bodies(self):
        # each failure of the input, none of them a refusal the tool raises itself: a failed dry run never leaves loadable output
        directory = os.path.join(self.tmp.name, "a-directory")
        os.mkdir(directory)
        for name, content in (("malformed JSON", "{not json\n"), ("a line that is not an object", "[]\n"), ("a bad dump row", None)):
            with self.subTest(name):
                self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 0)
                self.assertTrue(os.path.exists(os.path.join(self.out, "yi", "tasks.jsonl")))
                if content is None:  # --dump with a task row that has no body
                    path = os.path.join(self.tmp.name, "bad-dump.jsonl")
                    open(path, "w", encoding="utf-8").write(json.dumps({"table": "tasks", "row": {"id": "yi/s/1~0"}}) + "\n")
                    r = self.run_tool("--lang", "yi", "--dump", path, "--out-dir", self.out)
                else:
                    path = os.path.join(self.tmp.name, "bad.jsonl")
                    open(path, "w", encoding="utf-8").write(content)
                    r = self.run_tool("--lang", "yi", "--bodies", path, "--out-dir", self.out)
                self.assert_stale_output_is_gone(r)
        for name, args in (("a missing file", ["--bodies", os.path.join(self.tmp.name, "nope.jsonl")]),
                           ("an unreadable file", ["--bodies", directory]),
                           ("a missing export", ["--export", os.path.join(self.tmp.name, "nope.jsonl")]),
                           ("a malformed export", ["--export", self.bodies + ".export"])):
            with self.subTest(name):
                open(self.bodies + ".export", "w", encoding="utf-8").write("{oops\n")
                self.assertEqual(self.run_tool("--lang", "yi", "--bodies", self.bodies, "--out-dir", self.out).returncode, 0)
                self.assert_stale_output_is_gone(self.run_tool("--lang", "yi", *args, "--out-dir", self.out))

    def test_an_unexpected_error_in_one_language_invalidates_that_language_only(self):
        for lang in ("yi", "sr"):
            d = os.path.join(self.out, lang)
            os.makedirs(d)
            for n in ("tasks.jsonl", "report.json", "report.txt"):
                open(os.path.join(d, n), "w").write("{\"passed\": true}" if n == "report.json" else "x")
        def boom(lang, recs, ctx, full):
            if lang == "yi":
                raise KeyError("oops")
            return [], {"passed": True, "preview": False, "lang": lang, "tasks_sha256": None, "input_sha256": None,
                        "totals": {"tasks_read": 1, "unchanged": 1, "changed": 0, "changed_by_field": {"lookalikes": 0, "mark_only": 0},
                                   "changed_in_both_fields": 0, "changed_by_kind": {}, "changed_by_mark": {}, "tasks_with_lookalikes_after": 0,
                                   "tasks_with_mark_only_after": 0}, "unmeasured_pairs": {}, "letter_diff_mismatch": [], "fonts": {}, "threshold": 0, "raqm": True}
        args = mock.Mock(lang=["yi", "sr"], out_dir=self.out)
        recs = [dict(plain_task(), lang="yi"), dict(plain_task("sr/s/1~0"), lang="sr")]
        with mock.patch.object(rm, "recompute_language", boom), mock.patch("sys.stderr"):
            self.assertEqual(rm.dry_run(args, context(), recs, True), 1)
        self.assertFalse(os.path.exists(os.path.join(self.out, "yi", "tasks.jsonl")))
        self.assertFalse(json.load(open(os.path.join(self.out, "yi", "report.json")))["passed"])
        self.assertTrue(json.load(open(os.path.join(self.out, "sr", "report.json")))["passed"])


class FakeSite(http.server.BaseHTTPRequestHandler):
    records = []
    posted = []

    def log_message(self, *a):
        pass

    def do_GET(self):
        assert self.headers["authorization"] == "Bearer tok"
        data = (dump_text(FakeSite.records) + "\n").encode()
        self.send_response(200)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        assert self.path == "/admin/tasks" and self.headers["authorization"] == "Bearer tok"
        lines = self.rfile.read(int(self.headers["content-length"])).decode().splitlines()
        stale = []
        for l in lines:  # what /admin/tasks does with if_body: the body is replaced only while it is the text the sender read
            t = json.loads(l)
            expected = t.pop("if_body")
            for i, r in enumerate(FakeSite.records):
                if r["id"] == t["id"]:
                    if body_text(r) == expected:
                        FakeSite.records[i] = t
                    else:
                        stale.append(t["id"])
        FakeSite.posted.append(len(lines))
        self.send_response(200)
        self.end_headers()
        self.wfile.write(json.dumps({"ok": True, "tasks": len(lines) - len(stale), "stale": stale}).encode())


@unittest.skipUnless(HAVE_RAQM, "Pillow with Raqm not installed")
class Load(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        FakeSite.records = [mark_task("yi/s/1~0"), plain_task("yi/s/2~0")]
        FakeSite.posted = []
        self.server = http.server.HTTPServer(("127.0.0.1", 0), FakeSite)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.shutdown)
        self.addCleanup(self.server.server_close)
        self.site = rm.Site(f"http://127.0.0.1:{self.server.server_port}", "tok")
        self.args = mock.Mock(lang=["yi"], out_dir=os.path.join(self.tmp.name, "out"))
        recs = self.site.dump_records()
        changed, rep = rm.recompute_language("yi", recs, context(), True)
        rm.write_language(self.args.out_dir, "yi", changed, rep)
        self.tasks_path = os.path.join(self.args.out_dir, "yi", "tasks.jsonl")

    def test_load_sends_the_changed_bodies_and_reads_them_back(self):
        self.assertEqual(rm.load(self.args, self.site), 0)
        self.assertEqual(FakeSite.posted, [1])
        self.assertIn("mark_only", FakeSite.records[0])
        self.assertEqual(rm.canonical(FakeSite.records[1]), rm.canonical(plain_task("yi/s/2~0")))

    def test_a_task_changed_after_the_read_is_not_overwritten(self):
        real = self.site.dump_records
        def read_then_edit(texts=None):
            out = real(texts)
            FakeSite.records[0] = dict(FakeSite.records[0], page="edited by another loader")  # after the dump, before the post
            return out
        with mock.patch.object(self.site, "dump_records", read_then_edit):
            self.assertEqual(rm.load(self.args, self.site), 1)
        self.assertEqual(FakeSite.records[0]["page"], "edited by another loader")
        self.assertNotIn("mark_only", FakeSite.records[0])

    def test_a_task_inserted_during_the_load_fails_the_read_back_check(self):
        real = self.site.post_tasks
        def post_then_insert(lines):
            out = real(lines)
            FakeSite.records.append(plain_task("yi/s/3~0"))  # another loader's insert, after the first snapshot
            return out
        with mock.patch.object(self.site, "post_tasks", post_then_insert), mock.patch("sys.stderr") as err, mock.patch("sys.stdout"):
            self.assertEqual(rm.load(self.args, self.site), 1)
        self.assertIn("yi/s/3~0", "".join(c.args[0] for c in err.write.call_args_list))

    def test_a_task_deleted_or_edited_during_the_load_still_fails_the_read_back_check(self):
        real = self.site.post_tasks
        def post_then_delete(lines):
            out = real(lines)
            del FakeSite.records[1]
            return out
        with mock.patch.object(self.site, "post_tasks", post_then_delete), mock.patch("sys.stderr"), mock.patch("sys.stdout"):
            self.assertEqual(rm.load(self.args, self.site), 1)

    def test_load_refuses_without_a_report_or_with_a_failed_one(self):
        os.remove(os.path.join(self.args.out_dir, "yi", "report.json"))
        with self.assertRaises(rm.Refused):
            rm.load(self.args, self.site)
        json.dump({"passed": False}, open(os.path.join(self.args.out_dir, "yi", "report.json"), "w"))
        with self.assertRaises(rm.Refused):
            rm.load(self.args, self.site)
        self.assertEqual(FakeSite.posted, [])

    def test_load_refuses_bodies_that_are_not_the_ones_the_report_passed(self):
        with open(self.tasks_path, "a", encoding="utf-8") as f:
            f.write("\n")
        with self.assertRaises(rm.Refused):
            rm.load(self.args, self.site)
        self.assertEqual(FakeSite.posted, [])

    def test_load_refuses_when_the_site_changed_since_the_dry_run(self):
        FakeSite.records[1] = dict(FakeSite.records[1], page="loaded since")
        with self.assertRaises(rm.Refused) as e:
            rm.load(self.args, self.site)
        self.assertIn("since", str(e.exception))
        self.assertEqual(FakeSite.posted, [])

    def test_load_refuses_a_preview_and_a_site_that_is_not_https(self):
        rep_path = os.path.join(self.args.out_dir, "yi", "report.json")
        rep = json.load(open(rep_path))
        rep["preview"] = True
        json.dump(rep, open(rep_path, "w"))
        with self.assertRaises(rm.Refused):
            rm.load(self.args, self.site)
        for bad in ("http://check.usnewsmap.com", "https://x/y", "ftp://x"):
            with self.assertRaises(rm.Refused):
                rm.Site(bad, "t")


if __name__ == "__main__":
    unittest.main()
