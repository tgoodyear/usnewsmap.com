#!/usr/bin/env python3
"""Grow the Czech, Serbian, Polish and Russian gold crop sets from 10 to 40 each (#968).

The first 40 crops (fixtures/ocr-gold/cze-srp-pol-rus/{manifest,crops}.jsonl, ids cze-01..10 and so on)
are frozen. This adds ids 11..40 per language to manifest-extra.jsonl and crops-extra.jsonl, with the same
schema, the same box rule as `gold.py cut` (crops.pick_box, seeded per page as f"{seed}:gold-box:{page_id}")
and the same seed (296). Every option is a named flag.

    gold_extend.py issues --out WORK/issues.jsonl
    gold_extend.py select --issues WORK/issues.jsonl --manifest fixtures/ocr-gold/cze-srp-pol-rus/manifest.jsonl \
                          --out WORK/selection-extra.jsonl [--per-language 30] [--spares 40] [--seed 296]
    gold_extend.py cut    --selection WORK/selection-extra.jsonl --manifest .../manifest.jsonl --out WORK/cze \
                          --languages cze [--replicas 4] [--per-language 30] [--first-id 11] [--seed 296]
    gold_extend.py merge  --parts WORK --manifest .../manifest.jsonl --out WORK   # parts WORK/cze, srp, pol, rus
    gold.py fetch         --manifest .../manifest.jsonl --out WORK/images       # re-cut the frozen 40

issues  Lists every digitized issue loc.gov's search has for a language (the Chronicling America collection's
        `language` facet), one row per issue: LCCN, date, edition, title. Paced as locfetch.py does.
select  Per language, a seeded order of issues, one page per issue and never an issue already in the frozen
        manifest. The survey sample has 80 pages and only one title for Czech and Serbian, so the pool is every
        issue the catalog lists for the language. The order is greedy: the next pick comes from the decade
        with the fewest crops so far (the frozen crops count), and inside the decade from the title with the
        fewest, ties broken by the seeded generator. `--spares` more rows than needed are listed, because
        `cut` skips a page that has no usable box.
cut     Walks each language's order, takes a page at random from the issue (seeded), fetches its ALTO, picks a
        box with the survey's rule, and skips the page if the box reads as English (ENGLISH_SHARE) or there
        is no box, trying up to PAGE_TRIES pages of the issue. Then it downloads the page's IIIF full-size
        JPEG, cuts the box (grayscale, quality 85) and records the row. It stops at --per-language crops.
        It is resumable: crops already in OUT/manifest-extra.jsonl and skips in OUT/skipped.jsonl are kept.
        `merge` joins the per-language directories (one `cut` process each, `--replicas 4`) and writes
        manifest-all.jsonl: the frozen manifest followed by the extra rows.

The box text is read only to refuse English columns of bilingual papers; the engine text goes into
crops-extra.jsonl's `hints` exactly like the frozen template, and never into manifest-extra.jsonl.
"""

from __future__ import annotations

import argparse
import io
import json
import random
import re
import sys
import tempfile
import urllib.error
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import gold  # noqa: E402

# crop id prefix -> (survey group, ISO 639-2 code, loc.gov language facet)
LANGUAGES = {
    "cze": ("czech", "cze", "czech"),
    "srp": ("serbian", "srp", "serbian"),
    "pol": ("polish", "pol", "polish"),
    "rus": ("russian", "rus", "russian"),
}
SEARCH = "https://www.loc.gov/collections/chronicling-america/?fa=language:{lang}&fo=json&c=160&at=results,pagination&sp={sp}"
ITEM = "https://www.loc.gov/item/{lccn}/{date}/ed-{ed}/?fo=json"
PAGE_TRIES = 3
ENGLISH_SHARE = 0.12  # a box whose words are 12% or more English function words is an English column
ENGLISH_WORDS = frozenset(
    "the and of to in that is for with was on he his by at as it this from be are have not but they which been "
    "were will has had you her she their would there what when who been into than then".split())


def english_share(text: str) -> float:
    words = re.findall(r"[a-z]+", text.lower())
    return sum(w in ENGLISH_WORDS for w in words) / len(words) if words else 0.0


# ---------------------------------------------------------------- fetching


def fetch(url: str, pacer, attempts: int = 6) -> bytes:
    """locfetch.fetch with short waits: a dropped connection mid-body is common on the image server and
    costs a 60 s wait there. A 403 still stops the run and a 404 still raises."""
    import http.client
    import time
    import urllib.error
    import urllib.request

    import locfetch

    for i in range(attempts):
        pacer.wait()
        try:
            req = urllib.request.Request(url, headers={"User-Agent": locfetch.UA, "DNT": "1"})
            with urllib.request.urlopen(req, timeout=180) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code == 403:
                raise locfetch.Blocked(f"403 from {url}") from e
            if e.code == 404:
                raise
            wait = max(min(10 * 2**i, 120), int(e.headers.get("Retry-After") or 0) if e.code in (429, 503) else 0)
        except (urllib.error.URLError, http.client.HTTPException, OSError):
            wait = min(10 * 2**i, 120)
        else:
            continue
        print(f"retry {url[:100]}: waiting {wait}s", file=sys.stderr, flush=True)
        time.sleep(wait)
    raise RuntimeError(f"giving up on {url}")


# ---------------------------------------------------------------- issues


def issue_row(r: dict, lang: str) -> dict | None:
    """One issue row from a search result, or None when it lacks a date, LCCN or edition."""
    try:
        lccn, ed, date = r["number_lccn"][0], int(r["number_edition"][0]), r["date"]
    except (KeyError, IndexError, ValueError):
        return None
    if not r.get("digitized", True) or not re.fullmatch(r"\d{4}-\d{2}-\d{2}", date or ""):
        return None
    return {"lang": lang, "lccn": lccn, "date": date, "edition": ed, "title": r.get("title"),
            "partof_title": (r.get("partof_title") or [None])[0], "languages": sorted(r.get("language") or [])}


def cmd_issues(a: argparse.Namespace) -> int:
    import locfetch

    rows, seen = [], set()
    for prefix, (_, lang, facet) in LANGUAGES.items():
        n, sp = 0, 1
        while True:
            d = json.loads(fetch(SEARCH.format(lang=facet, sp=sp), locfetch.API))
            for r in d["results"]:
                row = issue_row(r, lang)
                if row and (lang, row["lccn"], row["date"], row["edition"]) not in seen:
                    seen.add((lang, row["lccn"], row["date"], row["edition"]))
                    rows.append(row)
                    n += 1
            if not d["pagination"].get("next"):
                break
            sp += 1
        print(f"{facet}: {n} issues", file=sys.stderr, flush=True)
    gold.write_jsonl(a.out, rows)
    return 0


# ---------------------------------------------------------------- select


def order_issues(issues: list[dict], taken: list[dict], count: int, seed: int, lang: str) -> list[dict]:
    """A seeded greedy order of `count` issues: the least-used decade, then the least-used title in it.

    `taken` are the frozen crops (rows with `lccn`, `date`, `edition`): they count toward the tallies and
    their issues are never picked."""
    rnd = random.Random(f"{seed}:extend:{lang}")
    used = {(t["lccn"], t["date"], t["edition"]) for t in taken}
    pool: dict[str, dict[str, list[dict]]] = {}
    for i in sorted(issues, key=lambda i: (i["date"], i["lccn"], i["edition"])):
        if (i["lccn"], i["date"], i["edition"]) not in used:
            pool.setdefault(gold.decade(i["date"]), {}).setdefault(i["lccn"], []).append(i)
    by_decade = {d: 0 for d in pool}
    by_title: dict[str, int] = {}
    for t in taken:
        by_decade[gold.decade(t["date"])] = by_decade.get(gold.decade(t["date"]), 0) + 1
        by_title[t["lccn"]] = by_title.get(t["lccn"], 0) + 1
    out = []
    while len(out) < count:
        decades = [d for d in pool if pool[d]]
        if not decades:
            break
        low = min(by_decade[d] for d in decades)
        d = rnd.choice(sorted(x for x in decades if by_decade[x] == low))
        low = min(by_title.get(t, 0) for t in pool[d])
        t = rnd.choice(sorted(x for x in pool[d] if by_title.get(x, 0) == low))
        i = pool[d][t].pop(rnd.randrange(len(pool[d][t])))
        if not pool[d][t]:
            del pool[d][t]
        by_decade[d] += 1
        by_title[t] = by_title.get(t, 0) + 1
        out.append(i)
    return out


def cmd_select(a: argparse.Namespace) -> int:
    issues = gold.read_jsonl(a.issues)
    frozen = gold.read_jsonl(a.manifest)
    rows = []
    for prefix, (group, lang, _) in LANGUAGES.items():
        taken = [r for r in frozen if r["lang"] == lang]
        picked = order_issues([i for i in issues if i["lang"] == lang], taken, a.per_language + a.spares, a.seed, lang)
        rows += [{**i, "group": group, "prefix": prefix, "rank": n + 1} for n, i in enumerate(picked)]
        print(f"{lang}: {len(picked)} issues ordered, {len({i['lccn'] for i in picked})} titles", file=sys.stderr)
    gold.write_jsonl(a.out, rows)
    return 0


# ---------------------------------------------------------------- cut


def alto_words(xml: bytes, width: int):
    import crops

    with tempfile.NamedTemporaryFile(suffix=".xml") as f:
        f.write(xml)
        f.flush()
        return crops.alto_words(Path(f.name), width)


def cmd_cut(a: argparse.Namespace) -> int:
    import PIL
    from PIL import Image

    import crops
    import locfetch

    Image.MAX_IMAGE_PIXELS = None
    # N processes, one per language, each wait N times locfetch's gaps: together they keep its rate.
    api, tile = locfetch.Pacer(locfetch.API_GAP * a.replicas), locfetch.Pacer(locfetch.TILE_GAP * a.replicas)
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "images").mkdir(exist_ok=True)
    manifest_p, template_p, skipped_p = a.out / "manifest-extra.jsonl", a.out / "crops-extra.jsonl", a.out / "skipped.jsonl"
    manifest = gold.read_jsonl(manifest_p) if manifest_p.exists() else []
    template = gold.read_jsonl(template_p) if template_p.exists() else []
    skipped = gold.read_jsonl(skipped_p) if skipped_p.exists() else []
    done = {(r["lccn"], r["date"], r["edition"]) for r in manifest} | {(r["lccn"], r["date"], r["edition"]) for r in skipped}
    frozen = gold.read_jsonl(a.manifest)

    def skip(c: dict, why: str) -> None:
        skipped.append({"lccn": c["lccn"], "date": c["date"], "edition": c["edition"], "why": why})
        gold.write_jsonl(skipped_p, skipped)
        print(f"skipped {c['lccn']} {c['date']}: {why}", file=sys.stderr, flush=True)

    for c in gold.read_jsonl(a.selection):
        prefix = c["prefix"]
        if a.languages and prefix not in a.languages:
            continue
        have = [r for r in manifest if r["crop_id"].startswith(prefix + "-")]
        if len(have) >= a.per_language or (c["lccn"], c["date"], c["edition"]) in done:
            continue
        try:
            item = json.loads(fetch(ITEM.format(lccn=c["lccn"], date=c["date"], ed=c["edition"]), api))
        except (RuntimeError, urllib.error.HTTPError) as e:
            skip(c, f"issue JSON: {e}")
            continue
        files = (item.get("resources") or [{}])[0].get("files") or []
        images, altos = locfetch.page_images(item), locfetch.page_altos(item)
        sizes = {}
        for seq, page in enumerate(files, 1):
            jp2 = next((f for f in page if f.get("mimetype") == "image/jp2"), None)
            if jp2 and jp2.get("width") and jp2.get("height"):
                sizes[seq] = (jp2["width"], jp2["height"])
        seqs = sorted(s for s in images if s in altos and s in sizes)
        random.Random(f"{a.seed}:extend-seq:{c['lccn']}:{c['date']}:{c['edition']}").shuffle(seqs)
        found = None
        for seq in seqs[:PAGE_TRIES]:
            pid = f"{c['lccn']}_{c['date']}_ed{c['edition']}_seq{seq}"
            try:
                xml = fetch(altos[seq], tile)
            except (RuntimeError, urllib.error.HTTPError):
                continue
            loc = alto_words(xml, sizes[seq][0])
            box = crops.pick_box(loc, sizes[seq], random.Random(f"{a.seed}:gold-box:{pid}"))
            if box is None:
                continue
            text = crops.lines_in_box(loc, box)
            if english_share(text) >= ENGLISH_SHARE:
                continue
            found = (seq, pid, box, text, xml)
            break
        if not found:
            skip(c, "no usable box (none of 600+ pixels, or English text) in the pages tried")
            continue
        seq, pid, box, text, xml = found
        url = images[seq] + "/full/full/0/default.jpg"
        try:
            data = fetch(url, tile)
        except (RuntimeError, urllib.error.HTTPError) as e:
            skip(c, f"page image: {e}"[:200])
            continue
        img = Image.open(io.BytesIO(data))
        if img.size != sizes[seq]:  # the catalog's size can be a pixel off: the box rule uses the real image's
            loc = alto_words(xml, img.width)
            box = crops.pick_box(loc, img.size, random.Random(f"{a.seed}:gold-box:{pid}"))
            text = crops.lines_in_box(loc, box) if box else ""
            if box is None or english_share(text) >= ENGLISH_SHARE:
                skip(c, f"no usable box on the {img.size} image")
                continue
        n = a.first_id + len(have)
        crop_id = f"{prefix}-{n:02d}"
        crop = gold.encode_crop(img, box)
        (a.out / "images" / f"{crop_id}.jpg").write_bytes(crop)
        title = (item.get("item") or {}).get("title") or c["title"]
        row = {
            "crop_id": crop_id, "page_id": pid, "group": c["group"], "lang": c["lang"], "era": gold.decade(c["date"]),
            "stratum": f"{c['group']}/{gold.decade(c['date'])}", "lccn": c["lccn"], "date": c["date"],
            "edition": c["edition"], "seq": seq, "title": title, "box": list(box), "image": f"images/{crop_id}.jpg",
            "crop_sha256": gold.sha256(crop), "crop_bytes": len(crop), "encoder": f"Pillow {PIL.__version__}",
            "page_url": gold.loc_page_url({"lccn": c["lccn"], "date": c["date"], "edition": c["edition"], "seq": seq}),
            "page_image_url": url, "page_image_sha256": gold.sha256(data), "alto_url": altos[seq],
        }
        manifest.append(row)
        template.append({
            "crop_id": crop_id, "group": c["group"], "lang": c["lang"], "date": c["date"], "title": title,
            "image": row["image"], "hints": {"is_truth": False, "note": gold.HINT_NOTE, "texts": {"loc": text}},
            "transcription": None, "transcriber": None, "reviewer": None, "hint_copy_reviewed": False, "notes": "",
        })
        gold.write_jsonl(manifest_p, manifest)
        gold.write_jsonl(template_p, template)
        print(f"{crop_id} {pid} {box}", file=sys.stderr, flush=True)
    wanted = a.languages or list(LANGUAGES)
    tally = {p: sum(r["crop_id"].startswith(p + "-") for r in manifest) for p in wanted}
    print(f"{len(manifest)} extra crops {tally}")
    return 0 if all(v >= a.per_language for v in tally.values()) else 1


def cmd_merge(a: argparse.Namespace) -> int:
    """Join the per-language WORK dirs (each cut with --languages) into one: the extra manifest and template
    in language order, the images, and manifest-all.jsonl (the frozen manifest, then the extra rows)."""
    import shutil

    manifest, template = [], []
    (a.out / "images").mkdir(parents=True, exist_ok=True)
    for prefix in LANGUAGES:
        part = a.parts / prefix
        for name, rows in (("manifest-extra.jsonl", manifest), ("crops-extra.jsonl", template)):
            if (part / name).exists():
                rows += gold.read_jsonl(part / name)
        for img in sorted((part / "images").glob("*.jpg")):
            shutil.copy2(img, a.out / "images" / img.name)
    gold.write_jsonl(a.out / "manifest-extra.jsonl", manifest)
    gold.write_jsonl(a.out / "crops-extra.jsonl", template)
    gold.write_jsonl(a.out / "manifest-all.jsonl", gold.read_jsonl(a.manifest) + manifest)
    problems = check_extra(manifest, template)
    print(f"{len(manifest)} extra crops merged" + "".join(f"\nPROBLEM {x}" for x in problems))
    return 1 if problems else 0


def check_extra(manifest: list[dict], template: list[dict]) -> list[str]:
    """Problems with the extra rows: ids, keys, and the rule that the manifest carries no engine text."""
    bad = []
    for r in manifest:
        if "hints" in r or "texts" in r:
            bad.append(f"{r['crop_id']}: engine text in the manifest")
    if [r["crop_id"] for r in manifest] != [r["crop_id"] for r in template]:
        bad.append("manifest and template ids differ")
    if len({r["page_id"] for r in manifest}) != len(manifest):
        bad.append("a page is used twice")
    return bad


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("issues")
    i.add_argument("--out", type=Path, required=True)
    s = sub.add_parser("select")
    s.add_argument("--issues", type=Path, required=True)
    s.add_argument("--manifest", type=Path, required=True, help="the frozen manifest.jsonl")
    s.add_argument("--out", type=Path, required=True)
    s.add_argument("--per-language", type=int, default=30)
    s.add_argument("--spares", type=int, default=40)
    s.add_argument("--seed", type=int, default=gold.DEFAULT_SEED)
    c = sub.add_parser("cut")
    c.add_argument("--selection", type=Path, required=True)
    c.add_argument("--manifest", type=Path, required=True, help="the frozen manifest.jsonl")
    c.add_argument("--out", type=Path, required=True)
    c.add_argument("--per-language", type=int, default=30)
    c.add_argument("--first-id", type=int, default=11)
    c.add_argument("--seed", type=int, default=gold.DEFAULT_SEED)
    c.add_argument("--languages", nargs="*", choices=list(LANGUAGES), help="crop id prefixes to cut (default all)")
    c.add_argument("--replicas", type=int, default=1, help="processes cutting at once; each waits this times the gaps")
    m = sub.add_parser("merge")
    m.add_argument("--parts", type=Path, required=True, help="dir holding one cut --out dir per language prefix")
    m.add_argument("--manifest", type=Path, required=True, help="the frozen manifest.jsonl")
    m.add_argument("--out", type=Path, required=True)
    a = ap.parse_args(argv)
    return {"issues": cmd_issues, "select": cmd_select, "cut": cmd_cut, "merge": cmd_merge}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
