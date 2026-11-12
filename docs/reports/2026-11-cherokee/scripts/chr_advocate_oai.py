"""Count the Cherokee Advocate's holdings from the Gateway to Oklahoma History's OAI-PMH metadata.

Metadata only: no images, no OCR text. Pages through ListRecords for one set, follows resumption
tokens, and writes counts of issues, pages and years as JSON.

Pace and safety: waits at least 3 seconds between requests (the site's robots.txt asks for a
crawl delay of 3), makes no retries, and stops on any HTTP error (including 429 and redirects), any OAI-PMH
error, or the request cap.

Usage:
  chr_advocate_oai.py --set-name "Cherokee Advocate" --out advocate-holdings.json
  chr_advocate_oai.py --list-sets          # print the endpoint's sets and exit
  chr_advocate_oai.py --set SPEC ...       # skip the ListSets lookup
"""
import argparse, collections, datetime, json, os, re, sys, time
import urllib.error, urllib.parse, urllib.request
import xml.etree.ElementTree as ET

ENDPOINT = "https://gateway.okhistory.org/oai/"
USER_AGENT = "usnewsmap.com-research-script/1.0 (+https://usnewsmap.com; OAI-PMH metadata only)"
MIN_DELAY = 3.0
OAI = "{http://www.openarchives.org/OAI/2.0/}"
DC = "{http://purl.org/dc/elements/1.1/}"
NUMBER_WORDS = {w: i for i, w in enumerate(
    "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen sixteen".split())}
# The Gateway writes the extent first in dc:format, as words ("four pages : ill. ; page 20 x 14 in.") or
# digits ("4 p."). Anchor at the start so the "page 20 x 14 in." size note is not read as a count.
PAGES_RE = re.compile(r"^\s*(\d+|" + "|".join(NUMBER_WORDS) + r")\s*(?:pages?\b|pp?\.)", re.I)


class HarvestError(Exception):
    pass


def build_url(endpoint, verb, token=None, **params):
    # Per the OAI-PMH spec, a resumptionToken is the only argument besides the verb.
    q = {"verb": verb}
    if token:
        q["resumptionToken"] = token
    else:
        q.update({k: v for k, v in params.items() if v})
    return endpoint + "?" + urllib.parse.urlencode(q)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """A redirect would be a second request outside the pacing and the request cap: treat it as an error."""
    def redirect_request(self, *args, **kwargs):
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def fetch_url(url, timeout=60):
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Accept": "text/xml, application/xml"})
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            return r.read()
    except urllib.error.HTTPError as e:
        raise HarvestError(f"HTTP {e.code} for {url}") from None
    except urllib.error.URLError as e:
        raise HarvestError(f"request failed for {url}: {e.reason}") from None


def parse_page(body, list_tag):
    """Return (items, resumption_token) for one ListSets or ListRecords response."""
    root = ET.fromstring(body)
    err = root.find(OAI + "error")
    if err is not None:
        raise HarvestError(f"OAI-PMH error {err.get('code')}: {(err.text or '').strip()}")
    lst = root.find(OAI + list_tag)
    if lst is None:
        raise HarvestError(f"response has no {list_tag} element")
    rt = lst.find(OAI + "resumptionToken")
    token = (rt.text or "").strip() if rt is not None else ""
    if list_tag == "ListSets":
        items = [{"spec": (s.findtext(OAI + "setSpec") or "").strip(),
                  "name": (s.findtext(OAI + "setName") or "").strip()} for s in lst.findall(OAI + "set")]
    else:
        items = [parse_record(r) for r in lst.findall(OAI + "record")]
    return items, token or None


def parse_record(rec):
    head = rec.find(OAI + "header")
    deleted = head is not None and head.get("status") == "deleted"
    out = {"identifier": (head.findtext(OAI + "identifier") if head is not None else "") or "",
           "deleted": deleted, "year": None, "pages": None, "title": ""}
    dc = rec.find(OAI + "metadata")
    if dc is None or deleted:
        return out
    vals = lambda name: [(e.text or "").strip() for e in dc.iter(DC + name)]
    out["title"] = next(iter(vals("title")), "")
    for d in vals("date"):
        m = re.search(r"\b(1[0-9]{3}|20[0-9]{2})\b", d)
        if m:
            out["year"] = int(m.group(1)); break
    for v in vals("format") + vals("description"):
        m = PAGES_RE.match(v)
        if m:
            n = m.group(1).lower()
            out["pages"] = int(n) if n.isdigit() else NUMBER_WORDS[n]; break
    return out


def summarize(records):
    live = [r for r in records if not r["deleted"]]
    years = collections.defaultdict(lambda: {"issues": 0, "pages": 0})
    for r in live:
        if r["year"] is not None:
            y = years[str(r["year"])]
            y["issues"] += 1
            y["pages"] += r["pages"] or 0
    ys = sorted(int(y) for y in years)
    return {
        "deleted_records_skipped": len(records) - len(live),
        "issues": len(live),
        "issues_with_page_count": sum(1 for r in live if r["pages"] is not None),
        "issues_without_year": sum(1 for r in live if r["year"] is None),
        "pages": sum(r["pages"] or 0 for r in live),
        "year_first": ys[0] if ys else None,
        "year_last": ys[-1] if ys else None,
        "years": {y: years[y] for y in sorted(years)},
    }


class Harvester:
    def __init__(self, endpoint, delay, max_requests, fetch=fetch_url, sleep=time.sleep, clock=time.monotonic,
                 raw_dir=None):
        if not delay >= MIN_DELAY:  # also rejects NaN
            raise HarvestError(f"--delay-seconds must be at least {MIN_DELAY:g}")
        self.endpoint, self.delay, self.max_requests = endpoint, delay, max_requests
        self.fetch, self.sleep, self.clock, self.raw_dir = fetch, sleep, clock, raw_dir
        self.requests, self._last = 0, None

    def get(self, url):
        if self.requests >= self.max_requests:
            raise HarvestError(f"stopped at --max-requests {self.max_requests}")
        if self._last is not None:
            wait = self.delay - (self.clock() - self._last)
            if wait > 0:
                self.sleep(wait)
        self._last = self.clock()
        self.requests += 1
        body = self.fetch(url)
        if self.raw_dir:
            os.makedirs(self.raw_dir, exist_ok=True)
            with open(os.path.join(self.raw_dir, f"{self.requests:04d}.xml"), "wb") as f:
                f.write(body)
        return body

    def pages(self, verb, list_tag, **params):
        token = None
        while True:
            body = self.get(build_url(self.endpoint, verb, token, **params))
            items, token = parse_page(body, list_tag)
            yield items
            if not token:
                return

    def list_sets(self):
        return [s for page in self.pages("ListSets", "ListSets") for s in page]

    def list_records(self, set_spec, prefix):
        return [r for page in self.pages("ListRecords", "ListRecords", set=set_spec, metadataPrefix=prefix)
                for r in page]


def resolve_set(sets, name):
    hits = [s for s in sets if name.lower() in s["name"].lower()]
    if len(hits) != 1:
        raise HarvestError(f"--set-name {name!r} matches {len(hits)} sets: {[h['spec'] for h in hits]}; use --set")
    return hits[0]


def main(argv=None, fetch=fetch_url, sleep=time.sleep):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--endpoint", default=ENDPOINT)
    ap.add_argument("--set", dest="set_spec", help="OAI setSpec of the collection")
    ap.add_argument("--set-name", help="find the set whose name contains this text (via ListSets)")
    ap.add_argument("--list-sets", action="store_true", help="print the sets and exit")
    ap.add_argument("--metadata-prefix", default="oai_dc")
    ap.add_argument("--delay-seconds", type=float, default=MIN_DELAY, help="pause between requests (minimum 3)")
    ap.add_argument("--max-requests", type=int, default=200)
    ap.add_argument("--raw-dir", help="also save each response body here")
    ap.add_argument("--out", help="write the counts JSON here as well as to standard output")
    a = ap.parse_args(argv)
    try:
        h = Harvester(a.endpoint, a.delay_seconds, a.max_requests, fetch=fetch, sleep=sleep, raw_dir=a.raw_dir)
        if a.list_sets:
            print(json.dumps(h.list_sets(), indent=1, ensure_ascii=False)); return 0
        spec, set_name = a.set_spec, None
        if not spec:
            if not a.set_name:
                raise HarvestError("give --set or --set-name")
            found = resolve_set(h.list_sets(), a.set_name)
            spec, set_name = found["spec"], found["name"]
        result = summarize(h.list_records(spec, a.metadata_prefix))
    except HarvestError as e:
        print(f"error: {e}", file=sys.stderr); return 1
    result.update({"endpoint": a.endpoint, "harvested_on": datetime.date.today().isoformat(),
                   "metadata_prefix": a.metadata_prefix, "requests": h.requests,
                   "set_name": set_name, "set_spec": spec})
    text = json.dumps(result, indent=1, sort_keys=True, ensure_ascii=False) + "\n"
    if a.out:
        with open(a.out, "w", encoding="utf-8") as f:
            f.write(text)
    sys.stdout.write(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
