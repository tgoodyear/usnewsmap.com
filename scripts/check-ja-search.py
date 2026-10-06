#!/usr/bin/env python3
"""Check that our Japanese OCR is searchable on the live version (#139).

Run it after a release publishes. It asks the API, as a visitor would (with
Do Not Track, so the searches stay out of the search log):

1. /v1/meta names a Japanese index, and how many pages it has.
2. Common Japanese words return pages (a version without a Japanese index
   answers them 422).
3. Every hit on the first page of results is marked as our OCR (`ocr`).
4. Each issue given with --expect has hits, searched by its paper and day.
5. Each issue given with --late (OCR'd after the release read the OCR) has
   none yet: it comes with a later release.

    scripts/check-ja-search.py --expect sn87062142_1943-05-22_ed-1 --late sn94050512_1945-05-02_ed-1

Issues are named as the OCR job logs them (`<lccn>_<date>_ed-<n>`). Exits 1
if a check fails.
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

USER_AGENT = "usnm-check-ja/1 (scripts/check-ja-search.py)"
MAX_WAIT_S = 150
WORDS = ["日本", "戦争", "米国", "真珠湾", "収容所"]
# Nearly every Japanese page has one of these, so an issue's pages match.
COMMON = "の OR に OR は OR を"


def ask(base, path):
    """(status, doc), waiting out 202, 429 and 503 busy as the web app does."""
    started = time.monotonic()
    while True:
        req = urllib.request.Request(
            base + path, headers={"User-Agent": USER_AGENT, "Accept": "application/json", "DNT": "1"}
        )
        try:
            with urllib.request.urlopen(req, timeout=max(MAX_WAIT_S - (time.monotonic() - started), 1)) as r:
                status, headers, body = r.status, r.headers, r.read()
        except urllib.error.HTTPError as e:
            status, headers, body = e.code, e.headers, e.read()
        try:
            doc = json.loads(body)
        except ValueError:
            doc = None
        busy = status == 503 and isinstance(doc, dict) and doc.get("type") == "/errors/busy"
        if status not in (202, 429) and not busy:
            return status, doc
        wait = int(headers.get("Retry-After") or 2)
        if time.monotonic() - started + wait > MAX_WAIT_S:
            return "gave up", None
        time.sleep(wait)


def issue_query(issue, version):
    """The search for one issue's pages: its paper, its day, common characters."""
    lccn, date, _ = issue.split("_", 2)
    return "?" + urllib.parse.urlencode(
        {"q": COMMON, "lccn": lccn, "from": date, "to": date, "bucket": "day", "v": version}
    )


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--api", default="https://api.usnewsmap.com")
    ap.add_argument("--expect", nargs="*", default=[], help="issues the release should have")
    ap.add_argument("--late", nargs="*", default=[], help="issues it should not have yet")
    args = ap.parse_args()
    failed = []

    def check(ok, what):
        print(("ok    " if ok else "FAIL  ") + what)
        if not ok:
            failed.append(what)

    status, meta = ask(args.api, "/v1/meta")
    if status != 200:
        print(f"/v1/meta answered {status}")
        return 1
    version, ja = meta["index_version"], meta.get("ja")
    print(f"version {version}")
    check(bool(ja), f"Japanese index: {ja['indexes'] if ja else None}, {ja['pages'] if ja else 0:,} pages")

    for word in WORDS:
        q = urllib.parse.urlencode({"q": word, "v": version})
        status, doc = ask(args.api, f"/v1/aggregate?{q}")
        hits = (doc or {}).get("total", {}).get("hits") if status == 200 else None
        places = (doc or {}).get("total", {}).get("places") if status == 200 else None
        check(status == 200 and (hits or 0) > 0, f"{word}: {status}, {hits} pages in {places} places")

    status, doc = ask(args.api, "/v1/hits?" + urllib.parse.urlencode({"q": WORDS[0], "v": version}))
    items = (doc or {}).get("items", []) if status == 200 else []
    marked = [i for i in items if (i.get("ocr") or {}).get("source")]
    engines = sorted({(i.get("ocr") or {}).get("engine") or "?" for i in marked})
    check(bool(items) and len(marked) == len(items), f"{WORDS[0]} hits marked as our OCR: {len(marked)} of {len(items)} ({', '.join(engines)})")

    for issue in args.expect:
        status, doc = ask(args.api, "/v1/aggregate" + issue_query(issue, version))
        hits = (doc or {}).get("total", {}).get("hits") if status == 200 else None
        check(status == 200 and (hits or 0) > 0, f"in this release: {issue}: {status}, {hits} pages")
    for issue in args.late:
        status, doc = ask(args.api, "/v1/aggregate" + issue_query(issue, version))
        hits = (doc or {}).get("total", {}).get("hits") if status == 200 else None
        check(status == 200 and hits == 0, f"not yet: {issue}: {status}, {hits} pages")

    print(f"{len(failed)} failed" if failed else "all passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
