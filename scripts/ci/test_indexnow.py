"""IndexNow submits the deployed sitemap, without a local sitemap or network access."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HOST = "usnewsmap.com"
KEY = "0123456789abcdef0123456789abcdef"
NS = "http://www.sitemaps.org/schemas/sitemap/0.9"


def sitemap(urls, dates=None):
    root = ET.Element("urlset", xmlns=NS)
    for url in urls:
        entry = ET.SubElement(root, "url")
        ET.SubElement(entry, "loc").text = url
        if dates and url in dates:
            ET.SubElement(entry, "lastmod").text = dates[url]
    return ET.tostring(root, encoding="unicode")


class IndexNow(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        script = self.root / "scripts/ci/indexnow.sh"
        script.parent.mkdir(parents=True)
        shutil.copy(ROOT / "scripts/ci/indexnow.sh", script)
        shutil.copy(ROOT / "scripts/ci/indexnow-plan.py", script.parent)
        public = self.root / "web/public"
        public.mkdir(parents=True)
        (public / f"{KEY}.txt").write_text(KEY)
        (public / "robots.txt").write_text(f"Sitemap: https://{HOST}/sitemap.xml\n")
        bins = self.root / "bin"
        bins.mkdir()
        for name in ("az", "curl"):
            stub = bins / name
            stub.write_text(f"#!{sys.executable}\n" + """
import json
import os
import sys
from pathlib import Path

args = sys.argv[1:]
root = Path(os.environ["STUB_ROOT"])
if Path(sys.argv[0]).name == "az":
    print("usnewsmap.com")
elif "-X" in args:
    with (root / "posts.jsonl").open("a") as out:
        out.write(json.dumps({
            "endpoint": args[-1],
            "body": json.loads(Path(args[args.index("--data-binary") + 1][1:]).read_text()),
        }) + "\\n")
    Path(args[args.index("-o") + 1]).write_text("test response")
    count = len((root / "posts.jsonl").read_text().splitlines())
    status = os.environ.get("STUB_STATUS", "200")
    if str(count) == os.environ.get("STUB_FAIL_BATCH"):
        status = "503"
    print(status, end="")
elif args[-1].endswith("/sitemap.xml"):
    print((root / "live-sitemap.xml").read_text())
elif args[-1].endswith(".txt"):
    print(os.environ["STUB_KEY"])
else:
    raise RuntimeError(f"unexpected request: {args}")
""")
            stub.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bins}{os.pathsep}{os.environ['PATH']}",
                        APP="production", RG="production", STUB_ROOT=str(self.root), STUB_KEY=KEY,
                        INDEXNOW_STATE_FILE=str(self.root / "state/sitemap.sha256"),
                        GITHUB_EVENT_NAME="push", GITHUB_REF="refs/heads/main",
                        GITHUB_OUTPUT=str(self.root / "outputs"))

    def submit(self, xml):
        (self.root / "live-sitemap.xml").write_text(xml)
        result = subprocess.run(["bash", "scripts/ci/indexnow.sh"], cwd=self.root,
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        posts = self.root / "posts.jsonl"
        return ([json.loads(line) for line in posts.read_text().splitlines()]
                if posts.exists() else []), result.stdout

    def test_every_generated_sitemap_url_is_submitted_without_a_local_sitemap(self):
        # The deployed build, rather than examples.json or a source-tree list, is authoritative.
        urls = [f"https://{HOST}/", f"https://{HOST}/privacy"] + [
            f"https://{HOST}/search/common-query-{i}" for i in range(100)
        ]
        posts, output = self.submit(sitemap(urls))
        self.assertEqual(len(posts), 1)
        self.assertEqual(posts[0]["endpoint"], "https://api.indexnow.org/indexnow")
        self.assertEqual(posts[0]["body"], {
            "host": HOST, "key": KEY, "keyLocation": f"https://{HOST}/{KEY}.txt", "urlList": urls,
        })
        self.assertIn(f"({len(urls)} URL(s), {HOST})", output)

    def test_batches_all_urls_and_decodes_xml_entities(self):
        urls = [f"https://{HOST}/search/query-{i}" for i in range(10001)]
        urls[-1] = f"https://{HOST}/?q=gold&from=1896-01-01"
        posts, _ = self.submit(sitemap(urls))
        self.assertEqual([len(post["body"]["urlList"]) for post in posts], [10000, 1])
        self.assertEqual([url for post in posts for url in post["body"]["urlList"]], urls)

    def test_wrong_host_is_not_submitted(self):
        posts, output = self.submit(sitemap([f"https://{HOST}/", "https://example.com/"]))
        self.assertEqual(posts, [])
        self.assertIn("::warning::IndexNow:", output)

    def test_invalid_sitemap_is_not_partially_submitted(self):
        posts, output = self.submit(sitemap([f"https://{HOST}/"])[:-5])
        self.assertEqual(posts, [])
        self.assertIn("not a valid URL sitemap", output)

    def test_unverified_live_key_is_not_submitted(self):
        self.env["STUB_KEY"] = "wrong-key"
        posts, output = self.submit(sitemap([f"https://{HOST}/"]))
        self.assertEqual(posts, [])
        self.assertIn("does not hold the key", output)

    def test_rejection_warns_without_failing_the_deployment(self):
        self.env["STUB_STATUS"] = "429"
        posts, output = self.submit(sitemap([f"https://{HOST}/"]))
        self.assertEqual(len(posts), 1)
        self.assertIn("::warning::IndexNow batch 1/1: HTTP 429", output)
        self.assertFalse(Path(self.env["INDEXNOW_STATE_FILE"]).exists())

    def test_unchanged_url_set_is_not_resubmitted(self):
        urls = [f"https://{HOST}/", f"https://{HOST}/search/gold"]
        self.submit(sitemap(urls))
        posts, output = self.submit(sitemap(list(reversed(urls))))
        self.assertEqual(len(posts), 1)
        self.assertIn("sitemap URLs unchanged", output)

    def test_added_url_submits_only_the_new_url(self):
        urls = [f"https://{HOST}/"]
        self.submit(sitemap(urls))
        urls.append(f"https://{HOST}/search/new-query")
        posts, _ = self.submit(sitemap(urls))
        self.assertEqual(len(posts), 2)
        self.assertEqual(posts[-1]["body"]["urlList"], urls[-1:])
        self.assertIn("submitted=true", (self.root / "outputs").read_text())

    def test_only_changed_dates_are_submitted_and_failures_retried(self):
        urls = [f"https://{HOST}/", f"https://{HOST}/search/gold"]
        before = {url: "2026-10-25T10:00:00Z" for url in urls}
        self.submit(sitemap(urls, before))
        after = {**before, urls[1]: "2026-10-26T10:00:00Z"}
        self.env["STUB_STATUS"] = "429"
        self.submit(sitemap(urls, after))
        self.env["STUB_STATUS"] = "200"
        posts, _ = self.submit(sitemap(urls, after))
        self.assertEqual([p["body"]["urlList"] for p in posts], [urls, urls[1:], urls[1:]])
        posts, _ = self.submit(sitemap(urls, after))
        self.assertEqual(len(posts), 3)

    def test_removed_urls_are_notified(self):
        urls = [f"https://{HOST}/", f"https://{HOST}/search/old"]
        self.submit(sitemap(urls))
        posts, _ = self.submit(sitemap(urls[:1]))
        self.assertEqual(posts[-1]["body"]["urlList"], urls[1:])

    def test_partial_batch_failure_keeps_the_acknowledged_state(self):
        urls = [f"https://{HOST}/search/query-{i}" for i in range(10001)]
        self.env["STUB_FAIL_BATCH"] = "2"
        self.submit(sitemap(urls))
        self.assertFalse(Path(self.env["INDEXNOW_STATE_FILE"]).exists())
        self.env.pop("STUB_FAIL_BATCH")
        posts, _ = self.submit(sitemap(urls))
        self.assertEqual([len(p["body"]["urlList"]) for p in posts], [10000, 1, 10000, 1])
        self.assertTrue(Path(self.env["INDEXNOW_STATE_FILE"]).exists())

    def test_migrates_digest_state_and_then_preserves_dates(self):
        state = Path(self.env["INDEXNOW_STATE_FILE"])
        state.parent.mkdir()
        state.write_text("0" * 64 + "\n")
        urls = [f"https://{HOST}/"]
        self.submit(sitemap(urls, {urls[0]: "2026-10-26"}))
        posts, _ = self.submit(sitemap(urls, {urls[0]: "2026-10-26"}))
        self.assertEqual(len(posts), 1)
        self.assertEqual(json.loads(state.read_text())["version"], 2)

    def test_invalid_lastmod_does_not_acknowledge_changes(self):
        url = f"https://{HOST}/"
        posts, output = self.submit(sitemap([url], {url: "not-a-date"}))
        self.assertEqual(posts, [])
        self.assertIn("::warning::IndexNow:", output)
        self.assertFalse(Path(self.env["INDEXNOW_STATE_FILE"]).exists())

    def test_equivalent_datetimes_are_not_resubmitted(self):
        url = f"https://{HOST}/"
        self.submit(sitemap([url], {url: "2026-10-26T10:00:00Z"}))
        posts, _ = self.submit(sitemap([url], {url: "2026-10-26T06:00:00-04:00"}))
        self.assertEqual(len(posts), 1)

    def test_key_rotation_resubmits_current_urls(self):
        urls = [f"https://{HOST}/", f"https://{HOST}/search/gold"]
        self.submit(sitemap(urls))
        state = Path(self.env["INDEXNOW_STATE_FILE"])
        previous = json.loads(state.read_text())
        previous["key"] = "previous-key"
        state.write_text(json.dumps(previous))
        posts, _ = self.submit(sitemap(urls))
        self.assertEqual(posts[-1]["body"]["urlList"], urls)

    def test_scheduled_main_runs_notify_without_a_deploy(self):
        self.env["GITHUB_EVENT_NAME"] = "schedule"
        posts, _ = self.submit(sitemap([f"https://{HOST}/"]))
        self.assertEqual(len(posts), 1)

    def test_failed_submission_is_retried(self):
        xml = sitemap([f"https://{HOST}/"])
        self.env["STUB_STATUS"] = "429"
        self.submit(xml)
        self.env["STUB_STATUS"] = "200"
        posts, _ = self.submit(xml)
        self.assertEqual(len(posts), 2)
        self.assertTrue(Path(self.env["INDEXNOW_STATE_FILE"]).exists())

    def test_prs_and_branch_runs_never_submit(self):
        for event, ref in [("pull_request", "refs/pull/1175/merge"),
                           ("push", "refs/heads/feature")]:
            with self.subTest(event=event):
                self.env.update(GITHUB_EVENT_NAME=event, GITHUB_REF=ref)
                posts, output = self.submit(sitemap([f"https://{HOST}/"]))
                self.assertEqual(posts, [])
                self.assertIn("not a production main deploy", output)


if __name__ == "__main__":
    unittest.main()
