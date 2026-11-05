import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("push-boinc-scan.sh")
# The JP2s the quality feed's truncation check and its Rust port are tested on (#1457).
JP2_VECTORS = {v["name"]: v for v in json.loads(
    (Path(__file__).parents[2] / "fixtures" / "jp2-truncation-vectors.json").read_text())}


class PushKeepPage(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        bin_dir = self.tmp / "bin"
        bin_dir.mkdir()
        # az: no blob exists yet, uploads succeed; every call is logged.
        az = bin_dir / "az"
        az.write_text('#!/usr/bin/env bash\necho "$*" >> "$STUB_LOG"\n'
                      '[ "$3" = show ] && exit 1\nexit 0\n')
        az.chmod(0o755)
        self.log = self.tmp / "az.log"
        self.root = self.tmp / "scans"
        self.env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "STUB_LOG": str(self.log)}

    def scan(self, page, ext, cache_set="keep", data=b"scan"):
        path = self.root / cache_set / f"{page}.{ext}"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        Path(f"{path}.json").write_text("{}")

    def run_push(self, *pages, extra=()):
        argv = [str(SCRIPT), "--account", "usnmex", "--scans-root", str(self.root)]
        for page in pages:
            argv += ["--keep-page", page]
        return subprocess.run(argv + list(extra), env=self.env, capture_output=True, text=True)

    def test_finds_the_format_and_reports_what_it_pushed(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        self.scan("nbu_gage/sn1/1890-05-03/1/8", "jpg")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/8")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            r.stdout.splitlines(),
            ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "pushed keep/nbu_gage/sn1/1890-05-03/1/8.jpg"],
        )
        uploads = [line for line in self.log.read_text().splitlines() if "upload" in line]
        self.assertEqual(len(uploads), 4)  # each scan and its sidecar

    def test_a_missing_page_is_named_and_the_others_still_go_up(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/9")
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertEqual(
            r.stdout.splitlines(),
            ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "missing nbu_gage/sn1/1890-05-03/1/9"],
        )
        self.assertEqual(len([l for l in self.log.read_text().splitlines() if "upload" in l]), 2)

    def test_a_scan_without_its_sidecar_is_missing(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        (self.root / "keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json").unlink()
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7")
        self.assertEqual(r.returncode, 3)
        self.assertEqual(r.stdout.splitlines(), ["missing nbu_gage/sn1/1890-05-03/1/7"])

    def test_a_buffer_scan_goes_up_under_its_keep_name(self):
        # #1055: the fetcher files a queued OCR request's scan in its buffer set.
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2", cache_set="buffer")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.splitlines(), ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2"])
        uploads = [line for line in self.log.read_text().splitlines() if " upload " in line]
        self.assertEqual(len(uploads), 2)
        self.assertIn(f"--name scans/keep/nbu_gage/sn1/1890-05-03/1/7.jp2 --file {self.root}/buffer/", uploads[0])
        self.assertIn("--name scans/keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json ", uploads[1])

    def test_keep_wins_over_buffer(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jpg", cache_set="buffer")
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7")
        self.assertEqual(r.stdout.splitlines(), ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2"])
        self.assertIn(f"--file {self.root}/keep/", self.log.read_text())
        self.assertNotIn("/buffer/", self.log.read_text())

    def test_to_onprem_sends_a_tar_to_the_step_command_and_nothing_to_azure(self):
        # #1116: the scans go to the BOINC host over the controller's step command.
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2", cache_set="buffer", data=b"buffer scan")
        self.scan("nbu_gage/sn1/1890-05-03/1/8", "jpg", data=b"keep scan")
        step = self.tmp / "step"
        got = self.tmp / "got.tar"
        args = self.tmp / "args"
        step.write_text(f'#!/usr/bin/env bash\necho "$*" > {args}\ncat > {got}\necho received >&2\n')
        step.chmod(0o755)
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/8", "nbu_gage/sn1/1890-05-03/1/9",
                          extra=["--to-onprem", "--onprem-env", "prod", "--onprem-step-arg", str(step),
                                 "--onprem-step-arg", "--", "--manifest", str(self.tmp / "m.parquet")])
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertEqual(r.stdout.splitlines(), [
            "pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "pushed keep/nbu_gage/sn1/1890-05-03/1/8.jpg",
            "missing nbu_gage/sn1/1890-05-03/1/9"])
        self.assertEqual(args.read_text().split(),
                         ["--", "--env", "prod", "--target", "scans", "--step", "receive", "--wait-mins", "30"])
        import tarfile
        with tarfile.open(got) as tar:
            files = {m.name: tar.extractfile(m).read() for m in tar if m.isreg()}
        self.assertEqual(set(files), {
            "keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json",
            "keep/nbu_gage/sn1/1890-05-03/1/8.jpg", "keep/nbu_gage/sn1/1890-05-03/1/8.jpg.json"})
        self.assertEqual(files["keep/nbu_gage/sn1/1890-05-03/1/7.jp2"], b"buffer scan")
        self.assertFalse(self.log.exists() and self.log.read_text().strip(), "nothing went to Azure")

    def test_to_onprem_fails_when_the_host_refuses(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2")
        step = self.tmp / "step"
        step.write_text("#!/usr/bin/env bash\ncat > /dev/null\nexit 1\n")
        step.chmod(0o755)
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", extra=["--to-onprem", "--onprem-step-arg", str(step)])
        self.assertEqual(r.returncode, 1)
        self.assertNotIn("pushed", r.stdout)

    def test_skip_truncated_names_a_jp2_cut_short_at_loc_and_pushes_the_rest(self):
        complete = JP2_VECTORS["encoded 16x16 tiles: complete"]
        cut = JP2_VECTORS["encoded 16x16 tiles: jp2c box to end of file, last 9 bytes cut"]
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2", data=bytes.fromhex(complete["hex"]))
        self.scan("nbu_gage/sn1/1890-05-03/1/8", "jp2", cache_set="buffer", data=bytes.fromhex(cut["hex"]))
        self.scan("nbu_gage/sn1/1890-05-03/1/9", "jpg", data=b"not a JP2, not checked")
        pages = ["nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/8", "nbu_gage/sn1/1890-05-03/1/9",
                 "nbu_gage/sn1/1890-05-03/1/10"]
        r = self.run_push(*pages, extra=["--skip-truncated"])
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertEqual(r.stdout.splitlines(), [
            "pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "pushed keep/nbu_gage/sn1/1890-05-03/1/9.jpg",
            "missing nbu_gage/sn1/1890-05-03/1/10",
            f"truncated nbu_gage/sn1/1890-05-03/1/8 {cut['reason']}"])
        uploads = [line for line in self.log.read_text().splitlines() if " upload " in line]
        self.assertEqual(len(uploads), 4)
        self.assertNotIn("/8.jp2", "".join(uploads))
        # Without the flag the cut-short scan goes up as before.
        self.log.unlink()
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/8")
        self.assertEqual((r.returncode, r.stdout.splitlines()), (0, ["pushed keep/nbu_gage/sn1/1890-05-03/1/8.jp2"]))

    def test_skip_truncated_keeps_a_cut_short_jp2_out_of_the_onprem_tar(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2", data=bytes.fromhex(JP2_VECTORS["complete: one tile"]["hex"]))
        cut = JP2_VECTORS["cut short: no EOC"]
        self.scan("nbu_gage/sn1/1890-05-03/1/8", "jp2", data=bytes.fromhex(cut["hex"]))
        step, got = self.tmp / "step", self.tmp / "got.tar"
        step.write_text(f"#!/usr/bin/env bash\ncat > {got}\n")
        step.chmod(0o755)
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1/8",
                          extra=["--skip-truncated", "--to-onprem", "--onprem-step-arg", str(step)])
        self.assertEqual(r.returncode, 3, r.stderr)
        self.assertEqual(r.stdout.splitlines(), [
            "pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2", f"truncated nbu_gage/sn1/1890-05-03/1/8 {cut['reason']}"])
        import tarfile
        with tarfile.open(got) as tar:
            self.assertEqual({m.name for m in tar if m.isreg()},
                             {"keep/nbu_gage/sn1/1890-05-03/1/7.jp2", "keep/nbu_gage/sn1/1890-05-03/1/7.jp2.json"})

    def test_skip_truncated_with_nothing_cut_short_pushes_everything(self):
        self.scan("nbu_gage/sn1/1890-05-03/1/7", "jp2", data=bytes.fromhex(JP2_VECTORS["complete: one tile"]["hex"]))
        r = self.run_push("nbu_gage/sn1/1890-05-03/1/7", extra=["--skip-truncated"])
        self.assertEqual((r.returncode, r.stdout.splitlines()), (0, ["pushed keep/nbu_gage/sn1/1890-05-03/1/7.jp2"]))

    def test_refuses_a_path_that_is_not_a_page(self):
        for bad in ("../x/y/1890-05-03/1/7", "nbu_gage/sn1/1890-05-03/1", "NBU/sn1/1890-05-03/1/7"):
            self.assertEqual(self.run_push(bad).returncode, 1, bad)


if __name__ == "__main__":
    unittest.main()
