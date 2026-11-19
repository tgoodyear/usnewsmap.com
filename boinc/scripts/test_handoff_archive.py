"""Tests for handoff_archive.py (#1367): the per-job hand-off objects on the BOINC host, over a
scratch hand-off root."""

import hashlib
import importlib.util
import io
import json
import os
import tempfile
import time
import unittest
import unittest.mock
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("handoff_archive", Path(__file__).with_name("handoff_archive.py"))
H = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(H)

JOB_A = "a" * 64
JOB_B = "b" * 64
JOB_0 = "0" * 64


def call(root, *args, stdin=b""):
    out = io.BytesIO()
    code = H.main(["--root", str(root), *args], stdin=io.BytesIO(stdin), stdout=out)
    return code, out.getvalue()


class ArchiveTool(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name).resolve() / "root"
        self.root.mkdir(mode=0o700)
        self.put(f"dispatch/jobs/{JOB_A}/record.json", b'{"stage": "published"}')
        self.put(f"dispatch/jobs/{JOB_A}/accepted/result.json", b"{}")
        self.put(f"dispatch/jobs/{JOB_B}/record.json", b'{"stage": "failed"}')
        self.put(f"dispatch/quarantine/{JOB_A}/reason.json", b"{}")
        self.put(f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json", b'{"completed_at": "x"}')
        self.put(f"spool/{JOB_A}/host_id", b"13\n")
        self.put(f"spool/{JOB_0}/host_id", b"9\n")
        # Not per-job: never reachable.
        self.put("scheduler/inventory.json", b"{}")

    def tearDown(self):
        self.tmp.cleanup()

    def put(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)

    def names(self, *args):
        code, out = call(self.root, "list", *args)
        self.assertEqual(code, 0)
        return [entry["name"] for entry in json.loads(out)["entries"]]

    def test_list_finds_files_by_prefix_like_a_container_listing(self):
        self.assertEqual(self.names("--prefix", "dispatch/jobs/a"), [
            f"dispatch/jobs/{JOB_A}/accepted/result.json", f"dispatch/jobs/{JOB_A}/record.json"])
        self.assertEqual(self.names("--prefix", "dispatch/jobs/"), [
            f"dispatch/jobs/{JOB_A}/accepted/result.json", f"dispatch/jobs/{JOB_A}/record.json",
            f"dispatch/jobs/{JOB_B}/record.json"])
        self.assertEqual(self.names("--prefix", "dispatch/jobs/c"), [])
        self.assertEqual(self.names("--prefix", "dispatch/quarantine/0"), [])
        self.assertEqual(self.names("--prefix", f"spool/{JOB_A}/"), [
            f"spool/{JOB_A}/host_id", f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json"])
        self.assertEqual(self.names("--prefix", f"spool/{JOB_A}/usnm_"), [
            f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json"])

    def test_list_entries_carry_an_etag_size_and_http_date_and_no_md5(self):
        code, out = call(self.root, "list", "--prefix", f"spool/{JOB_0}/")
        entry = json.loads(out)["entries"][0]
        info = (self.root / entry["name"]).stat()
        self.assertEqual(entry["etag"], f"{info.st_dev:x}.{info.st_ino:x}-{info.st_size:x}-{info.st_mtime_ns:x}")
        self.assertEqual((entry["size"], entry["md5"]), (2, None))
        self.assertTrue(entry["modified"].endswith(" GMT"))

    def test_list_suffix_and_age_filters(self):
        self.assertEqual(self.names("--prefix", "spool/", "--suffix", "__file_result.json"), [
            f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json"])
        old = self.root / f"spool/{JOB_0}/host_id"
        os.utime(old, (time.time() - 5 * 86400,) * 2)
        self.assertEqual(self.names("--prefix", "spool/", "--suffix", "host_id", "--max-age-secs", "86400"), [
            f"spool/{JOB_A}/host_id"])

    def test_list_skips_a_writers_temp_files_and_refuses_links_and_odd_names(self):
        self.put(f"spool/{JOB_A}/.tmp-1-x-1", b"partial")
        self.assertNotIn(f"spool/{JOB_A}/.tmp-1-x-1", self.names("--prefix", "spool/"))
        (self.root / f"spool/{JOB_A}/link").symlink_to(self.root / "scheduler/inventory.json")
        code, _ = call(self.root, "list", "--prefix", f"spool/{JOB_A}/")
        self.assertEqual(code, 1)
        (self.root / f"spool/{JOB_A}/link").unlink()
        self.put(f"spool/{JOB_A}/bad name", b"x")
        self.assertEqual(call(self.root, "list", "--prefix", f"spool/{JOB_A}/")[0], 1)

    def test_only_the_per_job_prefixes_are_reachable(self):
        for prefix in ("scheduler/", "", "dispatch/", "dispatch/jobs/../../scheduler", "spool//", "claims/x"):
            self.assertEqual(call(self.root, "list", "--prefix", prefix)[0], 1, prefix)
        for name in ("scheduler/inventory.json", f"dispatch/jobs/{JOB_A}", "spool/short/x",
                     f"spool/{JOB_A}/../../scheduler/inventory.json", f"spool/{JOB_A}//x"):
            self.assertEqual(call(self.root, "read", "--object", name)[0], 1, name)
            self.assertEqual(call(self.root, "write", "--object", name, stdin=b"x")[0], 1, name)
            self.assertEqual(call(self.root, "delete", stdin=json.dumps(
                {"items": [{"name": name, "etag": "x"}]}).encode())[0], 1, name)
        self.assertTrue((self.root / "scheduler/inventory.json").exists())

    def test_read_gives_a_header_then_the_bytes_and_refuses_a_link(self):
        name = f"dispatch/jobs/{JOB_A}/record.json"
        code, out = call(self.root, "read", "--object", name)
        header, _, data = out.partition(b"\n")
        header = json.loads(header)
        self.assertEqual((code, data), (0, b'{"stage": "published"}'))
        self.assertEqual(header["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(header["size"], len(data))
        listed = json.loads(call(self.root, "list", "--prefix", name)[1])["entries"][0]
        self.assertEqual(header["etag"], listed["etag"])
        self.assertEqual(call(self.root, "read", "--object", f"spool/{JOB_B}/host_id")[0], 1)
        (self.root / f"spool/{JOB_B}").mkdir()
        (self.root / f"spool/{JOB_B}/host_id").symlink_to(self.root / "scheduler/inventory.json")
        self.assertEqual(call(self.root, "read", "--object", f"spool/{JOB_B}/host_id")[0], 1)

    def test_read_many_gives_a_frame_for_each_name_in_order(self):
        names = [f"spool/{JOB_A}/host_id", f"dispatch/jobs/{JOB_A}/record.json", f"spool/{JOB_0}/host_id"]
        code, out = call(self.root, "read-many", stdin=json.dumps({"names": names}).encode())
        self.assertEqual(code, 0)
        pos, frames = 0, []
        for name in names:
            newline = out.index(b"\n", pos)
            header = json.loads(out[pos:newline])
            data = out[newline + 1:newline + 1 + header["size"]]
            self.assertEqual((header["name"], header["sha256"]), (name, hashlib.sha256(data).hexdigest()))
            frames.append(data)
            pos = newline + 1 + header["size"]
        self.assertEqual((pos, frames), (len(out), [b"13\n", b'{"stage": "published"}', b"9\n"]))

    def test_read_many_refuses_what_read_refuses_and_is_bounded(self):
        for names in ([], [f"spool/{JOB_B}/host_id"], ["scheduler/inventory.json"], [f"spool/{JOB_A}/host_id", 5],
                      [f"spool/{JOB_A}/host_id"] * 1001):
            code, _ = call(self.root, "read-many", stdin=json.dumps({"names": names}).encode())
            self.assertEqual(code, 1, names[:2])
        self.assertEqual(call(self.root, "read-many", stdin=b"not json")[0], 1)
        with unittest.mock.patch.object(H, "MAX_READ_MANY_BYTES", 4):
            code, _ = call(self.root, "read-many", stdin=json.dumps(
                {"names": [f"spool/{JOB_A}/host_id", f"spool/{JOB_0}/host_id", f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json"]}).encode())
            self.assertEqual(code, 1)

    def etags(self, *names):
        return {entry["name"]: entry["etag"] for entry in json.loads(
            call(self.root, "list", "--prefix", "dispatch/jobs/")[1])["entries"] if entry["name"] in names}

    def test_delete_removes_in_order_only_while_the_etag_is_the_archived_one(self):
        names = [f"dispatch/jobs/{JOB_A}/accepted/result.json", f"dispatch/jobs/{JOB_A}/record.json"]
        etags = self.etags(*names)
        body = {"items": [{"name": name, "etag": etags[name]} for name in names]}
        code, out = call(self.root, "delete", stdin=json.dumps(body).encode())
        self.assertEqual((code, json.loads(out)), (0, {"deleted": 2, "gone": 0}))
        # The job's emptied directories go; the per-job root and the other job stay.
        self.assertFalse((self.root / f"dispatch/jobs/{JOB_A}").exists())
        self.assertTrue((self.root / f"dispatch/jobs/{JOB_B}/record.json").exists())
        self.assertTrue((self.root / "dispatch/jobs").is_dir())
        # A repeat finds them gone.
        code, out = call(self.root, "delete", stdin=json.dumps(body).encode())
        self.assertEqual((code, json.loads(out)), (0, {"deleted": 0, "gone": 2}))

    def test_delete_stops_at_a_changed_blob_and_touches_nothing_after_it(self):
        first, second = f"spool/{JOB_A}/host_id", f"spool/{JOB_A}/usnm_{JOB_A}__file_result.json"
        etags = {entry["name"]: entry["etag"] for entry in json.loads(
            call(self.root, "list", "--prefix", f"spool/{JOB_A}/")[1])["entries"]}
        # The first is replaced after it was listed (a new file, as every write makes).
        (self.root / first).unlink()
        self.put(first, b"14\n")
        body = {"items": [{"name": first, "etag": etags[first]}, {"name": second, "etag": etags[second]}]}
        code, _ = call(self.root, "delete", stdin=json.dumps(body).encode())
        self.assertEqual(code, 1)
        self.assertTrue((self.root / first).exists() and (self.root / second).exists())
        body["items"].reverse()
        code, out = call(self.root, "delete", stdin=json.dumps(body).encode())
        self.assertEqual(code, 1)
        self.assertFalse((self.root / second).exists())
        self.assertTrue((self.root / first).exists())

    def test_delete_input_is_checked(self):
        for body in (b"", b"{}", b'{"items": []}', b'{"items": [{"name": "x"}]}',
                     json.dumps({"items": [{"name": f"spool/{JOB_A}/host_id", "etag": "e", "extra": 1}]}).encode()):
            self.assertEqual(call(self.root, "delete", stdin=body)[0], 1, body)

    def test_write_creates_only_where_none_is_and_checks_the_md5(self):
        import base64
        name = f"dispatch/jobs/{JOB_B}/accepted/text.txt"
        md5 = base64.b64encode(hashlib.md5(b"hello").digest()).decode()
        code, out = call(self.root, "write", "--object", name, "--md5", md5, stdin=b"hello")
        self.assertEqual((code, json.loads(out)), (0, {"written": True}))
        self.assertEqual((self.root / name).read_bytes(), b"hello")
        code, out = call(self.root, "write", "--object", name, stdin=b"other")
        self.assertEqual((code, json.loads(out)), (0, {"written": False}))
        self.assertEqual((self.root / name).read_bytes(), b"hello")
        code, _ = call(self.root, "write", "--object", f"spool/{JOB_B}/x", "--md5", md5, stdin=b"not hello")
        self.assertEqual(code, 1)
        self.assertFalse((self.root / f"spool/{JOB_B}/x").exists())
        # No partial or temporary file is left behind.
        self.assertEqual([p.name for p in (self.root / f"dispatch/jobs/{JOB_B}/accepted").iterdir()], ["text.txt"])

    def test_the_root_must_be_private_and_canonical(self):
        self.root.chmod(0o755)
        self.assertEqual(call(self.root, "list", "--prefix", "spool/")[0], 1)
        self.root.chmod(0o700)
        link = self.root.parent / "link"
        link.symlink_to(self.root)
        self.assertEqual(call(link, "list", "--prefix", "spool/")[0], 1)


if __name__ == "__main__":
    unittest.main()
