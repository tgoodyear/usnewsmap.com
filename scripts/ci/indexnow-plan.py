#!/usr/bin/env python3
"""Compare a live sitemap with the last acknowledged IndexNow submission."""

import argparse
import datetime as dt
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from urllib.parse import urlsplit


def sitemap_urls(xml, host):
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
    return urls


def plan(xml, host, key, previous):
    urls = sitemap_urls(xml, host)
    state = {"version": 2, "key": key, "urls": urls}
    # A digest-only baseline cannot track per-URL lastmod values, so it requires
    # a complete submission before individual URLs can be tracked.
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


def same_urls(public_xml, deployed_xml, host):
    public = sitemap_urls(public_xml, host)
    deployed = sitemap_urls(deployed_xml, host)
    return public.keys() == deployed.keys()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--key")
    parser.add_argument("--state", type=Path)
    parser.add_argument("--output-state", type=Path)
    parser.add_argument("--compare", type=Path, help="require stdin sitemap to have the same URLs as this deployed sitemap")
    args = parser.parse_args()
    xml = sys.stdin.read()
    if args.compare:
        if not same_urls(xml, args.compare.read_text(), args.host):
            sys.exit(1)
        sys.exit(0)
    if not args.key or not args.state or not args.output_state:
        parser.error("--key, --state and --output-state are required unless --compare is used")
    previous = args.state
    state, changed = plan(
        xml, args.host, args.key,
        previous.read_text() if previous.exists() else None,
    )
    args.output_state.write_text(json.dumps(state, sort_keys=True) + "\n")
    for url in changed:
        print(url)
