#!/usr/bin/env python3
# #968 metadata A/B (28 Nov 2026): every crop's title, place and date, each with its source, and
# the one sentence the metadata arm prepends. Writes ab-meta-fields.jsonl and ab-meta-prefixes.json
# in the work dir. Read-only on the sets and the LoC title catalogue.
import datetime
import json
import re
from pathlib import Path

B = Path("/tank/usnm-lake/968-dev2-20261112")
TITLES = {r["lccn"]: r for r in json.load(open("/tank/usnm-lake/reference/catalog/titles.json"))}
TPL_DAY = "This text is from {title}, {place}, printed on {date}."
TPL_YEAR = "This text is from {title}, {place}, printed in {date}."
SETS = ["de-nzz", "de-rz81", "en-op", "fi-52", "fr-52", "new-newseye-bnf", "new-newseye-nlf-fi",
        "new-newseye-nlf-sv", "sv-52"]


def day(iso):
    d = datetime.date.fromisoformat(iso)
    return f"{d.day} {d.strftime('%B')} {d.year}"


fields, prefixes, counts = [], {}, {}
for s in SETS:
    man = {}
    for line in open(B / "sets" / s / "manifest.jsonl"):
        r = json.loads(line)
        man[r["crop_id"]] = r
    for line in open(B / "sets" / s / "crops.jsonl"):
        c = json.loads(line)
        cid = c["crop_id"]
        m = man[cid]
        row = {"set": s, "crop_id": cid}
        if s == "de-nzz":
            y, mo, d = re.search(r"nzz_(\d{4})(\d{2})(\d{2})_", m["page_id"]).groups()
            row.update(title="Neue Zürcher Zeitung", title_source="dataset (NZZ black-letter GT, zenodo 3333627)",
                       place="Zürich", place_source="dataset (NZZ black-letter GT)",
                       date=day(f"{y}-{mo}-{d}"), date_precision="day",
                       date_source=f"file name {m['source_image']}")
        elif s == "de-rz81":
            y = re.match(r"(\d{4})_", m["source_image"]).group(1)
            row.update(title="Deutscher Reichsanzeiger", title_source="dataset (reichsanzeiger-gt, zenodo 10144094)",
                       place="Berlin", place_source="dataset (reichsanzeiger-gt)", date=y, date_precision="year",
                       date_source=f"file name {m['source_image']} (year_issue_page)")
        elif s == "en-op":
            lccn = m["page_id"].split("-")[2]
            t = TITLES[lccn]
            row.update(title=t["name"], title_source=f"LoC title catalogue titles.json, lccn {lccn}, name",
                       place=t["place_of_publication"],
                       place_source=f"LoC title catalogue titles.json, lccn {lccn}, place_of_publication",
                       date=day(c["date"]), date_precision="day",
                       date_source="crop record date (Chronicling America issue date)")
        else:
            if "bnf" in m["source_dataset"]:
                why = "NewsEye BnF: no title or date in the dataset's metadata"
            else:
                why = ("NewsEye NLF: the title is known only by ISSN, and the one lookup failed on 28 Nov 2026 "
                       "(portal.issn.org: bot-protection page; digi.kansalliskirjasto.fi/serial/<ISSN>: 404 "
                       "and no response)")
            row.update(excluded=True, why=why)
        if not row.get("excluded"):
            tpl = TPL_YEAR if row["date_precision"] == "year" else TPL_DAY
            row["template"] = tpl
            row["sentence"] = tpl.format(**row)
            prefixes[cid] = row["sentence"]
        key = "excluded" if row.get("excluded") else "included"
        counts.setdefault(s, {"included": 0, "excluded": 0})[key] += 1
        fields.append(row)

assert len(prefixes) == sum(v["included"] for v in counts.values())
(B / "ab-meta-fields.jsonl").write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in fields))
(B / "ab-meta-prefixes.json").write_text(json.dumps(prefixes, ensure_ascii=False, indent=0, sort_keys=True) + "\n")
print(json.dumps(counts))
print(len(prefixes), "sentences")
for s in ("de-nzz", "de-rz81", "en-op"):
    print(next(r["sentence"] for r in fields if r["set"] == s))
