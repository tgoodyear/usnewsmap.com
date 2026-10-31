#!/usr/bin/env python3
"""Turn the query results in data/ into the report's tables and derived data files.

Reads the CSVs that run-queries.sh writes, plus data/titles.csv (GET /v1/titles). Writes
language-bands.csv, language-mismatch.csv, title-summary.csv, title-decades.csv and
german-fraktur-pilot-candidates.csv into --data-dir, and the report's tables to --tables.
"""
import argparse
import collections
import csv
import pathlib

NUMERIC_SKIP = {"lang", "decision", "lccn"}
LANGUAGES = {
    "eng": "English", "ger": "German", "spa": "Spanish", "pol": "Polish", "fre": "French", "yid": "Yiddish",
    "ita": "Italian", "cze": "Czech", "nor": "Norwegian", "srp": "Serbian", "dan": "Danish", "fin": "Finnish",
    "lit": "Lithuanian", "slv": "Slovenian", "rus": "Russian", "swe": "Swedish", "hun": "Hungarian",
    "slo": "Slovak", "hrv": "Croatian", "rum": "Romanian", "ice": "Icelandic", "heb": "Hebrew",
    "haw": "Hawaiian", "chr": "Cherokee", "jpn": "Japanese", "cho": "Choctaw", "dak": "Dakota",
    "nav": "Navajo", "mus": "Creek",
}
NAMED = ["ger", "spa", "pol", "fre", "yid", "ita", "cze", "nor", "srp", "dan", "eng"]


def read(path):
    with open(path, newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))


def write(path, rows, columns):
    with open(path, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=columns, extrasaction="ignore")
        w.writeheader()
        w.writerows(rows)


def table(out, head, rows):
    out.append("| " + " | ".join(head) + " |")
    out.append("|" + "|".join("---" for _ in head) + "|")
    for r in rows:
        out.append("| " + " | ".join(str(x) for x in r) + " |")
    out.append("")


def n(x):
    return f"{int(x):,}"


def pct(a, b, digits=1):
    return f"{100 * a / b:.{digits}f}%" if b else "n/a"


def add(total, row):
    for k, v in row.items():
        if k not in NUMERIC_SKIP:
            total[k] += float(v or 0)


def bucket(rows, key):
    out = collections.defaultdict(lambda: collections.Counter())
    for r in rows:
        add(out[key(r)], r)
    return out


def language_bands(cube, quantiles, und_by_catalog):
    scored = [r for r in cube if r["decision"] in ("detected", "mixed")]
    by = bucket(scored, lambda r: r["lang"] if r["lang"] in NAMED else "other")
    q = {r["lang"]: r for r in quantiles}
    rows = []
    for code in NAMED + ["other"]:
        c = by[code]
        qr = q.get(code, {})
        rows.append({
            "language": LANGUAGES.get(code, "All others"), "lang": code, "pages": int(c["pages"]),
            "scored": int(c["scored"]), "fine": int(c["fine"]), "moderate": int(c["moderate"]), "bad": int(c["bad"]),
            "severe": int(c["severe"]), "weak": int(c["weak"]), "bad_weak": int(c["bad_weak"]),
            "median_damage": qr.get("damage_p50", ""), "p90_damage": qr.get("damage_p90", ""),
            "our_ocr_pages": int(c["ocr_done"]), "bad_our_ocr": int(c["bad_ocr_done"]),
            "strong_todo": int(c["strong_todo"]), "strong_todo_with_as": int(c["strong_todo_with_as"]),
            "moderate_todo": int(c["moderate_todo"]), "as_pair": int(c["as_pair"]), "as_placed": int(c["as_placed"]),
            "median_dice": qr.get("dice_p50", ""), "median_local_cer": qr.get("local_cer_p50", ""),
        })
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data-dir", required=True, type=pathlib.Path)
    ap.add_argument("--tables", required=True, type=pathlib.Path, help="Markdown file for the report's tables")
    ap.add_argument("--min-title-pages", type=int, default=1000, help="Smallest title in the share rankings")
    args = ap.parse_args()
    d = args.data_dir
    cube = read(d / "cube.csv")
    titles = {r["lccn"]: r for r in read(d / "titles.csv")}
    medians = {r["lccn"]: r for r in read(d / "title-median.csv")}
    quantiles = read(d / "language-quantiles.csv")
    coverage = read(d / "coverage.csv")[0]
    out = []

    missing = sorted({r["lccn"] for r in cube} - set(titles))
    out.append(f"Titles in the cube not in the catalog: {len(missing)} {missing[:5]}\n")
    first = lambda lccn: titles[lccn]["languages"].split("|")[0] if lccn in titles else "?"
    listed = lambda lccn: titles[lccn]["languages"].split("|") if lccn in titles else []

    # Language bands.
    bands = language_bands(cube, quantiles, None)
    write(d / "language-bands.csv", bands, list(bands[0].keys()))
    out.append("## Language bands\n")
    rows = []
    for b in bands:
        rows.append([b["language"], n(b["scored"]), b["median_damage"], f'{n(b["fine"])} ({pct(b["fine"], b["scored"])})',
                     f'{n(b["moderate"])} ({pct(b["moderate"], b["scored"])})', f'{n(b["bad"])} ({pct(b["bad"], b["scored"])})',
                     pct(b["weak"], b["scored"])])
    t = {k: sum(b[k] for b in bands) for k in ("scored", "fine", "moderate", "bad", "weak", "strong_todo", "moderate_todo", "bad_weak", "bad_our_ocr", "strong_todo_with_as", "severe", "our_ocr_pages")}
    rows.append(["All", n(t["scored"]), "", f'{n(t["fine"])} ({pct(t["fine"], t["scored"])})', f'{n(t["moderate"])} ({pct(t["moderate"], t["scored"])})',
                 f'{n(t["bad"])} ({pct(t["bad"], t["scored"])})', pct(t["weak"], t["scored"])])
    table(out, ["Language", "Scored", "Median", "Fine", "Moderate", "Bad", "Weak evidence"], rows)
    out.append(f"totals: {t}\n")

    out.append("## Worth re-OCRing\n")
    rows = []
    for b in bands:
        rows.append([b["language"], n(b["strong_todo"]), pct(b["strong_todo"], t["strong_todo"]), n(b["strong_todo_with_as"]), n(b["bad_our_ocr"]), n(b["bad_weak"]), n(b["moderate_todo"])])
    rows.append(["All", n(t["strong_todo"]), "100%", n(t["strong_todo_with_as"]), n(t["bad_our_ocr"]), n(t["bad_weak"]), n(t["moderate_todo"])])
    table(out, ["Language", "Re-OCR candidates", "Share", "Of which with AS", "Bad, already OCR'd", "Bad, weak evidence", "Moderate, unproven"], rows)

    # Agreement.
    out.append("## Agreement\n")
    rows = []
    for b in bands:
        rows.append([b["language"], n(b["as_pair"]), pct(b["as_pair"], b["pages"]), b["median_dice"] or "n/a", n(b["as_placed"]), pct(b["as_placed"], b["pages"]), b["median_local_cer"] or "n/a"])
    table(out, ["Language", "Pairs", "Share", "Median Dice", "Placed", "Share", "Median local CER"], rows)

    # Unscored pages by the title's first catalog language.
    out.append("## Unscored pages by first catalog language\n")
    unscored = bucket([r for r in cube if r["decision"] in ("und", "no_text")], lambda r: first(r["lccn"]))
    all_by_first = bucket(cube, lambda r: first(r["lccn"]))
    rows = []
    for code, c in sorted(unscored.items(), key=lambda kv: -kv[1]["pages"]):
        rows.append([LANGUAGES.get(code, code), n(all_by_first[code]["pages"]), n(c["pages"]), pct(c["pages"], all_by_first[code]["pages"]), n(c["under_20_words"]), n(c["und_100_words"])])
    table(out, ["First catalog language", "Pages", "Unscored", "Share", "Under 20 words", "Und, 100+ words"], rows[:14])
    tot = collections.Counter()
    for c in unscored.values():
        tot.update(c)
    out.append(f"unscored totals: pages {tot['pages']} under20 {tot['under_20_words']} und100 {tot['und_100_words']}\n")

    # Language labels: detected language against the title's first catalog language.
    out.append("## Page language against title language\n")
    groups = collections.defaultdict(collections.Counter)
    single = collections.Counter()
    for r in cube:
        lccn, pages, lang, decision = r["lccn"], float(r["pages"]), r["lang"], r["decision"]
        lst = listed(lccn)
        if not lst:
            continue
        code = lst[0]
        kind = "first" if lang == code else "und" if decision in ("und", "no_text") else "eng" if lang == "eng" else "listed" if lang in lst else "other"
        groups[code][kind] += pages
        groups[code]["pages"] += pages
        if decision == "mixed":
            groups[code]["mixed"] += pages
        if lang not in lst and lang not in ("eng", "und", "none"):
            single[lang] += pages
    rows, mismatch = [], []
    for code, c in sorted(groups.items(), key=lambda kv: -kv[1]["pages"]):
        rec = {"first_catalog_language": code, "pages": int(c["pages"]), "first": int(c["first"]), "english": int(c["eng"]), "other_listed": int(c["listed"]), "undetermined": int(c["und"]), "unlisted": int(c["other"]), "mixed": int(c["mixed"])}
        mismatch.append(rec)
        if code in NAMED + ["fin", "lit", "swe"]:
            rows.append([LANGUAGES.get(code, code), n(c["pages"]), pct(c["first"], c["pages"]), pct(c["eng"], c["pages"]) if code != "eng" else "", pct(c["listed"], c["pages"]), pct(c["und"], c["pages"])])
    write(d / "language-mismatch.csv", mismatch, list(mismatch[0].keys()))
    table(out, ["First catalog language", "Pages", "In that language", "English", "Other listed", "Undetermined"], rows)
    out.append(f"detected outside catalog and not English: {dict(single)}\n")

    # Titles.
    per_title = bucket(cube, lambda r: r["lccn"])
    per_title_lang = collections.defaultdict(collections.Counter)
    for r in cube:
        per_title_lang[r["lccn"]][r["lang"]] += float(r["pages"])
    per_title_und = collections.Counter()
    for r in cube:
        if r["decision"] in ("und", "no_text"):
            per_title_und[r["lccn"]] += float(r["pages"])
    title_rows = []
    for lccn, c in per_title.items():
        meta = titles.get(lccn, {})
        top = per_title_lang[lccn].most_common(1)[0][0]
        title_rows.append({
            "lccn": lccn, "title": meta.get("title", ""), "state": meta.get("state", ""), "catalog_languages": meta.get("languages", ""),
            "main_page_language": top, "pages": int(c["pages"]), "scored": int(c["scored"]), "median_damage": medians.get(lccn, {}).get("damage_p50", ""),
            "fine": int(c["fine"]), "moderate": int(c["moderate"]), "bad": int(c["bad"]), "bad_share": round(c["bad"] / c["scored"], 3) if c["scored"] else "",
            "strong_todo": int(c["strong_todo"]), "moderate_todo": int(c["moderate_todo"]), "our_ocr_pages": int(c["ocr_done"]),
            "as_pair": int(c["as_pair"]), "mean_dice": round(c["sum_dice"] / c["as_pair"], 3) if c["as_pair"] else "",
            "as_placed": int(c["as_placed"]), "mean_local_cer": round(c["sum_local_cer_placed"] / c["as_placed"], 3) if c["as_placed"] else "",
            "undetermined_or_no_text": int(per_title_und[lccn]),
        })
    title_rows.sort(key=lambda r: (-r["strong_todo"], r["lccn"]))
    write(d / "title-summary.csv", title_rows, list(title_rows[0].keys()))
    label = lambda r: f'*{r["title"]}* ({r["state"]})'
    for name, subset in (("all", title_rows), ("non-English main language", [r for r in title_rows if r["main_page_language"] != "eng"])):
        out.append(f"## Top titles by re-OCR candidates: {name}\n")
        table(out, ["Title", "LCCN", "Language", "Pages", "Median", "Bad share", "Candidates"],
              [[label(r), r["lccn"], LANGUAGES.get(r["main_page_language"], r["main_page_language"]), n(r["pages"]), r["median_damage"], f'{100 * r["bad_share"]:.1f}%', n(r["strong_todo"])] for r in subset[:15]])
    big = [r for r in title_rows if r["scored"] >= args.min_title_pages]
    big.sort(key=lambda r: (-r["bad_share"], r["lccn"]))
    out.append(f"## Top titles by bad share (at least {args.min_title_pages:,} scored pages; {len(big)} titles)\n")
    table(out, ["Title", "LCCN", "Language", "Pages", "Median", "Bad share", "Candidates"],
          [[label(r), r["lccn"], LANGUAGES.get(r["main_page_language"], r["main_page_language"]), n(r["pages"]), r["median_damage"], f'{100 * r["bad_share"]:.1f}%', n(r["strong_todo"])] for r in big[:15]])

    # Per language: its worst titles.
    out.append("## Worst titles per language\n")
    by_lang_title = bucket([r for r in cube if r["lang"] in NAMED and r["lang"] != "eng"], lambda r: (r["lang"], r["lccn"]))
    rows = []
    for code in NAMED[:-1]:
        items = sorted(((k[1], c) for k, c in by_lang_title.items() if k[0] == code and c["strong_todo"]), key=lambda kv: -kv[1]["strong_todo"])
        total = sum(c["strong_todo"] for _, c in items)
        names = "; ".join(f'*{titles[l]["title"]}* {n(c["strong_todo"])}' for l, c in items[:3] if l in titles)
        rows.append([LANGUAGES[code], n(total), len(items), names])
    table(out, ["Language", "Candidates", "Titles with any", "Largest three"], rows)

    # Concentration.
    out.append("## Concentration\n")
    cand = sorted((r["strong_todo"] for r in title_rows), reverse=True)
    total = sum(cand)
    acc, marks = 0, {}
    for i, v in enumerate(cand, 1):
        acc += v
        for m in (0.25, 0.5, 0.8):
            if m not in marks and acc >= m * total:
                marks[m] = i
    out.append(f"titles with candidates {sum(1 for v in cand if v)}; titles to reach share: {marks}; top10 {sum(cand[:10])}\n")

    # Title-decades.
    td = bucket(cube, lambda r: (r["lccn"], r["decade"]))
    td_rows = []
    for (lccn, decade), c in td.items():
        meta = titles.get(lccn, {})
        td_rows.append({"lccn": lccn, "title": meta.get("title", ""), "state": meta.get("state", ""), "decade": decade, "pages": int(c["pages"]), "scored": int(c["scored"]), "bad": int(c["bad"]),
                        "bad_share": round(c["bad"] / c["scored"], 3) if c["scored"] else "", "mean_damage": round(c["sum_damage"] / c["scored"], 3) if c["scored"] else "", "strong_todo": int(c["strong_todo"])})
    td_rows.sort(key=lambda r: (-r["strong_todo"], r["lccn"], r["decade"]))
    write(d / "title-decades.csv", td_rows, list(td_rows[0].keys()))
    out.append("## Top title-decades\n")
    table(out, ["Title", "Decade", "Pages", "Mean damage", "Bad share", "Candidates"],
          [[f'*{r["title"]}* ({r["state"]})', f'{r["decade"]}s', n(r["pages"]), r["mean_damage"], f'{100 * r["bad_share"]:.1f}%', n(r["strong_todo"])] for r in td_rows[:15]])

    # Decades per language.
    out.append("## Candidates by language and decade\n")
    ld = bucket([r for r in cube if r["decision"] in ("detected", "mixed")], lambda r: (r["lang"] if r["lang"] in NAMED else "other", int(r["decade"])))
    decades = list(range(1830, 1970, 10))
    rows = []
    for code in NAMED + ["other"]:
        rows.append([LANGUAGES.get(code, "All others")] + [n(ld[(code, x)]["strong_todo"]) if ld[(code, x)]["strong_todo"] else "" for x in decades])
    table(out, ["Language"] + [f"{x}s" for x in decades], rows)
    out.append("## Bad share by language and decade (cells with at least 500 scored pages)\n")
    rows = []
    for code in NAMED + ["other"]:
        row = [LANGUAGES.get(code, "All others")]
        for x in decades:
            c = ld[(code, x)]
            row.append(pct(c["bad"], c["scored"], 0) if c["scored"] >= 500 else "")
        rows.append(row)
    table(out, ["Language"] + [f"{x}s" for x in decades], rows)
    all_decades = bucket(cube, lambda r: int(r["decade"]))
    out.append("## All pages by decade\n")
    table(out, ["Decade", "Scored", "Bad", "Bad share", "Candidates"], [[f"{k}s", n(c["scored"]), n(c["bad"]), pct(c["bad"], c["scored"]), n(c["strong_todo"])] for k, c in sorted(all_decades.items())])

    # Batches.
    b = read(d / "batches.csv")
    big_b = [r for r in b if int(r["scored"]) >= 1000]
    big_b.sort(key=lambda r: -int(r["bad"]) / int(r["scored"]))
    out.append(f"## Batches: {len(big_b)} with 1,000+ scored pages; share above 50% bad: {sum(1 for r in big_b if int(r['bad']) > 0.5 * int(r['scored']))}\n")
    table(out, ["Batch", "Pages", "Bad share", "Mean damage", "Titles", "Decades"],
          [[f'`{r["batch"]}`', n(r["pages"]), pct(int(r["bad"]), int(r["scored"])), r["mean_damage"], r["titles"], f'{r["first_decade"]}s-{r["last_decade"]}s'] for r in big_b[:12]])
    bad_all = sorted((int(r["bad"]) for r in b), reverse=True)
    out.append(f"top 10 batches hold {sum(bad_all[:10])} of {sum(bad_all)} bad pages; top 100 {sum(bad_all[:100])}\n")

    # Damage band against agreement class.
    cross = read(d / "signal-crosstab.csv")
    group = lambda lang: lang if lang in ("eng", "ger") else "other"
    cells = collections.Counter()
    dice = collections.Counter()
    for r in cross:
        key = (group(r["lang"]), r["damage_band"], r["agreement_class"])
        cells[key] += int(r["pages"])
        dice[key] += float(r["sum_dice"] or 0)
    classes = sorted({k[2] for k in cells})
    out.append("## Damage band against agreement class\n")
    rows = []
    for g in ("eng", "ger", "other"):
        for band in ("1 fine", "2 moderate", "3 bad"):
            total_cell = sum(cells[(g, band, c)] for c in classes)
            rows.append([g, band, n(total_cell)] + [pct(cells[(g, band, c)], total_cell) for c in classes])
    table(out, ["Language", "Damage band", "Pages"] + classes, rows)

    # American Stories' own damage on the pages LoC's text scores badly on.
    as_rows = [r for r in read(d / "as-text-on-candidates.csv") if r["eligible"] == "t" and r["loc_band"] == "3 bad"]
    as_classes = sorted({r["as_band"] for r in as_rows})
    by = collections.defaultdict(collections.Counter)
    for r in as_rows:
        by[r["lang"] if r["lang"] in NAMED else "other"][r["as_band"]] += int(r["pages"])
    out.append("## Candidates by American Stories text\n")
    rows = []
    totals = collections.Counter()
    for code in NAMED + ["other"]:
        totals.update(by[code])
        rows.append([LANGUAGES.get(code, "All others"), n(sum(by[code].values()))] + [n(by[code][c]) for c in as_classes])
    rows.append(["All", n(sum(totals.values()))] + [n(totals[c]) for c in as_classes])
    table(out, ["Language", "Candidates"] + as_classes, rows)

    # Language labels by title.
    out.append("## Titles whose first catalog language is under half their pages (1,000+ pages)\n")
    rows = []
    for r in title_rows:
        lst = r["catalog_languages"].split("|")
        pages_in = per_title_lang[r["lccn"]][lst[0]]
        if r["pages"] >= 1000 and lst[0] != "eng" and pages_in < 0.5 * r["pages"]:
            top = per_title_lang[r["lccn"]].most_common(1)[0]
            rows.append((pages_in / r["pages"], [f'*{r["title"]}* ({r["state"]})', r["lccn"], r["catalog_languages"].replace("|", ", "), n(r["pages"]), pct(pages_in, r["pages"]), f'{LANGUAGES.get(top[0], top[0])} {pct(top[1], r["pages"])}']))
    rows.sort(key=lambda x: (-int(x[1][3].replace(",", ""))))
    out.append(f"{len(rows)} titles, {n(sum(int(x[1][3].replace(',', '')) for x in rows))} pages\n")
    table(out, ["Title", "LCCN", "Catalog languages", "Pages", "In first language", "Largest"], [x[1] for x in rows[:12]])
    out.append("## English-only catalog entries with the most undetermined or textless pages\n")
    eng_only = [r for r in title_rows if r["catalog_languages"] == "eng" and r["pages"] >= 1000]
    eng_only.sort(key=lambda r: -r["undetermined_or_no_text"] / r["pages"])
    table(out, ["Title", "LCCN", "Pages", "Undetermined or no text"],
          [[f'*{r["title"]}* ({r["state"]})', r["lccn"], n(r["pages"]), pct(r["undetermined_or_no_text"], r["pages"])] for r in eng_only[:8]])
    out.append("## Bad share of pages detected English, by title's first language\n")
    ge = collections.defaultdict(collections.Counter)
    for r in cube:
        code = first(r["lccn"])
        if code in NAMED and code != "eng" and r["decision"] in ("detected", "mixed"):
            key = "eng" if r["lang"] == "eng" else "own" if r["lang"] == code else "x"
            add(ge[(code, key)], r)
    table(out, ["First language", "Own pages", "Own bad", "English pages", "English bad"],
          [[LANGUAGES[c], n(ge[(c, "own")]["scored"]), pct(ge[(c, "own")]["bad"], ge[(c, "own")]["scored"]), n(ge[(c, "eng")]["scored"]), pct(ge[(c, "eng")]["bad"], ge[(c, "eng")]["scored"])] for c in NAMED[:-1]])

    # Pilot candidates.
    pilot = read(d / "german-pilot.csv")
    quota = collections.Counter(r["tier"] for r in pilot)
    pilot.sort(key=lambda r: (int(r["tier_rank"]) / quota[r["tier"]], r["tier"] != "bad"))
    for i, r in enumerate(pilot, 1):
        meta = titles.get(r["lccn"], {})
        r["rank"] = i
        r.update({"title": meta.get("title", ""), "state": meta.get("state", ""), "catalog_languages": meta.get("languages", "")})
    columns = ["rank", "tier", "tier_rank", "stratum_rank", "doc_id", "lccn", "title", "state", "catalog_languages", "date", "edition", "seq", "decade", "batch", "batch_version",
               "damage_rate", "head_plus_near", "words", "garbage_share", "function_share", "loc_wc_mean", "loc_as_dice", "has_image_features"]
    write(d / "german-fraktur-pilot-candidates.csv", pilot, columns)
    out.append("## Pilot\n")
    elig = read(d / "german-eligible.csv")
    table(out, ["Tier", "Eligible", "Strata", "Titles", "Chosen", "Titles chosen", "Strata chosen", "Median damage chosen", "Max per title", "Max per stratum"],
          [pilot_row(tier, elig, pilot) for tier in ("bad", "moderate_high")])
    decs = sorted({int(r["decade"]) for r in pilot})
    rows = []
    for x in decs:
        chosen = [r for r in pilot if int(r["decade"]) == x]
        eb = sum(int(e["eligible_pages"]) for e in elig if int(e["decade"]) == x and e["tier"] == "bad")
        em = sum(int(e["eligible_pages"]) for e in elig if int(e["decade"]) == x and e["tier"] != "bad")
        rows.append([f"{x}s", n(eb), n(sum(r["tier"] == "bad" for r in chosen)), n(em), n(sum(r["tier"] != "bad" for r in chosen)), len({r["lccn"] for r in chosen})])
    table(out, ["Decade", "Eligible bad", "Chosen bad", "Eligible moderate", "Chosen moderate", "Titles chosen"], rows)
    bytitle = collections.Counter(r["lccn"] for r in pilot)
    table(out, ["Title", "Chosen", "Of which bad", "Decades"],
          [[f'*{titles[l]["title"]}* ({titles[l]["state"]})', c, sum(r["lccn"] == l and r["tier"] == "bad" for r in pilot), len({r["decade"] for r in pilot if r["lccn"] == l})] for l, c in bytitle.most_common(10)])
    issues = len({(r["lccn"], r["date"], r["edition"]) for r in pilot})
    f1000 = pilot[:1000]
    rates = sorted(float(r["function_share"]) for r in pilot)
    words = sorted(int(r["words"]) for r in pilot)
    garb = sorted(float(r["garbage_share"]) for r in pilot)
    out.append(f"pilot words min {words[0]} median {words[2500]}; function_share min {rates[0]} median {rates[2500]}; garbage_share median {garb[2500]} max {garb[-1]}\n")
    out.append(f"top 10 titles {sum(c for _, c in bytitle.most_common(10))}\n")
    out.append(f"issues {issues}; titles {len(bytitle)}; image features {sum(r['has_image_features'] == 't' for r in pilot)}; word conf {sum(bool(r['loc_wc_mean']) for r in pilot)}; dice {sum(bool(r['loc_as_dice']) for r in pilot)}; first 1000: titles {len({r['lccn'] for r in f1000})} decades {sorted({r['decade'] for r in f1000})} bad {sum(r['tier'] == 'bad' for r in f1000)}\n")
    args.tables.write_text("\n".join(out), encoding="utf-8")


def pilot_row(tier, elig, pilot):
    e = [x for x in elig if x["tier"] == tier]
    p = [x for x in pilot if x["tier"] == tier]
    rates = sorted(float(x["damage_rate"]) for x in p)
    per_title = collections.Counter(x["lccn"] for x in p)
    per_stratum = collections.Counter((x["lccn"], x["decade"]) for x in p)
    return [tier, n(sum(int(x["eligible_pages"]) for x in e)), len(e), len({x["lccn"] for x in e}), n(len(p)), len(per_title), len(per_stratum),
            f"{rates[len(rates) // 2]:.3f}", per_title.most_common(1)[0][1], per_stratum.most_common(1)[0][1]]


if __name__ == "__main__":
    main()
