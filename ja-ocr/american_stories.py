"""Compare American Stories' text with LoC's on our pages (#205).

    python3 jaocr.py american-stories --year 1865 --year 1925 [--sample-pct 10]

American Stories (Dell et al. 2023, arXiv:2308.12477; CC BY 4.0) re-read
Chronicling America with layout detection, a legibility classifier and
EfficientOCR. Each year is one tarball on Hugging Face of one JSON file per
scan, named <date>_p<page>_<lccn>_<reel>_<date><edition>_<image>.json, whose
p<page> is our seq-<n>. This streams each year's tarball (nothing is kept on
disk), keeps the scans in our usual fixed sample (quality.sampled: a hash of
the doc_id, so the same pages as the OCR quality audit), reads LoC's text for
the sampled pages of those years from the curated store, and measures, for
pages of titles whose first catalog language is English:

- join: our sampled pages in the year with an American Stories scan, and the
  sampled scans with no page of ours;
- quality: words per page, and the audit's English damage rate and function
  word share (metric v2) of each text;
- recall: for a fixed list of terms, the pages whose LoC text has the term,
  whose American Stories text has it, and either;
- legibility: the share of a page's text regions American Stories marked
  Illegible (it doesn't OCR those), against LoC's damage rate.

Writes audit/american-stories-<version>-<execution>.json and logs each row
as an "american stories" line. One replica of the execution works (solo.py).
"""

from __future__ import annotations

import io
import json
import os
import re
import statistics
import tarfile
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import jaocr
import quality
from solo import Solo

URL = "https://huggingface.co/datasets/dell-research-harvard/AmericanStories/resolve/main/faro_{year}.tar.gz"
NAME = re.compile(r"^(\d{4}-\d\d-\d\d)_p(\d+)_([a-z]+\d+)_")
BATCH = re.compile(r"/batches/([a-z0-9_]+?)(?:_ver\d+)?/")
TEXT_CLASSES = {"article", "headline", "byline", "caption"}
TERMS = ("lincoln", "railroad", "president", "election", "cotton", "gold", "fever", "telegraph", "slavery",
         "steamboat", "church", "automobile", "radio", "prohibition", "farmers")
DAMAGE_BANDS = (0.1, 0.25)  # quality.DAMAGED, quality.BADLY_DAMAGED
LEGIBLE_BANDS = (0.25, 0.5, 0.75)  # share of text regions Illegible
READERS = 16
FIRST_YEAR, LAST_YEAR = 1774, 1963  # the dataset's years


def doc_id(name: str, edition: str) -> str | None:
    """Our doc_id for a scan file, or None if its name doesn't parse."""
    m = NAME.match(os.path.basename(name))
    ed = re.fullmatch(r"ed-0*(\d+)", edition or "")
    if not m or not ed:
        return None
    date, page, lccn = m.groups()
    return f"{lccn}_{date}_ed-{int(ed.group(1))}_seq-{int(page)}"


def scan_text(scan: dict) -> tuple[str, dict[str, int]]:
    """A scan's text (headline, byline and article of each article) and its text regions by legibility."""
    parts = []
    for a in scan.get("full articles") or []:
        parts.extend(t for t in (a.get("headline"), a.get("byline"), a.get("article")) if t)
    legibility: dict[str, int] = {}
    for b in scan.get("bboxes") or []:
        if b.get("class") in TEXT_CLASSES:
            k = (b.get("legibility") or "unknown").lower()
            legibility[k] = legibility.get(k, 0) + 1
    return "\n".join(parts), legibility


def stream_year(year: int, cut: int, opener=None, keep=lambda: None) -> tuple[dict, dict]:
    """(sampled scans by doc_id: {batch, text, legibility}, totals) for one year's tarball."""
    req = urllib.request.Request(URL.format(year=year), headers={"User-Agent": jaocr.UA, "DNT": "1"})
    out, totals = {}, {"scans": 0, "unparsed": 0, "sampled": 0}
    with (opener or urllib.request.urlopen)(req) as resp, tarfile.open(fileobj=resp, mode="r|gz") as tar:
        for member in tar:
            if not (member.isfile() and member.name.endswith(".json")):
                continue
            keep()
            scan = json.load(tar.extractfile(member))
            totals["scans"] += 1
            d = doc_id(member.name, (scan.get("edition") or {}).get("edition", ""))
            if d is None:
                totals["unparsed"] += 1
                continue
            if not quality.sampled(d, cut):
                continue
            m = BATCH.search((scan.get("scan") or {}).get("raw_data_loc") or "")
            text, legibility = scan_text(scan)
            out[d] = {"batch": m.group(1) if m else None, "text": text, "legibility": legibility}
    totals["sampled"] = len(out)
    return out, totals


def loc_pages(curated, batches: list[dict], years: set[int], cut: int, keep=lambda: None) -> dict[str, dict]:
    """LoC's text for our sampled pages dated in `years`: {doc_id: {lccn, year, batch, text}}."""
    import pyarrow.parquet as pq

    picked = [b for b in batches if (c := b.get("curated") or {}).get("parts")
              and any(int(c.get("first", "0")[:4] or 0) <= y <= int(c.get("last", "9999")[:4] or 9999)
                      for y in years)]
    parts = [p for b in picked for p in b["curated"]["parts"]]
    out: dict[str, dict] = {}
    with ThreadPoolExecutor(READERS) as ex:
        for data in ex.map(curated.read, parts):
            keep()
            t = pq.read_table(io.BytesIO(data), columns=["doc_id", "lccn", "date", "batch", "text_status", "text"])
            for r in t.to_pylist():
                if r["date"].year in years and quality.sampled(r["doc_id"], cut):
                    out[r["doc_id"]] = {"lccn": r["lccn"], "year": r["date"].year, "batch": r["batch"],
                                        "text": r["text"] if r["text_status"] == "ok" else ""}
    return out


def terms_in(words: list[str]) -> set[str]:
    return set(words) & set(TERMS)


def band(x: float | None, edges: tuple) -> str:
    if x is None:
        return "none"
    for i, e in enumerate(edges):
        if x < e:
            return f"lt_{e}" if i == 0 else f"{edges[i - 1]}-{e}"
    return f"ge_{edges[-1]}"


def median(xs: list[float]) -> float | None:
    return round(statistics.median(xs), 4) if xs else None


def share(n: int, d: int) -> float | None:
    return round(n / d, 4) if d else None


def compare(year: int, ours: dict[str, dict], theirs: dict[str, dict], english: set[str], models) -> dict:
    """The year's rows: summary, recall by term, LoC damage by legibility band."""
    mine = {d: p for d, p in ours.items() if p["year"] == year and p["lccn"] in english}
    both = [d for d in mine if d in theirs]
    loc_damage, as_damage, loc_fs, as_fs, loc_words, as_words = [], [], [], [], [], []
    loc_bad = as_bad = 0  # of the pages with a damage rate (the audit leaves out the rest too)
    hits = {t: {"loc": 0, "american_stories": 0, "either": 0, "only_american_stories": 0} for t in TERMS}
    by_legibility: dict[str, list[float]] = {}
    for d in both:
        lt, at = mine[d]["text"] or "", theirs[d]["text"] or ""
        ls, as_ = quality.score_v2(lt, ("eng",), models), quality.score_v2(at, ("eng",), models)
        loc_words.append(ls["words"])
        as_words.append(as_["words"])
        for s, dmg, fs in ((ls, loc_damage, loc_fs), (as_, as_damage, as_fs)):
            if s["damage_rate"] is not None:
                dmg.append(s["damage_rate"])
            if s["function_share"] is not None:
                fs.append(s["function_share"])
        loc_bad += (ls["damage_rate"] or 0) > quality.BADLY_DAMAGED
        as_bad += (as_["damage_rate"] or 0) > quality.BADLY_DAMAGED
        lw, aw = terms_in(quality.text_words(lt)[1]), terms_in(quality.text_words(at)[1])
        for t in TERMS:
            h = hits[t]
            h["loc"] += t in lw
            h["american_stories"] += t in aw
            h["either"] += t in lw or t in aw
            h["only_american_stories"] += t in aw and t not in lw
        leg = theirs[d]["legibility"]
        regions = sum(leg.values())
        if ls["damage_rate"] is not None:
            by_legibility.setdefault(band(share(leg.get("illegible", 0), regions), LEGIBLE_BANDS), []).append(
                ls["damage_rate"])
    theirs_year = [d for d in theirs if d.split("_")[1][:4] == str(year)]
    summary = {
        "year": year,
        "our_sampled_english_pages": len(mine),
        "with_american_stories": len(both),
        "join_share": share(len(both), len(mine)),
        "american_stories_sampled_scans": len(theirs_year),
        "american_stories_without_our_page": sum(1 for d in theirs_year if d not in ours),
        "words_median_loc": median(loc_words), "words_median_american_stories": median(as_words),
        "words_total_ratio": share(sum(as_words), sum(loc_words)),
        "damage_median_loc": median(loc_damage), "damage_median_american_stories": median(as_damage),
        "scored_pages_loc": len(loc_damage), "scored_pages_american_stories": len(as_damage),
        "badly_damaged_share_loc": share(loc_bad, len(loc_damage)),
        "badly_damaged_share_american_stories": share(as_bad, len(as_damage)),
        "function_share_median_loc": median(loc_fs), "function_share_median_american_stories": median(as_fs),
    }
    recall = [{"year": year, "term": t, **h,
               "gain_share": share(h["only_american_stories"], h["loc"])} for t, h in hits.items()]
    legibility = [{"year": year, "illegible_regions": b, "pages": len(v), "loc_damage_median": median(v)}
                  for b, v in sorted(by_legibility.items())]
    return {"summary": summary, "recall": recall, "legibility": legibility}


def american_stories(reference, curated, years: list[int], sample_pct: float = 10.0, run: str | None = None,
                     opener=None, loader=None) -> dict | None:
    current = json.loads(reference.read("current.json"))
    version = current["reference"]
    if not jaocr.SAFE_SEGMENT.match(version):
        raise ValueError(f"current.json names an unsafe reference version: {version!r}")
    if not years or any(not FIRST_YEAR <= y <= LAST_YEAR for y in years):
        raise ValueError(f"years must be {FIRST_YEAR} to {LAST_YEAR} (American Stories' range), not {years}")
    if not 0 < sample_pct <= 100:
        raise ValueError(f"--sample-pct must be above 0 and at most 100, not {sample_pct}")
    cut = round(sample_pct * 100)
    if cut < 1:
        raise ValueError(f"--sample-pct {sample_pct} samples nothing: use at least 0.01")
    solo = Solo(curated, f"audit/american-stories-{version}", "american stories", run)
    if not solo.start():
        return None
    titles = quality.load_titles(reference, version)
    english = {lccn for lccn, t in titles.items() if (t.get("languages") or ["eng"])[0] == "eng"}
    jaocr.log("american stories starting", version=version, years=years, sample_pct=sample_pct, run=solo.run)
    theirs, totals = {}, {}
    for y in years:
        got, totals[y] = stream_year(y, cut, opener, solo.keep)
        theirs.update(got)
        jaocr.log("american stories year read", year=y, **totals[y])
    ours = loc_pages(curated, json.loads(reference.read(f"{version}/batches.json")), set(years), cut, solo.keep)
    models = quality.Models(loader)
    tables: dict[str, list] = {"summary": [], "recall": [], "legibility": []}
    for y in years:
        rows = compare(y, ours, theirs, english, models)
        tables["summary"].append({**rows["summary"], **{f"scans_{k}": v for k, v in totals[y].items()}})
        tables["recall"] += rows["recall"]
        tables["legibility"] += rows["legibility"]
    report = {"version": version, "years": years, "sample_pct": sample_pct, "terms": TERMS, **tables,
              "source": URL.format(year="<year>"), "license": "CC BY 4.0 (Dell et al. 2023)"}
    base = f"audit/american-stories-{version}-{solo.run}"
    curated.write(f"{base}.json", json.dumps(report, ensure_ascii=False, indent=1).encode())
    for name, rows in tables.items():
        for r in rows:
            jaocr.log("american stories", table=name, version=version, **r)
    solo.finish()
    jaocr.log("american stories finished", version=version, years=years, outputs=[f"{base}.json"])
    return report
