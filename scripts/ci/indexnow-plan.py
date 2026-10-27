#!/usr/bin/env python3
"""Compare a live sitemap with the last acknowledged IndexNow submission."""

import datetime as dt
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from urllib.parse import urlsplit


def plan(xml, host, key, previous):
    root = ET.fromstring(xml)
    ns = "{http://www.sitemaps.org/schemas/sitemap/0.9}"
    if root.tag != ns + "urlset":
        raise ValueError("expected a sitemap urlset")
    urls = {}
    for entry in root.findall(ns + "url"):
        url = entry.findtext(ns + "loc", "").strip()
        parsed = urlsplit(url)
        if (not url or any(c.isspace() for c in url) or parsed.scheme != "https"
                or parsed.netloc != host or parsed.fragment):
            raise ValueError(f"invalid or off-host sitemap URL: {url}")
        lastmod = entry.findtext(ns + "lastmod", "").strip()
        if lastmod:
            if re.fullmatch(r"\d{4}-\d{2}-\d{2}", lastmod):
                dt.date.fromisoformat(lastmod)
            else:
                date = dt.datetime.fromisoformat(lastmod.replace("Z", "+00:00"))
                if "T" not in lastmod or date.utcoffset() is None:
                    raise ValueError(f"lastmod needs a date or timezone-qualified datetime: {lastmod}")
                lastmod = date.astimezone(dt.timezone.utc).isoformat()
        if url in urls:
            raise ValueError(f"duplicate sitemap URL: {url}")
        urls[url] = lastmod
    if not urls:
        raise ValueError("sitemap has no <loc> entries")
    state = {"version": 2, "key": key, "urls": urls}
    # Old versions stored only a digest. A migration or evicted cache submits
    # the complete current set once, then tracks individual URLs.
    if previous is None or re.fullmatch(r"[0-9a-f]{64}\s*", previous):
        return state, list(urls)
    old = json.loads(previous)
    if (old.get("version") != 2 or not isinstance(old.get("urls"), dict)
            or not isinstance(old.get("key"), str)
            or not all(isinstance(u, str) and isinstance(d, str) for u, d in old["urls"].items())):
        raise ValueError("invalid IndexNow submission state")
    for url in old["urls"]:
        parsed = urlsplit(url)
        if parsed.scheme != "https" or parsed.netloc != host or parsed.fragment:
            raise ValueError("submission state includes an off-host URL")
    if old["key"] != key:
        return state, list(urls)
    changed = [url for url, date in urls.items() if old["urls"].get(url) != date]
    changed.extend(url for url in old["urls"] if url not in urls)
    return state, changed


if __name__ == "__main__":
    host, key, previous_path, next_path = sys.argv[1:]
    previous = Path(previous_path)
    state, changed = plan(
        sys.stdin.read(), host, key,
        previous.read_text() if previous.exists() else None,
    )
    Path(next_path).write_text(json.dumps(state, sort_keys=True) + "\n")
    for url in changed:
        print(url)
